//! Bidirectional type inference engine — T-AIR pass.
//!
//! This module implements the type checker / inference engine for Bock.
//! It walks an [`AIRNode`] module produced by the S-AIR lowering pass and:
//!
//! - **Synthesizes** types for expressions bottom-up (`infer_expr`).
//! - **Checks** expressions against an expected type top-down (`check_expr`).
//! - Annotates every node with a [`TypeInfo`] and records the resolved
//!   [`Type`] in an internal side-table keyed by [`NodeId`].
//!
//! # Architecture
//!
//! `check_module` performs a two-sub-pass approach:
//! 1. **Collect** — all top-level function signatures are entered into the
//!    type environment so that mutually-recursive calls resolve.
//! 2. **Check** — each top-level item is fully type-checked.
//!
//! During checking, the internal `infer_node` / `check_node` helpers
//! recursively walk the AIR tree via `&mut AIRNode`, recording types in
//! the side-table and stamping `type_info` on every node.
//!
//! The public `infer_expr` / `check_expr` methods provide read-only access
//! to the inference result for single nodes (no mutation — useful in tests
//! and for downstream passes that want to query type of a specific node).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};

use bock_air::stubs::{TypeInfo, Value};
use bock_air::{AIRNode, EnumVariantPayload, NodeId, NodeKind};
use bock_ast::{BinOp, Literal, TypeConstraint, TypeExpr, TypePath, UnaryOp};
use bock_errors::{DiagnosticBag, DiagnosticCode, Span};

use crate::traits::{resolve_impl, ImplTable, TraitRef};
use crate::{unify, EffectRef, FnType, GenericType, PrimitiveType, Substitution, Type, TypeVarId};

// ─── Diagnostic codes ─────────────────────────────────────────────────────────

const E_TYPE_MISMATCH: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 4001,
};
const E_UNDEFINED_VAR: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 4002,
};
const E_ARITY_MISMATCH: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 4003,
};
const E_NOT_CALLABLE: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 4004,
};
const E_WHERE_CLAUSE: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 4005,
};

// ─── TypeVarId generator ──────────────────────────────────────────────────────

/// Generates unique [`TypeVarId`]s across a compilation session.
struct TypeVarGen {
    counter: AtomicU32,
}

impl TypeVarGen {
    fn new() -> Self {
        Self {
            counter: AtomicU32::new(0),
        }
    }

    fn next(&self) -> TypeVarId {
        self.counter.fetch_add(1, Ordering::SeqCst)
    }
}

// ─── TypeEnv ──────────────────────────────────────────────────────────────────

/// Scoped type environment: maps variable/function names to their [`Type`]s.
///
/// Maintains a stack of scopes; inner scopes shadow outer ones.
pub struct TypeEnv {
    scopes: Vec<HashMap<String, Type>>,
}

impl TypeEnv {
    /// Create a new environment with a single (global) scope.
    #[must_use]
    pub fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
        }
    }

    /// Push a new inner scope onto the stack.
    pub fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    /// Pop the innermost scope from the stack.
    ///
    /// Panics in debug builds if the global scope would be popped.
    pub fn pop_scope(&mut self) {
        debug_assert!(self.scopes.len() > 1, "cannot pop the global scope");
        self.scopes.pop();
    }

    /// Bind `name` to `ty` in the current (innermost) scope.
    pub fn define(&mut self, name: impl Into<String>, ty: Type) {
        let scope = self.scopes.last_mut().expect("at least one scope");
        scope.insert(name.into(), ty);
    }

    /// Look up `name`, searching from the innermost scope outward.
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<&Type> {
        self.scopes.iter().rev().find_map(|s| s.get(name))
    }
}

impl Default for TypeEnv {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Function signature ───────────────────────────────────────────────────────

/// Cached function signature for use during call-site type checking.
#[derive(Debug, Clone)]
struct FnSig {
    /// Names of the generic type parameters (e.g. `["T", "U"]`).
    generic_params: Vec<String>,
    /// [`TypeVarId`]s assigned to each generic parameter during signature
    /// collection. Used by [`TypeChecker::replace_type_vars`] to create
    /// per-call-site fresh instantiations.
    generic_var_ids: Vec<TypeVarId>,
    /// Types of the value parameters (after substituting generic parameters
    /// with fresh type variables when the function is instantiated).
    param_types: Vec<Type>,
    /// Return type.
    return_type: Type,
    /// Where-clause constraints (trait bounds on generic parameters).
    where_clause: Vec<TypeConstraint>,
}

// ─── TypeChecker ──────────────────────────────────────────────────────────────

/// Bidirectional type checker / inference engine.
///
/// # Usage
/// ```ignore
/// let mut checker = TypeChecker::new();
/// checker.check_module(&mut air_module);
/// // inspect checker.diags for errors
/// // query checker.type_of(node_id) for resolved types
/// ```
pub struct TypeChecker {
    /// Scoped variable / function type environment.
    pub env: TypeEnv,
    /// Accumulated type-variable substitution from unification.
    pub subst: Substitution,
    /// Diagnostics emitted during type checking.
    pub diags: DiagnosticBag,
    /// Generator for fresh type variable ids.
    var_gen: TypeVarGen,
    /// Side-table: resolved type for each AIR node id.
    types: HashMap<NodeId, Type>,
    /// Known function signatures (populated during the collection phase).
    fn_sigs: HashMap<String, FnSig>,
    /// Stack of expected return types for the current function body.
    return_ty_stack: Vec<Type>,
    /// Trait impl table for checking where-clause bounds at call sites.
    /// When `None`, trait-bound checking is skipped.
    pub impl_table: Option<ImplTable>,
    /// Methods from inherent impl blocks: type_name → method_name → fn_type.
    method_types: HashMap<String, HashMap<String, Type>>,
    /// Effect operation types: effect_name → [(op_name, fn_type)].
    /// Populated during `collect_sig` for `EffectDecl` nodes.
    effect_op_types: HashMap<String, Vec<(String, Type)>>,
    /// Component effects for composite effects: effect_name → [component_name].
    effect_components: HashMap<String, Vec<String>>,
    /// Record field types: record_name → [(field_name, field_type)].
    /// Populated during `collect_sig` for `RecordDecl` nodes.
    record_field_types: HashMap<String, Vec<(String, Type)>>,
    /// Generic type parameter names for records: record_name → [param_names].
    /// Populated during `collect_sig` for `RecordDecl` nodes with generics.
    record_generic_params: HashMap<String, Vec<String>>,
    /// Type alias mappings: alias_name → underlying type.
    /// Populated during `collect_sig` for `TypeAlias` nodes.
    type_aliases: HashMap<String, Type>,
    /// Trait method signatures: trait_name → (method_name → fn_type).
    /// The fn_type uses `Named("Self")` for the self parameter so
    /// callers can substitute the concrete receiver type.
    /// Populated during `collect_sig` for `TraitDecl` nodes.
    trait_method_types: HashMap<String, HashMap<String, Type>>,
    /// Trait bounds on type variables: TypeVarId → [trait_name].
    /// Populated during `check_fn_decl` from inline generic param bounds
    /// and where-clause constraints. Used by the FieldAccess handler to
    /// resolve methods on bounded type parameters.
    type_var_bounds: HashMap<TypeVarId, Vec<String>>,
}

impl TypeChecker {
    /// Create a new, empty type checker.
    #[must_use]
    pub fn new() -> Self {
        Self {
            env: TypeEnv::new(),
            subst: Substitution::new(),
            diags: DiagnosticBag::new(),
            var_gen: TypeVarGen::new(),
            types: HashMap::new(),
            fn_sigs: HashMap::new(),
            return_ty_stack: Vec::new(),
            impl_table: None,
            method_types: HashMap::new(),
            effect_op_types: HashMap::new(),
            effect_components: HashMap::new(),
            record_field_types: HashMap::new(),
            record_generic_params: HashMap::new(),
            type_aliases: HashMap::new(),
            trait_method_types: HashMap::new(),
            type_var_bounds: HashMap::new(),
        }
    }

    // ── TypeVarId allocation ─────────────────────────────────────────────────

    /// Allocate a fresh type-inference variable.
    fn fresh_var(&self) -> Type {
        Type::TypeVar(self.var_gen.next())
    }

    // ── Side-table helpers ───────────────────────────────────────────────────

    /// Record the type of `node` in the side-table (after applying the current
    /// substitution), stamp the node's `type_info` slot, and return the type.
    fn record(&mut self, node: &mut AIRNode, ty: Type) -> Type {
        let resolved = self.subst.apply(&ty);
        self.types.insert(node.id, resolved.clone());
        node.type_info = Some(TypeInfo {
            resolved_type: None,
        });
        // Mark primitive types as copy so ownership analysis skips moves.
        if matches!(resolved, Type::Primitive(_)) {
            node.metadata
                .insert("copy_type".into(), Value::Bool(true));
        }
        resolved
    }

    /// Look up the resolved type for `node_id` from the side-table.
    #[must_use]
    pub fn type_of(&self, id: NodeId) -> Option<&Type> {
        self.types.get(&id)
    }

    // ── Getters for export collection ───────────────────────────────────────

    /// Record field types: record_name → [(field_name, field_type)].
    #[must_use]
    pub fn record_field_types(&self) -> &HashMap<String, Vec<(String, Type)>> {
        &self.record_field_types
    }

    /// Generic type parameter names for records: record_name → [param_names].
    #[must_use]
    pub fn record_generic_params(&self) -> &HashMap<String, Vec<String>> {
        &self.record_generic_params
    }

    /// Effect operation types: effect_name → [(op_name, fn_type)].
    #[must_use]
    pub fn effect_op_types(&self) -> &HashMap<String, Vec<(String, Type)>> {
        &self.effect_op_types
    }

    /// Component effects for composite effects: effect_name → [component_name].
    #[must_use]
    pub fn effect_components(&self) -> &HashMap<String, Vec<String>> {
        &self.effect_components
    }

    /// Inherent impl method signatures: type_name → (method_name → fn_type).
    #[must_use]
    pub fn method_types(&self) -> &HashMap<String, HashMap<String, Type>> {
        &self.method_types
    }

    /// Trait method signatures: trait_name → (method_name → fn_type).
    #[must_use]
    pub fn trait_method_types(&self) -> &HashMap<String, HashMap<String, Type>> {
        &self.trait_method_types
    }

    /// Type alias mappings: alias_name → underlying type.
    #[must_use]
    pub fn type_aliases(&self) -> &HashMap<String, Type> {
        &self.type_aliases
    }

    // ── Setters for import seeding ──────────────────────────────────────────

    /// Insert record field types for an imported record.
    pub fn insert_record_field_types(&mut self, name: String, fields: Vec<(String, Type)>) {
        self.record_field_types.insert(name, fields);
    }

    /// Insert generic parameter names for an imported record/enum.
    pub fn insert_record_generic_params(&mut self, name: String, params: Vec<String>) {
        self.record_generic_params.insert(name, params);
    }

    /// Insert trait method signatures for an imported trait.
    pub fn insert_trait_method_types(&mut self, name: String, methods: HashMap<String, Type>) {
        self.trait_method_types.insert(name, methods);
    }

    /// Insert effect operation types for an imported effect.
    pub fn insert_effect_op_types(&mut self, name: String, ops: Vec<(String, Type)>) {
        self.effect_op_types.insert(name, ops);
    }

    /// Insert component effects for an imported composite effect.
    pub fn insert_effect_components(&mut self, name: String, components: Vec<String>) {
        self.effect_components.insert(name, components);
    }

    /// Insert a type alias for an imported type alias.
    pub fn insert_type_alias(&mut self, name: String, underlying: Type) {
        self.type_aliases.insert(name, underlying);
    }

    /// Insert method types for an imported type's inherent impl.
    pub fn insert_method_types(&mut self, type_name: String, methods: HashMap<String, Type>) {
        self.method_types.insert(type_name, methods);
    }

    /// Seed an imported generic function signature so that each call site
    /// gets fresh [`TypeVarId`]s (just like local generic functions).
    ///
    /// When a generic function is exported, its type string contains the
    /// original [`TypeVarId`]s (e.g. `"Fn(?3) -> ?3"`). Without an [`FnSig`]
    /// entry the call-site instantiation logic in the `Call` handler is
    /// bypassed, causing the first call to bind those vars permanently.
    ///
    /// This method re-allocates fresh [`TypeVarId`]s, remaps the function
    /// type, stores the remapped type in `env`, and inserts a matching
    /// [`FnSig`] into `fn_sigs`.
    pub fn seed_imported_generic_fn(&mut self, name: &str, fn_ty: &FnType) -> Type {
        // Collect unique TypeVarIds from the function type in order of first
        // appearance, so the mapping is deterministic.
        let mut original_ids = Vec::new();
        collect_type_var_ids_fn(fn_ty, &mut original_ids);

        if original_ids.is_empty() {
            // Not actually generic — just define in env and return.
            let ty = Type::Function(fn_ty.clone());
            self.env.define(name, ty.clone());
            return ty;
        }

        // Allocate fresh TypeVarIds and build the replacement map.
        let remap: HashMap<TypeVarId, Type> = original_ids
            .iter()
            .map(|&id| (id, self.fresh_var()))
            .collect();

        let fresh_ids: Vec<TypeVarId> = original_ids
            .iter()
            .map(|id| match &remap[id] {
                Type::TypeVar(fresh) => *fresh,
                _ => unreachable!(),
            })
            .collect();

        // Remap the function type.
        let remapped = Type::Function(FnType {
            params: fn_ty
                .params
                .iter()
                .map(|t| self.replace_type_vars(t, &remap))
                .collect(),
            ret: Box::new(self.replace_type_vars(&fn_ty.ret, &remap)),
            effects: fn_ty.effects.clone(),
        });

        // Store remapped type in env.
        self.env.define(name, remapped.clone());

        // Create synthetic generic param names.
        let generic_params: Vec<String> = (0..original_ids.len())
            .map(|i| format!("T{i}"))
            .collect();

        // Extract param types and return type from remapped function.
        if let Type::Function(ref f) = remapped {
            self.fn_sigs.insert(
                name.to_string(),
                FnSig {
                    generic_params,
                    generic_var_ids: fresh_ids,
                    param_types: f.params.clone(),
                    return_type: (*f.ret).clone(),
                    where_clause: vec![],
                },
            );
        }

        remapped
    }

    // ── Unification helper ───────────────────────────────────────────────────

    /// Try to unify `a` and `b`. On failure emit a diagnostic at `span` and
    /// return `Type::Error`.
    fn unify_or_error(&mut self, a: &Type, b: &Type, span: Span, context: &str) -> Type {
        let a = self.resolve_alias(&self.subst.apply(a));
        let b = self.resolve_alias(&self.subst.apply(b));
        match unify(&a, &b, &mut self.subst) {
            Ok(()) => self.subst.apply(&a),
            Err(e) => {
                let diag = self.diags.error(
                    E_TYPE_MISMATCH,
                    format!("type mismatch in {context}: {e}"),
                    span,
                );
                if let Some(hint) = conversion_hint(&a, &b) {
                    diag.note(hint);
                }
                Type::Error
            }
        }
    }

    // ── Module-level pass ────────────────────────────────────────────────────

    /// Type-check an AIR module, annotating every node with its resolved type.
    ///
    /// Performs two sub-passes:
    /// 1. **Collect** — gather all top-level function signatures.
    /// 2. **Check** — infer/check each top-level item.
    pub fn check_module(&mut self, module: &mut AIRNode) {
        // Clone children out to avoid simultaneous borrow of `module`.
        let items = match &module.kind {
            NodeKind::Module { items, .. } => items.clone(),
            _ => return,
        };

        // Pass 1: collect signatures
        for item in &items {
            self.collect_sig(item);
        }

        // Pass 2: check items in place.
        // We re-borrow the items vec mutably.
        if let NodeKind::Module { items, .. } = &mut module.kind {
            for item in items.iter_mut() {
                self.check_item(item);
            }
        }

        self.record(module, Type::Primitive(PrimitiveType::Void));
    }

