//! Trait resolution — `ImplTable` construction, impl/method dispatch,
//! coherence checking, associated-type resolution, and supertrait obligations.
//!
//! # Overview
//!
//! The [`ImplTable`] is the central data structure. It is built from an AIR
//! module node by [`ImplTable::build_from`], which walks all top-level
//! [`NodeKind::ImplBlock`] items and registers them. Two free functions then
//! provide the main resolution queries:
//!
//! - [`resolve_impl`] — find the [`ImplId`] for a `(trait, type)` pair.
//! - [`resolve_method`] — dispatch a `.method()` call on a receiver type.
//!
//! Coherence checking (exact-type overlap detection) runs during construction.
//! Generic-parameter impls are registered but exempted from the coherence check.
//!
//! # Supertrait obligations
//!
//! Call [`check_supertrait_obligations`] after [`resolve_impl`] to verify that
//! all transitively required supertraits are also satisfied.

use std::collections::{HashMap, HashSet, VecDeque};

use bock_air::{AIRNode, NodeKind};
use bock_ast::TypePath;
use bock_errors::{DiagnosticBag, DiagnosticCode};

use crate::{GenericType, NamedType, PrimitiveType, Type};

// ─── Diagnostic codes ─────────────────────────────────────────────────────────

const E_COHERENCE_OVERLAP: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 4010,
};

// ─── Core types ───────────────────────────────────────────────────────────────

/// Unique identifier for a registered impl block.
pub type ImplId = u32;

/// A reference to a named trait, identified by its fully-qualified name.
///
/// Examples: `"Equatable"`, `"Std.Io.Writable"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TraitRef {
    /// Fully-qualified name of the trait.
    pub name: String,
}

impl TraitRef {
    /// Create a `TraitRef` from any string-like value.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    fn from_path(path: &TypePath) -> Self {
        let name = path
            .segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(".");
        Self { name }
    }
}

/// The result of a successful method dispatch via [`resolve_method`].
#[derive(Debug, Clone)]
pub struct ResolvedMethod {
    /// The impl block that provides this method.
    pub impl_id: ImplId,
    /// The trait this method comes from, or `None` for inherent impls.
    pub trait_ref: Option<TraitRef>,
    /// The resolved method name.
    pub method: String,
}

// ─── ImplTable ────────────────────────────────────────────────────────────────

/// A record of a single impl block registered in the [`ImplTable`].
#[derive(Debug, Clone)]
pub struct ImplEntry {
    /// Unique id allocated for this impl block.
    pub id: ImplId,
    /// Trait being implemented, or `None` for an inherent impl.
    pub trait_ref: Option<TraitRef>,
    /// Canonical string key for the target type (see [`type_key`]).
    pub type_key: String,
    /// Names of the methods provided by this impl.
    pub methods: Vec<String>,
    /// `true` when the impl has at least one generic type parameter.
    ///
    /// Generic impls are registered but skipped during coherence checking.
    pub is_generic: bool,
}

/// Maps `(TraitRef, Type)` pairs to impl blocks and supports method dispatch.
///
/// # Construction
///
/// ```ignore
/// let table = ImplTable::build_from(&air_module_node);
/// // inspect table.diags for coherence errors
/// ```
///
/// # Querying
///
/// ```ignore
/// let impl_id = resolve_impl(&TraitRef::new("Equatable"), &ty, &table)?;
/// let method  = resolve_method(&receiver_ty, "equals", &table)?;
/// ```
pub struct ImplTable {
    /// All registered impl entries indexed by [`ImplId`].
    entries: HashMap<ImplId, ImplEntry>,
    /// Trait impl index: `(trait_name, type_key) → ImplId` (concrete impls only).
    trait_impl_index: HashMap<(String, String), ImplId>,
    /// Inherent impl index: `type_key → ImplId`.
    inherent_impl_index: HashMap<String, ImplId>,
    /// Supertrait graph: `trait_name → list of direct supertrait names`.
    supertraits: HashMap<String, Vec<String>>,
    /// Associated type bindings: `(ImplId, assoc_type_name) → Type`.
    assoc_types: HashMap<(ImplId, String), Type>,
    /// Monotonically increasing id counter.
    next_id: u32,
    /// Diagnostics emitted during construction (coherence errors use `E4010`).
    pub diags: DiagnosticBag,
}

