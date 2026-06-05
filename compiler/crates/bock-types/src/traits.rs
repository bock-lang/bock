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

/// `E4011` — a user `impl` tries to implement a sealed core trait for a
/// primitive type (orphan-rule violation). See [`SEALED_CORE_TRAITS`].
const E_SEALED_PRIMITIVE_IMPL: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 4011,
};

/// `E4012` — the single-method-namespace rule (DQ27) is violated: a method name
/// is defined more than once for one type, across any combination of inherent
/// `impl T {}`, `class T {}` body, and `impl Trait for T {}` blocks. A type has
/// exactly one method namespace keyed by method name; a trait requirement is
/// satisfied by a name+signature match *anywhere* in that namespace, so the
/// same name cannot be defined twice. See spec §6.4/§6.5/§6.7.
const E_DUPLICATE_METHOD: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 4012,
};

// ─── Core types ───────────────────────────────────────────────────────────────

/// Unique identifier for a registered impl block.
pub type ImplId = u32;

/// A reference to a named trait, identified by its fully-qualified name.
///
/// Examples: `"Equatable"`, `"Std.Io.Writable"`.
#[derive(Debug, Clone, PartialEq)]
pub struct TraitRef {
    /// Fully-qualified name of the trait.
    pub name: String,
    /// Type arguments applied to a parameterized trait, e.g. `[Int]` in
    /// `From[Int]`. Empty for non-parameterized traits.
    pub args: Vec<Type>,
}

impl TraitRef {
    /// Create a `TraitRef` for a non-parameterized trait from any string-like
    /// value. The argument list is empty.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            args: vec![],
        }
    }

    /// Create a `TraitRef` for a parameterized trait, e.g.
    /// `TraitRef::parameterized("From", vec![Type::Primitive(Int)])` for
    /// `From[Int]`.
    #[must_use]
    pub fn parameterized(name: impl Into<String>, args: Vec<Type>) -> Self {
        Self {
            name: name.into(),
            args,
        }
    }

    fn from_path(path: &TypePath) -> Self {
        let name = path
            .segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join(".");
        Self { name, args: vec![] }
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
    /// `true` when this entry was registered by the compiler as a canonical
    /// conformance for a primitive type (see
    /// [`register_canonical_conformances`]). User `impl` blocks always set
    /// this to `false`. Canonical entries are sealed: user code may not add
    /// its own `impl` of a core trait for a primitive (`E4011`).
    pub is_canonical: bool,
    /// Type arguments applied to the implemented trait, e.g. `[Int]` in
    /// `impl From[Int] for Float`. Empty for non-parameterized traits.
    pub trait_args: Vec<Type>,
    /// `true` when this entry was synthesized by the compiler from another
    /// impl rather than written by the user — e.g. the blanket
    /// `Into[U] for T` derived from an explicit `From[T] for U`. A derived
    /// entry never wins over an explicit one and never triggers a coherence
    /// error against an explicit impl.
    pub is_derived: bool,
    /// The structured target [`Type`] of the impl, when it could be resolved
    /// from the AIR target node. Used to synthesize blanket reverse impls
    /// (e.g. deriving `Into[U]` from `From[T] for U`). `None` for entries
    /// built from an unrecognized target node.
    pub target_type: Option<Type>,
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
    /// Trait impl index: `(trait_name, type_key) → ImplId` (concrete,
    /// non-parameterized impls only). Untouched by parameterized-trait support
    /// so all pre-existing (Q-bridge) behavior is bit-identical.
    trait_impl_index: HashMap<(String, String), ImplId>,
    /// Parameterized trait impl index:
    /// `(trait_name, trait_arg_key, target_type_key) → ImplId`. Used for traits
    /// that carry type arguments, e.g. `From[Int] for Float`. Keyed on the
    /// three-tuple so `From[Int] for Float` and `From[String] for Float`
    /// coexist without a coherence collision.
    param_trait_impl_index: HashMap<(String, String, String), ImplId>,
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
    /// Per-type record of every method *definition* seen while building the
    /// table, keyed by the target type key. Each entry captures the method's
    /// name, a structural signature key, its span, and the block it came from.
    /// Used after the visit pass to enforce the single-method-namespace rule
    /// (DQ27): defining a method name more than once for one type is an
    /// `E4012` coherence error, regardless of which blocks the definitions
    /// appear in. Drained into [`Self::diags`] by [`Self::check_method_namespace`].
    method_defs: HashMap<String, Vec<MethodDef>>,
}

/// A single method definition seen during [`ImplTable::build_from`], retained
/// only long enough to run the single-method-namespace coherence check.
struct MethodDef {
    /// The method's name (the namespace key within a type).
    name: String,
    /// A structural key for the method's signature (param + return shape),
    /// used to distinguish a true duplicate (matching signature) from two
    /// genuinely-conflicting requirements (differing signatures).
    sig_key: String,
    /// Span of the method's `fn` declaration, for the diagnostic.
    span: bock_errors::Span,
    /// Human-readable description of the block the method was declared in,
    /// e.g. `"inherent impl"`, `"class body"`, or `"impl Component for ..."`.
    origin: String,
}