    /// Collect a top-level function signature into `self.fn_sigs` and `self.env`.
    fn collect_sig(&mut self, node: &AIRNode) {
        match &node.kind {
            NodeKind::FnDecl {
                name,
                generic_params,
                params,
                return_type,
                effect_clause,
                where_clause,
                ..
            } => {
                let gp_names: Vec<String> =
                    generic_params.iter().map(|g| g.name.name.clone()).collect();

                // Build placeholder types for generic params
                let gp_map: HashMap<String, Type> = gp_names
                    .iter()
                    .map(|n| (n.clone(), self.fresh_var()))
                    .collect();

                // Extract TypeVarIds so instantiate_and_check can map them
                // to fresh vars at each call site.
                let gp_var_ids: Vec<TypeVarId> = gp_names
                    .iter()
                    .map(|n| match &gp_map[n] {
                        Type::TypeVar(id) => *id,
                        _ => unreachable!(),
                    })
                    .collect();

                // Convert AIR param nodes to Types
                let param_types: Vec<Type> = params
                    .iter()
                    .map(|p| self.air_type_node_to_type(p.kind.param_ty_node(), &gp_map))
                    .collect();

                let ret_ty = return_type
                    .as_deref()
                    .map(|n| self.air_type_node_to_type(n, &gp_map))
                    .unwrap_or(Type::Primitive(PrimitiveType::Void));

                // Convert effect clause to EffectRef list
                let effects: Vec<EffectRef> = effect_clause
                    .iter()
                    .map(|tp| {
                        let name = tp
                            .segments
                            .iter()
                            .map(|s| s.name.as_str())
                            .collect::<Vec<_>>()
                            .join(".");
                        EffectRef::new(name)
                    })
                    .collect();

                // Also define in env as a function type
                let fn_ty = Type::Function(FnType {
                    params: param_types.clone(),
                    ret: Box::new(ret_ty.clone()),
                    effects: effects.clone(),
                });
                self.env.define(name.name.clone(), fn_ty);

                self.fn_sigs.insert(
                    name.name.clone(),
                    FnSig {
                        generic_params: gp_names,
                        generic_var_ids: gp_var_ids,
                        param_types,
                        return_type: ret_ty,
                        where_clause: where_clause.clone(),
                    },
                );
            }
            NodeKind::ConstDecl { name, ty, .. } => {
                let const_ty = self.air_type_node_to_type(ty, &HashMap::new());
                self.env.define(name.name.clone(), const_ty);
            }
            NodeKind::EnumDecl { name, variants, generic_params, .. } => {
                let enum_name = name.name.clone();

                // Extract generic param names.
                let gp_names: Vec<String> = generic_params
                    .iter()
                    .map(|g| g.name.name.clone())
                    .collect();

                // For generic enums, build a gp_map so variant field types
                // resolve type parameters (e.g. L, R) as fresh type vars
                // instead of Named("L"), and build a Generic return type
                // for tuple-variant constructor fn_sigs.
                //
                // The gp_map type vars are "template" vars: they must only
                // appear inside fn_sigs entries (which create per-call-site
                // fresh vars).  Unit/struct variants use Named to avoid
                // binding the template vars through unification.
                let named_ty = Type::Named(crate::NamedType {
                    name: enum_name.clone(),
                });
                let (gp_map, gp_var_ids, generic_ret_ty) = if gp_names.is_empty() {
                    (HashMap::new(), vec![], named_ty.clone())
                } else {
                    let gp_map: HashMap<String, Type> = gp_names
                        .iter()
                        .map(|n| (n.clone(), self.fresh_var()))
                        .collect();
                    let gp_var_ids: Vec<TypeVarId> = gp_names
                        .iter()
                        .map(|n| match &gp_map[n] {
                            Type::TypeVar(id) => *id,
                            _ => unreachable!(),
                        })
                        .collect();
                    let type_args: Vec<Type> =
                        gp_names.iter().map(|n| gp_map[n].clone()).collect();
                    let generic_ret_ty = Type::Generic(GenericType {
                        constructor: enum_name.clone(),
                        args: type_args,
                    });
                    (gp_map, gp_var_ids, generic_ret_ty)
                };

                // Register the enum type name itself (always Named so
                // it doesn't leak template type vars).
                self.env.define(enum_name.clone(), named_ty.clone());

                // Store generic params for struct-variant field lookup.
                if !gp_names.is_empty() {
                    self.record_generic_params
                        .insert(enum_name.clone(), gp_names.clone());
                }

                // Register each variant as a value/constructor in scope.
                for variant in variants {
                    if let NodeKind::EnumVariant {
                        name: vname,
                        payload,
                    } = &variant.kind
                    {
                        match payload {
                            EnumVariantPayload::Unit => {
                                // Unit variant — use Named (not Generic with
                                // template vars) so unification doesn't bind
                                // the shared template type vars.
                                self.env.define(vname.name.clone(), named_ty.clone());
                            }
                            EnumVariantPayload::Tuple(param_nodes) => {
                                // Tuple variant is a constructor function.
                                let param_tys: Vec<Type> = param_nodes
                                    .iter()
                                    .map(|p| self.air_type_node_to_type(p, &gp_map))
                                    .collect();
                                let fn_ty = Type::Function(FnType {
                                    params: param_tys.clone(),
                                    ret: Box::new(generic_ret_ty.clone()),
                                    effects: vec![],
                                });
                                self.env.define(vname.name.clone(), fn_ty);

                                // For generic enums, register in fn_sigs so
                                // each call site gets fresh type var instantiation.
                                if !gp_names.is_empty() {
                                    self.fn_sigs.insert(
                                        vname.name.clone(),
                                        FnSig {
                                            generic_params: gp_names.clone(),
                                            generic_var_ids: gp_var_ids.clone(),
                                            param_types: param_tys,
                                            return_type: generic_ret_ty.clone(),
                                            where_clause: vec![],
                                        },
                                    );
                                }
                            }
                            EnumVariantPayload::Struct(fields) => {
                                // Record variant — use Named (not Generic)
                                // so template type vars are not leaked.
                                self.env.define(vname.name.clone(), named_ty.clone());
                                // Register field types so record construction
                                // can type-check individual fields.
                                let field_types: Vec<(String, Type)> = fields
                                    .iter()
                                    .map(|f| {
                                        let ty = self.type_expr_to_type(
                                            &f.ty,
                                            &gp_map,
                                        );
                                        (f.name.name.clone(), ty)
                                    })
                                    .collect();
                                self.record_field_types
                                    .insert(vname.name.clone(), field_types);
                                // For generic enum struct variants, register
                                // their params for record construction lookup.
                                if !gp_names.is_empty() {
                                    self.record_generic_params
                                        .insert(vname.name.clone(), gp_names.clone());
                                }
                            }
                        }
                    }
                }
            }
            NodeKind::ImplBlock {
                target, methods, ..
            } => {
                let target_name = match &target.kind {
                    NodeKind::TypeNamed { path, .. } => type_path_to_name(path),
                    _ => return,
                };
                let target_ty = Type::Named(crate::NamedType {
                    name: target_name.clone(),
                });
                for method in methods {
                    if let NodeKind::FnDecl {
                        name,
                        params,
                        return_type,
                        ..
                    } = &method.kind
                    {
                        let gp_map: HashMap<String, Type> = HashMap::new();

                        let param_types: Vec<Type> = params
                            .iter()
                            .map(|p| {
                                // For `self` params (no type annotation), use the target type.
                                if let NodeKind::Param {
                                    pattern, ty: None, ..
                                } = &p.kind
                                {
                                    if let NodeKind::BindPat { name, .. } = &pattern.kind {
                                        if name.name == "self" {
                                            return target_ty.clone();
                                        }
                                    }
                                }
                                self.air_type_node_to_type(p, &gp_map)
                            })
                            .collect();

                        let ret_ty = return_type
                            .as_deref()
                            .map(|n| self.air_type_node_to_type(n, &gp_map))
                            .unwrap_or(Type::Primitive(PrimitiveType::Void));

                        let fn_ty = Type::Function(FnType {
                            params: param_types,
                            ret: Box::new(ret_ty),
                            effects: vec![],
                        });

                        self.method_types
                            .entry(target_name.clone())
                            .or_default()
                            .insert(name.name.clone(), fn_ty);
                    }
                }
            }
            NodeKind::EffectDecl {
                name,
                operations,
                components,
                ..
            } => {
                // Collect operation signatures so `with` clauses can inject
                // them into function type environments.
                let mut ops = Vec::new();
                for op in operations {
                    if let NodeKind::FnDecl {
                        name: op_name,
                        params,
                        return_type,
                        ..
                    } = &op.kind
                    {
                        let param_types: Vec<Type> = params
                            .iter()
                            .map(|p| {
                                self.air_type_node_to_type(p.kind.param_ty_node(), &HashMap::new())
                            })
                            .collect();
                        let ret_ty = return_type
                            .as_deref()
                            .map(|n| self.air_type_node_to_type(n, &HashMap::new()))
                            .unwrap_or(Type::Primitive(PrimitiveType::Void));
                        let fn_ty = Type::Function(FnType {
                            params: param_types,
                            ret: Box::new(ret_ty),
                            effects: vec![],
                        });
                        ops.push((op_name.name.clone(), fn_ty));
                    }
                }
                self.effect_op_types.insert(name.name.clone(), ops);

                let comp_names: Vec<String> =
                    components.iter().map(type_path_to_name).collect();
                if !comp_names.is_empty() {
                    self.effect_components
                        .insert(name.name.clone(), comp_names);
                }
            }
            NodeKind::RecordDecl {
                name, fields, generic_params, ..
            } => {
                let record_name = name.name.clone();
                let gp_names: Vec<String> = generic_params
                    .iter()
                    .map(|g| g.name.name.clone())
                    .collect();
                let field_types: Vec<(String, Type)> = fields
                    .iter()
                    .map(|f| {
                        let ty = self.type_expr_to_type(&f.ty, &HashMap::new());
                        (f.name.name.clone(), ty)
                    })
                    .collect();
                self.record_field_types
                    .insert(record_name.clone(), field_types);
                if !gp_names.is_empty() {
                    self.record_generic_params
                        .insert(record_name.clone(), gp_names);
                }
                // Also register the record name as a Named type in env.
                self.env.define(
                    record_name.clone(),
                    Type::Named(crate::NamedType {
                        name: record_name,
                    }),
                );
            }
            NodeKind::TypeAlias {
                name, ty, ..
            } => {
                let underlying = self.air_type_node_to_type(ty, &HashMap::new());
                self.type_aliases
                    .insert(name.name.clone(), underlying);
            }
            NodeKind::ClassDecl {
                name,
                fields,
                methods,
                base,
                generic_params,
                ..
            } => {
                let class_name = name.name.clone();

                // Register generic params if present.
                let gp_names: Vec<String> = generic_params
                    .iter()
                    .map(|g| g.name.name.clone())
                    .collect();
                if !gp_names.is_empty() {
                    self.record_generic_params
                        .insert(class_name.clone(), gp_names);
                }

                // Register field types (same as RecordDecl).
                let field_types: Vec<(String, Type)> = fields
                    .iter()
                    .map(|f| {
                        let ty = self.type_expr_to_type(&f.ty, &HashMap::new());
                        (f.name.name.clone(), ty)
                    })
                    .collect();
                self.record_field_types
                    .insert(class_name.clone(), field_types);

                // Register the class name as a Named type.
                let class_ty = Type::Named(crate::NamedType {
                    name: class_name.clone(),
                });
                self.env.define(class_name.clone(), class_ty.clone());

                // Inherit methods from base class if present.
                if let Some(base_path) = base {
                    let base_name = type_path_to_name(base_path);
                    if let Some(base_methods) = self.method_types.get(&base_name).cloned() {
                        self.method_types
                            .entry(class_name.clone())
                            .or_default()
                            .extend(base_methods);
                    }
                }

                // Register methods (same logic as ImplBlock).
                for method in methods {
                    if let NodeKind::FnDecl {
                        name: method_name,
                        params,
                        return_type,
                        ..
                    } = &method.kind
                    {
                        let gp_map: HashMap<String, Type> = HashMap::new();

                        let param_types: Vec<Type> = params
                            .iter()
                            .map(|p| {
                                if let NodeKind::Param {
                                    pattern, ty: None, ..
                                } = &p.kind
                                {
                                    if let NodeKind::BindPat { name, .. } = &pattern.kind {
                                        if name.name == "self" {
                                            return class_ty.clone();
                                        }
                                    }
                                }
                                self.air_type_node_to_type(p, &gp_map)
                            })
                            .collect();

                        let ret_ty = return_type
                            .as_deref()
                            .map(|n| self.air_type_node_to_type(n, &gp_map))
                            .unwrap_or(Type::Primitive(PrimitiveType::Void));

                        let fn_ty = Type::Function(FnType {
                            params: param_types,
                            ret: Box::new(ret_ty),
                            effects: vec![],
                        });

                        self.method_types
                            .entry(class_name.clone())
                            .or_default()
                            .insert(method_name.name.clone(), fn_ty);
                    }
                }
            }
            NodeKind::TraitDecl {
                name, methods, ..
            } => {
                let trait_name = name.name.clone();
                let self_ty = Type::Named(crate::NamedType {
                    name: "Self".to_string(),
                });
                let mut trait_methods = HashMap::new();
                for method in methods {
                    if let NodeKind::FnDecl {
                        name: method_name,
                        params,
                        return_type,
                        ..
                    } = &method.kind
                    {
                        let gp_map: HashMap<String, Type> = HashMap::new();
                        let param_types: Vec<Type> = params
                            .iter()
                            .map(|p| {
                                if let NodeKind::Param {
                                    pattern, ty: None, ..
                                } = &p.kind
                                {
                                    if let NodeKind::BindPat { name, .. } = &pattern.kind {
                                        if name.name == "self" {
                                            return self_ty.clone();
                                        }
                                    }
                                }
                                self.air_type_node_to_type(p, &gp_map)
                            })
                            .collect();
                        let ret_ty = return_type
                            .as_deref()
                            .map(|n| self.air_type_node_to_type(n, &gp_map))
                            .unwrap_or(Type::Primitive(PrimitiveType::Void));
                        let fn_ty = Type::Function(FnType {
                            params: param_types,
                            ret: Box::new(ret_ty),
                            effects: vec![],
                        });
                        trait_methods.insert(method_name.name.clone(), fn_ty);
                    }
                }
                if !trait_methods.is_empty() {
                    self.trait_method_types.insert(trait_name, trait_methods);
                }
            }
            _ => {}
        }
    }

    /// Resolve a type through type aliases. If `ty` is a `Named` type whose
    /// name is a registered type alias, return the underlying type instead.
    fn resolve_alias(&self, ty: &Type) -> Type {
        match ty {
            Type::Named(nt) => {
                if let Some(underlying) = self.type_aliases.get(&nt.name) {
                    underlying.clone()
                } else {
                    ty.clone()
                }
            }
            _ => ty.clone(),
        }
    }

    /// Type-check a top-level item node (mutates the node tree).
    fn check_item(&mut self, node: &mut AIRNode) {
        match &node.kind {
            NodeKind::FnDecl { .. } => {
                self.check_fn_decl(node);
            }
            NodeKind::ConstDecl { .. } => {
                self.check_const_decl(node);
            }
            // Other top-level items: record as Void for now.
            _ => {
                self.record(node, Type::Primitive(PrimitiveType::Void));
            }
        }
    }

    /// Type-check a function declaration node in place.
    fn check_fn_decl(&mut self, node: &mut AIRNode) {
        // Extract what we need by cloning to avoid borrow conflicts.
        let (_name, generic_params, params, return_type, effect_clause, where_clause, _body) =
            match node.kind.clone() {
                NodeKind::FnDecl {
                    name,
                    generic_params,
                    params,
                    return_type,
                    effect_clause,
                    where_clause,
                    body,
                    ..
                } => (
                    name,
                    generic_params,
                    params,
                    return_type,
                    effect_clause,
                    where_clause,
                    body,
                ),
                _ => return,
            };

        self.env.push_scope();

        // Introduce generic params as fresh type vars
        let gp_map: HashMap<String, Type> = generic_params
            .iter()
            .map(|g| (g.name.name.clone(), self.fresh_var()))
            .collect();

        // Record trait bounds on type variables from inline bounds
        // (e.g. `T: Describable`) and where-clause constraints.
        for gp in &generic_params {
            if let Some(Type::TypeVar(id)) = gp_map.get(&gp.name.name) {
                let bound_names: Vec<String> = gp
                    .bounds
                    .iter()
                    .map(type_path_to_name)
                    .collect();
                if !bound_names.is_empty() {
                    self.type_var_bounds.entry(*id).or_default().extend(bound_names);
                }
            }
        }
        for clause in &where_clause {
            if let Some(Type::TypeVar(id)) = gp_map.get(&clause.param.name) {
                let bound_names: Vec<String> = clause
                    .bounds
                    .iter()
                    .map(type_path_to_name)
                    .collect();
                if !bound_names.is_empty() {
                    self.type_var_bounds.entry(*id).or_default().extend(bound_names);
                }
            }
        }

        // Bind params
        let param_types: Vec<Type> = params
            .iter()
            .map(|p| {
                let ty = self.air_type_node_to_type(p.kind.param_ty_node(), &gp_map);
                let pat_name = p.kind.param_pat_name();
                if let Some(n) = pat_name {
                    self.env.define(n, ty.clone());
                }
                ty
            })
            .collect();

        let ret_ty = return_type
            .as_deref()
            .map(|n| self.air_type_node_to_type(n, &gp_map))
            .unwrap_or(Type::Primitive(PrimitiveType::Void));

        // Inject effect operation types from the `with` clause so that
        // calls like `log("msg")` type-check inside effectful functions.
        {
            let mut visited = std::collections::HashSet::new();
            for effect_tp in &effect_clause {
                let ename = type_path_to_name(effect_tp);
                self.inject_effect_ops_into_env(&ename, &mut visited);
            }
        }

        // Check where clause bounds (simple existence check — full trait
        // resolution is out of scope).
        self.check_where_clause(&where_clause, &gp_map, node.span);

        // Push return type for `return` expressions
        self.return_ty_stack.push(ret_ty.clone());

        // Check body — need mutable access via the original node.
        if let NodeKind::FnDecl { body, .. } = &mut node.kind {
            self.check_node(body, &ret_ty);
        }

        self.return_ty_stack.pop();
        self.env.pop_scope();

        let effects: Vec<EffectRef> = effect_clause
            .iter()
            .map(|tp| {
                let name = tp
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                EffectRef::new(name)
            })
            .collect();

        let fn_ty = Type::Function(FnType {
            params: param_types,
            ret: Box::new(ret_ty),
            effects,
        });
        self.record(node, fn_ty);
    }

    /// Type-check a constant declaration node in place.
    fn check_const_decl(&mut self, node: &mut AIRNode) {
        let (name, ty_node, _value_node) = match node.kind.clone() {
            NodeKind::ConstDecl {
                name, ty, value, ..
            } => (name, ty, value),
            _ => return,
        };
        let expected_ty = self.air_type_node_to_type(&ty_node, &HashMap::new());
        if let NodeKind::ConstDecl { value, .. } = &mut node.kind {
            self.check_node(value, &expected_ty);
        }
        self.env.define(name.name, expected_ty.clone());
        self.record(node, expected_ty);
    }

    // ── Where-clause verification ────────────────────────────────────────────

    /// Emit a diagnostic if any where-clause bound refers to a type parameter
    /// that is not in scope. Full trait-satisfaction checking is deferred.
    fn check_where_clause(
        &mut self,
        clauses: &[TypeConstraint],
        gp_map: &HashMap<String, Type>,
        span: Span,
    ) {
        for clause in clauses {
            if !gp_map.contains_key(&clause.param.name) {
                self.diags.error(
                    E_WHERE_CLAUSE,
                    format!(
                        "where-clause references unknown type parameter `{}`",
                        clause.param.name
                    ),
                    span,
                );
            }
        }
    }

    // ── Effect operation injection ─────────────────────────────────────────

    /// Recursively inject effect operation types into the current type
    /// environment. Handles composite effects by resolving components.
    fn inject_effect_ops_into_env(
        &mut self,
        effect_name: &str,
        visited: &mut std::collections::HashSet<String>,
    ) {
        if !visited.insert(effect_name.to_string()) {
            return;
        }
        if let Some(ops) = self.effect_op_types.get(effect_name).cloned() {
            for (op_name, fn_ty) in ops {
                self.env.define(op_name, fn_ty);
            }
        }
        if let Some(components) = self.effect_components.get(effect_name).cloned() {
            for comp in &components {
                self.inject_effect_ops_into_env(comp, visited);
            }
        }
    }

    // ── Trait-bound enforcement at call sites ─────────────────────────────

    /// Check that all where-clause bounds are satisfied after generic
    /// type-variable binding at a call site.
    ///
    /// `fn_name` is used in diagnostics.  `sig` provides the where-clause
    /// constraints and the mapping from generic-param names to the original
    /// [`TypeVarId`]s.  `fresh_map` maps those original ids to the fresh
    /// call-site type variables whose concrete types can be read from
    /// `self.subst`.
    fn check_trait_bounds_at_call(
        &mut self,
        fn_name: &str,
        sig: &FnSig,
        fresh_map: &HashMap<TypeVarId, Type>,
        span: Span,
    ) {
        let impl_table = match &self.impl_table {
            Some(t) => t,
            None => return, // no impl table → skip bound checking
        };

        // Build name→fresh_type_var map for looking up the concrete type
        // each generic parameter was resolved to.
        let name_to_fresh: HashMap<&str, &Type> = sig
            .generic_params
            .iter()
            .zip(sig.generic_var_ids.iter())
            .filter_map(|(name, orig_id)| {
                fresh_map
                    .get(orig_id)
                    .map(|fresh_ty| (name.as_str(), fresh_ty))
            })
            .collect();

        for clause in &sig.where_clause {
            let param_name = &clause.param.name;
            let concrete_ty = match name_to_fresh.get(param_name.as_str()) {
                Some(fresh) => self.subst.apply(fresh),
                None => continue, // unknown param — already diagnosed by check_where_clause
            };

            for bound_path in &clause.bounds {
                let trait_name = bound_path
                    .segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join(".");
                let trait_ref = TraitRef::new(&trait_name);
                if resolve_impl(&trait_ref, &concrete_ty, impl_table).is_none() {
                    self.diags.error(
                        E_WHERE_CLAUSE,
                        format!(
                            "type `{concrete_ty:?}` does not satisfy bound `{trait_name}` \
                             required by function `{fn_name}`",
                        ),
                        span,
                    );
                }
            }
        }
    }

    // ── Bidirectional core ───────────────────────────────────────────────────