impl ImplTable {
    /// Create a new, empty `ImplTable`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            trait_impl_index: HashMap::new(),
            inherent_impl_index: HashMap::new(),
            supertraits: HashMap::new(),
            assoc_types: HashMap::new(),
            next_id: 0,
            diags: DiagnosticBag::new(),
        }
    }

    /// Build an `ImplTable` by walking the top-level items of an AIR module node.
    ///
    /// The node must have `NodeKind::Module`; other node kinds produce an empty
    /// table. Coherence checking (exact-type `impl` overlap detection) runs
    /// during construction and emits `E4010` diagnostics for any duplicate
    /// `(Trait, Type)` pair.
    #[must_use]
    pub fn build_from(module: &AIRNode) -> Self {
        let mut table = Self::new();
        if let NodeKind::Module { items, .. } = &module.kind {
            for item in items {
                table.visit_item(item);
            }
        }
        table
    }

    fn visit_item(&mut self, node: &AIRNode) {
        match &node.kind {
            NodeKind::ImplBlock {
                trait_path,
                target,
                methods,
                generic_params,
                ..
            } => {
                let trait_ref = trait_path.as_ref().map(TraitRef::from_path);
                let type_key = type_key_from_node(target);
                let is_generic = !generic_params.is_empty();

                // Coherence: detect exact-type duplicates (skip generic impls).
                if !is_generic {
                    if let Some(tr) = &trait_ref {
                        let index_key = (tr.name.clone(), type_key.clone());
                        if self.trait_impl_index.contains_key(&index_key) {
                            self.diags.error(
                                E_COHERENCE_OVERLAP,
                                format!(
                                    "conflicting implementations of trait `{}` for type `{}`",
                                    tr.name, type_key,
                                ),
                                node.span,
                            );
                            return;
                        }
                    }
                }

                let id = self.alloc_id();

                // Collect method names, and register any associated type aliases.
                let mut method_names = Vec::new();
                for m in methods {
                    match &m.kind {
                        NodeKind::FnDecl { name, .. } => {
                            method_names.push(name.name.clone());
                        }
                        NodeKind::TypeAlias { name, ty, .. } => {
                            // Associated type binding: `type Assoc = ConcreteType`.
                            let resolved = type_from_node(ty);
                            self.assoc_types.insert((id, name.name.clone()), resolved);
                        }
                        _ => {}
                    }
                }

                // Register in the appropriate index.
                if let Some(tr) = &trait_ref {
                    if !is_generic {
                        self.trait_impl_index
                            .insert((tr.name.clone(), type_key.clone()), id);
                    }
                } else {
                    // Inherent impl — last registration wins for the type key.
                    self.inherent_impl_index.insert(type_key.clone(), id);
                }

                self.entries.insert(
                    id,
                    ImplEntry {
                        id,
                        trait_ref,
                        type_key,
                        methods: method_names,
                        is_generic,
                    },
                );
            }

            NodeKind::TraitDecl {
                name,
                generic_params,
                ..
            } => {
                // Extract supertrait bounds: any bound on a `Self` generic param
                // is treated as a supertrait requirement.
                for param in generic_params {
                    if param.name.name == "Self" {
                        for bound in &param.bounds {
                            let supertrait = trait_name_from_path(bound);
                            self.register_supertrait(name.name.clone(), supertrait);
                        }
                    }
                }
            }

            _ => {}
        }
    }

    fn alloc_id(&mut self) -> ImplId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Register a direct supertrait relationship: every impl of `sub_trait`
    /// must also impl `super_trait`.
    pub fn register_supertrait(
        &mut self,
        sub_trait: impl Into<String>,
        super_trait: impl Into<String>,
    ) {
        self.supertraits
            .entry(sub_trait.into())
            .or_default()
            .push(super_trait.into());
    }

    /// Programmatically register a trait impl for a concrete type.
    ///
    /// This is a convenience method for tests and downstream passes that need
    /// to populate the table without building from AIR nodes.
    pub fn register_trait_impl(&mut self, trait_name: impl Into<String>, ty: &Type) -> ImplId {
        let id = self.alloc_id();
        let trait_name = trait_name.into();
        let key = type_key(ty);
        self.entries.insert(
            id,
            ImplEntry {
                id,
                trait_ref: Some(TraitRef::new(&trait_name)),
                type_key: key.clone(),
                methods: vec![],
                is_generic: false,
            },
        );
        self.trait_impl_index.insert((trait_name, key), id);
        id
    }

    /// Register an associated type binding for a given impl block.
    ///
    /// Overrides any existing binding for `(impl_id, name)`.
    pub fn register_assoc_type(&mut self, impl_id: ImplId, name: impl Into<String>, ty: Type) {
        self.assoc_types.insert((impl_id, name.into()), ty);
    }

    /// Look up an associated type binding by impl id and name.
    ///
    /// Returns `None` if no binding has been registered.
    #[must_use]
    pub fn resolve_assoc_type(&self, impl_id: ImplId, name: &str) -> Option<&Type> {
        self.assoc_types.get(&(impl_id, name.to_owned()))
    }

    /// Return all supertraits of `trait_name`, collected transitively via BFS.
    ///
    /// The result list is in BFS order (direct supertraits before their
    /// supertraits) with no duplicates. The original `trait_name` is **not**
    /// included.
    #[must_use]
    pub fn all_supertraits(&self, trait_name: &str) -> Vec<String> {
        let mut result = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = VecDeque::new();

        if let Some(direct) = self.supertraits.get(trait_name) {
            for st in direct {
                if visited.insert(st.clone()) {
                    queue.push_back(st.clone());
                }
            }
        }

        while let Some(name) = queue.pop_front() {
            result.push(name.clone());
            if let Some(supers) = self.supertraits.get(&name) {
                for st in supers {
                    if visited.insert(st.clone()) {
                        queue.push_back(st.clone());
                    }
                }
            }
        }

        result
    }

    /// Get the impl entry for an id.
    #[must_use]
    pub fn get_entry(&self, id: ImplId) -> Option<&ImplEntry> {
        self.entries.get(&id)
    }

    /// Iterate over all registered entries.
    pub fn entries(&self) -> impl Iterator<Item = &ImplEntry> {
        self.entries.values()
    }

    // ── Internal lookup helpers ────────────────────────────────────────────────

    fn find_trait_impl(&self, trait_name: &str, type_key: &str) -> Option<ImplId> {
        self.trait_impl_index
            .get(&(trait_name.to_owned(), type_key.to_owned()))
            .copied()
    }

    fn find_inherent_impl(&self, type_key: &str) -> Option<ImplId> {
        self.inherent_impl_index.get(type_key).copied()
    }
}