impl ImplTable {
    /// Create a new, empty `ImplTable`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            trait_impl_index: HashMap::new(),
            param_trait_impl_index: HashMap::new(),
            inherent_impl_index: HashMap::new(),
            supertraits: HashMap::new(),
            assoc_types: HashMap::new(),
            next_id: 0,
            diags: DiagnosticBag::new(),
            method_defs: HashMap::new(),
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
        // Single-method-namespace coherence (DQ27): every method that applies
        // to a type — inherent, class-body, or trait-impl — shares one
        // namespace keyed by name. Defining a name twice for one type is an
        // `E4012` error. Run after all items are visited so the check sees the
        // type's whole namespace.
        table.check_method_namespace();
        // Second pass: synthesize blanket `Into[U] for T` from each explicit
        // `From[T] for U`. Runs after all explicit impls so an explicit
        // `Into` always wins (the synthesized entry is skipped if its slot
        // is occupied — see `synthesize_blanket_into`).
        table.synthesize_blanket_into();
        table
    }

    /// Enforce the single-method-namespace rule (DQ27) across every block that
    /// contributes methods to a type.
    ///
    /// A type — `record` or `class` — has exactly one method namespace keyed by
    /// method name. Methods declared in an inherent `impl T {}`, a `class T {}`
    /// body, or any `impl Trait for T {}` block all share that namespace. A
    /// trait requirement is satisfied by a name+signature match *anywhere* in
    /// the namespace (so an empty `impl Trait for T {}` is well-formed when an
    /// inherent/class-body method already provides the required method), which
    /// in turn means a name cannot be defined twice for one type:
    ///
    /// - Two definitions with **matching signatures** are a redundant
    ///   duplicate (e.g. an inherent `render` plus a trait-impl `render`
    ///   forwarder) — the classic react-components collision.
    /// - Two definitions with **differing signatures** (e.g. two traits that
    ///   both require a `foo` with incompatible signatures) are genuinely
    ///   unsatisfiable on the v1 targets, which have one method slot per name.
    ///
    /// Both are reported as `E4012`. The message distinguishes the two cases so
    /// the user knows whether to delete a redundant definition or rename.
    fn check_method_namespace(&mut self) {
        // Stable ordering for deterministic diagnostics across runs.
        let mut type_keys: Vec<&String> = self.method_defs.keys().collect();
        type_keys.sort();
        let type_keys: Vec<String> = type_keys.into_iter().cloned().collect();

        for type_key in type_keys {
            let defs = &self.method_defs[&type_key];
            // Group definition indices by method name, preserving declaration
            // order so the *first* definition is treated as canonical and
            // later ones are flagged.
            let mut by_name: HashMap<&str, Vec<usize>> = HashMap::new();
            for (idx, def) in defs.iter().enumerate() {
                by_name.entry(def.name.as_str()).or_default().push(idx);
            }
            let mut names: Vec<&str> = by_name.keys().copied().collect();
            names.sort();

            for name in names {
                let indices = &by_name[name];
                if indices.len() < 2 {
                    continue;
                }
                let first = &defs[indices[0]];
                for &dup_idx in &indices[1..] {
                    let dup = &defs[dup_idx];
                    let same_sig = dup.sig_key == first.sig_key;
                    let detail = if same_sig {
                        format!(
                            "a method named `{name}` is already defined for type `{type_key}` \
                             in the {}; a type has one method namespace, so the same method \
                             may not be defined twice",
                            first.origin,
                        )
                    } else {
                        format!(
                            "method `{name}` is defined for type `{type_key}` with conflicting \
                             signatures (in the {} and the {}); a type has one method slot per \
                             name and cannot satisfy two requirements with incompatible signatures",
                            first.origin, dup.origin,
                        )
                    };
                    self.diags
                        .error(E_DUPLICATE_METHOD, detail, dup.span)
                        .note(format!(
                            "a trait requirement is satisfied by a matching method anywhere in \
                             the type's namespace; if `{name}` should satisfy a trait, define it \
                             once (as an inherent/class-body method or inside the trait impl) and \
                             leave the other block empty",
                        ));
                }
            }
        }
    }

    /// Record a method definition against its target type for the
    /// single-method-namespace check. Called from [`Self::visit_item`] for
    /// inherent/trait `impl` blocks and `class` bodies.
    fn record_method_def(&mut self, type_key: &str, method: &AIRNode, origin: &str) {
        if let NodeKind::FnDecl { name, .. } = &method.kind {
            self.method_defs
                .entry(type_key.to_owned())
                .or_default()
                .push(MethodDef {
                    name: name.name.clone(),
                    sig_key: method_sig_key(method),
                    span: method.span,
                    origin: origin.to_owned(),
                });
        }
    }

    /// For every explicit `impl From[T] for U`, synthesize the blanket reverse
    /// `impl Into[U] for T`, marked `is_derived`.
    ///
    /// Skips synthesis when the `Into[U] for T` slot is already occupied — an
    /// explicit `Into` impl always wins and a derived entry never triggers an
    /// `E4010` coherence error against an explicit impl. Only `From` impls
    /// with exactly one trait argument participate (the v1 surface);
    /// `TryFrom` is intentionally NOT blanket-reversed.
    fn synthesize_blanket_into(&mut self) {
        // Collect first to avoid mutating while iterating.
        let froms: Vec<(Type, Type)> = self
            .entries
            .values()
            .filter_map(|e| {
                let tr = e.trait_ref.as_ref()?;
                if tr.name != "From" || tr.args.len() != 1 || e.is_generic {
                    return None;
                }
                // From[T] for U  →  source T = tr.args[0], target U = e.type_key's type.
                let source = tr.args[0].clone();
                let target = e.target_type.clone()?;
                Some((source, target))
            })
            .collect();

        for (source, target) in froms {
            // Reverse: Into[target] for source.
            let into_arg_key = trait_arg_key(std::slice::from_ref(&target));
            let into_target_key = type_key(&source);
            let occupied = self.param_trait_impl_index.contains_key(&(
                "Into".to_owned(),
                into_arg_key,
                into_target_key,
            ));
            if occupied {
                // Explicit (or already-derived) Into wins — never clobber.
                continue;
            }
            self.register_param_trait_impl("Into", std::slice::from_ref(&target), &source, true);
        }
    }

    fn visit_item(&mut self, node: &AIRNode) {
        match &node.kind {
            NodeKind::ImplBlock {
                trait_path,
                trait_args,
                target,
                methods,
                generic_params,
                ..
            } => {
                // Resolve the trait's type arguments (e.g. `[Int]` in
                // `impl From[Int] for Float`) into `Type`s, and build a
                // parameterized `TraitRef` when present.
                let resolved_trait_args: Vec<Type> =
                    trait_args.iter().map(type_from_node).collect();
                let trait_ref = trait_path.as_ref().map(|p| {
                    let mut tr = TraitRef::from_path(p);
                    tr.args = resolved_trait_args.clone();
                    tr
                });
                let type_key = type_key_from_node(target);
                let is_generic = !generic_params.is_empty();

                // Sealing (Q1b): a *user* `impl <CoreTrait> for <Primitive>` is
                // an orphan-rule violation — core traits have sealed,
                // compiler-provided conformances for primitives. The newtype
                // pattern is the escape hatch. Scoped strictly to the (core
                // trait, primitive) quadrant; this is NOT a general orphan
                // model. The compiler's own canonical conformances are added
                // later via `register_trait_impl_inner`, which bypasses this
                // check, so they are never rejected.
                if let Some(tr) = &trait_ref {
                    if SEALED_CORE_TRAITS.contains(&tr.name.as_str())
                        && SEALED_PRIMITIVE_KEYS.contains(&type_key.as_str())
                    {
                        self.diags
                            .error(
                                E_SEALED_PRIMITIVE_IMPL,
                                format!(
                                    "cannot implement core trait `{}` for primitive type                                      `{}`: its conformance is provided by the compiler and                                      is sealed",
                                    tr.name, type_key,
                                ),
                                node.span,
                            )
                            .note(format!(
                                "wrap `{type_key}` in a newtype (e.g. `record My{type_key}                                  {{ value: {type_key} }}`) and implement `{}` for that instead",
                                tr.name,
                            ));
                        return;
                    }
                }

                // Coherence: detect exact-type duplicates (skip generic impls).
                // Parameterized traits key on the (trait, trait-args, target)
                // three-tuple, so `From[Int] for Float` and
                // `From[String] for Float` do not collide; non-parameterized
                // traits use the original two-tuple index (bit-identical to
                // the Q-bridge behavior).
                if !is_generic {
                    if let Some(tr) = &trait_ref {
                        if tr.args.is_empty() {
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
                        } else {
                            let arg_key = trait_arg_key(&tr.args);
                            let index_key = (tr.name.clone(), arg_key.clone(), type_key.clone());
                            if self.param_trait_impl_index.contains_key(&index_key) {
                                self.diags.error(
                                    E_COHERENCE_OVERLAP,
                                    format!(
                                        "conflicting implementations of trait `{}[{}]` for type `{}`",
                                        tr.name, arg_key, type_key,
                                    ),
                                    node.span,
                                );
                                return;
                            }
                        }
                    }
                }

                let id = self.alloc_id();

                // Human-readable origin used by the single-method-namespace
                // check's diagnostics (DQ27): inherent vs `impl Trait for T`.
                let origin = match &trait_ref {
                    Some(tr) => format!("`impl {} for {}` block", tr.name, type_key),
                    None => format!("inherent `impl {type_key}` block"),
                };

                // The single-method-namespace check (DQ27) governs methods that
                // are dispatched by name (`x.foo()`). It excludes:
                //   * parameterized trait impls (`From[Int]`, `From[String]`,
                //     `Into[U]`): each instantiation legitimately carries the
                //     same method name (`from`/`into`) and is selected by the
                //     trait argument through the parameterized index, never as a
                //     bare `x.from()` collision; and
                //   * generic/blanket impls (`impl[T] Foo[T]`), which are also
                //     exempt from the `E4010` exact-overlap check.
                let trait_is_parameterized =
                    trait_ref.as_ref().is_some_and(|tr| !tr.args.is_empty());
                let track_namespace = !is_generic && !trait_is_parameterized;

                // Collect method names, and register any associated type aliases.
                let mut method_names = Vec::new();
                for m in methods {
                    match &m.kind {
                        NodeKind::FnDecl { name, .. } => {
                            method_names.push(name.name.clone());
                            if track_namespace {
                                self.record_method_def(&type_key, m, &origin);
                            }
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
                        if tr.args.is_empty() {
                            self.trait_impl_index
                                .insert((tr.name.clone(), type_key.clone()), id);
                        } else {
                            self.param_trait_impl_index.insert(
                                (tr.name.clone(), trait_arg_key(&tr.args), type_key.clone()),
                                id,
                            );
                        }
                    }
                } else {
                    // Inherent impl — last registration wins for the type key.
                    self.inherent_impl_index.insert(type_key.clone(), id);
                }

                let target_type = Some(type_from_node(target));
                self.entries.insert(
                    id,
                    ImplEntry {
                        id,
                        trait_ref,
                        type_key,
                        methods: method_names,
                        is_generic,
                        is_canonical: false,
                        trait_args: resolved_trait_args,
                        is_derived: false,
                        target_type,
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

            NodeKind::ClassDecl { name, methods, .. } => {
                // Class-body methods share the type's single method namespace
                // (DQ27), so they participate in the duplicate-method check
                // alongside inherent and trait-impl methods. (Class bodies are
                // not registered into the impl indices here — method dispatch
                // for class bodies is handled in the checker — but they must be
                // visible to the namespace coherence check.)
                let class_key = name.name.clone();
                let origin = format!("`class {class_key}` body");
                for m in methods {
                    if matches!(m.kind, NodeKind::FnDecl { .. }) {
                        self.record_method_def(&class_key, m, &origin);
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
    /// to populate the table without building from AIR nodes. The registered
    /// entry is *not* marked canonical; use
    /// [`register_canonical_conformances`] for the compiler-provided
    /// primitive conformances.
    pub fn register_trait_impl(&mut self, trait_name: impl Into<String>, ty: &Type) -> ImplId {
        self.register_trait_impl_inner(trait_name, &[], ty, false, false)
    }

    /// Programmatically register a *parameterized* trait impl for a concrete
    /// type, e.g. `From[Source] for Target`. Like [`Self::register_trait_impl`] the
    /// entry is not marked canonical. Pass `is_derived = true` for an entry
    /// synthesized from another impl (e.g. the blanket `Into`).
    pub fn register_param_trait_impl(
        &mut self,
        trait_name: impl Into<String>,
        trait_args: &[Type],
        ty: &Type,
        is_derived: bool,
    ) -> ImplId {
        self.register_trait_impl_inner(trait_name, trait_args, ty, false, is_derived)
    }

    /// Inner registration shared by [`Self::register_trait_impl`],
    /// [`Self::register_param_trait_impl`], and [`register_canonical_conformances`].
    ///
    /// This bypasses the orphan/sealing check applied to user `impl` blocks in
    /// [`Self::visit_item`], so the compiler's own canonical registration is
    /// never rejected.
    ///
    /// `trait_args` empty routes into the original two-tuple `trait_impl_index`
    /// (bit-identical to the Q-bridge behavior); non-empty routes into the
    /// parameterized three-tuple `param_trait_impl_index`.
    fn register_trait_impl_inner(
        &mut self,
        trait_name: impl Into<String>,
        trait_args: &[Type],
        ty: &Type,
        is_canonical: bool,
        is_derived: bool,
    ) -> ImplId {
        let id = self.alloc_id();
        let trait_name = trait_name.into();
        let key = type_key(ty);
        let args_vec = trait_args.to_vec();
        let trait_ref = if args_vec.is_empty() {
            TraitRef::new(&trait_name)
        } else {
            TraitRef::parameterized(&trait_name, args_vec.clone())
        };
        self.entries.insert(
            id,
            ImplEntry {
                id,
                trait_ref: Some(trait_ref),
                type_key: key.clone(),
                methods: vec![],
                is_generic: false,
                is_canonical,
                trait_args: args_vec.clone(),
                is_derived,
                target_type: Some(ty.clone()),
            },
        );
        if args_vec.is_empty() {
            self.trait_impl_index.insert((trait_name, key), id);
        } else {
            self.param_trait_impl_index
                .insert((trait_name, trait_arg_key(&args_vec), key), id);
        }
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

    fn find_param_trait_impl(
        &self,
        trait_name: &str,
        trait_arg_key: &str,
        type_key: &str,
    ) -> Option<ImplId> {
        self.param_trait_impl_index
            .get(&(
                trait_name.to_owned(),
                trait_arg_key.to_owned(),
                type_key.to_owned(),
            ))
            .copied()
    }

    /// Returns `true` if *any* parameterized impl of `trait_name` exists for
    /// `type_key`, regardless of the trait's type argument.
    ///
    /// This backs the v1 *arg-imprecise* satisfaction of a parameterized bound
    /// such as `T: Into[U]`: because the bound's type argument is dropped at
    /// parse time (the `where`-clause stores only the trait path), the checker
    /// cannot key on the exact `[U]`. It instead accepts the bound when the
    /// concrete `T` implements the trait for *some* argument. See the
    /// session PR notes for this documented limitation.
    #[must_use]
    pub fn has_any_param_trait_impl(&self, trait_name: &str, type_key: &str) -> bool {
        self.param_trait_impl_index
            .keys()
            .any(|(t, _arg, ty)| t == trait_name && ty == type_key)
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
///
/// For a parameterized trait (`trait_ref.args` non-empty), the lookup keys on
/// the `(trait_name, trait_arg_key, target_type_key)` three-tuple, so
/// `From[Int] for Float` and `From[String] for Float` resolve independently.
/// Non-parameterized traits use the original two-tuple index (unchanged from
/// the Q-bridge behavior).
#[must_use]
pub fn resolve_impl(trait_ref: &TraitRef, ty: &Type, impls: &ImplTable) -> Option<ImplId> {
    let key = type_key(ty);
    if trait_ref.args.is_empty() {
        impls.find_trait_impl(&trait_ref.name, &key)
    } else {
        impls.find_param_trait_impl(&trait_ref.name, &trait_arg_key(&trait_ref.args), &key)
    }
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

/// Produce a canonical key string for a parameterized trait's type arguments.
///
/// `From[Int]` yields `"Int"`; `Pair[Int, String]` yields `"Int, String"`;
/// a non-parameterized trait yields the empty string. The key is the third
/// component (well, second of the trait portion) of the parameterized
/// lookup tuple and mirrors [`type_key`]'s encoding.
#[must_use]
pub fn trait_arg_key(args: &[Type]) -> String {
    args.iter().map(type_key).collect::<Vec<_>>().join(", ")
}

/// Produce a structural signature key for a method (`NodeKind::FnDecl`) used by
/// the single-method-namespace coherence check (DQ27).
///
/// The key encodes the parameter shape and the return type, *not* the method
/// name. An unannotated receiver (`self`) and an explicit `Self` type both
/// normalize the same way so an inherent `render(self) -> String` and a
/// trait-impl `render(self) -> String` produce identical keys (a true
/// duplicate), while `foo(self) -> Int` and `foo(self) -> String` differ (a
/// genuine signature conflict). The key is deterministic and intended only for
/// equality comparison, not display.
fn method_sig_key(method: &AIRNode) -> String {
    let NodeKind::FnDecl { params, return_type, .. } = &method.kind else {
        return String::new();
    };
    let param_keys: Vec<String> = params
        .iter()
        .map(|p| match &p.kind {
            NodeKind::Param {
                pattern, ty: None, ..
            } => {
                // Unannotated param — keyed by its bound name. The receiver
                // `self` thus normalizes to `"self"` regardless of which block
                // declared the method.
                if let NodeKind::BindPat { name, .. } = &pattern.kind {
                    format!("@{}", name.name)
                } else {
                    "@_".to_owned()
                }
            }
            NodeKind::Param {
                ty: Some(ty_node), ..
            } => type_key_from_node(ty_node),
            _ => "?".to_owned(),
        })
        .collect();
    let ret_key = return_type
        .as_deref()
        .map_or_else(|| "Void".to_owned(), type_key_from_node);
    format!("({}) -> {}", param_keys.join(", "), ret_key)
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

// ─── Canonical primitive conformances (Q-bridge) ────────────────────────────────

/// The set of core-trait names whose conformances for primitive types are
/// *sealed*: user code may not write its own `impl <CoreTrait> for <Primitive>`
/// (orphan-rule violation → `E4011`). The newtype pattern is the escape hatch.
///
/// Scoped strictly to the (core trait, primitive) quadrant — this is **not** a
/// general orphan model.
pub const SEALED_CORE_TRAITS: &[&str] = &["Equatable", "Comparable", "Displayable", "Hashable"];

/// The set of primitive type keys (as produced by [`type_key`]) for which core
/// traits are sealed. Used together with [`SEALED_CORE_TRAITS`] to detect a
/// user `impl <CoreTrait> for <Primitive>`.
pub const SEALED_PRIMITIVE_KEYS: &[&str] = &[
    "Int", "Float", "String", "Bool", "Char", "Int8", "Int16", "Int32", "Int64", "Int128", "UInt8",
    "UInt16", "UInt32", "UInt64", "Float32", "Float64",
];

/// Sized signed/unsigned integer primitives that share `Int`'s conformances.
const SIZED_INTS: &[PrimitiveType] = &[
    PrimitiveType::Int8,
    PrimitiveType::Int16,
    PrimitiveType::Int32,
    PrimitiveType::Int64,
    PrimitiveType::Int128,
    PrimitiveType::UInt8,
    PrimitiveType::UInt16,
    PrimitiveType::UInt32,
    PrimitiveType::UInt64,
];

/// Sized floating-point primitives that share `Float`'s conformances.
const SIZED_FLOATS: &[PrimitiveType] = &[PrimitiveType::Float32, PrimitiveType::Float64];

/// Register the compiler-provided canonical trait conformances for primitive
/// types into `table`.
///
/// These conformances populate the *same* trait-impl index that user `impl`
/// blocks do, so the type checker resolves primitives' trait methods and
/// generic-bound satisfaction uniformly (codegen still lowers primitive
/// operations via the existing intrinsic fast path — no dynamic dispatch).
///
/// Registration uses `ImplTable::register_trait_impl_inner` directly, which
/// bypasses the sealing check applied to user `impl` blocks, so the compiler's
/// own registration is never rejected. Call this **after**
/// [`ImplTable::build_from`] so user-impl sealing runs first.
///
/// The matrix (see the Q-bridge plan; the normative matrix is tracked as
/// DQ10):
/// - `Equatable`:   Int, Float, String, Bool, Char + sized ints/floats
/// - `Comparable`:  Int, Float, String, Char (not Bool) + sized ints/floats
/// - `Displayable`: Int, Float, String, Bool, Char + sized ints/floats
/// - `Hashable`:    Int, String, Bool, Char (not Float — NaN) + sized ints
///
/// Also registers the supertrait edge `Comparable → Equatable` (§18.5).
pub fn register_canonical_conformances(table: &mut ImplTable) {
    // Supertrait obligation: every `Comparable` type is also `Equatable`.
    table.register_supertrait("Comparable", "Equatable");

    // Helper: register `trait_name` for each primitive in `prims`.
    let register = |table: &mut ImplTable, trait_name: &str, prims: &[PrimitiveType]| {
        for p in prims {
            let ty = Type::Primitive(p.clone());
            table.register_trait_impl_inner(trait_name, &[], &ty, true, false);
        }
    };

    // Base scalar sets per trait (sized numerics appended below).
    const EQUATABLE_BASE: &[PrimitiveType] = &[
        PrimitiveType::Int,
        PrimitiveType::Float,
        PrimitiveType::String,
        PrimitiveType::Bool,
        PrimitiveType::Char,
    ];
    const COMPARABLE_BASE: &[PrimitiveType] = &[
        PrimitiveType::Int,
        PrimitiveType::Float,
        PrimitiveType::String,
        PrimitiveType::Char,
    ];
    const DISPLAYABLE_BASE: &[PrimitiveType] = &[
        PrimitiveType::Int,
        PrimitiveType::Float,
        PrimitiveType::String,
        PrimitiveType::Bool,
        PrimitiveType::Char,
    ];
    const HASHABLE_BASE: &[PrimitiveType] = &[
        PrimitiveType::Int,
        PrimitiveType::String,
        PrimitiveType::Bool,
        PrimitiveType::Char,
    ];

    // Equatable: base + sized ints + sized floats.
    register(table, "Equatable", EQUATABLE_BASE);
    register(table, "Equatable", SIZED_INTS);
    register(table, "Equatable", SIZED_FLOATS);

    // Comparable: base (no Bool) + sized ints + sized floats.
    register(table, "Comparable", COMPARABLE_BASE);
    register(table, "Comparable", SIZED_INTS);
    register(table, "Comparable", SIZED_FLOATS);

    // Displayable: base + sized ints + sized floats.
    register(table, "Displayable", DISPLAYABLE_BASE);
    register(table, "Displayable", SIZED_INTS);
    register(table, "Displayable", SIZED_FLOATS);

    // Hashable: base (no Float — NaN breaks the hash/eq law) + sized ints only.
    register(table, "Hashable", HASHABLE_BASE);
    register(table, "Hashable", SIZED_INTS);
}

/// Register the compiler-provided canonical *conversions* between primitive
/// types into `table`.
///
/// These populate the parameterized `From`/`TryFrom` indexes (and, for each
/// `From`, the blanket reverse `Into`) so that `(5).into()` resolving to a
/// `Float`, `Float.from(3)`, and `Int.try_from(s)` all type-check uniformly
/// with user conversions. Call this **after** [`ImplTable::build_from`] and
/// [`register_canonical_conformances`].
///
/// The v1 *minimum-useful* matrix (the normative matrix is escalated to Design,
/// parallel to DQ10):
/// - `From[Int] for Float`
/// - signed-integer widening: each narrower signed int → each wider one, and
///   every sized signed int → the unsized `Int`
/// - `From[Float32] for Float`
/// - `From[Char] for String`
/// - `TryFrom[String] for Int` and `TryFrom[String] for Float`
///
/// **Lossy / narrowing conversions are intentionally excluded from v1** — a
/// narrowing conversion must go through `TryFrom`, or is deferred. These
/// canonical conversions ship **unsealed** (whether to seal them under §18.5
/// is an escalated Design question): user code may currently add its own
/// conversions for primitives.
pub fn register_canonical_conversions(table: &mut ImplTable) {
    // Register `From[source] for target`, plus its blanket reverse
    // `Into[target] for source` (marked derived). Both are canonical-internal
    // registrations that bypass user sealing.
    let from = |table: &mut ImplTable, source: PrimitiveType, target: PrimitiveType| {
        let source_ty = Type::Primitive(source);
        let target_ty = Type::Primitive(target);
        table.register_param_trait_impl(
            "From",
            std::slice::from_ref(&source_ty),
            &target_ty,
            false,
        );
        // Blanket reverse: Into[target] for source.
        table.register_param_trait_impl("Into", std::slice::from_ref(&target_ty), &source_ty, true);
    };

    // From[Int] for Float.
    from(table, PrimitiveType::Int, PrimitiveType::Float);

    // From[Float32] for Float.
    from(table, PrimitiveType::Float32, PrimitiveType::Float);

    // From[Char] for String.
    from(table, PrimitiveType::Char, PrimitiveType::String);

    // Signed-integer widening: narrower → wider, in size order, plus every
    // sized signed int → the unsized `Int`. Widening is always lossless.
    const SIGNED_WIDENING: &[PrimitiveType] = &[
        PrimitiveType::Int8,
        PrimitiveType::Int16,
        PrimitiveType::Int32,
        PrimitiveType::Int64,
        PrimitiveType::Int128,
    ];
    for (i, narrow) in SIGNED_WIDENING.iter().enumerate() {
        for wide in &SIGNED_WIDENING[i + 1..] {
            from(table, narrow.clone(), wide.clone());
        }
        // Every sized signed int widens into the unsized `Int`.
        from(table, narrow.clone(), PrimitiveType::Int);
    }

    // Fallible parsing conversions: TryFrom[String] for Int / for Float. These
    // are NOT blanket-reversed (no TryInto in v1).
    let string_ty = Type::Primitive(PrimitiveType::String);
    table.register_param_trait_impl(
        "TryFrom",
        std::slice::from_ref(&string_ty),
        &Type::Primitive(PrimitiveType::Int),
        false,
    );
    table.register_param_trait_impl(
        "TryFrom",
        std::slice::from_ref(&string_ty),
        &Type::Primitive(PrimitiveType::Float),
        false,
    );
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
        make_fn_decl_ret(name, None)
    }

    /// Like [`make_fn_decl`] but with an explicit return-type name, so the
    /// single-method-namespace check can distinguish matching vs conflicting
    /// signatures.
    fn make_fn_decl_ret(name: &str, ret: Option<&str>) -> AIRNode {
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
            return_type: ret.map(|r| Box::new(make_type_named(r))),
            effect_clause: vec![],
            where_clause: vec![],
            body: Box::new(body),
        })
    }

    /// Build `class Name { methods }` (fields omitted — irrelevant to the
    /// method-namespace check).
    fn make_class_decl(name: &str, methods: Vec<AIRNode>) -> AIRNode {
        use bock_ast::{Ident, Visibility};
        make_air_node(NodeKind::ClassDecl {
            annotations: vec![],
            visibility: Visibility::Private,
            name: Ident {
                name: name.to_owned(),
                span: dummy_span(),
            },
            generic_params: vec![],
            base: None,
            traits: vec![],
            fields: vec![],
            methods,
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
            trait_args: vec![],
            target: Box::new(make_type_named(target_name)),
            where_clause: vec![],
            methods,
        })
    }

    /// Build `impl Trait[arg_names...] for Target { methods }`.
    fn make_param_impl_block(
        trait_name: &str,
        trait_arg_names: &[&str],
        target_name: &str,
        methods: Vec<AIRNode>,
    ) -> AIRNode {
        use bock_ast::{Ident, TypePath};
        let trait_path = Some(TypePath {
            segments: vec![Ident {
                name: trait_name.to_owned(),
                span: dummy_span(),
            }],
            span: dummy_span(),
        });
        let trait_args = trait_arg_names
            .iter()
            .map(|n| make_type_named(n))
            .collect::<Vec<_>>();
        make_air_node(NodeKind::ImplBlock {
            annotations: vec![],
            generic_params: vec![],
            trait_path,
            trait_args,
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
                is_canonical: false,
                trait_args: vec![],
                is_derived: false,
                target_type: None,
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
                is_canonical: false,
                trait_args: vec![],
                is_derived: false,
                target_type: None,
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
        // Same trait on two *distinct* (user) types is not an overlap. Uses
        // user types (`Point`/`Line`) rather than primitives so the (now
        // sealed) core-trait-for-primitive rule does not apply.
        let impl1 = make_impl_block(Some("Equatable"), "Point", vec![make_fn_decl("equals")]);
        let impl2 = make_impl_block(Some("Equatable"), "Line", vec![make_fn_decl("equals")]);
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

    // ── DQ27 single-method-namespace coherence (E4012) ─────────────────────────

    /// Helper: count diagnostics carrying the `E4012` duplicate-method code.
    fn count_e4012(table: &ImplTable) -> usize {
        table
            .diags
            .iter()
            .filter(|d| d.code == E_DUPLICATE_METHOD)
            .count()
    }

    #[test]
    fn namespace_rejects_inherent_and_trait_same_method() {
        // The react-components collision: an inherent `render` plus a trait-impl
        // `render` for the same type defines `render` twice in one namespace.
        let inherent = make_impl_block(
            None,
            "Button",
            vec![make_fn_decl_ret("render", Some("String"))],
        );
        let trait_impl = make_impl_block(
            Some("Component"),
            "Button",
            vec![make_fn_decl_ret("render", Some("String"))],
        );
        let module = make_module(vec![inherent, trait_impl]);
        let table = ImplTable::build_from(&module);

        assert!(table.diags.has_errors());
        assert_eq!(count_e4012(&table), 1);
    }

    #[test]
    fn namespace_allows_empty_trait_impl_satisfied_by_inherent() {
        // An inherent `render` plus an EMPTY `impl Component for Button {}` is
        // well-formed: the inherent method satisfies the requirement, and
        // `render` is defined exactly once.
        let inherent = make_impl_block(
            None,
            "Button",
            vec![make_fn_decl_ret("render", Some("String"))],
        );
        let trait_impl = make_impl_block(Some("Component"), "Button", vec![]);
        let module = make_module(vec![inherent, trait_impl]);
        let table = ImplTable::build_from(&module);

        assert!(!table.diags.has_errors());
        assert_eq!(count_e4012(&table), 0);
    }

    #[test]
    fn namespace_allows_distinct_method_names() {
        // Inherent `click` + trait-impl `render` are distinct names — no clash.
        let inherent = make_impl_block(None, "Button", vec![make_fn_decl("click")]);
        let trait_impl = make_impl_block(
            Some("Component"),
            "Button",
            vec![make_fn_decl_ret("render", Some("String"))],
        );
        let module = make_module(vec![inherent, trait_impl]);
        let table = ImplTable::build_from(&module);

        assert!(!table.diags.has_errors());
        assert_eq!(count_e4012(&table), 0);
    }

    #[test]
    fn namespace_rejects_conflicting_signatures_across_traits() {
        // Two traits both requiring `foo` for the same type, with incompatible
        // return types, are unsatisfiable on v1 targets (one slot per name).
        let impl_a = make_impl_block(
            Some("TraitA"),
            "Widget",
            vec![make_fn_decl_ret("foo", Some("Int"))],
        );
        let impl_b = make_impl_block(
            Some("TraitB"),
            "Widget",
            vec![make_fn_decl_ret("foo", Some("String"))],
        );
        let module = make_module(vec![impl_a, impl_b]);
        let table = ImplTable::build_from(&module);

        assert!(table.diags.has_errors());
        assert_eq!(count_e4012(&table), 1);
    }

    #[test]
    fn namespace_rejects_class_body_and_trait_same_method() {
        // A class-body `render` plus a trait-impl `render` is also a duplicate:
        // class-body methods share the type's single namespace.
        let class = make_class_decl("Button", vec![make_fn_decl_ret("render", Some("String"))]);
        let trait_impl = make_impl_block(
            Some("Component"),
            "Button",
            vec![make_fn_decl_ret("render", Some("String"))],
        );
        let module = make_module(vec![class, trait_impl]);
        let table = ImplTable::build_from(&module);

        assert!(table.diags.has_errors());
        assert_eq!(count_e4012(&table), 1);
    }

    #[test]
    fn namespace_allows_class_body_satisfying_trait() {
        // A class-body `render` plus an EMPTY `impl Component for Button {}` is
        // well-formed (mirrors §6.4's `class Button : Component { fn render }`).
        let class = make_class_decl("Button", vec![make_fn_decl_ret("render", Some("String"))]);
        let trait_impl = make_impl_block(Some("Component"), "Button", vec![]);
        let module = make_module(vec![class, trait_impl]);
        let table = ImplTable::build_from(&module);

        assert!(!table.diags.has_errors());
        assert_eq!(count_e4012(&table), 0);
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
            trait_args: vec![],
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
            trait_args: vec![],
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

    // ── Canonical primitive conformances (Q-bridge) ────────────────────────────

    fn float() -> Type {
        Type::Primitive(PrimitiveType::Float)
    }

    fn string() -> Type {
        Type::Primitive(PrimitiveType::String)
    }

    fn char_ty() -> Type {
        Type::Primitive(PrimitiveType::Char)
    }

    #[test]
    fn canonical_comparable_int_is_registered() {
        let mut table = ImplTable::new();
        register_canonical_conformances(&mut table);
        assert!(resolve_impl(&TraitRef::new("Comparable"), &int(), &table).is_some());
    }

    #[test]
    fn canonical_equatable_covers_expected_primitives() {
        let mut table = ImplTable::new();
        register_canonical_conformances(&mut table);
        for ty in [int(), float(), string(), bool_ty(), char_ty()] {
            assert!(
                resolve_impl(&TraitRef::new("Equatable"), &ty, &table).is_some(),
                "Equatable should cover {ty:?}"
            );
        }
    }

    #[test]
    fn canonical_comparable_excludes_bool() {
        let mut table = ImplTable::new();
        register_canonical_conformances(&mut table);
        // Bool is Equatable but intentionally NOT Comparable.
        assert!(resolve_impl(&TraitRef::new("Equatable"), &bool_ty(), &table).is_some());
        assert!(resolve_impl(&TraitRef::new("Comparable"), &bool_ty(), &table).is_none());
    }

    #[test]
    fn canonical_hashable_excludes_float() {
        let mut table = ImplTable::new();
        register_canonical_conformances(&mut table);
        // Float is Equatable/Comparable but NOT Hashable (NaN breaks hash/eq).
        assert!(resolve_impl(&TraitRef::new("Equatable"), &float(), &table).is_some());
        assert!(resolve_impl(&TraitRef::new("Hashable"), &float(), &table).is_none());
        // Int is Hashable.
        assert!(resolve_impl(&TraitRef::new("Hashable"), &int(), &table).is_some());
    }

    #[test]
    fn canonical_covers_sized_numerics() {
        let mut table = ImplTable::new();
        register_canonical_conformances(&mut table);
        let i32_ty = Type::Primitive(PrimitiveType::Int32);
        let u64_ty = Type::Primitive(PrimitiveType::UInt64);
        let f32_ty = Type::Primitive(PrimitiveType::Float32);
        assert!(resolve_impl(&TraitRef::new("Comparable"), &i32_ty, &table).is_some());
        assert!(resolve_impl(&TraitRef::new("Equatable"), &u64_ty, &table).is_some());
        assert!(resolve_impl(&TraitRef::new("Comparable"), &f32_ty, &table).is_some());
        // Sized float is not Hashable, matching Float.
        assert!(resolve_impl(&TraitRef::new("Hashable"), &f32_ty, &table).is_none());
        // Sized int IS Hashable.
        assert!(resolve_impl(&TraitRef::new("Hashable"), &i32_ty, &table).is_some());
    }

    #[test]
    fn canonical_entries_are_marked_canonical() {
        let mut table = ImplTable::new();
        register_canonical_conformances(&mut table);
        let id = resolve_impl(&TraitRef::new("Comparable"), &int(), &table).unwrap();
        assert!(table.get_entry(id).unwrap().is_canonical);
    }

    #[test]
    fn canonical_registers_comparable_equatable_supertrait() {
        let mut table = ImplTable::new();
        register_canonical_conformances(&mut table);
        assert_eq!(table.all_supertraits("Comparable"), vec!["Equatable"]);
    }

    #[test]
    fn user_register_trait_impl_is_not_canonical() {
        let mut table = ImplTable::new();
        let id = table.register_trait_impl("MyTrait", &named("User"));
        assert!(!table.get_entry(id).unwrap().is_canonical);
    }

    // ── Sealing: user `impl <CoreTrait> for <Primitive>` (Q1b / E4011) ─────────

    #[test]
    fn sealing_rejects_user_impl_core_trait_for_primitive() {
        // `impl Comparable for Int` in user code must be rejected (E4011).
        let method = make_fn_decl("compare");
        let impl_block = make_impl_block(Some("Comparable"), "Int", vec![method]);
        let module = make_module(vec![impl_block]);
        let table = ImplTable::build_from(&module);

        assert!(table.diags.has_errors());
        assert_eq!(table.diags.error_count(), 1);
        let diag = table.diags.iter().next().unwrap();
        assert_eq!(diag.code, E_SEALED_PRIMITIVE_IMPL);
        // The offending impl must NOT have been registered.
        assert!(resolve_impl(&TraitRef::new("Comparable"), &int(), &table).is_none());
        // A newtype help note is attached.
        assert!(diag.notes.iter().any(|n| n.contains("newtype")));
    }

    #[test]
    fn sealing_rejects_each_sealed_core_trait_for_primitive() {
        for trait_name in ["Equatable", "Comparable", "Displayable", "Hashable"] {
            let impl_block = make_impl_block(Some(trait_name), "String", vec![make_fn_decl("m")]);
            let module = make_module(vec![impl_block]);
            let table = ImplTable::build_from(&module);
            assert!(
                table.diags.has_errors(),
                "{trait_name} for String should be sealed"
            );
        }
    }

    #[test]
    fn sealing_allows_user_impl_core_trait_for_newtype() {
        // Positive control: `impl Comparable for MyNewtype` is fine.
        let method = make_fn_decl("compare");
        let impl_block = make_impl_block(Some("Comparable"), "MyNewtype", vec![method]);
        let module = make_module(vec![impl_block]);
        let table = ImplTable::build_from(&module);

        assert!(!table.diags.has_errors());
        assert!(resolve_impl(&TraitRef::new("Comparable"), &named("MyNewtype"), &table).is_some());
    }

    #[test]
    fn sealing_allows_user_impl_noncore_trait_for_primitive() {
        // A non-core trait for a primitive is out of scope of the seal — the
        // seal is strictly the (core trait, primitive) quadrant.
        let impl_block = make_impl_block(Some("MyTrait"), "Int", vec![make_fn_decl("m")]);
        let module = make_module(vec![impl_block]);
        let table = ImplTable::build_from(&module);
        assert!(!table.diags.has_errors());
    }

    #[test]
    fn canonical_registration_bypasses_sealing() {
        // The compiler's own canonical conformances use
        // `register_trait_impl_inner` and must NOT trip the seal even though
        // they are (core trait, primitive) pairs.
        let mut table = ImplTable::new();
        register_canonical_conformances(&mut table);
        assert!(!table.diags.has_errors());
        assert!(resolve_impl(&TraitRef::new("Comparable"), &int(), &table).is_some());
    }

    // ── Parameterized-trait resolution (T2/T3) ─────────────────────────────────

    #[test]
    fn param_impl_distinct_args_resolve_independently() {
        // impl From[Int] for Float  and  impl From[String] for Float
        // must resolve to different impls — no false collision.
        let from_int = make_param_impl_block("From", &["Int"], "Float", vec![make_fn_decl("from")]);
        let from_str =
            make_param_impl_block("From", &["String"], "Float", vec![make_fn_decl("from")]);
        let module = make_module(vec![from_int, from_str]);
        let table = ImplTable::build_from(&module);

        assert!(
            !table.diags.has_errors(),
            "distinct trait args must not collide"
        );
        let float = Type::Primitive(PrimitiveType::Float);
        let id_int = resolve_impl(
            &TraitRef::parameterized("From", vec![int()]),
            &float,
            &table,
        );
        let id_str = resolve_impl(
            &TraitRef::parameterized("From", vec![Type::Primitive(PrimitiveType::String)]),
            &float,
            &table,
        );
        assert!(id_int.is_some(), "From[Int] for Float should resolve");
        assert!(id_str.is_some(), "From[String] for Float should resolve");
        assert_ne!(id_int, id_str, "the two impls must be distinct");
    }

    #[test]
    fn param_impl_missing_arg_does_not_resolve() {
        // Only From[Int] for Float is registered; From[Bool] for Float is not.
        let from_int = make_param_impl_block("From", &["Int"], "Float", vec![make_fn_decl("from")]);
        let module = make_module(vec![from_int]);
        let table = ImplTable::build_from(&module);
        let float = Type::Primitive(PrimitiveType::Float);
        assert!(resolve_impl(
            &TraitRef::parameterized("From", vec![bool_ty()]),
            &float,
            &table
        )
        .is_none());
    }

    #[test]
    fn param_impl_duplicate_args_collide() {
        // Two identical impl From[Int] for Float → E4010 coherence error.
        let a = make_param_impl_block("From", &["Int"], "Float", vec![make_fn_decl("from")]);
        let b = make_param_impl_block("From", &["Int"], "Float", vec![make_fn_decl("from")]);
        let module = make_module(vec![a, b]);
        let table = ImplTable::build_from(&module);
        assert!(
            table.diags.has_errors(),
            "duplicate From[Int] for Float must be a coherence error"
        );
    }

    #[test]
    fn param_and_nonparam_indexes_are_independent() {
        // A non-parameterized impl and a parameterized impl of the same trait
        // name for the same target type live in separate indexes and do not
        // collide. (Edge case; primarily a sanity check on index isolation.)
        let bare = make_impl_block(Some("From"), "Float", vec![make_fn_decl("from")]);
        let param = make_param_impl_block("From", &["Int"], "Float", vec![make_fn_decl("from")]);
        let module = make_module(vec![bare, param]);
        let table = ImplTable::build_from(&module);
        assert!(!table.diags.has_errors());
        let float = Type::Primitive(PrimitiveType::Float);
        assert!(resolve_impl(&TraitRef::new("From"), &float, &table).is_some());
        assert!(resolve_impl(
            &TraitRef::parameterized("From", vec![int()]),
            &float,
            &table
        )
        .is_some());
    }

    // ── Blanket From ⇒ Into synthesis (T4) ─────────────────────────────────────

    #[test]
    fn blanket_into_derived_from_explicit_from() {
        // impl From[Int] for Float  ⇒  derived impl Into[Float] for Int.
        let from = make_param_impl_block("From", &["Int"], "Float", vec![make_fn_decl("from")]);
        let module = make_module(vec![from]);
        let table = ImplTable::build_from(&module);
        assert!(!table.diags.has_errors());

        let float = Type::Primitive(PrimitiveType::Float);
        // Into[Float] for Int must resolve.
        let into_id = resolve_impl(
            &TraitRef::parameterized("Into", vec![float.clone()]),
            &int(),
            &table,
        );
        assert!(
            into_id.is_some(),
            "blanket Into[Float] for Int should resolve"
        );
        let entry = table.get_entry(into_id.unwrap()).unwrap();
        assert!(
            entry.is_derived,
            "synthesized Into entry must be is_derived"
        );
    }

    #[test]
    fn blanket_into_does_not_clobber_explicit() {
        // Explicit  impl Into[U] for A  plus  impl From[A] for U  must not
        // produce E4010, and resolution must return the EXPLICIT Into.
        let explicit_into =
            make_param_impl_block("Into", &["Float"], "Apple", vec![make_fn_decl("into")]);
        let from = make_param_impl_block("From", &["Apple"], "Float", vec![make_fn_decl("from")]);
        let module = make_module(vec![explicit_into, from]);
        let table = ImplTable::build_from(&module);

        assert!(
            !table.diags.has_errors(),
            "explicit Into + blanket From must not collide"
        );
        let float = Type::Primitive(PrimitiveType::Float);
        let apple = Type::Named(NamedType {
            name: "Apple".to_owned(),
        });
        let into_id = resolve_impl(
            &TraitRef::parameterized("Into", vec![float]),
            &apple,
            &table,
        )
        .expect("Into[Float] for Apple should resolve");
        let entry = table.get_entry(into_id).unwrap();
        assert!(
            !entry.is_derived,
            "explicit Into must win over the blanket-derived one"
        );
    }

    // ── Canonical primitive conversions (T8) ───────────────────────────────────

    #[test]
    fn canonical_conversions_register_from_int_for_float() {
        let mut table = ImplTable::new();
        register_canonical_conversions(&mut table);
        let float = Type::Primitive(PrimitiveType::Float);
        // From[Int] for Float resolves.
        assert!(resolve_impl(
            &TraitRef::parameterized("From", vec![int()]),
            &float,
            &table
        )
        .is_some());
        // Blanket Into[Float] for Int resolves.
        assert!(resolve_impl(
            &TraitRef::parameterized("Into", vec![float]),
            &int(),
            &table
        )
        .is_some());
    }

    #[test]
    fn canonical_conversions_widen_signed_ints() {
        let mut table = ImplTable::new();
        register_canonical_conversions(&mut table);
        // Int8 -> Int64 (widening) and Int8 -> Int.
        let i8 = Type::Primitive(PrimitiveType::Int8);
        let i64 = Type::Primitive(PrimitiveType::Int64);
        assert!(resolve_impl(
            &TraitRef::parameterized("From", vec![i8.clone()]),
            &i64,
            &table
        )
        .is_some());
        assert!(resolve_impl(
            &TraitRef::parameterized("From", vec![i8.clone()]),
            &int(),
            &table
        )
        .is_some());
        // Narrowing Int64 -> Int8 is NOT registered (lossy, excluded from v1).
        assert!(resolve_impl(&TraitRef::parameterized("From", vec![i64]), &i8, &table).is_none());
    }

    #[test]
    fn canonical_conversions_try_from_string() {
        let mut table = ImplTable::new();
        register_canonical_conversions(&mut table);
        let string = Type::Primitive(PrimitiveType::String);
        assert!(resolve_impl(
            &TraitRef::parameterized("TryFrom", vec![string.clone()]),
            &int(),
            &table
        )
        .is_some());
        assert!(resolve_impl(
            &TraitRef::parameterized("TryFrom", vec![string]),
            &Type::Primitive(PrimitiveType::Float),
            &table
        )
        .is_some());
    }

    #[test]
    fn blanket_into_not_derived_for_try_from() {
        // TryFrom is intentionally NOT blanket-reversed (no TryInto in v1).
        let tryfrom = make_param_impl_block(
            "TryFrom",
            &["String"],
            "Int",
            vec![make_fn_decl("try_from")],
        );
        let module = make_module(vec![tryfrom]);
        let table = ImplTable::build_from(&module);
        let string = Type::Primitive(PrimitiveType::String);
        assert!(
            resolve_impl(
                &TraitRef::parameterized("TryInto", vec![int()]),
                &string,
                &table
            )
            .is_none(),
            "no TryInto should be synthesized"
        );
        assert!(
            resolve_impl(
                &TraitRef::parameterized("Into", vec![int()]),
                &string,
                &table
            )
            .is_none(),
            "TryFrom must not synthesize a (lossless) Into"
        );
    }
}