    /// **Synthesis** (bottom-up): infer a type for `node` and record it.
    ///
    /// This is the internal mutable-node version; the public `infer_expr`
    /// provides read-only access via the side-table.
    fn infer_node(&mut self, node: &mut AIRNode) -> Type {
        let span = node.span;
        let ty = match &node.kind {
            // ── Literals ────────────────────────────────────────────────────
            NodeKind::Literal { lit } => self.infer_literal(lit),

            // ── Identifier reference ─────────────────────────────────────────
            NodeKind::Identifier { name } => {
                let name = name.name.clone();
                match self.env.lookup(&name) {
                    Some(ty) => {
                        let ty = ty.clone();
                        self.subst.apply(&ty)
                    }
                    None => {
                        self.diags.error(
                            E_UNDEFINED_VAR,
                            format!("undefined variable `{name}`"),
                            span,
                        );
                        Type::Error
                    }
                }
            }

            // ── Binary operations ─────────────────────────────────────────────
            NodeKind::BinaryOp { op, .. } => {
                let op = *op;
                // Infer operands (need mutable access)
                let (lt, rt) = if let NodeKind::BinaryOp { left, right, .. } = &mut node.kind {
                    let lt = self.infer_node(left);
                    let rt = self.infer_node(right);
                    (lt, rt)
                } else {
                    unreachable!()
                };
                self.infer_binop(op, &lt, &rt, span)
            }

            // ── Unary operations ──────────────────────────────────────────────
            NodeKind::UnaryOp { op, .. } => {
                let op = *op;
                let operand_ty = if let NodeKind::UnaryOp { operand, .. } = &mut node.kind {
                    self.infer_node(operand)
                } else {
                    unreachable!()
                };
                self.infer_unop(op, &operand_ty, span)
            }

            // ── Field access ──────────────────────────────────────────────────
            NodeKind::FieldAccess { field, .. } => {
                let field_name = field.name.clone();
                let obj_ty = if let NodeKind::FieldAccess { object, .. } = &mut node.kind {
                    self.infer_node(object)
                } else {
                    unreachable!()
                };
                let obj_ty = self.subst.apply(&obj_ty);
                match &obj_ty {
                    Type::Error => Type::Error,
                    Type::Named(nt) => {
                        // Look up method on the named type from inherent impls.
                        if let Some(methods) = self.method_types.get(&nt.name) {
                            if let Some(fn_ty) = methods.get(&field_name) {
                                return self.record(node, fn_ty.clone());
                            }
                        }
                        // Look up record field type from the declaration.
                        if let Some(fields) = self.record_field_types.get(&nt.name) {
                            if let Some((_, field_ty)) =
                                fields.iter().find(|(n, _)| n == &field_name)
                            {
                                return self.record(node, field_ty.clone());
                            }
                        }
                        self.fresh_var()
                    }
                    Type::Generic(g) => {
                        // User-defined generic type: look up methods/fields
                        // by constructor name, substituting type params.
                        if let Some(methods) = self.method_types.get(&g.constructor) {
                            if let Some(fn_ty) = methods.get(&field_name) {
                                let resolved = if let Some(params) =
                                    self.record_generic_params.get(&g.constructor)
                                {
                                    substitute_type_params(fn_ty, params, &g.args)
                                } else {
                                    fn_ty.clone()
                                };
                                return self.record(node, resolved);
                            }
                        }
                        if let Some(fields) = self.record_field_types.get(&g.constructor) {
                            if let Some((_, field_ty)) =
                                fields.iter().find(|(n, _)| n == &field_name)
                            {
                                let resolved = if let Some(params) =
                                    self.record_generic_params.get(&g.constructor)
                                {
                                    substitute_type_params(field_ty, params, &g.args)
                                } else {
                                    field_ty.clone()
                                };
                                return self.record(node, resolved);
                            }
                        }
                        // Fall through to built-in methods.
                        if let Some(fn_ty) =
                            self.resolve_builtin_method_fn_type(&obj_ty, &field_name)
                        {
                            fn_ty
                        } else {
                            self.fresh_var()
                        }
                    }
                    Type::TypeVar(id) => {
                        // Look up trait bounds for this type variable and
                        // resolve methods from the bounded traits.
                        if let Some(bounds) = self.type_var_bounds.get(id).cloned() {
                            let self_params = vec!["Self".to_string()];
                            let self_args = vec![obj_ty.clone()];
                            for trait_name in &bounds {
                                if let Some(methods) = self.trait_method_types.get(trait_name).cloned() {
                                    if let Some(fn_ty) = methods.get(&field_name) {
                                        let resolved = substitute_type_params(
                                            fn_ty,
                                            &self_params,
                                            &self_args,
                                        );
                                        return self.record(node, resolved);
                                    }
                                }
                            }
                        }
                        // Fall through to built-in methods.
                        if let Some(fn_ty) = self.resolve_builtin_method_fn_type(&obj_ty, &field_name) {
                            fn_ty
                        } else {
                            self.fresh_var()
                        }
                    }
                    _ => {
                        // Check built-in method signatures for Generic / Primitive types.
                        if let Some(fn_ty) = self.resolve_builtin_method_fn_type(&obj_ty, &field_name) {
                            fn_ty
                        } else {
                            // Return a fresh type var; downstream calls may unify it.
                            self.fresh_var()
                        }
                    }
                }
            }

            // ── Index access ──────────────────────────────────────────────────
            NodeKind::Index { .. } => {
                let (obj_ty, idx_ty) = if let NodeKind::Index { object, index } = &mut node.kind {
                    let o = self.infer_node(object);
                    let i = self.infer_node(index);
                    (o, i)
                } else {
                    unreachable!()
                };
                // Check index is an integer
                self.unify_or_error(&idx_ty, &Type::Primitive(PrimitiveType::Int), span, "index");
                // Element type is a fresh var
                match &obj_ty {
                    Type::Error => Type::Error,
                    Type::Generic(g) if g.constructor == "List" && g.args.len() == 1 => {
                        g.args[0].clone()
                    }
                    _ => self.fresh_var(),
                }
            }

            // ── Function call ─────────────────────────────────────────────────
            NodeKind::Call { .. } => {
                // Clone callee/args to avoid borrow issues; rewrite below.
                let (callee_clone, args_clone, _type_args_clone) = if let NodeKind::Call {
                    callee,
                    args,
                    type_args,
                } = &node.kind
                {
                    (*callee.clone(), args.clone(), type_args.clone())
                } else {
                    unreachable!()
                };

                // Extract callee name for generic function lookup.
                let callee_name = if let NodeKind::Identifier { name } = &callee_clone.kind {
                    Some(name.name.clone())
                } else {
                    None
                };

                // Infer callee type via mutable sub-node
                let callee_ty = if let NodeKind::Call { callee, .. } = &mut node.kind {
                    self.infer_node(callee)
                } else {
                    unreachable!()
                };

                // For generic functions, create a fresh instantiation with
                // new type vars so each call site gets independent inference.
                // Also capture the sig + fresh_map for trait-bound checking.
                let mut call_site_info: Option<(String, FnSig, HashMap<TypeVarId, Type>)> = None;
                let effective_ty = match (&callee_name, &callee_ty) {
                    (Some(name), Type::Function(f)) => {
                        if let Some(sig) = self.fn_sigs.get(name).cloned() {
                            if !sig.generic_params.is_empty() {
                                let fresh_map: HashMap<TypeVarId, Type> = sig
                                    .generic_var_ids
                                    .iter()
                                    .map(|&id| (id, self.fresh_var()))
                                    .collect();
                                let ety = Type::Function(FnType {
                                    params: f
                                        .params
                                        .iter()
                                        .map(|t| self.replace_type_vars(t, &fresh_map))
                                        .collect(),
                                    ret: Box::new(self.replace_type_vars(&f.ret, &fresh_map)),
                                    effects: f.effects.clone(),
                                });
                                call_site_info = Some((name.clone(), sig, fresh_map));
                                ety
                            } else {
                                callee_ty.clone()
                            }
                        } else {
                            callee_ty.clone()
                        }
                    }
                    _ => callee_ty.clone(),
                };

                let ret_ty = self.check_call(callee_clone.span, &effective_ty, &args_clone, span);

                // Now type-check each arg node in place
                if let NodeKind::Call { args, .. } = &mut node.kind {
                    match &effective_ty {
                        Type::Function(f) => {
                            for (arg, param_ty) in args.iter_mut().zip(f.params.iter()) {
                                let pt = self.subst.apply(param_ty);
                                self.check_node(&mut arg.value, &pt);
                            }
                        }
                        _ => {
                            for arg in args.iter_mut() {
                                self.infer_node(&mut arg.value);
                            }
                        }
                    }
                }

                // After args are checked (and type vars unified), verify
                // where-clause trait bounds.
                if let Some((fn_name, sig, fresh_map)) = &call_site_info {
                    self.check_trait_bounds_at_call(fn_name, sig, fresh_map, span);
                }

                ret_ty
            }

            // ── Method call ───────────────────────────────────────────────────
            NodeKind::MethodCall { method, .. } => {
                let method_name = method.name.clone();
                let receiver_ty =
                    if let NodeKind::MethodCall { receiver, args, .. } = &mut node.kind {
                        let rt = self.infer_node(receiver);
                        for arg in args.iter_mut() {
                            self.infer_node(&mut arg.value);
                        }
                        rt
                    } else {
                        unreachable!()
                    };
                self.resolve_method_return_type(&receiver_ty, &method_name)
            }

            // ── Lambda ────────────────────────────────────────────────────────
            NodeKind::Lambda { .. } => {
                // With no expected type, give each param a fresh var and infer body.
                let (param_tys, body_ty) = self.infer_lambda(node);
                Type::Function(FnType {
                    params: param_tys,
                    ret: Box::new(body_ty),
                    effects: vec![],
                })
            }

            // ── Pipe ──────────────────────────────────────────────────────────
            NodeKind::Pipe { .. } => {
                // `left |> f` desugars to `f(left)`.
                let (lty, rty) = if let NodeKind::Pipe { left, right } = &mut node.kind {
                    let l = self.infer_node(left);
                    let r = self.infer_node(right);
                    (l, r)
                } else {
                    unreachable!()
                };
                // rty should be a function; its return type is the pipe result.
                match &rty {
                    Type::Function(f) if f.params.len() == 1 => {
                        let param_ty = self.subst.apply(&f.params[0]);
                        self.unify_or_error(&lty, &param_ty, span, "pipe");
                        self.subst.apply(&f.ret)
                    }
                    Type::Error => Type::Error,
                    _ => self.fresh_var(),
                }
            }

            // ── If expression ─────────────────────────────────────────────────
            NodeKind::If { .. } => self.infer_if(node),

            // ── Match expression ──────────────────────────────────────────────
            NodeKind::Match { .. } => self.infer_match(node),

            // ── Block ─────────────────────────────────────────────────────────
            NodeKind::Block { .. } => self.infer_block(node),

            // ── Let binding ───────────────────────────────────────────────────
            NodeKind::LetBinding { .. } => {
                self.check_let_binding(node);
                Type::Primitive(PrimitiveType::Void)
            }

            // ── Return ────────────────────────────────────────────────────────
            NodeKind::Return { .. } => {
                let expected = self.return_ty_stack.last().cloned();
                if let NodeKind::Return { value } = &mut node.kind {
                    match (value, &expected) {
                        (Some(v), Some(e)) => {
                            let et = e.clone();
                            self.check_node(v, &et);
                        }
                        (Some(v), None) => {
                            self.infer_node(v);
                        }
                        _ => {}
                    }
                }
                Type::Primitive(PrimitiveType::Never)
            }

            // ── List literal ──────────────────────────────────────────────────
            NodeKind::ListLiteral { .. } => {
                let elem_ty = self.fresh_var();
                if let NodeKind::ListLiteral { elems } = &mut node.kind {
                    for elem in elems.iter_mut() {
                        let et = elem_ty.clone();
                        self.check_node(elem, &et);
                    }
                }
                Type::Generic(GenericType {
                    constructor: "List".into(),
                    args: vec![self.subst.apply(&elem_ty)],
                })
            }

            // ── Tuple literal ─────────────────────────────────────────────────
            NodeKind::TupleLiteral { .. } => {
                let elem_tys: Vec<Type> = if let NodeKind::TupleLiteral { elems } = &mut node.kind {
                    elems.iter_mut().map(|e| self.infer_node(e)).collect()
                } else {
                    vec![]
                };
                Type::Tuple(elem_tys)
            }

            // ── Map literal ───────────────────────────────────────────────────
            NodeKind::MapLiteral { .. } => {
                let k_ty = self.fresh_var();
                let v_ty = self.fresh_var();
                if let NodeKind::MapLiteral { entries } = &mut node.kind {
                    for entry in entries.iter_mut() {
                        let kt = k_ty.clone();
                        let vt = v_ty.clone();
                        self.check_node(&mut entry.key, &kt);
                        self.check_node(&mut entry.value, &vt);
                    }
                }
                Type::Generic(GenericType {
                    constructor: "Map".into(),
                    args: vec![self.subst.apply(&k_ty), self.subst.apply(&v_ty)],
                })
            }

            // ── Set literal ───────────────────────────────────────────────────
            NodeKind::SetLiteral { .. } => {
                let elem_ty = self.fresh_var();
                if let NodeKind::SetLiteral { elems } = &mut node.kind {
                    for elem in elems.iter_mut() {
                        let et = elem_ty.clone();
                        self.check_node(elem, &et);
                    }
                }
                Type::Generic(GenericType {
                    constructor: "Set".into(),
                    args: vec![self.subst.apply(&elem_ty)],
                })
            }

            // ── String interpolation ──────────────────────────────────────────
            NodeKind::Interpolation { .. } => {
                if let NodeKind::Interpolation { parts } = &mut node.kind {
                    for part in parts.iter_mut() {
                        if let bock_air::AirInterpolationPart::Expr(e) = part {
                            self.infer_node(e);
                        }
                    }
                }
                Type::Primitive(PrimitiveType::String)
            }

            // ── Optional / Result construction ────────────────────────────────
            NodeKind::ResultConstruct { variant, .. } => {
                // Copy variant (it's Copy) so we drop the immutable borrow before
                // we need &mut node.kind below.
                let variant = *variant;
                let has_value =
                    matches!(&node.kind, NodeKind::ResultConstruct { value: Some(_), .. });
                let inner_ty = if has_value {
                    if let NodeKind::ResultConstruct { value: Some(v), .. } = &mut node.kind {
                        self.infer_node(v)
                    } else {
                        unreachable!()
                    }
                } else {
                    Type::Primitive(PrimitiveType::Void)
                };
                let err_ty = self.fresh_var();
                let ok_ty = self.fresh_var();
                match variant {
                    bock_air::ResultVariant::Ok => {
                        self.unify_or_error(&inner_ty, &ok_ty, span, "Ok construct");
                        Type::Result(Box::new(ok_ty), Box::new(err_ty))
                    }
                    bock_air::ResultVariant::Err => {
                        self.unify_or_error(&inner_ty, &err_ty, span, "Err construct");
                        Type::Result(Box::new(ok_ty), Box::new(err_ty))
                    }
                }
            }

            // ── Propagate (?) ─────────────────────────────────────────────────
            NodeKind::Propagate { .. } => {
                let inner_ty = if let NodeKind::Propagate { expr } = &mut node.kind {
                    self.infer_node(expr)
                } else {
                    unreachable!()
                };
                // `expr?` unwraps a Result[T, E] or Optional[T]; type is T.
                match &inner_ty {
                    Type::Result(ok, _) => *ok.clone(),
                    Type::Optional(inner) => *inner.clone(),
                    Type::Error => Type::Error,
                    _ => self.fresh_var(),
                }
            }

            // ── Await ─────────────────────────────────────────────────────────
            NodeKind::Await { .. } => {
                if let NodeKind::Await { expr } = &mut node.kind {
                    self.infer_node(expr);
                }
                self.fresh_var()
            }

            // ── Borrow / Move ─────────────────────────────────────────────────
            NodeKind::Borrow { .. } | NodeKind::MutableBorrow { .. } => {
                // Ownership tracking is done in a later pass; propagate inner type.
                match &mut node.kind {
                    NodeKind::Borrow { expr } | NodeKind::MutableBorrow { expr } => {
                        self.infer_node(expr)
                    }
                    _ => unreachable!(),
                }
            }

            NodeKind::Move { .. } => {
                if let NodeKind::Move { expr } = &mut node.kind {
                    self.infer_node(expr)
                } else {
                    unreachable!()
                }
            }

            // ── Assign ────────────────────────────────────────────────────────
            NodeKind::Assign { .. } => {
                let (tty, vty) = if let NodeKind::Assign { target, value, .. } = &mut node.kind {
                    let t = self.infer_node(target);
                    let v = self.infer_node(value);
                    (t, v)
                } else {
                    unreachable!()
                };
                self.unify_or_error(&tty, &vty, span, "assignment");
                Type::Primitive(PrimitiveType::Void)
            }

            // ── Range ─────────────────────────────────────────────────────────
            NodeKind::Range { .. } => {
                let (lty, hty) = if let NodeKind::Range { lo, hi, .. } = &mut node.kind {
                    let l = self.infer_node(lo);
                    let h = self.infer_node(hi);
                    (l, h)
                } else {
                    unreachable!()
                };
                self.unify_or_error(&lty, &hty, span, "range bounds");
                Type::Generic(GenericType {
                    constructor: "Range".into(),
                    args: vec![lty],
                })
            }

            // ── Loops ─────────────────────────────────────────────────────────
            NodeKind::For { .. } => {
                self.env.push_scope();
                if let NodeKind::For {
                    pattern,
                    iterable,
                    body,
                } = &mut node.kind
                {
                    let iter_ty = self.infer_node(iterable);
                    // Bind pattern variable to element type
                    let elem_ty = match &iter_ty {
                        Type::Generic(g) if g.constructor == "List" && g.args.len() == 1 => {
                            g.args[0].clone()
                        }
                        Type::Generic(g) if g.constructor == "Range" && g.args.len() == 1 => {
                            g.args[0].clone()
                        }
                        _ => self.fresh_var(),
                    };
                    self.bind_pattern_type(pattern, &elem_ty);
                    self.infer_node(body);
                }
                self.env.pop_scope();
                Type::Primitive(PrimitiveType::Void)
            }

            NodeKind::While { .. } => {
                if let NodeKind::While { condition, body } = &mut node.kind {
                    let bool_ty = Type::Primitive(PrimitiveType::Bool);
                    self.check_node(condition, &bool_ty);
                    self.infer_node(body);
                }
                Type::Primitive(PrimitiveType::Void)
            }

            NodeKind::Loop { .. } => {
                if let NodeKind::Loop { body } = &mut node.kind {
                    self.infer_node(body);
                }
                // A `loop` with a break value would need more analysis; use fresh var.
                self.fresh_var()
            }

            NodeKind::Break { .. } => {
                if let NodeKind::Break { value: Some(v) } = &mut node.kind {
                    self.infer_node(v);
                }
                Type::Primitive(PrimitiveType::Never)
            }

            NodeKind::Continue => Type::Primitive(PrimitiveType::Never),

            NodeKind::Guard { .. } => {
                if let NodeKind::Guard {
                    let_pattern,
                    condition,
                    else_block,
                } = &mut node.kind
                {
                    if let_pattern.is_some() {
                        // guard (let pat = expr) — infer the condition type
                        // and bind pattern variables into the current scope
                        // (they must be visible after the guard statement).
                        let cond_ty = self.infer_node(condition);
                        if let Some(pat) = let_pattern {
                            self.bind_pattern_type(pat, &cond_ty);
                        }
                    } else {
                        let bool_ty = Type::Primitive(PrimitiveType::Bool);
                        self.check_node(condition, &bool_ty);
                    }
                    self.infer_node(else_block);
                }
                Type::Primitive(PrimitiveType::Void)
            }

            // ── Compose ───────────────────────────────────────────────────────
            NodeKind::Compose { .. } => {
                if let NodeKind::Compose { left, right } = &mut node.kind {
                    self.infer_node(left);
                    self.infer_node(right);
                }
                self.fresh_var() // f >> g: detailed typing deferred
            }

            // ── Placeholder ───────────────────────────────────────────────────
            NodeKind::Placeholder => self.fresh_var(),

            // ── Unreachable ───────────────────────────────────────────────────
            NodeKind::Unreachable => Type::Primitive(PrimitiveType::Never),

            // ── Handling block ────────────────────────────────────────────────
            NodeKind::HandlingBlock { .. } => {
                if let NodeKind::HandlingBlock { handlers, body } = &mut node.kind {
                    for hp in handlers.iter_mut() {
                        self.infer_node(&mut hp.handler);
                    }
                    self.infer_node(body)
                } else {
                    unreachable!()
                }
            }

            // ── RecordConstruct ───────────────────────────────────────────────
            NodeKind::RecordConstruct { path, .. } => {
                let name = path
                    .segments
                    .last()
                    .map(|s| s.name.clone())
                    .unwrap_or_default();

                // If this is a generic record, create fresh type vars for
                // each type parameter so we can infer them from field values.
                let generic_params = self.record_generic_params.get(&name).cloned();
                let fresh_type_args: Option<Vec<Type>> = generic_params
                    .as_ref()
                    .map(|params| params.iter().map(|_| self.fresh_var()).collect());

                if let NodeKind::RecordConstruct { fields, spread, .. } = &mut node.kind {
                    // Type-check each field value against the declared field type.
                    let declared_fields = self.record_field_types.get(&name).cloned();
                    for f in fields.iter_mut() {
                        if let Some(v) = &mut f.value {
                            if let Some(ref decl) = declared_fields {
                                if let Some((_, expected_ty)) =
                                    decl.iter().find(|(n, _)| n == &f.name.name)
                                {
                                    // For generic records, substitute param names
                                    // (e.g. Named("A")) with fresh type vars.
                                    let et = if let (Some(ref params), Some(ref args)) =
                                        (&generic_params, &fresh_type_args)
                                    {
                                        substitute_type_params(expected_ty, params, args)
                                    } else {
                                        expected_ty.clone()
                                    };
                                    self.check_node(v, &et);
                                } else {
                                    self.infer_node(v);
                                }
                            } else {
                                self.infer_node(v);
                            }
                        }
                    }
                    if let Some(s) = spread {
                        self.infer_node(s);
                    }
                }

                // For generic records, return Generic with the inferred type args.
                if let Some(type_args) = fresh_type_args {
                    Type::Generic(GenericType {
                        constructor: name,
                        args: type_args,
                    })
                } else {
                    // Non-generic: look up in env. For enum record variants,
                    // this resolves to the parent enum type.
                    self.env
                        .lookup(&name)
                        .cloned()
                        .unwrap_or(Type::Named(crate::NamedType { name }))
                }
            }

            // ── Error node ────────────────────────────────────────────────────
            NodeKind::Error => Type::Error,

            // ── Everything else: return a fresh var ───────────────────────────
            _ => self.fresh_var(),
        };

        self.record(node, ty)
    }