impl Default for ImplTable {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Public resolution functions ──────────────────────────────────────────────

/// Find the impl block that satisfies `trait_ref` for `ty` in `impls`.
///
/// Performs an exact-type lookup: the type key derived from `ty` must match
/// the key stored when the impl was registered. Returns `None` if no impl
/// exists for the `(trait, type)` pair.
///
/// To verify that supertrait obligations are also satisfied, call
/// [`check_supertrait_obligations`] on the result.
#[must_use]
pub fn resolve_impl(trait_ref: &TraitRef, ty: &Type, impls: &ImplTable) -> Option<ImplId> {
    let key = type_key(ty);
    impls.find_trait_impl(&trait_ref.name, &key)
}

/// Check that all transitively required supertraits of `trait_ref` are
/// satisfied by `ty` in `impls`.
///
/// Returns `true` if every supertrait reachable from `trait_ref` via the
/// supertrait graph has a registered impl for `ty`.
#[must_use]
pub fn check_supertrait_obligations(trait_ref: &TraitRef, ty: &Type, impls: &ImplTable) -> bool {
    let key = type_key(ty);
    for supertrait in impls.all_supertraits(&trait_ref.name) {
        if impls.find_trait_impl(&supertrait, &key).is_none() {
            return false;
        }
    }
    true
}

/// Dispatch a method call on `receiver` by searching `impls`.
///
/// Search order:
/// 1. The inherent impl registered for `receiver`'s type key (if any).
/// 2. All trait impls whose target type key matches `receiver`.
///
/// Returns the first match found, or `None` if no impl provides `method`.
#[must_use]
pub fn resolve_method(receiver: &Type, method: &str, impls: &ImplTable) -> Option<ResolvedMethod> {
    let key = type_key(receiver);

    // 1. Inherent impl.
    if let Some(impl_id) = impls.find_inherent_impl(&key) {
        if let Some(entry) = impls.get_entry(impl_id) {
            if entry.methods.iter().any(|m| m == method) {
                return Some(ResolvedMethod {
                    impl_id,
                    trait_ref: None,
                    method: method.to_owned(),
                });
            }
        }
    }

    // 2. Trait impls — iterate all entries whose type key matches.
    for entry in impls.entries() {
        if entry.type_key == key
            && entry.trait_ref.is_some()
            && entry.methods.iter().any(|m| m == method)
        {
            return Some(ResolvedMethod {
                impl_id: entry.id,
                trait_ref: entry.trait_ref.clone(),
                method: method.to_owned(),
            });
        }
    }

    None
}

// ─── Key helpers ──────────────────────────────────────────────────────────────

/// Produce a canonical string key for a [`Type`].
///
/// The key is used as the second component of the `(trait, type)` lookup in
/// the [`ImplTable`]. It is deterministic and human-readable but is **not**
/// intended as a user-facing display format.
#[must_use]
pub fn type_key(ty: &Type) -> String {
    match ty {
        Type::Primitive(p) => format!("{p:?}"),
        Type::Named(n) => n.name.clone(),
        Type::Generic(g) => {
            let args = g.args.iter().map(type_key).collect::<Vec<_>>().join(", ");
            format!("{}[{}]", g.constructor, args)
        }
        Type::Tuple(elems) => {
            let elems = elems.iter().map(type_key).collect::<Vec<_>>().join(", ");
            format!("({})", elems)
        }
        Type::Function(f) => {
            let params = f.params.iter().map(type_key).collect::<Vec<_>>().join(", ");
            format!("Fn({}) -> {}", params, type_key(&f.ret))
        }
        Type::Optional(inner) => format!("{}?", type_key(inner)),
        Type::Result(ok, err) => format!("Result[{}, {}]", type_key(ok), type_key(err)),
        Type::TypeVar(id) => format!("?{id}"),
        Type::Refined(base, _) => type_key(base),
        Type::Flexible(_) => "Flexible".to_string(),
        Type::Error => "Error".to_string(),
    }
}

/// Extract a canonical trait name string from a [`TypePath`].
fn trait_name_from_path(path: &TypePath) -> String {
    path.segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join(".")
}

/// Produce a canonical type key string from an AIR type-expression node.
///
/// Used during [`ImplTable::build_from`] to key the target of an impl block.
fn type_key_from_node(node: &AIRNode) -> String {
    match &node.kind {
        NodeKind::TypeNamed { path, args } => {
            let name = path
                .segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(".");
            if args.is_empty() {
                name
            } else {
                let arg_keys: Vec<_> = args.iter().map(type_key_from_node).collect();
                format!("{}[{}]", name, arg_keys.join(", "))
            }
        }
        NodeKind::TypeTuple { elems } => {
            let elem_keys: Vec<_> = elems.iter().map(type_key_from_node).collect();
            format!("({})", elem_keys.join(", "))
        }
        NodeKind::TypeOptional { inner } => format!("{}?", type_key_from_node(inner)),
        NodeKind::TypeFunction { params, ret, .. } => {
            let param_keys: Vec<_> = params.iter().map(type_key_from_node).collect();
            format!(
                "Fn({}) -> {}",
                param_keys.join(", "),
                type_key_from_node(ret)
            )
        }
        NodeKind::TypeSelf => "Self".to_string(),
        _ => "Unknown".to_string(),
    }
}

/// Best-effort conversion of a type-expression AIR node to a [`Type`].
///
/// Used when extracting associated type bindings from impl body items.
/// Unrecognised nodes produce [`Type::Error`].
fn type_from_node(node: &AIRNode) -> Type {
    match &node.kind {
        NodeKind::TypeNamed { path, args } => {
            let name = path
                .segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(".");
            if args.is_empty() {
                match name.as_str() {
                    "Int" => Type::Primitive(PrimitiveType::Int),
                    "Float" => Type::Primitive(PrimitiveType::Float),
                    "Bool" => Type::Primitive(PrimitiveType::Bool),
                    "String" => Type::Primitive(PrimitiveType::String),
                    "Char" => Type::Primitive(PrimitiveType::Char),
                    "Void" => Type::Primitive(PrimitiveType::Void),
                    "Never" => Type::Primitive(PrimitiveType::Never),
                    _ => Type::Named(NamedType { name }),
                }
            } else {
                let type_args: Vec<_> = args.iter().map(type_from_node).collect();
                Type::Generic(GenericType {
                    constructor: name,
                    args: type_args,
                })
            }
        }
        NodeKind::TypeOptional { inner } => Type::Optional(Box::new(type_from_node(inner))),
        NodeKind::TypeTuple { elems } => Type::Tuple(elems.iter().map(type_from_node).collect()),
        NodeKind::TypeSelf => Type::Named(NamedType {
            name: "Self".to_owned(),
        }),
        _ => Type::Error,
    }
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NamedType, PrimitiveType, Type};

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn named(name: &str) -> Type {
        Type::Named(NamedType {
            name: name.to_owned(),
        })
    }

    fn int() -> Type {
        Type::Primitive(PrimitiveType::Int)
    }

    fn bool_ty() -> Type {
        Type::Primitive(PrimitiveType::Bool)
    }

    fn dummy_span() -> bock_errors::Span {
        use bock_errors::{FileId, Span};
        Span {
            file: FileId(0),
            start: 0,
            end: 0,
        }
    }

    fn make_air_node(kind: NodeKind) -> AIRNode {
        AIRNode::new(0, dummy_span(), kind)
    }

    fn make_module(items: Vec<AIRNode>) -> AIRNode {
        make_air_node(NodeKind::Module {
            path: None,
            annotations: vec![],
            imports: vec![],
            items,
        })
    }

    fn make_type_named(name: &str) -> AIRNode {
        use bock_ast::{Ident, TypePath};
        let ident = Ident {
            name: name.to_owned(),
            span: dummy_span(),
        };
        make_air_node(NodeKind::TypeNamed {
            path: TypePath {
                segments: vec![ident],
                span: dummy_span(),
            },
            args: vec![],
        })
    }

    fn make_fn_decl(name: &str) -> AIRNode {
        use bock_ast::{Ident, Visibility};
        let body = make_air_node(NodeKind::Block {
            stmts: vec![],
            tail: None,
        });
        make_air_node(NodeKind::FnDecl {
            annotations: vec![],
            visibility: Visibility::Private,
            is_async: false,
            name: Ident {
                name: name.to_owned(),
                span: dummy_span(),
            },
            generic_params: vec![],
            params: vec![],
            return_type: None,
            effect_clause: vec![],
            where_clause: vec![],
            body: Box::new(body),
        })
    }