    /// **Checking** (top-down): verify `node` has type `expected`, emitting a
    /// diagnostic if not. Falls back to `infer_node` for most expression forms.
    fn check_node(&mut self, node: &mut AIRNode, expected: &Type) {
        let span = node.span;
        match &node.kind {
            // ── Check mode for list literals ─────────────────────────────────
            NodeKind::ListLiteral { .. } => {
                if let Type::Generic(g) = expected {
                    if g.constructor == "List" && g.args.len() == 1 {
                        let elem_ty = g.args[0].clone();
                        if let NodeKind::ListLiteral { elems } = &mut node.kind {
                            for elem in elems.iter_mut() {
                                let et = elem_ty.clone();
                                self.check_node(elem, &et);
                            }
                        }
                        self.record(node, expected.clone());
                        return;
                    }
                }
                // Fallthrough to infer mode
                let inferred = self.infer_node(node);
                self.unify_or_error(&inferred, expected, span, "list literal");
            }

            // ── Check mode for lambdas (push param types from context) ────────
            NodeKind::Lambda { .. } => {
                if let Type::Function(f_expected) = expected {
                    let param_types = f_expected.params.clone();
                    let ret_ty = *f_expected.ret.clone();

                    self.env.push_scope();
                    if let NodeKind::Lambda { params, body } = &mut node.kind {
                        for (param, pty) in params.iter_mut().zip(param_types.iter()) {
                            if let Some(name) = param.kind.param_pat_name() {
                                self.env.define(name, pty.clone());
                            }
                            self.record(param, pty.clone());
                        }
                        self.check_node(body, &ret_ty);
                    }
                    self.env.pop_scope();
                    self.record(node, expected.clone());
                } else {
                    let inferred = self.infer_node(node);
                    self.unify_or_error(&inferred, expected, span, "lambda");
                }
            }

            // ── Check mode for match expression ───────────────────────────────
            NodeKind::Match { .. } => {
                // Type-check scrutinee by inference, then check each arm body
                // against the expected type.
                let scrutinee_ty = if let NodeKind::Match { scrutinee, .. } = &mut node.kind {
                    self.infer_node(scrutinee)
                } else {
                    unreachable!()
                };

                if let NodeKind::Match { arms, .. } = &mut node.kind {
                    for arm in arms.iter_mut() {
                        self.env.push_scope();
                        if let NodeKind::MatchArm {
                            pattern,
                            guard,
                            body,
                        } = &mut arm.kind
                        {
                            self.bind_pattern_type(pattern, &scrutinee_ty.clone());
                            if let Some(g) = guard {
                                let bt = Type::Primitive(PrimitiveType::Bool);
                                self.check_node(g, &bt);
                            }
                            let et = expected.clone();
                            self.check_node(body, &et);
                        }
                        self.env.pop_scope();
                        self.record(arm, expected.clone());
                    }
                }
                self.record(node, expected.clone());
            }

            // ── Check mode for if expression ──────────────────────────────────
            NodeKind::If { .. } => {
                if let NodeKind::If {
                    condition,
                    then_block,
                    else_block,
                    ..
                } = &mut node.kind
                {
                    let bt = Type::Primitive(PrimitiveType::Bool);
                    self.check_node(condition, &bt);
                    let et = expected.clone();
                    self.check_node(then_block, &et);
                    if let Some(eb) = else_block {
                        let et2 = expected.clone();
                        self.check_node(eb, &et2);
                    }
                }
                self.record(node, expected.clone());
            }

            // ── Check mode for block ──────────────────────────────────────────
            NodeKind::Block { .. } => {
                if let NodeKind::Block { stmts, tail } = &mut node.kind {
                    self.env.push_scope();
                    for stmt in stmts.iter_mut() {
                        self.infer_node(stmt);
                    }
                    if let Some(tail_expr) = tail {
                        let et = expected.clone();
                        self.check_node(tail_expr, &et);
                    } else {
                        // No tail: block type is Void; unify with expected.
                        let void_ty = Type::Primitive(PrimitiveType::Void);
                        self.unify_or_error(&void_ty, expected, node.span, "block");
                    }
                    self.env.pop_scope();
                }
                self.record(node, expected.clone());
            }

            // ── Everything else: infer then check ─────────────────────────────
            _ => {
                let inferred = self.infer_node(node);
                let expected = self.subst.apply(expected);
                self.unify_or_error(&inferred, &expected, span, "expression");
            }
        }
    }

    // ── If expression ────────────────────────────────────────────────────────

    fn infer_if(&mut self, node: &mut AIRNode) -> Type {
        let span = node.span;
        if let NodeKind::If {
            condition,
            then_block,
            else_block,
            ..
        } = &mut node.kind
        {
            let bool_ty = Type::Primitive(PrimitiveType::Bool);
            self.check_node(condition, &bool_ty);
            let then_ty = self.infer_node(then_block);
            if let Some(eb) = else_block {
                let else_ty = self.infer_node(eb);
                let never = Type::Primitive(PrimitiveType::Never);
                // If one branch diverges (Never), the result is the other branch's type.
                let (a, b) = if then_ty == never {
                    (&else_ty, &then_ty)
                } else {
                    (&then_ty, &else_ty)
                };
                self.unify_or_error(a, b, span, "if-else branches")
            } else {
                // No else: result is Optional[then_ty] or Void
                Type::Primitive(PrimitiveType::Void)
            }
        } else {
            unreachable!()
        }
    }

    // ── Match expression ─────────────────────────────────────────────────────

    fn infer_match(&mut self, node: &mut AIRNode) -> Type {
        let span = node.span;
        let never = Type::Primitive(PrimitiveType::Never);
        // Infer scrutinee type
        let scrutinee_ty = if let NodeKind::Match { scrutinee, .. } = &mut node.kind {
            self.infer_node(scrutinee)
        } else {
            unreachable!()
        };

        // Infer each arm's body type, collecting them.
        let mut arm_types: Vec<Type> = Vec::new();
        if let NodeKind::Match { arms, .. } = &mut node.kind {
            for arm in arms.iter_mut() {
                self.env.push_scope();
                let arm_ty = if let NodeKind::MatchArm {
                    pattern,
                    guard,
                    body,
                } = &mut arm.kind
                {
                    self.bind_pattern_type(pattern, &scrutinee_ty.clone());
                    if let Some(g) = guard {
                        let bt = Type::Primitive(PrimitiveType::Bool);
                        self.check_node(g, &bt);
                    }
                    self.infer_node(body)
                } else {
                    self.fresh_var()
                };
                self.env.pop_scope();
                self.record(arm, arm_ty.clone());
                arm_types.push(arm_ty);
            }
        }

        // Filter out Never arms; unify the rest.
        let non_never: Vec<&Type> = arm_types.iter().filter(|t| **t != never).collect();
        if non_never.is_empty() {
            // All arms diverge — match type is Never.
            never
        } else {
            let result_ty = self.fresh_var();
            for t in &non_never {
                let rt = result_ty.clone();
                self.unify_or_error(t, &rt, span, "match arm");
            }
            self.subst.apply(&result_ty)
        }
    }

    // ── Block expression ─────────────────────────────────────────────────────

    fn infer_block(&mut self, node: &mut AIRNode) -> Type {
        self.env.push_scope();
        let ty = if let NodeKind::Block { stmts, tail } = &mut node.kind {
            for stmt in stmts.iter_mut() {
                self.infer_node(stmt);
            }
            if let Some(tail_expr) = tail {
                self.infer_node(tail_expr)
            } else {
                Type::Primitive(PrimitiveType::Void)
            }
        } else {
            unreachable!()
        };
        self.env.pop_scope();
        ty
    }

    // ── Let binding ──────────────────────────────────────────────────────────

    fn check_let_binding(&mut self, node: &mut AIRNode) {
        let (ty_node, _value_clone) = match &node.kind {
            NodeKind::LetBinding { ty, value, .. } => (ty.clone(), *value.clone()),
            _ => return,
        };

        if let Some(ty_ann) = &ty_node {
            let expected = self.air_type_node_to_type(ty_ann, &HashMap::new());
            if let NodeKind::LetBinding { value, pattern, .. } = &mut node.kind {
                self.check_node(value, &expected);
                self.bind_pattern_type(pattern, &expected);
            }
        } else {
            // No annotation: infer from value
            let inferred = if let NodeKind::LetBinding { value, .. } = &mut node.kind {
                self.infer_node(value)
            } else {
                unreachable!()
            };
            let resolved = self.subst.apply(&inferred);
            if let NodeKind::LetBinding { pattern, .. } = &mut node.kind {
                self.bind_pattern_type(pattern, &resolved);
            }
        }
    }

    // ── Lambda inference ─────────────────────────────────────────────────────

    /// Infer types for a lambda with no check context (fresh vars for params).
    fn infer_lambda(&mut self, node: &mut AIRNode) -> (Vec<Type>, Type) {
        self.env.push_scope();
        let (param_tys, body_ty) = if let NodeKind::Lambda { params, body } = &mut node.kind {
            let param_tys: Vec<Type> = params
                .iter_mut()
                .map(|p| {
                    let ty = self.fresh_var();
                    if let Some(name) = p.kind.param_pat_name() {
                        self.env.define(name, ty.clone());
                    }
                    ty
                })
                .collect();
            let body_ty = self.infer_node(body);
            (param_tys, body_ty)
        } else {
            unreachable!()
        };
        self.env.pop_scope();
        (param_tys, body_ty)
    }

    // ── Function call type checking ──────────────────────────────────────────

    /// Given the type of the callee and the argument list, return the return type.
    /// Handles generic instantiation.
    fn check_call(
        &mut self,
        callee_span: Span,
        callee_ty: &Type,
        args: &[bock_air::AirArg],
        call_span: Span,
    ) -> Type {
        match callee_ty {
            Type::Error => Type::Error,
            Type::Function(f) => {
                // Non-generic call: check arity then return ret type.
                if f.params.len() != args.len() {
                    self.diags.error(
                        E_ARITY_MISMATCH,
                        format!(
                            "function expects {} argument(s), got {}",
                            f.params.len(),
                            args.len()
                        ),
                        call_span,
                    );
                    return Type::Error;
                }
                self.subst.apply(&f.ret)
            }
            _ => {
                // Could still be a named function looked up in env.
                // If callee_ty is Named, try to find in fn_sigs.
                if let Type::Named(nt) = callee_ty {
                    if let Some(sig) = self.fn_sigs.get(&nt.name).cloned() {
                        return self.instantiate_and_check(&nt.name, &sig, args, call_span);
                    }
                }
                // If the callee's type is still an inference variable (e.g.
                // a method call on a parameter with an unknown built-in
                // type like `Channel[T]`), don't commit to "not callable" —
                // return a fresh var so downstream code can continue.
                if matches!(callee_ty, Type::TypeVar(_)) {
                    return self.fresh_var();
                }
                self.diags.error(
                    E_NOT_CALLABLE,
                    format!("expected a function type, got {callee_ty:?}"),
                    callee_span,
                );
                Type::Error
            }
        }
    }

    /// Instantiate a generic function signature with fresh type vars and
    /// return the (substituted) return type.
    ///
    /// Maps the original [`TypeVarId`]s from the signature to new fresh
    /// variables using [`replace_type_vars`](Self::replace_type_vars), so
    /// each call site gets independent type inference.
    fn instantiate_and_check(
        &mut self,
        fn_name: &str,
        sig: &FnSig,
        args: &[bock_air::AirArg],
        span: Span,
    ) -> Type {
        if sig.param_types.len() != args.len() {
            self.diags.error(
                E_ARITY_MISMATCH,
                format!(
                    "function expects {} argument(s), got {}",
                    sig.param_types.len(),
                    args.len()
                ),
                span,
            );
            return Type::Error;
        }

        // Create fresh vars for each generic parameter, keyed by the
        // original TypeVarId from collect_sig.
        let fresh_map: HashMap<TypeVarId, Type> = sig
            .generic_var_ids
            .iter()
            .map(|&id| (id, self.fresh_var()))
            .collect();

        // Substitute generic params in param types (used for arg unification
        // by the caller) and return type.
        let _param_tys: Vec<Type> = sig
            .param_types
            .iter()
            .map(|t| self.replace_type_vars(t, &fresh_map))
            .collect();

        // Check where-clause trait bounds.
        self.check_trait_bounds_at_call(fn_name, sig, &fresh_map, span);

        // Substitute in return type.
        self.replace_type_vars(&sig.return_type, &fresh_map)
    }

    // ── Method return-type resolution ─────────────────────────────────────

    /// Resolve the return type of a method call on a known receiver type.
    ///
    /// Returns a concrete type when the receiver type and method name
    /// identify a well-known built-in method; falls back to a fresh type
    /// variable otherwise.
    fn resolve_method_return_type(&self, receiver_ty: &Type, method: &str) -> Type {
        let receiver_ty = self.subst.apply(receiver_ty);
        match &receiver_ty {
            Type::Error => Type::Error,
            // List[T] methods
            Type::Generic(g) if g.constructor == "List" && g.args.len() == 1 => {
                let elem_ty = &g.args[0];
                match method {
                    "len" | "length" | "count" => Type::Primitive(PrimitiveType::Int),
                    "first" | "last" | "find" | "get" => {
                        Type::Optional(Box::new(elem_ty.clone()))
                    }
                    "index_of" => {
                        Type::Optional(Box::new(Type::Primitive(PrimitiveType::Int)))
                    }
                    "contains" | "is_empty" | "any" | "all" => {
                        Type::Primitive(PrimitiveType::Bool)
                    }
                    "push" | "append" | "pop" | "insert" | "remove" | "concat"
                    | "reverse" | "sort" | "filter" | "dedup" | "take" | "skip"
                    | "flat_map" | "slice" | "flatten" => receiver_ty.clone(),
                    "clear" | "for_each" => Type::Primitive(PrimitiveType::Void),
                    "join" | "display" => Type::Primitive(PrimitiveType::String),
                    "enumerate" => Type::Generic(GenericType {
                        constructor: "List".into(),
                        args: vec![Type::Tuple(vec![
                            Type::Primitive(PrimitiveType::Int),
                            elem_ty.clone(),
                        ])],
                    }),
                    "to_set" => Type::Generic(GenericType {
                        constructor: "Set".into(),
                        args: vec![elem_ty.clone()],
                    }),
                    _ => self.fresh_var(),
                }
            }
            // Map[K, V] methods
            Type::Generic(g) if g.constructor == "Map" && g.args.len() == 2 => {
                let key_ty = &g.args[0];
                let val_ty = &g.args[1];
                match method {
                    "len" | "length" | "count" => Type::Primitive(PrimitiveType::Int),
                    "contains_key" | "is_empty" => Type::Primitive(PrimitiveType::Bool),
                    "get" => Type::Optional(Box::new(val_ty.clone())),
                    "set" | "delete" | "merge" | "filter" => receiver_ty.clone(),
                    "for_each" => Type::Primitive(PrimitiveType::Void),
                    "keys" => Type::Generic(GenericType {
                        constructor: "List".into(),
                        args: vec![key_ty.clone()],
                    }),
                    "values" => Type::Generic(GenericType {
                        constructor: "List".into(),
                        args: vec![val_ty.clone()],
                    }),
                    "entries" | "to_list" => Type::Generic(GenericType {
                        constructor: "List".into(),
                        args: vec![Type::Tuple(vec![key_ty.clone(), val_ty.clone()])],
                    }),
                    _ => self.fresh_var(),
                }
            }
            // String methods
            Type::Primitive(PrimitiveType::String) => match method {
                "len" | "length" | "count" | "byte_len" => {
                    Type::Primitive(PrimitiveType::Int)
                }
                "contains" | "starts_with" | "ends_with" | "is_empty"
                | "regex_match" => Type::Primitive(PrimitiveType::Bool),
                "to_upper" | "to_lower" | "trim" | "trim_start" | "trim_end"
                | "reverse" | "slice" | "substring" | "replace" | "to_string"
                | "display" | "repeat" | "pad_start" | "pad_end" | "format"
                | "regex_replace" | "join" => Type::Primitive(PrimitiveType::String),
                "split" | "regex_find" => Type::Generic(GenericType {
                    constructor: "List".into(),
                    args: vec![Type::Primitive(PrimitiveType::String)],
                }),
                "chars" => Type::Generic(GenericType {
                    constructor: "List".into(),
                    args: vec![Type::Primitive(PrimitiveType::Char)],
                }),
                "bytes" => Type::Generic(GenericType {
                    constructor: "List".into(),
                    args: vec![Type::Primitive(PrimitiveType::Int)],
                }),
                "index_of" => Type::Optional(Box::new(Type::Primitive(PrimitiveType::Int))),
                "char_at" => {
                    Type::Optional(Box::new(Type::Primitive(PrimitiveType::Char)))
                }
                _ => self.fresh_var(),
            },
            // Int methods
            Type::Primitive(PrimitiveType::Int) => match method {
                "abs" | "min" | "max" | "clamp" | "shift_left" | "shift_right"
                | "compare" | "hash_code" => Type::Primitive(PrimitiveType::Int),
                "to_float" => Type::Primitive(PrimitiveType::Float),
                "to_string" | "display" => Type::Primitive(PrimitiveType::String),
                "equals" => Type::Primitive(PrimitiveType::Bool),
                _ => self.fresh_var(),
            },
            // Float methods
            Type::Primitive(PrimitiveType::Float) => match method {
                "abs" | "floor" | "ceil" | "round" | "sqrt" | "min" | "max" | "clamp" => {
                    Type::Primitive(PrimitiveType::Float)
                }
                "to_int" => Type::Primitive(PrimitiveType::Int),
                "to_string" | "display" => Type::Primitive(PrimitiveType::String),
                "is_nan" | "is_infinite" | "equals" => Type::Primitive(PrimitiveType::Bool),
                "compare" | "hash_code" => Type::Primitive(PrimitiveType::Int),
                _ => self.fresh_var(),
            },
            // Bool methods
            Type::Primitive(PrimitiveType::Bool) => match method {
                "negate" => Type::Primitive(PrimitiveType::Bool),
                "to_int" => Type::Primitive(PrimitiveType::Int),
                "to_string" | "display" => Type::Primitive(PrimitiveType::String),
                "compare" | "hash_code" => Type::Primitive(PrimitiveType::Int),
                "equals" => Type::Primitive(PrimitiveType::Bool),
                _ => self.fresh_var(),
            },
            // Char methods
            Type::Primitive(PrimitiveType::Char) => match method {
                "to_upper" | "to_lower" => Type::Primitive(PrimitiveType::Char),
                "is_alpha" | "is_digit" | "is_whitespace" | "equals" => {
                    Type::Primitive(PrimitiveType::Bool)
                }
                "to_int" | "compare" | "hash_code" => Type::Primitive(PrimitiveType::Int),
                "to_string" | "display" => Type::Primitive(PrimitiveType::String),
                _ => self.fresh_var(),
            },
            // Set[E] methods
            Type::Generic(g) if g.constructor == "Set" && g.args.len() == 1 => {
                let elem_ty = &g.args[0];
                match method {
                    "len" | "length" | "count" => Type::Primitive(PrimitiveType::Int),
                    "contains" | "is_empty" | "is_subset" | "is_superset"
                    | "is_disjoint" => Type::Primitive(PrimitiveType::Bool),
                    "add" | "remove" | "union" | "intersection" | "difference"
                    | "symmetric_difference" | "filter" | "map" => receiver_ty.clone(),
                    "for_each" => Type::Primitive(PrimitiveType::Void),
                    "to_list" => Type::Generic(GenericType {
                        constructor: "List".into(),
                        args: vec![elem_ty.clone()],
                    }),
                    _ => self.fresh_var(),
                }
            }
            // Optional[T] methods
            Type::Optional(inner_ty) => match method {
                "is_some" | "is_none" => Type::Primitive(PrimitiveType::Bool),
                "unwrap" | "unwrap_or" => *inner_ty.clone(),
                _ => self.fresh_var(),
            },
            // Result[T, E] methods
            Type::Result(ok_ty, _err_ty) => match method {
                "is_ok" | "is_err" => Type::Primitive(PrimitiveType::Bool),
                "unwrap" | "unwrap_or" => *ok_ty.clone(),
                _ => self.fresh_var(),
            },
            // User-defined types: look up inherent impl methods.
            Type::Named(nt) => {
                if let Some(methods) = self.method_types.get(&nt.name) {
                    if let Some(Type::Function(f)) = methods.get(method) {
                        return self.subst.apply(&f.ret);
                    }
                }
                self.fresh_var()
            }
            // User-defined generic types: look up inherent impl methods and
            // substitute type parameters in the return type.
            Type::Generic(g) => {
                if let Some(methods) = self.method_types.get(&g.constructor) {
                    if let Some(Type::Function(f)) = methods.get(method) {
                        let ret_ty = self.subst.apply(&f.ret);
                        if let Some(params) = self.record_generic_params.get(&g.constructor) {
                            return substitute_type_params(&ret_ty, params, &g.args);
                        }
                        return ret_ty;
                    }
                }
                self.fresh_var()
            }
            _ => self.fresh_var(),
        }
    }