    fn make_impl_block(
        trait_name: Option<&str>,
        target_name: &str,
        methods: Vec<AIRNode>,
    ) -> AIRNode {
        use bock_ast::{Ident, TypePath};
        let trait_path = trait_name.map(|n| TypePath {
            segments: vec![Ident {
                name: n.to_owned(),
                span: dummy_span(),
            }],
            span: dummy_span(),
        });
        make_air_node(NodeKind::ImplBlock {
            annotations: vec![],
            generic_params: vec![],
            trait_path,
            target: Box::new(make_type_named(target_name)),
            where_clause: vec![],
            methods,
        })
    }

    // ── type_key ──────────────────────────────────────────────────────────────

    #[test]
    fn type_key_primitive() {
        assert_eq!(type_key(&int()), "Int");
        assert_eq!(type_key(&bool_ty()), "Bool");
    }

    #[test]
    fn type_key_named() {
        assert_eq!(type_key(&named("User")), "User");
    }

    #[test]
    fn type_key_generic() {
        use crate::GenericType;
        let ty = Type::Generic(GenericType {
            constructor: "List".to_owned(),
            args: vec![int()],
        });
        assert_eq!(type_key(&ty), "List[Int]");
    }

    #[test]
    fn type_key_optional() {
        assert_eq!(type_key(&Type::Optional(Box::new(int()))), "Int?");
    }

    #[test]
    fn type_key_result() {
        assert_eq!(
            type_key(&Type::Result(Box::new(int()), Box::new(named("Err")))),
            "Result[Int, Err]"
        );
    }

    // ── ImplTable construction ─────────────────────────────────────────────────

    #[test]
    fn build_empty_module() {
        let module = make_module(vec![]);
        let table = ImplTable::build_from(&module);
        assert!(!table.diags.has_errors());
        assert_eq!(table.entries.len(), 0);
    }

    #[test]
    fn build_registers_trait_impl() {
        let eq_method = make_fn_decl("equals");
        let impl_block = make_impl_block(Some("Equatable"), "User", vec![eq_method]);
        let module = make_module(vec![impl_block]);
        let table = ImplTable::build_from(&module);
        assert!(!table.diags.has_errors());
        let id = resolve_impl(&TraitRef::new("Equatable"), &named("User"), &table);
        assert!(id.is_some());
    }

    #[test]
    fn build_registers_inherent_impl() {
        let method = make_fn_decl("greet");
        let impl_block = make_impl_block(None, "User", vec![method]);
        let module = make_module(vec![impl_block]);
        let table = ImplTable::build_from(&module);
        let result = resolve_method(&named("User"), "greet", &table);
        assert!(result.is_some());
        let r = result.unwrap();
        assert!(r.trait_ref.is_none());
        assert_eq!(r.method, "greet");
    }

    // ── resolve_impl ──────────────────────────────────────────────────────────

    #[test]
    fn resolve_impl_found() {
        let mut table = ImplTable::new();
        let id = table.alloc_id();
        table.entries.insert(
            id,
            ImplEntry {
                id,
                trait_ref: Some(TraitRef::new("Printable")),
                type_key: "Int".to_owned(),
                methods: vec!["print".to_owned()],
                is_generic: false,
            },
        );
        table
            .trait_impl_index
            .insert(("Printable".to_owned(), "Int".to_owned()), id);

        assert_eq!(
            resolve_impl(&TraitRef::new("Printable"), &int(), &table),
            Some(id)
        );
    }

    #[test]
    fn resolve_impl_not_found() {
        let table = ImplTable::new();
        assert_eq!(
            resolve_impl(&TraitRef::new("Printable"), &int(), &table),
            None
        );
    }

    #[test]
    fn resolve_impl_wrong_type() {
        let mut table = ImplTable::new();
        let id = table.alloc_id();
        table.entries.insert(
            id,
            ImplEntry {
                id,
                trait_ref: Some(TraitRef::new("Printable")),
                type_key: "Int".to_owned(),
                methods: vec!["print".to_owned()],
                is_generic: false,
            },
        );
        table
            .trait_impl_index
            .insert(("Printable".to_owned(), "Int".to_owned()), id);

        // Bool does not implement Printable.
        assert_eq!(
            resolve_impl(&TraitRef::new("Printable"), &bool_ty(), &table),
            None
        );
    }

    // ── resolve_method ────────────────────────────────────────────────────────