    /// Return the full `Function` type for a built-in method accessed as a
    /// field (e.g. `items.len` yields `Fn(List[Int]) -> Int`).
    ///
    /// The AIR lowering desugars `obj.method(args)` into
    /// `Call(FieldAccess(obj, method), [obj, ...args])`, so the receiver
    /// is always the first parameter in the returned function type.
    ///
    /// Returns `None` when the method is unknown so the caller can fall
    /// back to a fresh type var.
    fn resolve_builtin_method_fn_type(&self, receiver_ty: &Type, method: &str) -> Option<Type> {
        let receiver_ty = self.subst.apply(receiver_ty);
        let mk = |recv: &Type, params: Vec<Type>, ret: Type| -> Option<Type> {
            let mut all_params = vec![recv.clone()];
            all_params.extend(params);
            Some(Type::Function(FnType {
                params: all_params,
                ret: Box::new(ret),
                effects: vec![],
            }))
        };
        match &receiver_ty {
            Type::Generic(g) if g.constructor == "List" && g.args.len() == 1 => {
                let elem = &g.args[0];
                let r = &receiver_ty;
                match method {
                    "len" | "length" | "count" => mk(r, vec![], Type::Primitive(PrimitiveType::Int)),
                    "is_empty" => mk(r, vec![], Type::Primitive(PrimitiveType::Bool)),
                    "contains" => mk(r, vec![elem.clone()], Type::Primitive(PrimitiveType::Bool)),
                    "first" | "last" => mk(r, vec![], Type::Optional(Box::new(elem.clone()))),
                    "find" => {
                        let cb = Type::Function(FnType {
                            params: vec![elem.clone()],
                            ret: Box::new(Type::Primitive(PrimitiveType::Bool)),
                            effects: vec![],
                        });
                        mk(r, vec![cb], Type::Optional(Box::new(elem.clone())))
                    }
                    "get" => mk(
                        r,
                        vec![Type::Primitive(PrimitiveType::Int)],
                        Type::Optional(Box::new(elem.clone())),
                    ),
                    "index_of" => mk(
                        r,
                        vec![elem.clone()],
                        Type::Optional(Box::new(Type::Primitive(PrimitiveType::Int))),
                    ),
                    "push" | "append" => mk(r, vec![elem.clone()], receiver_ty.clone()),
                    "pop" => mk(r, vec![], receiver_ty.clone()),
                    "insert" => mk(
                        r,
                        vec![Type::Primitive(PrimitiveType::Int), elem.clone()],
                        receiver_ty.clone(),
                    ),
                    "remove" => mk(
                        r,
                        vec![Type::Primitive(PrimitiveType::Int)],
                        receiver_ty.clone(),
                    ),
                    "concat" => mk(r, vec![receiver_ty.clone()], receiver_ty.clone()),
                    "clear" => mk(r, vec![], Type::Primitive(PrimitiveType::Void)),
                    "reverse" | "sort" | "dedup" | "flatten" => {
                        mk(r, vec![], receiver_ty.clone())
                    }
                    "take" | "skip" => mk(
                        r,
                        vec![Type::Primitive(PrimitiveType::Int)],
                        receiver_ty.clone(),
                    ),
                    "slice" => mk(
                        r,
                        vec![
                            Type::Primitive(PrimitiveType::Int),
                            Type::Primitive(PrimitiveType::Int),
                        ],
                        receiver_ty.clone(),
                    ),
                    "filter" => {
                        let cb = Type::Function(FnType {
                            params: vec![elem.clone()],
                            ret: Box::new(Type::Primitive(PrimitiveType::Bool)),
                            effects: vec![],
                        });
                        mk(r, vec![cb], receiver_ty.clone())
                    }
                    "map" => {
                        let u = self.fresh_var();
                        let cb = Type::Function(FnType {
                            params: vec![elem.clone()],
                            ret: Box::new(u.clone()),
                            effects: vec![],
                        });
                        let ret = Type::Generic(GenericType {
                            constructor: "List".into(),
                            args: vec![u],
                        });
                        mk(r, vec![cb], ret)
                    }
                    "flat_map" => {
                        let u = self.fresh_var();
                        let inner_list = Type::Generic(GenericType {
                            constructor: "List".into(),
                            args: vec![u.clone()],
                        });
                        let cb = Type::Function(FnType {
                            params: vec![elem.clone()],
                            ret: Box::new(inner_list),
                            effects: vec![],
                        });
                        let ret = Type::Generic(GenericType {
                            constructor: "List".into(),
                            args: vec![u],
                        });
                        mk(r, vec![cb], ret)
                    }
                    "fold" => {
                        let acc = self.fresh_var();
                        let cb = Type::Function(FnType {
                            params: vec![acc.clone(), elem.clone()],
                            ret: Box::new(acc.clone()),
                            effects: vec![],
                        });
                        mk(r, vec![acc.clone(), cb], acc)
                    }
                    "reduce" => {
                        let cb = Type::Function(FnType {
                            params: vec![elem.clone(), elem.clone()],
                            ret: Box::new(elem.clone()),
                            effects: vec![],
                        });
                        mk(r, vec![cb], elem.clone())
                    }
                    "for_each" => {
                        let cb = Type::Function(FnType {
                            params: vec![elem.clone()],
                            ret: Box::new(Type::Primitive(PrimitiveType::Void)),
                            effects: vec![],
                        });
                        mk(r, vec![cb], Type::Primitive(PrimitiveType::Void))
                    }
                    "any" | "all" => {
                        let cb = Type::Function(FnType {
                            params: vec![elem.clone()],
                            ret: Box::new(Type::Primitive(PrimitiveType::Bool)),
                            effects: vec![],
                        });
                        mk(r, vec![cb], Type::Primitive(PrimitiveType::Bool))
                    }
                    "enumerate" => {
                        let pair = Type::Tuple(vec![
                            Type::Primitive(PrimitiveType::Int),
                            elem.clone(),
                        ]);
                        mk(r, vec![], Type::Generic(GenericType {
                            constructor: "List".into(),
                            args: vec![pair],
                        }))
                    }
                    "zip" => {
                        let f = self.fresh_var();
                        let other_list = Type::Generic(GenericType {
                            constructor: "List".into(),
                            args: vec![f.clone()],
                        });
                        let pair = Type::Tuple(vec![elem.clone(), f]);
                        mk(r, vec![other_list], Type::Generic(GenericType {
                            constructor: "List".into(),
                            args: vec![pair],
                        }))
                    }
                    "join" => mk(
                        r,
                        vec![Type::Primitive(PrimitiveType::String)],
                        Type::Primitive(PrimitiveType::String),
                    ),
                    "to_set" => mk(r, vec![], Type::Generic(GenericType {
                        constructor: "Set".into(),
                        args: vec![elem.clone()],
                    })),
                    _ => None,
                }
            }
            Type::Generic(g) if g.constructor == "Map" && g.args.len() == 2 => {
                let key = &g.args[0];
                let val = &g.args[1];
                let r = &receiver_ty;
                match method {
                    "len" | "length" | "count" => mk(r, vec![], Type::Primitive(PrimitiveType::Int)),
                    "is_empty" => mk(r, vec![], Type::Primitive(PrimitiveType::Bool)),
                    "contains_key" => {
                        mk(r, vec![key.clone()], Type::Primitive(PrimitiveType::Bool))
                    }
                    "get" => mk(r, vec![key.clone()], Type::Optional(Box::new(val.clone()))),
                    "set" => mk(r, vec![key.clone(), val.clone()], receiver_ty.clone()),
                    "delete" => mk(r, vec![key.clone()], receiver_ty.clone()),
                    "merge" => mk(r, vec![receiver_ty.clone()], receiver_ty.clone()),
                    "keys" => mk(
                        r,
                        vec![],
                        Type::Generic(GenericType {
                            constructor: "List".into(),
                            args: vec![key.clone()],
                        }),
                    ),
                    "values" => mk(
                        r,
                        vec![],
                        Type::Generic(GenericType {
                            constructor: "List".into(),
                            args: vec![val.clone()],
                        }),
                    ),
                    "entries" | "to_list" => mk(
                        r,
                        vec![],
                        Type::Generic(GenericType {
                            constructor: "List".into(),
                            args: vec![Type::Tuple(vec![key.clone(), val.clone()])],
                        }),
                    ),
                    "map_values" => {
                        let u = self.fresh_var();
                        let cb = Type::Function(FnType {
                            params: vec![val.clone()],
                            ret: Box::new(u.clone()),
                            effects: vec![],
                        });
                        mk(r, vec![cb], Type::Generic(GenericType {
                            constructor: "Map".into(),
                            args: vec![key.clone(), u],
                        }))
                    }
                    "filter" => {
                        let cb = Type::Function(FnType {
                            params: vec![key.clone(), val.clone()],
                            ret: Box::new(Type::Primitive(PrimitiveType::Bool)),
                            effects: vec![],
                        });
                        mk(r, vec![cb], receiver_ty.clone())
                    }
                    "for_each" => {
                        let cb = Type::Function(FnType {
                            params: vec![key.clone(), val.clone()],
                            ret: Box::new(Type::Primitive(PrimitiveType::Void)),
                            effects: vec![],
                        });
                        mk(r, vec![cb], Type::Primitive(PrimitiveType::Void))
                    }
                    _ => None,
                }
            }
            Type::Generic(g) if g.constructor == "Set" && g.args.len() == 1 => {
                let elem = &g.args[0];
                let r = &receiver_ty;
                match method {
                    "len" | "length" | "count" => mk(r, vec![], Type::Primitive(PrimitiveType::Int)),
                    "is_empty" => mk(r, vec![], Type::Primitive(PrimitiveType::Bool)),
                    "contains" => mk(r, vec![elem.clone()], Type::Primitive(PrimitiveType::Bool)),
                    "add" | "remove" => mk(r, vec![elem.clone()], receiver_ty.clone()),
                    "union" | "intersection" | "difference"
                    | "symmetric_difference" => {
                        mk(r, vec![receiver_ty.clone()], receiver_ty.clone())
                    }
                    "is_subset" | "is_superset" | "is_disjoint" => {
                        mk(r, vec![receiver_ty.clone()], Type::Primitive(PrimitiveType::Bool))
                    }
                    "filter" => {
                        let cb = Type::Function(FnType {
                            params: vec![elem.clone()],
                            ret: Box::new(Type::Primitive(PrimitiveType::Bool)),
                            effects: vec![],
                        });
                        mk(r, vec![cb], receiver_ty.clone())
                    }
                    "map" => {
                        let cb = Type::Function(FnType {
                            params: vec![elem.clone()],
                            ret: Box::new(elem.clone()),
                            effects: vec![],
                        });
                        mk(r, vec![cb], receiver_ty.clone())
                    }
                    "for_each" => {
                        let cb = Type::Function(FnType {
                            params: vec![elem.clone()],
                            ret: Box::new(Type::Primitive(PrimitiveType::Void)),
                            effects: vec![],
                        });
                        mk(r, vec![cb], Type::Primitive(PrimitiveType::Void))
                    }
                    "to_list" => mk(r, vec![], Type::Generic(GenericType {
                        constructor: "List".into(),
                        args: vec![elem.clone()],
                    })),
                    _ => None,
                }
            }
            Type::Primitive(PrimitiveType::String) => {
                let r = &receiver_ty;
                let str_ty = Type::Primitive(PrimitiveType::String);
                let int_ty = Type::Primitive(PrimitiveType::Int);
                match method {
                    "len" | "length" | "count" | "byte_len" => {
                        mk(r, vec![], int_ty)
                    }
                    "is_empty" => mk(r, vec![], Type::Primitive(PrimitiveType::Bool)),
                    "contains" | "starts_with" | "ends_with" => {
                        mk(r, vec![str_ty.clone()], Type::Primitive(PrimitiveType::Bool))
                    }
                    "regex_match" => {
                        mk(r, vec![str_ty.clone()], Type::Primitive(PrimitiveType::Bool))
                    }
                    "to_upper" | "to_lower" | "trim" | "trim_start" | "trim_end"
                    | "reverse" | "to_string" | "display" => {
                        mk(r, vec![], str_ty)
                    }
                    "repeat" => mk(
                        r,
                        vec![Type::Primitive(PrimitiveType::Int)],
                        str_ty,
                    ),
                    "slice" | "substring" => mk(
                        r,
                        vec![
                            Type::Primitive(PrimitiveType::Int),
                            Type::Primitive(PrimitiveType::Int),
                        ],
                        str_ty,
                    ),
                    "replace" | "regex_replace" => mk(
                        r,
                        vec![str_ty.clone(), str_ty.clone()],
                        str_ty,
                    ),
                    "pad_start" | "pad_end" => mk(
                        r,
                        vec![Type::Primitive(PrimitiveType::Int), str_ty.clone()],
                        str_ty,
                    ),
                    "format" => mk(r, vec![], str_ty),
                    "join" => mk(
                        r,
                        vec![Type::Generic(GenericType {
                            constructor: "List".into(),
                            args: vec![str_ty.clone()],
                        })],
                        str_ty,
                    ),
                    "split" => mk(
                        r,
                        vec![str_ty],
                        Type::Generic(GenericType {
                            constructor: "List".into(),
                            args: vec![Type::Primitive(PrimitiveType::String)],
                        }),
                    ),
                    "regex_find" => mk(
                        r,
                        vec![Type::Primitive(PrimitiveType::String)],
                        Type::Generic(GenericType {
                            constructor: "List".into(),
                            args: vec![Type::Primitive(PrimitiveType::String)],
                        }),
                    ),
                    "chars" => mk(
                        r,
                        vec![],
                        Type::Generic(GenericType {
                            constructor: "List".into(),
                            args: vec![Type::Primitive(PrimitiveType::Char)],
                        }),
                    ),
                    "bytes" => mk(
                        r,
                        vec![],
                        Type::Generic(GenericType {
                            constructor: "List".into(),
                            args: vec![Type::Primitive(PrimitiveType::Int)],
                        }),
                    ),
                    "index_of" => mk(
                        r,
                        vec![Type::Primitive(PrimitiveType::String)],
                        Type::Optional(Box::new(Type::Primitive(PrimitiveType::Int))),
                    ),
                    "char_at" => mk(
                        r,
                        vec![Type::Primitive(PrimitiveType::Int)],
                        Type::Optional(Box::new(Type::Primitive(PrimitiveType::Char))),
                    ),
                    _ => None,
                }
            }
            Type::Primitive(PrimitiveType::Int) => {
                let r = &receiver_ty;
                let int_ty = Type::Primitive(PrimitiveType::Int);
                match method {
                    "abs" => mk(r, vec![], int_ty),
                    "min" | "max" | "shift_left" | "shift_right" | "compare" => mk(
                        r,
                        vec![int_ty.clone()],
                        int_ty,
                    ),
                    "clamp" => mk(
                        r,
                        vec![int_ty.clone(), int_ty.clone()],
                        int_ty,
                    ),
                    "equals" => mk(
                        r,
                        vec![Type::Primitive(PrimitiveType::Int)],
                        Type::Primitive(PrimitiveType::Bool),
                    ),
                    "hash_code" => mk(r, vec![], Type::Primitive(PrimitiveType::Int)),
                    "to_float" => mk(r, vec![], Type::Primitive(PrimitiveType::Float)),
                    "to_string" | "display" => {
                        mk(r, vec![], Type::Primitive(PrimitiveType::String))
                    }
                    _ => None,
                }
            }
            Type::Primitive(PrimitiveType::Float) => {
                let r = &receiver_ty;
                let float_ty = Type::Primitive(PrimitiveType::Float);
                match method {
                    "abs" | "floor" | "ceil" | "round" | "sqrt" => {
                        mk(r, vec![], float_ty)
                    }
                    "min" | "max" => mk(
                        r,
                        vec![float_ty.clone()],
                        float_ty,
                    ),
                    "clamp" => mk(
                        r,
                        vec![float_ty.clone(), float_ty.clone()],
                        float_ty,
                    ),
                    "to_int" => mk(r, vec![], Type::Primitive(PrimitiveType::Int)),
                    "to_string" | "display" => {
                        mk(r, vec![], Type::Primitive(PrimitiveType::String))
                    }
                    "is_nan" | "is_infinite" | "equals" => {
                        mk(r, vec![], Type::Primitive(PrimitiveType::Bool))
                    }
                    "compare" | "hash_code" => {
                        mk(r, vec![], Type::Primitive(PrimitiveType::Int))
                    }
                    _ => None,
                }
            }
            Type::Primitive(PrimitiveType::Bool) => {
                let r = &receiver_ty;
                match method {
                    "negate" | "equals" => mk(r, vec![], Type::Primitive(PrimitiveType::Bool)),
                    "to_int" | "compare" | "hash_code" => {
                        mk(r, vec![], Type::Primitive(PrimitiveType::Int))
                    }
                    "to_string" | "display" => {
                        mk(r, vec![], Type::Primitive(PrimitiveType::String))
                    }
                    _ => None,
                }
            }
            Type::Primitive(PrimitiveType::Char) => {
                let r = &receiver_ty;
                match method {
                    "to_upper" | "to_lower" => mk(r, vec![], Type::Primitive(PrimitiveType::Char)),
                    "is_alpha" | "is_digit" | "is_whitespace" | "equals" => {
                        mk(r, vec![], Type::Primitive(PrimitiveType::Bool))
                    }
                    "to_int" | "compare" | "hash_code" => {
                        mk(r, vec![], Type::Primitive(PrimitiveType::Int))
                    }
                    "to_string" | "display" => {
                        mk(r, vec![], Type::Primitive(PrimitiveType::String))
                    }
                    _ => None,
                }
            }
            // Optional[T] methods
            Type::Optional(inner_ty) => {
                let r = &receiver_ty;
                let inner = *inner_ty.clone();
                match method {
                    "is_some" | "is_none" => {
                        mk(r, vec![], Type::Primitive(PrimitiveType::Bool))
                    }
                    "unwrap" => mk(r, vec![], inner),
                    "unwrap_or" => mk(r, vec![inner.clone()], inner),
                    "map" => {
                        let u = self.fresh_var();
                        let cb = Type::Function(FnType {
                            params: vec![inner],
                            ret: Box::new(u.clone()),
                            effects: vec![],
                        });
                        mk(r, vec![cb], Type::Optional(Box::new(u)))
                    }
                    "flat_map" => {
                        let u = self.fresh_var();
                        let opt_u = Type::Optional(Box::new(u));
                        let cb = Type::Function(FnType {
                            params: vec![inner],
                            ret: Box::new(opt_u.clone()),
                            effects: vec![],
                        });
                        mk(r, vec![cb], opt_u)
                    }
                    _ => None,
                }
            }
            // Result[T, E] methods
            Type::Result(ok_ty, err_ty) => {
                let r = &receiver_ty;
                let ok = *ok_ty.clone();
                let err = *err_ty.clone();
                match method {
                    "is_ok" | "is_err" => {
                        mk(r, vec![], Type::Primitive(PrimitiveType::Bool))
                    }
                    "unwrap" => mk(r, vec![], ok),
                    "unwrap_or" => mk(r, vec![ok.clone()], ok),
                    "map" => {
                        let u = self.fresh_var();
                        let cb = Type::Function(FnType {
                            params: vec![ok],
                            ret: Box::new(u.clone()),
                            effects: vec![],
                        });
                        mk(r, vec![cb], Type::Result(Box::new(u), Box::new(err)))
                    }
                    "map_err" => {
                        let e2 = self.fresh_var();
                        let cb = Type::Function(FnType {
                            params: vec![err],
                            ret: Box::new(e2.clone()),
                            effects: vec![],
                        });
                        mk(r, vec![cb], Type::Result(Box::new(ok), Box::new(e2)))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    // ── Generic type-var replacement ──────────────────────────────────────

    /// Walk `ty` and replace any [`TypeVarId`] found in `map` with the
    /// corresponding fresh type. Used to create per-call-site instantiations
    /// of generic function types.
    fn replace_type_vars(&self, ty: &Type, map: &HashMap<TypeVarId, Type>) -> Type {
        match ty {
            Type::TypeVar(id) => map.get(id).cloned().unwrap_or_else(|| ty.clone()),
            Type::Function(f) => Type::Function(FnType {
                params: f
                    .params
                    .iter()
                    .map(|t| self.replace_type_vars(t, map))
                    .collect(),
                ret: Box::new(self.replace_type_vars(&f.ret, map)),
                effects: f.effects.clone(),
            }),
            Type::Generic(g) => Type::Generic(GenericType {
                constructor: g.constructor.clone(),
                args: g
                    .args
                    .iter()
                    .map(|t| self.replace_type_vars(t, map))
                    .collect(),
            }),
            Type::Tuple(elems) => Type::Tuple(
                elems
                    .iter()
                    .map(|t| self.replace_type_vars(t, map))
                    .collect(),
            ),
            Type::Optional(inner) => Type::Optional(Box::new(self.replace_type_vars(inner, map))),
            Type::Result(ok, err) => Type::Result(
                Box::new(self.replace_type_vars(ok, map)),
                Box::new(self.replace_type_vars(err, map)),
            ),
            _ => ty.clone(),
        }
    }

    // ── Binary / unary op typing ─────────────────────────────────────────────

    fn infer_binop(&mut self, op: BinOp, lt: &Type, rt: &Type, span: Span) -> Type {
        match op {
            // Arithmetic: operands and result are numeric
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem | BinOp::Pow => {
                self.unify_or_error(lt, rt, span, "arithmetic operands");
                self.subst.apply(lt)
            }

            // Comparison: operands must unify; result is Bool
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                self.unify_or_error(lt, rt, span, "comparison operands");
                Type::Primitive(PrimitiveType::Bool)
            }

            // Logical: both sides must be Bool; result is Bool
            BinOp::And | BinOp::Or => {
                let bool_ty = Type::Primitive(PrimitiveType::Bool);
                self.unify_or_error(lt, &bool_ty, span, "logical operand");
                self.unify_or_error(rt, &bool_ty, span, "logical operand");
                bool_ty
            }

            // Bitwise: operands must unify (typically Int); result same
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                self.unify_or_error(lt, rt, span, "bitwise operands");
                self.subst.apply(lt)
            }

            // Compose (>>): Fn(A)->B >> Fn(B)->C = Fn(A)->C
            BinOp::Compose => self.fresh_var(),

            // Type membership (is): result is Bool
            BinOp::Is => Type::Primitive(PrimitiveType::Bool),
        }
    }

    fn infer_unop(&mut self, op: UnaryOp, operand_ty: &Type, span: Span) -> Type {
        match op {
            UnaryOp::Neg => {
                // Numeric negation
                self.subst.apply(operand_ty)
            }
            UnaryOp::Not => {
                // Logical not: operand must be Bool
                let bool_ty = Type::Primitive(PrimitiveType::Bool);
                self.unify_or_error(operand_ty, &bool_ty, span, "logical not operand");
                bool_ty
            }
            UnaryOp::BitNot => {
                // Bitwise not: integer operand
                self.subst.apply(operand_ty)
            }
        }
    }

    // ── Literal typing ───────────────────────────────────────────────────────

    fn infer_literal(&self, lit: &Literal) -> Type {
        match lit {
            Literal::Int(s) => {
                let (_, suffix) = bock_ast::strip_type_suffix(s);
                match suffix {
                    Some("i8") => Type::Primitive(PrimitiveType::Int8),
                    Some("i16") => Type::Primitive(PrimitiveType::Int16),
                    Some("i32") => Type::Primitive(PrimitiveType::Int32),
                    Some("i64") => Type::Primitive(PrimitiveType::Int64),
                    Some("i128") => Type::Primitive(PrimitiveType::Int128),
                    Some("u8") => Type::Primitive(PrimitiveType::UInt8),
                    Some("u16") => Type::Primitive(PrimitiveType::UInt16),
                    Some("u32") => Type::Primitive(PrimitiveType::UInt32),
                    Some("u64") => Type::Primitive(PrimitiveType::UInt64),
                    _ => Type::Primitive(PrimitiveType::Int),
                }
            }
            Literal::Float(s) => {
                let (_, suffix) = bock_ast::strip_type_suffix(s);
                match suffix {
                    Some("f32") => Type::Primitive(PrimitiveType::Float32),
                    Some("f64") => Type::Primitive(PrimitiveType::Float64),
                    _ => Type::Primitive(PrimitiveType::Float),
                }
            }
            Literal::Bool(_) => Type::Primitive(PrimitiveType::Bool),
            Literal::Char(_) => Type::Primitive(PrimitiveType::Char),
            Literal::String(_) => Type::Primitive(PrimitiveType::String),
            Literal::Unit => Type::Primitive(PrimitiveType::Void),
        }
    }

    // ── Pattern binding ──────────────────────────────────────────────────────

    /// Bind variables introduced by `pattern` to the appropriate component
    /// types of `ty` in the current scope.
    fn bind_pattern_type(&mut self, pattern: &mut AIRNode, ty: &Type) {
        match &pattern.kind {
            NodeKind::WildcardPat | NodeKind::RestPat => {
                self.record(pattern, ty.clone());
            }
            NodeKind::BindPat { name, .. } => {
                let name = name.name.clone();
                self.env.define(name, ty.clone());
                self.record(pattern, ty.clone());
            }
            NodeKind::LiteralPat { lit } => {
                let lit_ty = self.infer_literal(lit);
                self.unify_or_error(&lit_ty, ty, pattern.span, "literal pattern");
                self.record(pattern, lit_ty);
            }
            NodeKind::TuplePat { .. } => {
                if let NodeKind::TuplePat { elems } = &mut pattern.kind {
                    if let Type::Tuple(elem_tys) = ty {
                        for (e, et) in elems.iter_mut().zip(elem_tys.iter()) {
                            let et = et.clone();
                            self.bind_pattern_type(e, &et);
                        }
                    } else {
                        for e in elems.iter_mut() {
                            let fv = self.fresh_var();
                            self.bind_pattern_type(e, &fv);
                        }
                    }
                }
                self.record(pattern, ty.clone());
            }
            NodeKind::ConstructorPat { .. } => {
                // Extract constructor name before mutable borrow.
                let ctor_name = if let NodeKind::ConstructorPat { path, .. } = &pattern.kind {
                    type_path_to_name(path)
                } else {
                    String::new()
                };
                let resolved_ty = self.subst.apply(ty);
                if let NodeKind::ConstructorPat { fields, .. } = &mut pattern.kind {
                    match (ctor_name.as_str(), &resolved_ty) {
                        // Some(x) on Optional[T] — bind x to T
                        ("Some", Type::Optional(inner)) if fields.len() == 1 => {
                            let inner_ty = self.subst.apply(inner);
                            self.bind_pattern_type(&mut fields[0], &inner_ty);
                        }
                        // Ok(v) on Result[T, E] — bind v to T
                        ("Ok", Type::Result(ok, _)) if fields.len() == 1 => {
                            let ok_ty = self.subst.apply(ok);
                            self.bind_pattern_type(&mut fields[0], &ok_ty);
                        }
                        // Err(e) on Result[T, E] — bind e to E
                        ("Err", Type::Result(_, err)) if fields.len() == 1 => {
                            let err_ty = self.subst.apply(err);
                            self.bind_pattern_type(&mut fields[0], &err_ty);
                        }
                        // Fallback: fresh vars for unknown constructors.
                        _ => {
                            for f in fields.iter_mut() {
                                let fv = self.fresh_var();
                                self.bind_pattern_type(f, &fv);
                            }
                        }
                    }
                }
                self.record(pattern, ty.clone());
            }
            NodeKind::OrPat { .. } => {
                if let NodeKind::OrPat { alternatives } = &mut pattern.kind {
                    for alt in alternatives.iter_mut() {
                        let t = ty.clone();
                        self.bind_pattern_type(alt, &t);
                    }
                }
                self.record(pattern, ty.clone());
            }
            NodeKind::ListPat { .. } => {
                let elem_ty = match ty {
                    Type::Generic(g) if g.constructor == "List" && g.args.len() == 1 => {
                        g.args[0].clone()
                    }
                    _ => self.fresh_var(),
                };
                if let NodeKind::ListPat { elems, rest } = &mut pattern.kind {
                    for e in elems.iter_mut() {
                        let et = elem_ty.clone();
                        self.bind_pattern_type(e, &et);
                    }
                    if let Some(r) = rest {
                        let list_ty = Type::Generic(GenericType {
                            constructor: "List".into(),
                            args: vec![elem_ty],
                        });
                        self.bind_pattern_type(r, &list_ty);
                    }
                }
                self.record(pattern, ty.clone());
            }
            NodeKind::RecordPat { .. } => {
                if let NodeKind::RecordPat { fields, .. } = &mut pattern.kind {
                    for f in fields.iter_mut() {
                        let fv = self.fresh_var();
                        if let Some(sub_pat) = &mut f.pattern {
                            // Rename form: `{ x: px }` — bind the sub-pattern.
                            self.bind_pattern_type(sub_pat, &fv);
                        } else {
                            // Shorthand: `{ field }` — bind field name as a variable.
                            self.env.define(f.name.name.clone(), fv);
                        }
                    }
                }
                self.record(pattern, ty.clone());
            }
            _ => {
                self.record(pattern, ty.clone());
            }
        }
    }

    // ── Type-expression node conversion ──────────────────────────────────────

    /// Convert an AIR type-expression node into a [`Type`], substituting generic
    /// parameter names from `gp_map`.
    fn air_type_node_to_type(&mut self, node: &AIRNode, gp_map: &HashMap<String, Type>) -> Type {
        match &node.kind {
            NodeKind::TypeNamed { path, args } => {
                let name = type_path_to_name(path);
                // Check if it's a known generic param
                if let Some(ty) = gp_map.get(&name) {
                    return ty.clone();
                }
                // Check for built-in primitives
                if let Some(prim) = name_to_primitive(&name) {
                    return Type::Primitive(prim);
                }
                // Generic application or named type
                if args.is_empty() {
                    // Resolve type aliases to their underlying type
                    if let Some(underlying) = self.type_aliases.get(&name) {
                        return underlying.clone();
                    }
                    Type::Named(crate::NamedType { name })
                } else {
                    let converted_args: Vec<Type> = args
                        .iter()
                        .map(|a| self.air_type_node_to_type(a, gp_map))
                        .collect();
                    // Special-case Result[T, E] and Optional[T] so that
                    // annotations produce the same Type variant as
                    // Ok(v)/Some(v) constructors.
                    match (name.as_str(), converted_args.len()) {
                        ("Result", 2) => Type::Result(
                            Box::new(converted_args[0].clone()),
                            Box::new(converted_args[1].clone()),
                        ),
                        ("Optional", 1) => Type::Optional(Box::new(converted_args[0].clone())),
                        _ => Type::Generic(GenericType {
                            constructor: name,
                            args: converted_args,
                        }),
                    }
                }
            }
            NodeKind::TypeTuple { elems } => {
                let elem_tys: Vec<Type> = elems
                    .iter()
                    .map(|e| self.air_type_node_to_type(e, gp_map))
                    .collect();
                Type::Tuple(elem_tys)
            }
            NodeKind::TypeFunction { params, ret, .. } => {
                let param_tys: Vec<Type> = params
                    .iter()
                    .map(|p| self.air_type_node_to_type(p, gp_map))
                    .collect();
                let ret_ty = self.air_type_node_to_type(ret, gp_map);
                Type::Function(FnType {
                    params: param_tys,
                    ret: Box::new(ret_ty),
                    effects: vec![],
                })
            }
            NodeKind::TypeOptional { inner } => {
                Type::Optional(Box::new(self.air_type_node_to_type(inner, gp_map)))
            }
            NodeKind::TypeSelf => Type::Named(crate::NamedType {
                name: "Self".into(),
            }),
            NodeKind::Param { ty, .. } => {
                if let Some(ty_node) = ty {
                    self.air_type_node_to_type(ty_node, gp_map)
                } else {
                    self.fresh_var()
                }
            }
            _ => self.fresh_var(),
        }
    }

    /// Convert an AST [`TypeExpr`] directly to a [`Type`].
    ///
    /// Used for record field type declarations where the type is stored as
    /// an AST `TypeExpr` rather than a lowered AIR node.
    fn type_expr_to_type(
        &self,
        ty: &TypeExpr,
        gp_map: &HashMap<String, Type>,
    ) -> Type {
        match ty {
            TypeExpr::Named { path, args, .. } => {
                let name = type_path_to_name(path);
                if let Some(t) = gp_map.get(&name) {
                    return t.clone();
                }
                if let Some(prim) = name_to_primitive(&name) {
                    return Type::Primitive(prim);
                }
                if args.is_empty() {
                    // Resolve type aliases to their underlying type
                    if let Some(underlying) = self.type_aliases.get(&name) {
                        return underlying.clone();
                    }
                    Type::Named(crate::NamedType { name })
                } else {
                    let converted_args: Vec<Type> = args
                        .iter()
                        .map(|a| self.type_expr_to_type(a, gp_map))
                        .collect();
                    match (name.as_str(), converted_args.len()) {
                        ("Result", 2) => Type::Result(
                            Box::new(converted_args[0].clone()),
                            Box::new(converted_args[1].clone()),
                        ),
                        ("Optional", 1) => {
                            Type::Optional(Box::new(converted_args[0].clone()))
                        }
                        _ => Type::Generic(GenericType {
                            constructor: name,
                            args: converted_args,
                        }),
                    }
                }
            }
            TypeExpr::Tuple { elems, .. } => {
                Type::Tuple(elems.iter().map(|e| self.type_expr_to_type(e, gp_map)).collect())
            }
            TypeExpr::Function {
                params, ret, ..
            } => {
                let param_tys: Vec<Type> = params
                    .iter()
                    .map(|p| self.type_expr_to_type(p, gp_map))
                    .collect();
                let ret_ty = self.type_expr_to_type(ret, gp_map);
                Type::Function(FnType {
                    params: param_tys,
                    ret: Box::new(ret_ty),
                    effects: vec![],
                })
            }
            TypeExpr::Optional { inner, .. } => {
                Type::Optional(Box::new(self.type_expr_to_type(inner, gp_map)))
            }
            TypeExpr::SelfType { .. } => Type::Named(crate::NamedType {
                name: "Self".into(),
            }),
        }
    }

    // ── Public API ───────────────────────────────────────────────────────────

    /// **Synthesis** (public): query the side-table for the type of an expression
    /// node, or re-infer it on a temporary clone if not yet visited.
    ///
    /// Callers that have already called `check_module` should use `type_of`
    /// to avoid re-inference overhead.
    pub fn infer_expr(&mut self, expr: &AIRNode) -> Type {
        if let Some(ty) = self.types.get(&expr.id) {
            return ty.clone();
        }
        // Infer on a temporary clone so we don't need `&mut AIRNode`.
        let mut cloned = expr.clone();
        self.infer_node(&mut cloned)
    }

    /// **Checking** (public): verify that `expr` has the given `expected` type.
    ///
    /// Emits a diagnostic if the types do not unify. Like `infer_expr`, this
    /// operates on a clone when the node has not yet been visited.
    pub fn check_expr(&mut self, expr: &AIRNode, expected: &Type) {
        if let Some(ty) = self.types.get(&expr.id) {
            let ty = ty.clone();
            self.unify_or_error(&ty, expected, expr.span, "expression");
            return;
        }
        let mut cloned = expr.clone();
        self.check_node(&mut cloned, expected);
    }
}

impl Default for TypeChecker {
    fn default() -> Self {
        Self::new()
    }
}

// ─── NodeKind helpers ─────────────────────────────────────────────────────────

/// Extension methods on [`NodeKind`] used internally by the type checker.
trait NodeKindExt {
    /// If this is a `Param` node, return its type annotation sub-node.
    fn param_ty_node(&self) -> &AIRNode;
    /// If this is a `Param` node, extract the bound variable name (if any).
    fn param_pat_name(&self) -> Option<String>;
}

impl NodeKindExt for NodeKind {
    fn param_ty_node(&self) -> &AIRNode {
        // We can't return a reference to a locally-created node, so we use the
        // pattern node as a best-effort fallback; callers handle `None` ty specially.
        // This method is only called when we have a reference to the param's kind,
        // and the Param node has a `ty: Option<Box<AIRNode>>` field.
        // Since we need to return a reference, the only case we can handle is
        // when ty is Some(_); callers should use `param_ty_type` instead for
        // the type value. This method is here for structural reasons and returns
        // the pattern node as a fallback (type will be fresh var in that case).
        match self {
            NodeKind::Param { ty, pattern, .. } => ty.as_deref().unwrap_or(pattern),
            // SAFETY: callers only invoke this on Param nodes
            _ => unreachable!("param_ty_node called on non-Param node"),
        }
    }

    fn param_pat_name(&self) -> Option<String> {
        match self {
            NodeKind::Param { pattern, .. } => match &pattern.kind {
                NodeKind::BindPat { name, .. } => Some(name.name.clone()),
                NodeKind::WildcardPat => None,
                _ => None,
            },
            _ => None,
        }
    }
}

// ─── Generic parameter substitution ──────────────────────────────────────────

/// Collect unique [`TypeVarId`]s from a function type in order of first
/// appearance. Used by [`TypeChecker::seed_imported_generic_fn`] to discover
/// which type variables represent generic parameters.
fn collect_type_var_ids_fn(fn_ty: &FnType, out: &mut Vec<TypeVarId>) {
    for param in &fn_ty.params {
        collect_type_var_ids(param, out);
    }
    collect_type_var_ids(&fn_ty.ret, out);
}

/// Recursively collect unique [`TypeVarId`]s from a type.
fn collect_type_var_ids(ty: &Type, out: &mut Vec<TypeVarId>) {
    match ty {
        Type::TypeVar(id) => {
            if !out.contains(id) {
                out.push(*id);
            }
        }
        Type::Function(f) => {
            for p in &f.params {
                collect_type_var_ids(p, out);
            }
            collect_type_var_ids(&f.ret, out);
        }
        Type::Generic(g) => {
            for a in &g.args {
                collect_type_var_ids(a, out);
            }
        }
        Type::Tuple(elems) => {
            for e in elems {
                collect_type_var_ids(e, out);
            }
        }
        Type::Optional(inner) => collect_type_var_ids(inner, out),
        Type::Result(ok, err) => {
            collect_type_var_ids(ok, out);
            collect_type_var_ids(err, out);
        }
        _ => {}
    }
}

/// Replace `Named("A")`, `Named("B")`, etc. in `ty` with the corresponding
/// type from `args`, based on the positional mapping in `param_names`.
///
/// This is used when a record declared as `record Foo[A, B] { ... }` stores
/// field types containing `Named("A")` / `Named("B")`. At construction sites
/// and field accesses, these placeholders are replaced with the actual type
/// arguments inferred or provided.
fn substitute_type_params(ty: &Type, param_names: &[String], args: &[Type]) -> Type {
    match ty {
        Type::Named(nt) => {
            if let Some(idx) = param_names.iter().position(|n| n == &nt.name) {
                if idx < args.len() {
                    return args[idx].clone();
                }
            }
            ty.clone()
        }
        Type::Generic(g) => Type::Generic(GenericType {
            constructor: g.constructor.clone(),
            args: g
                .args
                .iter()
                .map(|a| substitute_type_params(a, param_names, args))
                .collect(),
        }),
        Type::Optional(inner) => {
            Type::Optional(Box::new(substitute_type_params(inner, param_names, args)))
        }
        Type::Result(ok, err) => Type::Result(
            Box::new(substitute_type_params(ok, param_names, args)),
            Box::new(substitute_type_params(err, param_names, args)),
        ),
        Type::Tuple(elems) => Type::Tuple(
            elems
                .iter()
                .map(|e| substitute_type_params(e, param_names, args))
                .collect(),
        ),
        Type::Function(f) => Type::Function(FnType {
            params: f
                .params
                .iter()
                .map(|p| substitute_type_params(p, param_names, args))
                .collect(),
            ret: Box::new(substitute_type_params(&f.ret, param_names, args)),
            effects: f.effects.clone(),
        }),
        _ => ty.clone(),
    }
}

// ─── Type name helpers ────────────────────────────────────────────────────────

/// Convert a `TypePath` to a dot-joined string.
fn type_path_to_name(path: &TypePath) -> String {
    path.segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(".")
}

/// Map a built-in type name to its [`PrimitiveType`] variant, if any.
fn name_to_primitive(name: &str) -> Option<PrimitiveType> {
    match name {
        "Int" => Some(PrimitiveType::Int),
        "Float" => Some(PrimitiveType::Float),
        "Bool" => Some(PrimitiveType::Bool),
        "String" => Some(PrimitiveType::String),
        "Char" => Some(PrimitiveType::Char),
        "Void" => Some(PrimitiveType::Void),
        "Never" => Some(PrimitiveType::Never),
        "Byte" => Some(PrimitiveType::Byte),
        "Bytes" => Some(PrimitiveType::Bytes),
        "Int8" => Some(PrimitiveType::Int8),
        "Int16" => Some(PrimitiveType::Int16),
        "Int32" => Some(PrimitiveType::Int32),
        "Int64" => Some(PrimitiveType::Int64),
        "Int128" => Some(PrimitiveType::Int128),
        "UInt8" => Some(PrimitiveType::UInt8),
        "UInt16" => Some(PrimitiveType::UInt16),
        "UInt32" => Some(PrimitiveType::UInt32),
        "UInt64" => Some(PrimitiveType::UInt64),
        "Float32" => Some(PrimitiveType::Float32),
        "Float64" => Some(PrimitiveType::Float64),
        "BigInt" => Some(PrimitiveType::BigInt),
        "BigFloat" => Some(PrimitiveType::BigFloat),
        "Decimal" => Some(PrimitiveType::Decimal),
        _ => None,
    }
}

/// Suggest a conversion method for common numeric/string primitive mismatches.
///
/// The caller may pass the two types in either order; this helper is
/// symmetric and produces a single note describing the conversions available
/// between them. Returns `None` for type pairs without a trivial conversion.
fn conversion_hint(lhs: &Type, rhs: &Type) -> Option<String> {
    let l = as_primitive(lhs)?;
    let r = as_primitive(rhs)?;
    use PrimitiveType as P;
    let is_int = |p: &P| matches!(p, P::Int | P::Int8 | P::Int16 | P::Int32 | P::Int64 | P::Int128 | P::UInt8 | P::UInt16 | P::UInt32 | P::UInt64 | P::BigInt);
    let is_float = |p: &P| matches!(p, P::Float | P::Float32 | P::Float64 | P::BigFloat);
    if (is_int(&l) && is_float(&r)) || (is_float(&l) && is_int(&r)) {
        return Some(
            "mixed Int/Float — call `.to_float()` on the Int, or `.to_int()` on the Float (truncates), to make the types match".into(),
        );
    }
    if matches!(l, P::String) || matches!(r, P::String) {
        return Some("use `.to_string()` to convert the non-`String` operand".into());
    }
    None
}

/// Extract the underlying `PrimitiveType` if `ty` is `Type::Primitive(_)`.
fn as_primitive(ty: &Type) -> Option<PrimitiveType> {
    match ty {
        Type::Primitive(p) => Some(p.clone()),
        _ => None,
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bock_air::{AIRNode, NodeIdGen, NodeKind};
    use bock_ast::{BinOp, Ident, Literal, TypePath};
    use bock_errors::{FileId, Span};

    fn span() -> Span {
        Span {
            file: FileId(0),
            start: 0,
            end: 0,
        }
    }

    fn ident(name: &str) -> Ident {
        Ident {
            name: name.into(),
            span: span(),
        }
    }

    fn make_node(gen: &NodeIdGen, kind: NodeKind) -> AIRNode {
        AIRNode::new(gen.next(), span(), kind)
    }

    fn int_lit(gen: &NodeIdGen) -> AIRNode {
        make_node(
            gen,
            NodeKind::Literal {
                lit: Literal::Int("42".into()),
            },
        )
    }

    fn bool_lit(gen: &NodeIdGen, v: bool) -> AIRNode {
        make_node(
            gen,
            NodeKind::Literal {
                lit: Literal::Bool(v),
            },
        )
    }

    fn str_lit(gen: &NodeIdGen) -> AIRNode {
        make_node(
            gen,
            NodeKind::Literal {
                lit: Literal::String("hello".into()),
            },
        )
    }

    fn float_lit(gen: &NodeIdGen) -> AIRNode {
        make_node(
            gen,
            NodeKind::Literal {
                lit: Literal::Float("3.14".into()),
            },
        )
    }

    fn type_named_node(gen: &NodeIdGen, name: &str) -> AIRNode {
        make_node(
            gen,
            NodeKind::TypeNamed {
                path: TypePath {
                    segments: vec![ident(name)],
                    span: span(),
                },
                args: vec![],
            },
        )
    }

    // ── Literal inference ──────────────────────────────────────────────────

    #[test]
    fn infer_int_literal() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let node = int_lit(&gen);
        let ty = checker.infer_expr(&node);
        assert_eq!(ty, Type::Primitive(PrimitiveType::Int));
    }

    #[test]
    fn infer_float_literal() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let node = float_lit(&gen);
        let ty = checker.infer_expr(&node);
        assert_eq!(ty, Type::Primitive(PrimitiveType::Float));
    }

    #[test]
    fn infer_bool_literal() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let node = bool_lit(&gen, true);
        let ty = checker.infer_expr(&node);
        assert_eq!(ty, Type::Primitive(PrimitiveType::Bool));
    }

    #[test]
    fn infer_string_literal() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let node = str_lit(&gen);
        let ty = checker.infer_expr(&node);
        assert_eq!(ty, Type::Primitive(PrimitiveType::String));
    }