    #[test]
    fn resolve_method_inherent() {
        let method = make_fn_decl("to_string");
        let impl_block = make_impl_block(None, "User", vec![method]);
        let module = make_module(vec![impl_block]);
        let table = ImplTable::build_from(&module);

        let r = resolve_method(&named("User"), "to_string", &table);
        assert!(r.is_some());
        let r = r.unwrap();
        assert!(r.trait_ref.is_none());
        assert_eq!(r.method, "to_string");
    }

    #[test]
    fn resolve_method_from_trait_impl() {
        let method = make_fn_decl("serialize");
        let impl_block = make_impl_block(Some("Serializable"), "User", vec![method]);
        let module = make_module(vec![impl_block]);
        let table = ImplTable::build_from(&module);

        let r = resolve_method(&named("User"), "serialize", &table);
        assert!(r.is_some());
        let r = r.unwrap();
        assert_eq!(
            r.trait_ref.as_ref().map(|t| t.name.as_str()),
            Some("Serializable")
        );
        assert_eq!(r.method, "serialize");
    }

    #[test]
    fn resolve_method_not_found() {
        let table = ImplTable::new();
        assert!(resolve_method(&int(), "foo", &table).is_none());
    }

    #[test]
    fn resolve_method_inherent_takes_priority_over_trait() {
        let inherent_method = make_fn_decl("display");
        let trait_method = make_fn_decl("display");
        let inherent_impl = make_impl_block(None, "User", vec![inherent_method]);
        let trait_impl = make_impl_block(Some("Display"), "User", vec![trait_method]);
        let module = make_module(vec![inherent_impl, trait_impl]);
        let table = ImplTable::build_from(&module);

        let r = resolve_method(&named("User"), "display", &table).unwrap();
        // Inherent impl has priority — no trait_ref.
        assert!(r.trait_ref.is_none());
    }

    // ── Coherence ─────────────────────────────────────────────────────────────

    #[test]
    fn coherence_detects_exact_overlap() {
        let impl1 = make_impl_block(Some("Equatable"), "Point", vec![make_fn_decl("equals")]);
        let impl2 = make_impl_block(Some("Equatable"), "Point", vec![make_fn_decl("equals")]);
        let module = make_module(vec![impl1, impl2]);
        let table = ImplTable::build_from(&module);

        assert!(table.diags.has_errors());
        assert_eq!(table.diags.error_count(), 1);
    }

    #[test]
    fn coherence_allows_different_types() {
        let impl1 = make_impl_block(Some("Equatable"), "Int", vec![make_fn_decl("equals")]);
        let impl2 = make_impl_block(Some("Equatable"), "Bool", vec![make_fn_decl("equals")]);
        let module = make_module(vec![impl1, impl2]);
        let table = ImplTable::build_from(&module);

        assert!(!table.diags.has_errors());
    }

    #[test]
    fn coherence_allows_different_traits() {
        let impl1 = make_impl_block(Some("Equatable"), "Point", vec![make_fn_decl("equals")]);
        let impl2 = make_impl_block(Some("Comparable"), "Point", vec![make_fn_decl("compare")]);
        let module = make_module(vec![impl1, impl2]);
        let table = ImplTable::build_from(&module);

        assert!(!table.diags.has_errors());
    }

    #[test]
    fn coherence_skips_generic_impls() {
        use bock_ast::{GenericParam, Ident, TypePath};

        let generic_param = GenericParam {
            id: 0,
            span: dummy_span(),
            name: Ident {
                name: "T".to_owned(),
                span: dummy_span(),
            },
            bounds: vec![],
        };
        let impl1 = make_air_node(NodeKind::ImplBlock {
            annotations: vec![],
            generic_params: vec![generic_param.clone()],
            trait_path: Some(TypePath {
                segments: vec![Ident {
                    name: "Printable".to_owned(),
                    span: dummy_span(),
                }],
                span: dummy_span(),
            }),
            target: Box::new(make_type_named("T")),
            where_clause: vec![],
            methods: vec![],
        });
        let impl2 = make_air_node(NodeKind::ImplBlock {
            annotations: vec![],
            generic_params: vec![generic_param],
            trait_path: Some(TypePath {
                segments: vec![Ident {
                    name: "Printable".to_owned(),
                    span: dummy_span(),
                }],
                span: dummy_span(),
            }),
            target: Box::new(make_type_named("T")),
            where_clause: vec![],
            methods: vec![],
        });
        let module = make_module(vec![impl1, impl2]);
        let table = ImplTable::build_from(&module);

        // Generic impls are exempt from exact-type coherence.
        assert!(!table.diags.has_errors());
    }

    // ── Supertrait obligations ─────────────────────────────────────────────────

    #[test]
    fn supertrait_registration_and_lookup() {
        let mut table = ImplTable::new();
        table.register_supertrait("Hashable", "Equatable");
        let supers = table.all_supertraits("Hashable");
        assert_eq!(supers, vec!["Equatable"]);
    }

    #[test]
    fn supertrait_transitive() {
        let mut table = ImplTable::new();
        table.register_supertrait("C", "B");
        table.register_supertrait("B", "A");
        let supers = table.all_supertraits("C");
        assert_eq!(supers, vec!["B", "A"]);
    }

    #[test]
    fn check_supertrait_obligations_satisfied() {
        let mut table = ImplTable::new();
        table.register_supertrait("Hashable", "Equatable");

        // Register impls for both Equatable and Hashable on User.
        let eq_method = make_fn_decl("equals");
        let hash_method = make_fn_decl("hash");
        let eq_impl = make_impl_block(Some("Equatable"), "User", vec![eq_method]);
        let hash_impl = make_impl_block(Some("Hashable"), "User", vec![hash_method]);
        let module = make_module(vec![eq_impl, hash_impl]);
        let table_built = ImplTable::build_from(&module);

        // Manually merge supertrait registration into built table.
        let mut full_table = table_built;
        full_table.register_supertrait("Hashable", "Equatable");

        assert!(check_supertrait_obligations(
            &TraitRef::new("Hashable"),
            &named("User"),
            &full_table
        ));
    }

    #[test]
    fn check_supertrait_obligations_missing() {
        let mut table = ImplTable::new();
        table.register_supertrait("Hashable", "Equatable");

        // Only Hashable impl, missing Equatable.
        let hash_method = make_fn_decl("hash");
        let hash_impl = make_impl_block(Some("Hashable"), "User", vec![hash_method]);
        let module = make_module(vec![hash_impl]);
        let table_built = ImplTable::build_from(&module);
        let mut full_table = table_built;
        full_table.register_supertrait("Hashable", "Equatable");

        // Equatable supertrait is not satisfied.
        assert!(!check_supertrait_obligations(
            &TraitRef::new("Hashable"),
            &named("User"),
            &full_table
        ));
    }

    // ── Associated types ───────────────────────────────────────────────────────

    #[test]
    fn assoc_type_manual_registration() {
        let mut table = ImplTable::new();
        let impl_id = table.alloc_id();
        table.register_assoc_type(impl_id, "Item", int());

        assert_eq!(table.resolve_assoc_type(impl_id, "Item"), Some(&int()));
        assert_eq!(table.resolve_assoc_type(impl_id, "Missing"), None);
    }

    #[test]
    fn assoc_type_not_found_for_other_impl() {
        let mut table = ImplTable::new();
        let id1 = table.alloc_id();
        let id2 = table.alloc_id();
        table.register_assoc_type(id1, "Item", int());

        assert_eq!(table.resolve_assoc_type(id2, "Item"), None);
    }

    // ── Method dispatch on generic types ──────────────────────────────────────

    #[test]
    fn resolve_method_on_generic_type() {
        let method = make_fn_decl("push");
        let impl_block = make_impl_block(None, "List", vec![method]);
        let module = make_module(vec![impl_block]);
        let table = ImplTable::build_from(&module);

        // The inherent impl is registered under the key "List" (no type args in
        // the target node). A receiver of List[Int] won't match by key since its
        // key is "List[Int]", but a plain Named("List") will.
        let receiver = Type::Named(NamedType {
            name: "List".to_owned(),
        });
        let r = resolve_method(&receiver, "push", &table);
        assert!(r.is_some());
    }
}