    // ── Variable inference ─────────────────────────────────────────────────

    #[test]
    fn infer_defined_variable() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        checker.env.define("x", Type::Primitive(PrimitiveType::Int));
        let node = make_node(&gen, NodeKind::Identifier { name: ident("x") });
        let ty = checker.infer_expr(&node);
        assert_eq!(ty, Type::Primitive(PrimitiveType::Int));
    }

    #[test]
    fn infer_undefined_variable_emits_error() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let node = make_node(
            &gen,
            NodeKind::Identifier {
                name: ident("unknown"),
            },
        );
        let ty = checker.infer_expr(&node);
        assert_eq!(ty, Type::Error);
        assert!(checker.diags.has_errors());
    }

    // ── Binary op inference ────────────────────────────────────────────────

    #[test]
    fn infer_int_addition() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let left = int_lit(&gen);
        let right = int_lit(&gen);
        let node = make_node(
            &gen,
            NodeKind::BinaryOp {
                op: BinOp::Add,
                left: Box::new(left),
                right: Box::new(right),
            },
        );
        let ty = checker.infer_expr(&node);
        assert_eq!(ty, Type::Primitive(PrimitiveType::Int));
    }

    #[test]
    fn infer_comparison_returns_bool() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let left = int_lit(&gen);
        let right = int_lit(&gen);
        let node = make_node(
            &gen,
            NodeKind::BinaryOp {
                op: BinOp::Lt,
                left: Box::new(left),
                right: Box::new(right),
            },
        );
        let ty = checker.infer_expr(&node);
        assert_eq!(ty, Type::Primitive(PrimitiveType::Bool));
    }

    #[test]
    fn infer_logical_and_requires_bool() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let left = bool_lit(&gen, true);
        let right = bool_lit(&gen, false);
        let node = make_node(
            &gen,
            NodeKind::BinaryOp {
                op: BinOp::And,
                left: Box::new(left),
                right: Box::new(right),
            },
        );
        let ty = checker.infer_expr(&node);
        assert_eq!(ty, Type::Primitive(PrimitiveType::Bool));
    }

    #[test]
    fn type_mismatch_in_binop_emits_error() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let left = int_lit(&gen);
        let right = bool_lit(&gen, true);
        let node = make_node(
            &gen,
            NodeKind::BinaryOp {
                op: BinOp::Add,
                left: Box::new(left),
                right: Box::new(right),
            },
        );
        checker.infer_expr(&node);
        assert!(checker.diags.has_errors());
    }

    // ── Unary op inference ─────────────────────────────────────────────────

    #[test]
    fn infer_neg_int() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let operand = int_lit(&gen);
        let node = make_node(
            &gen,
            NodeKind::UnaryOp {
                op: UnaryOp::Neg,
                operand: Box::new(operand),
            },
        );
        let ty = checker.infer_expr(&node);
        assert_eq!(ty, Type::Primitive(PrimitiveType::Int));
    }

    #[test]
    fn infer_not_bool() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let operand = bool_lit(&gen, true);
        let node = make_node(
            &gen,
            NodeKind::UnaryOp {
                op: UnaryOp::Not,
                operand: Box::new(operand),
            },
        );
        let ty = checker.infer_expr(&node);
        assert_eq!(ty, Type::Primitive(PrimitiveType::Bool));
    }

    // ── List literal (check mode) ──────────────────────────────────────────

    #[test]
    fn check_list_literal_against_list_int() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let expected = Type::Generic(GenericType {
            constructor: "List".into(),
            args: vec![Type::Primitive(PrimitiveType::Int)],
        });
        let node = make_node(
            &gen,
            NodeKind::ListLiteral {
                elems: vec![int_lit(&gen), int_lit(&gen)],
            },
        );
        checker.check_expr(&node, &expected);
        assert!(!checker.diags.has_errors());
    }

    #[test]
    fn list_element_mismatch_emits_error() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let expected = Type::Generic(GenericType {
            constructor: "List".into(),
            args: vec![Type::Primitive(PrimitiveType::Int)],
        });
        let node = make_node(
            &gen,
            NodeKind::ListLiteral {
                elems: vec![int_lit(&gen), bool_lit(&gen, true)],
            },
        );
        checker.check_expr(&node, &expected);
        assert!(checker.diags.has_errors());
    }

    // ── Infer mode for list ────────────────────────────────────────────────

    #[test]
    fn infer_list_literal() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let node = make_node(
            &gen,
            NodeKind::ListLiteral {
                elems: vec![int_lit(&gen), int_lit(&gen)],
            },
        );
        let ty = checker.infer_expr(&node);
        assert!(matches!(&ty, Type::Generic(g) if g.constructor == "List"
                && g.args.len() == 1
                && g.args[0] == Type::Primitive(PrimitiveType::Int)));
    }

    // ── Tuple literal ──────────────────────────────────────────────────────

    #[test]
    fn infer_tuple_literal() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let node = make_node(
            &gen,
            NodeKind::TupleLiteral {
                elems: vec![int_lit(&gen), bool_lit(&gen, false)],
            },
        );
        let ty = checker.infer_expr(&node);
        assert_eq!(
            ty,
            Type::Tuple(vec![
                Type::Primitive(PrimitiveType::Int),
                Type::Primitive(PrimitiveType::Bool),
            ])
        );
    }

    // ── Block inference ────────────────────────────────────────────────────

    #[test]
    fn infer_block_tail_expression() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let tail = int_lit(&gen);
        let node = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![],
                tail: Some(Box::new(tail)),
            },
        );
        let ty = checker.infer_expr(&node);
        assert_eq!(ty, Type::Primitive(PrimitiveType::Int));
    }

    #[test]
    fn infer_block_no_tail_is_void() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let node = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![],
                tail: None,
            },
        );
        let ty = checker.infer_expr(&node);
        assert_eq!(ty, Type::Primitive(PrimitiveType::Void));
    }

    // ── Let binding ────────────────────────────────────────────────────────

    #[test]
    fn let_binding_infers_and_binds() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let pat = make_node(
            &gen,
            NodeKind::BindPat {
                name: ident("x"),
                is_mut: false,
            },
        );
        let val = int_lit(&gen);
        let let_node = make_node(
            &gen,
            NodeKind::LetBinding {
                is_mut: false,
                pattern: Box::new(pat),
                ty: None,
                value: Box::new(val),
            },
        );
        // Wrap in a block with x used after
        let ident_x = make_node(&gen, NodeKind::Identifier { name: ident("x") });
        let block = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![let_node],
                tail: Some(Box::new(ident_x)),
            },
        );
        let ty = checker.infer_expr(&block);
        assert_eq!(ty, Type::Primitive(PrimitiveType::Int));
        assert!(!checker.diags.has_errors());
    }

    // ── Generic instantiation ──────────────────────────────────────────────

    #[test]
    fn fresh_var_for_generic_params() {
        let mut checker = TypeChecker::new();
        // Simulate: first[T](list: List[T]) -> Optional[T]
        // Build the sig manually
        let t_var = checker.fresh_var(); // T placeholder
        let t_id = match &t_var {
            Type::TypeVar(id) => *id,
            _ => unreachable!(),
        };
        let sig = FnSig {
            generic_params: vec!["T".into()],
            generic_var_ids: vec![t_id],
            param_types: vec![Type::Generic(GenericType {
                constructor: "List".into(),
                args: vec![t_var.clone()],
            })],
            return_type: Type::Optional(Box::new(t_var)),
            where_clause: vec![],
        };

        let gen = NodeIdGen::new();
        let arg = make_node(
            &gen,
            NodeKind::ListLiteral {
                elems: vec![int_lit(&gen)],
            },
        );
        let args: Vec<bock_air::AirArg> = vec![bock_air::AirArg {
            label: None,
            value: arg,
        }];

        let ret = checker.instantiate_and_check("first", &sig, &args, span());
        // Return type should be Optional[?fresh_var]; the fresh var is
        // distinct from the original t_var.
        assert!(!checker.diags.has_errors());
        assert!(matches!(ret, Type::Optional(_)));
    }

    /// Helper: register a generic function in both `env` and `fn_sigs`.
    fn register_generic_fn(
        checker: &mut TypeChecker,
        name: &str,
        generic_names: &[&str],
        build_sig: impl FnOnce(&[Type]) -> (Vec<Type>, Type),
    ) {
        let vars: Vec<Type> = generic_names.iter().map(|_| checker.fresh_var()).collect();
        let var_ids: Vec<TypeVarId> = vars
            .iter()
            .map(|t| match t {
                Type::TypeVar(id) => *id,
                _ => unreachable!(),
            })
            .collect();
        let (param_types, return_type) = build_sig(&vars);
        let fn_ty = Type::Function(FnType {
            params: param_types.clone(),
            ret: Box::new(return_type.clone()),
            effects: vec![],
        });
        checker.env.define(name, fn_ty);
        checker.fn_sigs.insert(
            name.into(),
            FnSig {
                generic_params: generic_names.iter().map(|s| (*s).into()).collect(),
                generic_var_ids: var_ids,
                param_types,
                return_type,
                where_clause: vec![],
            },
        );
    }

    #[test]
    fn generic_first_infers_int() {
        // fn first[T](list: List[T]) -> T; first([1,2,3]) → Int
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        register_generic_fn(&mut checker, "first", &["T"], |vars| {
            let t = vars[0].clone();
            let params = vec![Type::Generic(GenericType {
                constructor: "List".into(),
                args: vec![t.clone()],
            })];
            (params, t)
        });

        let callee = make_node(
            &gen,
            NodeKind::Identifier {
                name: ident("first"),
            },
        );
        let list_arg = make_node(
            &gen,
            NodeKind::ListLiteral {
                elems: vec![int_lit(&gen), int_lit(&gen), int_lit(&gen)],
            },
        );
        let call = make_node(
            &gen,
            NodeKind::Call {
                callee: Box::new(callee),
                type_args: vec![],
                args: vec![bock_air::AirArg {
                    label: None,
                    value: list_arg,
                }],
            },
        );

        let ty = checker.infer_expr(&call);
        assert_eq!(ty, Type::Primitive(PrimitiveType::Int));
        assert!(!checker.diags.has_errors());
    }

    #[test]
    fn generic_identity_infers_string() {
        // fn identity[T](x: T) -> T; identity("hello") → String
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        register_generic_fn(&mut checker, "identity", &["T"], |vars| {
            let t = vars[0].clone();
            (vec![t.clone()], t)
        });

        let callee = make_node(
            &gen,
            NodeKind::Identifier {
                name: ident("identity"),
            },
        );
        let call = make_node(
            &gen,
            NodeKind::Call {
                callee: Box::new(callee),
                type_args: vec![],
                args: vec![bock_air::AirArg {
                    label: None,
                    value: str_lit(&gen),
                }],
            },
        );

        let ty = checker.infer_expr(&call);
        assert_eq!(ty, Type::Primitive(PrimitiveType::String));
        assert!(!checker.diags.has_errors());
    }

    #[test]
    fn generic_two_params_swap() {
        // fn swap[A, B](a: A, b: B) -> (B, A); swap(1, "hi") → (String, Int)
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        register_generic_fn(&mut checker, "swap", &["A", "B"], |vars| {
            let a = vars[0].clone();
            let b = vars[1].clone();
            let params = vec![a.clone(), b.clone()];
            let ret = Type::Tuple(vec![b, a]);
            (params, ret)
        });

        let callee = make_node(
            &gen,
            NodeKind::Identifier {
                name: ident("swap"),
            },
        );
        let call = make_node(
            &gen,
            NodeKind::Call {
                callee: Box::new(callee),
                type_args: vec![],
                args: vec![
                    bock_air::AirArg {
                        label: None,
                        value: int_lit(&gen),
                    },
                    bock_air::AirArg {
                        label: None,
                        value: str_lit(&gen),
                    },
                ],
            },
        );

        let ty = checker.infer_expr(&call);
        assert_eq!(
            ty,
            Type::Tuple(vec![
                Type::Primitive(PrimitiveType::String),
                Type::Primitive(PrimitiveType::Int),
            ])
        );
        assert!(!checker.diags.has_errors());
    }

    #[test]
    fn method_call_on_known_type_returns_correct_type() {
        // [1, 2, 3].len() → Int
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let list = make_node(
            &gen,
            NodeKind::ListLiteral {
                elems: vec![int_lit(&gen), int_lit(&gen), int_lit(&gen)],
            },
        );
        let method_call = make_node(
            &gen,
            NodeKind::MethodCall {
                receiver: Box::new(list),
                method: ident("len"),
                type_args: vec![],
                args: vec![],
            },
        );
        let ty = checker.infer_expr(&method_call);
        assert_eq!(ty, Type::Primitive(PrimitiveType::Int));
        assert!(!checker.diags.has_errors());
    }

    #[test]
    fn method_call_string_contains_returns_bool() {
        // "hello".contains("lo") → Bool
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let receiver = str_lit(&gen);
        let method_call = make_node(
            &gen,
            NodeKind::MethodCall {
                receiver: Box::new(receiver),
                method: ident("contains"),
                type_args: vec![],
                args: vec![bock_air::AirArg {
                    label: None,
                    value: str_lit(&gen),
                }],
            },
        );
        let ty = checker.infer_expr(&method_call);
        assert_eq!(ty, Type::Primitive(PrimitiveType::Bool));
        assert!(!checker.diags.has_errors());
    }

    // ── Interpolation ──────────────────────────────────────────────────────

    #[test]
    fn infer_interpolation_is_string() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let node = make_node(
            &gen,
            NodeKind::Interpolation {
                parts: vec![
                    bock_air::AirInterpolationPart::Literal("hello ".into()),
                    bock_air::AirInterpolationPart::Expr(Box::new(int_lit(&gen))),
                ],
            },
        );
        let ty = checker.infer_expr(&node);
        assert_eq!(ty, Type::Primitive(PrimitiveType::String));
    }

    // ── Unreachable / Never ────────────────────────────────────────────────

    #[test]
    fn infer_unreachable_is_never() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let node = make_node(&gen, NodeKind::Unreachable);
        let ty = checker.infer_expr(&node);
        assert_eq!(ty, Type::Primitive(PrimitiveType::Never));
    }

    // ── check_module with a simple function ──────────────────────────────

    #[test]
    fn check_module_simple_fn() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();

        // fn add(x: Int, y: Int) -> Int { x + y }
        let x_pat = make_node(
            &gen,
            NodeKind::BindPat {
                name: ident("x"),
                is_mut: false,
            },
        );
        let y_pat = make_node(
            &gen,
            NodeKind::BindPat {
                name: ident("y"),
                is_mut: false,
            },
        );

        let int_ty = type_named_node(&gen, "Int");

        let x_param = make_node(
            &gen,
            NodeKind::Param {
                pattern: Box::new(x_pat),
                ty: Some(Box::new(int_ty.clone())),
                default: None,
            },
        );
        let y_param = make_node(
            &gen,
            NodeKind::Param {
                pattern: Box::new(y_pat),
                ty: Some(Box::new(int_ty.clone())),
                default: None,
            },
        );

        let x_ref = make_node(&gen, NodeKind::Identifier { name: ident("x") });
        let y_ref = make_node(&gen, NodeKind::Identifier { name: ident("y") });
        let add_expr = make_node(
            &gen,
            NodeKind::BinaryOp {
                op: BinOp::Add,
                left: Box::new(x_ref),
                right: Box::new(y_ref),
            },
        );

        let body = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![],
                tail: Some(Box::new(add_expr)),
            },
        );

        let ret_ty = type_named_node(&gen, "Int");

        let fn_node = make_node(
            &gen,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: bock_ast::Visibility::Public,
                is_async: false,
                name: ident("add"),
                generic_params: vec![],
                params: vec![x_param, y_param],
                return_type: Some(Box::new(ret_ty)),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );

        let mut module = make_node(
            &gen,
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![fn_node],
            },
        );

        checker.check_module(&mut module);
        assert!(
            !checker.diags.has_errors(),
            "errors: {:?}",
            checker.diags.iter().collect::<Vec<_>>()
        );
    }

    // ── check_mode: lambda from context ──────────────────────────────────

    #[test]
    fn check_lambda_from_context() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();

        // let f: Fn(Int) -> Int = (x) => x + 1
        let x_pat = make_node(
            &gen,
            NodeKind::BindPat {
                name: ident("x"),
                is_mut: false,
            },
        );
        let x_param = make_node(
            &gen,
            NodeKind::Param {
                pattern: Box::new(x_pat),
                ty: None,
                default: None,
            },
        );
        let x_ref = make_node(&gen, NodeKind::Identifier { name: ident("x") });
        let one = make_node(
            &gen,
            NodeKind::Literal {
                lit: Literal::Int("1".into()),
            },
        );
        let body = make_node(
            &gen,
            NodeKind::BinaryOp {
                op: BinOp::Add,
                left: Box::new(x_ref),
                right: Box::new(one),
            },
        );

        let lambda = make_node(
            &gen,
            NodeKind::Lambda {
                params: vec![x_param],
                body: Box::new(body),
            },
        );

        let expected = Type::Function(FnType {
            params: vec![Type::Primitive(PrimitiveType::Int)],
            ret: Box::new(Type::Primitive(PrimitiveType::Int)),
            effects: vec![],
        });

        checker.check_expr(&lambda, &expected);
        assert!(!checker.diags.has_errors());
    }

    // ── Error propagation: Type::Error unifies with anything ──────────────

    #[test]
    fn error_type_prevents_cascade() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();

        // undefined + 1  → Error + Int = Error (no cascade error from second op)
        let undef = make_node(
            &gen,
            NodeKind::Identifier {
                name: ident("undefined_var"),
            },
        );
        let one = int_lit(&gen);
        let add = make_node(
            &gen,
            NodeKind::BinaryOp {
                op: BinOp::Add,
                left: Box::new(undef),
                right: Box::new(one),
            },
        );
        let ty = checker.infer_expr(&add);
        // Should have exactly 1 error (the undefined var), not 2.
        assert_eq!(checker.diags.error_count(), 1);
        assert_eq!(ty, Type::Error);
    }

    // ── where_clause verification ─────────────────────────────────────────

    #[test]
    fn where_clause_unknown_param_emits_error() {
        let mut checker = TypeChecker::new();
        let clauses = vec![TypeConstraint {
            id: 0,
            span: span(),
            param: ident("X"), // not in generic_params
            bounds: vec![TypePath {
                segments: vec![ident("Equatable")],
                span: span(),
            }],
        }];
        checker.check_where_clause(&clauses, &HashMap::new(), span());
        assert!(checker.diags.has_errors());
    }

    // ── Result / Optional annotation unification (F2.06) ─────────────────

    fn type_named_node_with_args(gen: &NodeIdGen, name: &str, args: Vec<AIRNode>) -> AIRNode {
        make_node(
            gen,
            NodeKind::TypeNamed {
                path: TypePath {
                    segments: vec![ident(name)],
                    span: span(),
                },
                args,
            },
        )
    }

    #[test]
    fn result_annotation_produces_type_result() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let int_node = type_named_node(&gen, "Int");
        let string_node = type_named_node(&gen, "String");
        let result_node = type_named_node_with_args(&gen, "Result", vec![int_node, string_node]);
        let ty = checker.air_type_node_to_type(&result_node, &HashMap::new());
        assert_eq!(
            ty,
            Type::Result(
                Box::new(Type::Primitive(PrimitiveType::Int)),
                Box::new(Type::Primitive(PrimitiveType::String)),
            )
        );
    }

    #[test]
    fn optional_annotation_produces_type_optional() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let int_node = type_named_node(&gen, "Int");
        let optional_node = type_named_node_with_args(&gen, "Optional", vec![int_node]);
        let ty = checker.air_type_node_to_type(&optional_node, &HashMap::new());
        assert_eq!(
            ty,
            Type::Optional(Box::new(Type::Primitive(PrimitiveType::Int)))
        );
    }

    #[test]
    fn result_annotation_unifies_with_ok_construction() {
        // Result[Int, String] from annotation must unify with
        // Type::Result(Int, ?E) from Ok(42)
        let annotated = Type::Result(
            Box::new(Type::Primitive(PrimitiveType::Int)),
            Box::new(Type::Primitive(PrimitiveType::String)),
        );
        let constructed = Type::Result(
            Box::new(Type::Primitive(PrimitiveType::Int)),
            Box::new(Type::TypeVar(99)),
        );
        let mut subst = crate::Substitution::new();
        assert!(crate::unify(&annotated, &constructed, &mut subst).is_ok());
        assert_eq!(subst.lookup(99), Type::Primitive(PrimitiveType::String));
    }

    #[test]
    fn optional_annotation_unifies_with_some_construction() {
        // Optional[Int] from annotation must unify with
        // Type::Optional(Int) from Some(5)
        let annotated = Type::Optional(Box::new(Type::Primitive(PrimitiveType::Int)));
        let constructed = Type::Optional(Box::new(Type::Primitive(PrimitiveType::Int)));
        let mut subst = crate::Substitution::new();
        assert!(crate::unify(&annotated, &constructed, &mut subst).is_ok());
    }

    // ── Trait-bound enforcement at call sites (F2.08) ────────────────────

    /// Helper: register a generic function with where-clause bounds.
    fn register_generic_fn_with_bounds(
        checker: &mut TypeChecker,
        name: &str,
        generic_names: &[&str],
        bounds: Vec<TypeConstraint>,
        build_sig: impl FnOnce(&[Type]) -> (Vec<Type>, Type),
    ) {
        let vars: Vec<Type> = generic_names.iter().map(|_| checker.fresh_var()).collect();
        let var_ids: Vec<TypeVarId> = vars
            .iter()
            .map(|t| match t {
                Type::TypeVar(id) => *id,
                _ => unreachable!(),
            })
            .collect();
        let (param_types, return_type) = build_sig(&vars);
        let fn_ty = Type::Function(FnType {
            params: param_types.clone(),
            ret: Box::new(return_type.clone()),
            effects: vec![],
        });
        checker.env.define(name, fn_ty);
        checker.fn_sigs.insert(
            name.into(),
            FnSig {
                generic_params: generic_names.iter().map(|s| (*s).into()).collect(),
                generic_var_ids: var_ids,
                param_types,
                return_type,
                where_clause: bounds,
            },
        );
    }

    /// Build a `TypeConstraint` for `param: Bound1 + Bound2 + ...`.
    fn make_constraint(param: &str, bound_names: &[&str]) -> TypeConstraint {
        use bock_ast::TypeConstraint;
        TypeConstraint {
            id: 0,
            span: span(),
            param: ident(param),
            bounds: bound_names
                .iter()
                .map(|b| TypePath {
                    segments: vec![ident(b)],
                    span: span(),
                })
                .collect(),
        }
    }

    /// Build an `ImplTable` with specific (trait, type) registrations.
    fn make_impl_table(impls: &[(&str, Type)]) -> ImplTable {
        let mut table = ImplTable::new();
        for (trait_name, ty) in impls {
            table.register_trait_impl(*trait_name, ty);
        }
        table
    }

    #[test]
    fn trait_bound_satisfied_no_error() {
        // fn sort[T](list: List[T]) -> List[T] where (T: Comparable)
        // Calling sort([1, 2, 3]) with Int implementing Comparable — no error.
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();

        // Set up impl table: Int implements Comparable.
        checker.impl_table = Some(make_impl_table(&[(
            "Comparable",
            Type::Primitive(PrimitiveType::Int),
        )]));

        let bounds = vec![make_constraint("T", &["Comparable"])];
        register_generic_fn_with_bounds(&mut checker, "sort", &["T"], bounds, |vars| {
            let t = vars[0].clone();
            let list_t = Type::Generic(GenericType {
                constructor: "List".into(),
                args: vec![t.clone()],
            });
            (vec![list_t.clone()], list_t)
        });

        let callee = make_node(
            &gen,
            NodeKind::Identifier {
                name: ident("sort"),
            },
        );
        let list_arg = make_node(
            &gen,
            NodeKind::ListLiteral {
                elems: vec![int_lit(&gen), int_lit(&gen)],
            },
        );
        let call = make_node(
            &gen,
            NodeKind::Call {
                callee: Box::new(callee),
                type_args: vec![],
                args: vec![bock_air::AirArg {
                    label: None,
                    value: list_arg,
                }],
            },
        );

        checker.infer_expr(&call);
        assert!(
            !checker.diags.has_errors(),
            "expected no errors for Int: Comparable"
        );
    }

    #[test]
    fn trait_bound_violated_emits_diagnostic() {
        // fn sort[T](list: List[T]) -> List[T] where (T: Comparable)
        // Calling sort with a Bool list — Bool does NOT implement Comparable.
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();

        // Impl table: only Int implements Comparable (not Bool).
        checker.impl_table = Some(make_impl_table(&[(
            "Comparable",
            Type::Primitive(PrimitiveType::Int),
        )]));

        let bounds = vec![make_constraint("T", &["Comparable"])];
        register_generic_fn_with_bounds(&mut checker, "sort", &["T"], bounds, |vars| {
            let t = vars[0].clone();
            let list_t = Type::Generic(GenericType {
                constructor: "List".into(),
                args: vec![t.clone()],
            });
            (vec![list_t.clone()], list_t)
        });

        let callee = make_node(
            &gen,
            NodeKind::Identifier {
                name: ident("sort"),
            },
        );
        let list_arg = make_node(
            &gen,
            NodeKind::ListLiteral {
                elems: vec![bool_lit(&gen, true), bool_lit(&gen, false)],
            },
        );
        let call = make_node(
            &gen,
            NodeKind::Call {
                callee: Box::new(callee),
                type_args: vec![],
                args: vec![bock_air::AirArg {
                    label: None,
                    value: list_arg,
                }],
            },
        );

        checker.infer_expr(&call);
        assert!(
            checker.diags.has_errors(),
            "expected error: Bool does not implement Comparable"
        );
        assert_eq!(checker.diags.error_count(), 1);
    }

    #[test]
    fn multiple_trait_bounds_both_satisfied() {
        // fn display_sorted[T](list: List[T]) -> Void
        //   where (T: Comparable, T: Displayable)
        // Call with Int — Int implements both.
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();

        checker.impl_table = Some(make_impl_table(&[
            ("Comparable", Type::Primitive(PrimitiveType::Int)),
            ("Displayable", Type::Primitive(PrimitiveType::Int)),
        ]));

        let bounds = vec![make_constraint("T", &["Comparable", "Displayable"])];
        register_generic_fn_with_bounds(&mut checker, "display_sorted", &["T"], bounds, |vars| {
            let t = vars[0].clone();
            let list_t = Type::Generic(GenericType {
                constructor: "List".into(),
                args: vec![t],
            });
            (vec![list_t], Type::Primitive(PrimitiveType::Void))
        });

        let callee = make_node(
            &gen,
            NodeKind::Identifier {
                name: ident("display_sorted"),
            },
        );
        let list_arg = make_node(
            &gen,
            NodeKind::ListLiteral {
                elems: vec![int_lit(&gen)],
            },
        );
        let call = make_node(
            &gen,
            NodeKind::Call {
                callee: Box::new(callee),
                type_args: vec![],
                args: vec![bock_air::AirArg {
                    label: None,
                    value: list_arg,
                }],
            },
        );

        checker.infer_expr(&call);
        assert!(
            !checker.diags.has_errors(),
            "expected no errors: Int satisfies both bounds"
        );
    }

    #[test]
    fn multiple_trait_bounds_one_missing() {
        // fn display_sorted[T](list: List[T]) -> Void
        //   where (T: Comparable, T: Displayable)
        // Call with Int — Int implements Comparable but NOT Displayable.
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();

        // Only Comparable is registered for Int.
        checker.impl_table = Some(make_impl_table(&[(
            "Comparable",
            Type::Primitive(PrimitiveType::Int),
        )]));

        let bounds = vec![make_constraint("T", &["Comparable", "Displayable"])];
        register_generic_fn_with_bounds(&mut checker, "display_sorted", &["T"], bounds, |vars| {
            let t = vars[0].clone();
            let list_t = Type::Generic(GenericType {
                constructor: "List".into(),
                args: vec![t],
            });
            (vec![list_t], Type::Primitive(PrimitiveType::Void))
        });

        let callee = make_node(
            &gen,
            NodeKind::Identifier {
                name: ident("display_sorted"),
            },
        );
        let list_arg = make_node(
            &gen,
            NodeKind::ListLiteral {
                elems: vec![int_lit(&gen)],
            },
        );
        let call = make_node(
            &gen,
            NodeKind::Call {
                callee: Box::new(callee),
                type_args: vec![],
                args: vec![bock_air::AirArg {
                    label: None,
                    value: list_arg,
                }],
            },
        );

        checker.infer_expr(&call);
        assert!(
            checker.diags.has_errors(),
            "expected error: Int missing Displayable"
        );
        assert_eq!(checker.diags.error_count(), 1);
    }

    #[test]
    fn no_impl_table_skips_bound_checking() {
        // Without an impl_table, trait bounds should not be checked.
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        // impl_table is None by default.

        let bounds = vec![make_constraint("T", &["Comparable"])];
        register_generic_fn_with_bounds(&mut checker, "sort", &["T"], bounds, |vars| {
            let t = vars[0].clone();
            (vec![t.clone()], t)
        });

        let callee = make_node(
            &gen,
            NodeKind::Identifier {
                name: ident("sort"),
            },
        );
        let call = make_node(
            &gen,
            NodeKind::Call {
                callee: Box::new(callee),
                type_args: vec![],
                args: vec![bock_air::AirArg {
                    label: None,
                    value: int_lit(&gen),
                }],
            },
        );

        checker.infer_expr(&call);
        // No impl_table → no bound-check errors.
        assert!(!checker.diags.has_errors());
    }

    // ── M-064: Char literal inference ─────────────────────────────────────

    #[test]
    fn infer_char_literal() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let node = make_node(
            &gen,
            NodeKind::Literal {
                lit: Literal::Char("a".into()),
            },
        );
        let ty = checker.infer_expr(&node);
        assert_eq!(ty, Type::Primitive(PrimitiveType::Char));
    }

    // ── M-063: Function types carry effects ───────────────────────────────

    #[test]
    fn fn_type_carries_effects() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();

        // Build a FnDecl with effect_clause: [Log, Clock]
        let body = make_node(
            &gen,
            NodeKind::Block {
                stmts: vec![],
                tail: None,
            },
        );
        let fn_decl = make_node(
            &gen,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: bock_ast::Visibility::Public,
                is_async: false,
                name: ident("greet"),
                generic_params: vec![],
                params: vec![],
                return_type: None,
                effect_clause: vec![
                    TypePath {
                        segments: vec![ident("Log")],
                        span: span(),
                    },
                    TypePath {
                        segments: vec![ident("Clock")],
                        span: span(),
                    },
                ],
                where_clause: vec![],
                body: Box::new(body),
            },
        );

        let module = make_node(
            &gen,
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![fn_decl],
            },
        );

        let mut module = module;
        checker.check_module(&mut module);

        // Look up the function type and verify effects are present.
        let fn_ty = checker
            .env
            .lookup("greet")
            .expect("greet should be defined");
        match fn_ty {
            Type::Function(f) => {
                assert_eq!(f.effects.len(), 2);
                assert_eq!(f.effects[0].name, "Log");
                assert_eq!(f.effects[1].name, "Clock");
            }
            other => panic!("expected Function type, got {other:?}"),
        }
    }

    // ── M-067: Method calls on known types return correct types ───────────

    #[test]
    fn method_call_float_abs_returns_float() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let receiver = float_lit(&gen);
        let method_call = make_node(
            &gen,
            NodeKind::MethodCall {
                receiver: Box::new(receiver),
                method: ident("abs"),
                type_args: vec![],
                args: vec![],
            },
        );
        let ty = checker.infer_expr(&method_call);
        assert_eq!(ty, Type::Primitive(PrimitiveType::Float));
        assert!(!checker.diags.has_errors());
    }

    #[test]
    fn method_call_float_to_int_returns_int() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let receiver = float_lit(&gen);
        let method_call = make_node(
            &gen,
            NodeKind::MethodCall {
                receiver: Box::new(receiver),
                method: ident("to_int"),
                type_args: vec![],
                args: vec![],
            },
        );
        let ty = checker.infer_expr(&method_call);
        assert_eq!(ty, Type::Primitive(PrimitiveType::Int));
    }

    #[test]
    fn method_call_bool_negate_returns_bool() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let receiver = bool_lit(&gen, true);
        let method_call = make_node(
            &gen,
            NodeKind::MethodCall {
                receiver: Box::new(receiver),
                method: ident("negate"),
                type_args: vec![],
                args: vec![],
            },
        );
        let ty = checker.infer_expr(&method_call);
        assert_eq!(ty, Type::Primitive(PrimitiveType::Bool));
    }

    #[test]
    fn method_call_char_is_alpha_returns_bool() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let receiver = make_node(
            &gen,
            NodeKind::Literal {
                lit: Literal::Char("a".into()),
            },
        );
        let method_call = make_node(
            &gen,
            NodeKind::MethodCall {
                receiver: Box::new(receiver),
                method: ident("is_alpha"),
                type_args: vec![],
                args: vec![],
            },
        );
        let ty = checker.infer_expr(&method_call);
        assert_eq!(ty, Type::Primitive(PrimitiveType::Bool));
    }

    #[test]
    fn method_call_char_to_upper_returns_char() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let receiver = make_node(
            &gen,
            NodeKind::Literal {
                lit: Literal::Char("a".into()),
            },
        );
        let method_call = make_node(
            &gen,
            NodeKind::MethodCall {
                receiver: Box::new(receiver),
                method: ident("to_upper"),
                type_args: vec![],
                args: vec![],
            },
        );
        let ty = checker.infer_expr(&method_call);
        assert_eq!(ty, Type::Primitive(PrimitiveType::Char));
    }

    #[test]
    fn method_call_unknown_method_returns_fresh_var() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let receiver = int_lit(&gen);
        let method_call = make_node(
            &gen,
            NodeKind::MethodCall {
                receiver: Box::new(receiver),
                method: ident("nonexistent"),
                type_args: vec![],
                args: vec![],
            },
        );
        let ty = checker.infer_expr(&method_call);
        // Unknown method → fresh type variable
        assert!(matches!(ty, Type::TypeVar(_)));
    }
}
