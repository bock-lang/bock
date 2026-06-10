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

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU32, Ordering};

use bock_air::stubs::{TypeInfo, Value};
use bock_air::{AIRNode, EnumVariantPayload, NodeId, NodeKind};
use bock_ast::{BinOp, GenericParam, Literal, TypeConstraint, TypeExpr, TypePath, UnaryOp};
use bock_errors::{DiagnosticBag, DiagnosticCode, Span};

use crate::traits::{resolve_impl, ImplTable, TraitRef};
use crate::{
    unify, EffectRef, FnType, GenericType, PrimitiveType, Substitution, Type, TypeError, TypeVarId,
};

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
/// `E4015` — an `==`/`!=` operand (or an `Equatable` bound instantiation) is
/// not `Equatable` (DQ29, §18.5). Records/enums conform structurally iff every
/// field / variant payload type conforms; compound built-ins compose
/// conditionally; classes are excluded (explicit `impl Equatable` only); a
/// non-Equatable leaf (e.g. an `Fn` field) poisons the whole type. The message
/// names the offending field path and type; the note suggests the fix
/// (`impl Equatable for <T>` or removing the comparison). Sibling of the
/// `Comparable` ordering-operator gate, which reuses `E4005`.
const E_NOT_EQUATABLE: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 4015,
};
/// `E4012` — a `.into()` call (or `from`/`try_from`) could not be resolved:
/// no `From`/`Into`/`TryFrom` impl exists for the required source and target
/// types. For `.into()` the target is taken from the expected type, so the
/// call site must have a reachable expected type (a `let y: U =`, an `fn -> U`
/// return position, or an argument to a typed parameter); see the v1
/// annotation-required limitation in the `core.convert` docs.
const E_NO_CONVERSION: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 4012,
};
/// `E4013` — a method that does not exist on the receiver's **concrete** type
/// was called. This is the general "no such method" error: when the receiver
/// resolves to a fully-known type (a primitive, a built-in collection, an
/// `Optional`/`Result`, or a user record/class/enum whose definition is in
/// scope) and the method is in none of that type's method sets (intrinsic,
/// canonical-trait, inherent-impl, trait-impl, or bounded-trait), the call is
/// rejected instead of being silently resolved to a fresh type variable (which
/// would pass `bock check` yet emit no/garbage codegen — the DQ22 soundness
/// hole). When a near-miss method name exists the diagnostic carries a "did you
/// mean `…`?" suggestion (e.g. DQ22's `m.contains(k)` → `contains_key`).
///
/// The error is deliberately NOT raised for non-concrete receivers — inference
/// variables (`Type::TypeVar`), §4.9 `Flexible`/sketch-mode types, the `Error`
/// poison sentinel, function/tuple receivers, or user types whose definition
/// is not in scope — so aggressive sketch-mode narrowing keeps resolving
/// methods by design.
const E_NO_SUCH_METHOD: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 4013,
};
/// `E4014` — a `use` declaration named a module path with **neither a
/// brace-list nor a wildcard** (a bare `use core.error`). Per §12.2 / DQ8 this
/// is not a v1 import form; module-qualified access is deferred to v1.x. The
/// checker rejects it and points at the braced form (`use core.error.{…}`).
const E_BARE_MODULE_IMPORT: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 4014,
};
/// `E6006` — the lambda-handler surface `Effect.handler(...)` is **reserved
/// until v1.x** (§10.4). v1 supports exactly one handler form: a record with
/// an `impl <Effect> for <Record>`, installed via `handle <Effect> with
/// <record>` (module level) or `handling (<Effect> with <record>) { ... }`
/// (block level). Before this code existed the form surfaced as a doubled,
/// rule-less `E4002 undefined variable` at the effect name
/// (Q-diag-effect-violation-errors); it now names the actual rule. Lives in
/// the `6xxx` effects family because the violated rule is an effect-system
/// rule, even though the emitting pass is the type checker.
const E_RESERVED_LAMBDA_HANDLER: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 6006,
};

// ─── Receiver-type annotation (checker → codegen) ──────────────────────────────

/// AIR metadata key under which the checker stamps a method call's *receiver
/// type category* so codegen can lower receiver-dependent calls without
/// re-deriving the type.
///
/// The checker resolves a method call's receiver type during inference but
/// then drops its internal type side-table; codegen sees only the structural
/// AIR. A bare `(1).compare(2)` and `opt.unwrap_or(d)` are indistinguishable
/// to a backend without this hint — both desugar to
/// `Call(FieldAccess(recv, method), [recv, …])`, and the method names overlap
/// across `Optional`/`Result`/`List`. This key carries the resolved receiver
/// category from the checker's method-resolution sites to codegen.
///
/// The value is a [`Value::String`] produced by [`recv_kind_tag`]; see that
/// function for the tag grammar. The tag is stamped on the *method-call node*
/// (the desugared `Call`, or a `MethodCall` that survives to T-AIR), not on the
/// receiver — so a backend reads `call_node.metadata[RECV_KIND_META_KEY]`.
pub const RECV_KIND_META_KEY: &str = "recv_kind";

/// Metadata key stamped on a `BinaryOp { op: Add, .. }` node whose two operands
/// resolved to `List[T]`, marking the `+` as **list concatenation** rather than
/// numeric/string addition.
///
/// Codegen reads this (a `Value::Bool(true)`) to lower the `+` to each target's
/// list-concat idiom (`[...a, ...b]` on JS/TS, a clone-and-extend on Rust, an
/// `append(append(...), ...)` helper on Go, native `+` on Python where list `+`
/// is already concatenation). Without it the operator falls through to the native
/// `+`, which fails to compile on TS/Rust/Go and silently *string*-concatenates
/// on JS. The element type is intentionally not recorded — every target's concat
/// is element-type-agnostic.
pub const LIST_CONCAT_META_KEY: &str = "list_concat";

/// Metadata key stamped on a `+` `BinaryOp` node whose operands resolve to
/// `String`, marking it as string concatenation. Bock's `+` on strings is
/// concatenation, but Rust's `String + String` does not compile (`Add<String>`
/// is unimplemented — only `String + &str`), so the Rust backend reads this stamp
/// to lower the operator to `format!("{}{}", l, r)` regardless of whether each
/// side is an owned `String` or a `&str`. A purely syntactic check in codegen
/// cannot see that a bare identifier/parameter (`result + sep`) is `String`-typed,
/// so the type-aware checker records it here. The other backends concatenate
/// strings with `+` natively and ignore this key.
pub const STRING_CONCAT_META_KEY: &str = "string_concat";

/// Metadata key stamped on a `BinaryOp { op: Div | Rem, .. }` node whose two
/// operands both resolve to an **integer** primitive (`Int`, the sized
/// `Int8`…`Int128` / `UInt8`…`UInt64`). It marks the `/` or `%` as *integer*
/// division / remainder — distinct from float (true) division — so codegen can
/// lower it to the cross-target integer semantics fixed by DQ23 (§3.6):
///
/// - **`/` truncates toward zero** (`-17 / 5 == -3`, not the floor `-4`), yields
///   `Int`, and **aborts on a zero divisor** (a Panic ambient effect, §10.5).
/// - **`%` is the remainder of that truncated division**, taking the sign of the
///   *dividend* (`-17 % 5 == -2`, `17 % -5 == 2`), and likewise aborts on zero.
///
/// Rust and Go already match this with native `/` / `%`, so their backends ignore
/// the stamp. JS/TS need `Math.trunc` plus an explicit zero-abort (JS `/` is float
/// division and `Math.trunc(a/0)` yields `Infinity`, not a throw). Python needs an
/// integer-only toward-zero helper (its `//` *floors* and `int(a/b)` routes
/// through lossy float division) and a dividend-sign `%` helper (its `%` follows
/// floor division). A purely syntactic codegen check cannot see that bare
/// identifiers (`a / b`) are integer-typed, so the type-aware checker records it
/// here. The value is a `Value::Bool(true)`.
pub const INT_ARITH_META_KEY: &str = "int_arith";

/// Metadata key stamped on an expression node whose resolved type is `Bool` and
/// whose value is about to be *stringified* — an `${expr}` interpolation part or
/// the receiver of a `.to_string()` / `.display()` call. It tells codegen to emit
/// the **canonical lowercase spelling** `"true"` / `"false"` (§3.5), matching the
/// Bool literals, rather than the target's native default.
///
/// JS/TS template literals and Rust/Go formatting already print lowercase, so
/// those backends ignore the stamp. Python is the outlier: `f"{b}"` and `str(b)`
/// print the capitalized `True` / `False`, so the Python backend reads this stamp
/// to map the value through a lowercase conversion. The interpolation expression
/// part's resolved type is not otherwise reachable from codegen (it lives only in
/// the dropped type side-table), so the checker records it on the node directly.
/// The value is a `Value::Bool(true)`.
pub const BOOL_STRINGIFY_META_KEY: &str = "bool_stringify";

/// Metadata key stamped on a `BinaryOp { op: Lt | Le | Gt | Ge, .. }` node whose
/// two operands resolve to a **user** (`Named` record / class) type that
/// implements `Comparable` (§18.5). It marks the ordering operator as a
/// *user-type* comparison that must be lowered through the type's
/// `compare(self, other) -> Ordering` method rather than the target's native
/// `<` / `<=` / `>` / `>=`:
///
/// - `a < b`  ⇒ `a.compare(b) == Less`
/// - `a > b`  ⇒ `a.compare(b) == Greater`
/// - `a <= b` ⇒ `a.compare(b) != Greater`
/// - `a >= b` ⇒ `a.compare(b) != Less`
///
/// Every backend lowers native `<` on two user values to a broken form — Python
/// raises `TypeError`, Rust/Go fail to compile (structs are not ordered), and JS
/// coerces the objects to `NaN` and silently yields `false`. Reusing the
/// per-target `Ordering` representation the stdlib already emits (`{ _tag: … }`
/// in JS/TS, the `_bock_*` singletons in Python, the `Ordering::*` variants in
/// Rust, the `Ordering*` structs in Go) keeps the lowering aligned with how a
/// hand-written `a.compare(b)` call already lowers.
///
/// The checker is the only pass that can see that two bare identifiers
/// (`a < b`) are a user `Comparable` type, so it records the marker here; codegen
/// reads the operator off the node itself. The value is a `Value::Bool(true)`.
/// Only the *ordering* operators are stamped — `==` / `!=` (Equatable) are a
/// separate lane and are never stamped here. Primitive comparisons and bounded
/// generic (`T: Comparable`) comparisons are likewise untouched: they already
/// lower correctly through the native operator / the trait-bound bridge.
pub const USER_COMPARE_META_KEY: &str = "user_compare";

/// Metadata key stamped on a `BinaryOp { op: Eq | Ne, .. }` node whose operands
/// need a non-native equality lowering on at least one target (DQ29, §18.5
/// structural Equatable). The value is a `Value::String` naming the lane:
///
/// - **`"impl"`** — the operand is a user (`Named`) type with an **explicit**
///   `impl Equatable`. Every backend must dispatch `==`/`!=` through the
///   impl's `eq(self, other) -> Bool` (negated for `!=`): native equality is
///   reference identity on JS/TS, field-wise on Python/Go, and a compile error
///   on Rust — none of which honor the user's `eq`
///   (Q-js-user-equality-reference / #339 and siblings).
/// - **`"structural"`** — the operand is a structurally-Equatable shape
///   (record / enum / tuple) containing no collection. Targets whose native
///   `==` is already field-wise (Python dataclasses, Go structs/interfaces,
///   Rust with the [`DERIVE_EQ_META_KEY`] derive) keep the native operator;
///   JS/TS lower through the `__bockEq` deep-equality runtime helper because
///   `===` on two objects is reference identity.
/// - **`"deep"`** — the operand (transitively) involves a `List`/`Map`/`Set`
///   (or `Optional`/`Result` wrapper). Same JS/TS lowering as `"structural"`;
///   Go must additionally route through its deep-equality runtime helper
///   (slices and maps do not support `==` at all — a compile error), with
///   `Map`/`Set` equality required to be order-independent.
/// - **`"generic"`** — the operand is an unsolved type variable carrying an
///   `Equatable` (or `Comparable`, via the supertrait edge) bound inside a
///   generic fn body. JS/TS lower through `__bockEq` (the concrete
///   instantiation may be a record); the other targets' native equality is
///   correct under their bound mapping (`PartialEq` on Rust, `comparable` on
///   Go, duck-typed `==` on Python).
///
/// Primitive operands are never stamped — every target's native `==` is
/// correct for them (including the IEEE `NaN != NaN` Float semantics, which
/// the structural lanes inherit per the DQ10 caveat).
pub const USER_EQ_META_KEY: &str = "user_eq";

/// Metadata key stamped on a `RecordDecl` / `EnumDecl` node that conforms to
/// `Equatable` **structurally** (DQ29, §18.5): every field / variant payload
/// type is Equatable and the type declares no explicit `impl Equatable` (the
/// explicit impl suppresses the structural default, and its `==` routes through
/// `eq` instead — see [`USER_EQ_META_KEY`]).
///
/// Consumed by the Rust backend, which adds `PartialEq` to the type's
/// `#[derive(..)]` list so native `==`/`!=` (and containment like
/// `Vec<T> == Vec<T>`) compile. For a generic record/enum the derive's
/// conditional `where` bounds implement rule 4 (a `Pair[A, B]` instantiation
/// is Equatable iff `A` and `B` are) natively. A type that is **not**
/// structurally Equatable (e.g. an `Fn` field) is left underivable — the
/// checker's operator gate already rejects every `==` over it, so the emitted
/// code never needs `PartialEq`. The value is a `Value::Bool(true)`.
pub const DERIVE_EQ_META_KEY: &str = "derive_structural_eq";

/// Base node id for AIR nodes the checker synthesizes (the `for`-over-`Iterable`
/// desugar). Chosen high enough to sit far above the dense, zero-based ids the
/// lowerer assigns to real nodes, so synthesized nodes never collide with real
/// ones for the `id`-equality checks codegen performs. A `u32` leaves ample
/// headroom above this base for the handful of nodes one module's `for` loops
/// expand to.
const SYNTH_ID_BASE: NodeId = 0x4000_0000;

/// One enum variant's payload entry in `TypeChecker::enum_variant_payloads`
/// (DQ29): the variant name and its `(component_label, type)` list — field
/// names for struct variants, `_0`-style indices for tuple payloads, empty
/// for unit variants.
type EnumVariantPayloadTypes = (String, Vec<(String, Type)>);

/// Compute the receiver-kind tag for a resolved receiver [`Type`].
///
/// The tag is a compact, codegen-facing string identifying which *family* the
/// receiver belongs to, so a backend can pick the right lowering for an
/// overloaded method name (`compare`/`eq`/`unwrap_or`/`is_ok`/…):
///
/// | Receiver type            | Tag                |
/// |--------------------------|--------------------|
/// | `Type::Primitive(Int)`   | `"Primitive:Int"`  |
/// | `Type::Primitive(Float)` | `"Primitive:Float"`|
/// | `Type::Optional(_)`      | `"Optional"`       |
/// | `Type::Result(_, _)`     | `"Result"`         |
/// | `List[T]`                | `"List"`           |
/// | `Set[T]` / `Map[K,V]`    | `"Set"` / `"Map"`  |
/// | other `Generic`          | `"Generic:<ctor>"` |
/// | `Named(n)`               | `"User:<n>"`       |
///
/// Returns `None` for type-inference variables, function types, and other
/// receivers a backend never needs to special-case — leaving the call to its
/// existing structural lowering.
///
/// One further tag exists that this function cannot produce because it needs
/// the bounds table: a *bounded type variable* receiver (`a.compare(b)` where
/// `a: T` and `T: Comparable`) is stamped `"TraitBound:<Trait>"` by the
/// checker's `stamp_recv_kind`, signalling that the method dispatches through a
/// trait bound rather than a concrete type.
///
/// The primitive variant carries the specific [`PrimitiveType`] (via its
/// `Debug` name, e.g. `Int`, `Float`, `String`) because the lowering of e.g.
/// `compare` differs by primitive (Rust `i64::cmp` vs `f64::partial_cmp`).
#[must_use]
pub fn recv_kind_tag(ty: &Type) -> Option<String> {
    match ty {
        Type::Primitive(p) => Some(format!("Primitive:{p:?}")),
        Type::Optional(_) => Some("Optional".to_string()),
        Type::Result(_, _) => Some("Result".to_string()),
        Type::Generic(g) => match g.constructor.as_str() {
            "List" => Some("List".to_string()),
            "Set" => Some("Set".to_string()),
            "Map" => Some("Map".to_string()),
            other => Some(format!("Generic:{other}")),
        },
        Type::Named(n) => Some(format!("User:{}", n.name)),
        _ => None,
    }
}

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
    /// Trait impls pulled in from imported modules (Q-xmod-impl), as
    /// `(trait_name, trait_args, target_type)`. Populated by `seed_imports`
    /// before `check_module`, then folded into the freshly-built `impl_table`
    /// in `check_module` so cross-module `.into()` (and `From`/`Into`
    /// resolution) and cross-module where-clause bounds see impls declared in
    /// other modules.
    imported_trait_impls: Vec<(String, Vec<Type>, Type)>,
    /// Methods from inherent impl blocks: type_name → method_name → fn_type.
    method_types: HashMap<String, HashMap<String, Type>>,
    /// A method's OWN generic type-parameter names, keyed
    /// type_name → method_name → \[param_name, …\]. Populated during
    /// `collect_sig` for `ImplBlock`/`ClassDecl` methods that declare their own
    /// `[U, …]` params (distinct from the type's params, which live in
    /// `record_generic_params`). At a call site these names are substituted with
    /// fresh inference variables so the method's own params are inferred from the
    /// arguments — the method-level analogue of free-function call inference
    /// (Q-checker-method-generic-call-infer). The type's own params stay as
    /// `Named(_)` placeholders pinned by the receiver via `record_generic_params`.
    method_generic_params: HashMap<String, HashMap<String, Vec<String>>>,
    /// Effect operation types: effect_name → [(op_name, fn_type)].
    /// Populated during `collect_sig` for `EffectDecl` nodes.
    effect_op_types: HashMap<String, Vec<(String, Type)>>,
    /// Component effects for composite effects: effect_name → Vec\<component_name\>.
    effect_components: HashMap<String, Vec<String>>,
    /// Record field types: record_name → [(field_name, field_type)].
    /// Populated during `collect_sig` for `RecordDecl` nodes.
    record_field_types: HashMap<String, Vec<(String, Type)>>,
    /// Generic type parameter names for records: record_name → Vec\<param_name\>.
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
    /// Names of locally-declared `class` types. Populated during `collect_sig`
    /// for `ClassDecl` nodes. The DQ29 structural-Equatable predicate consults
    /// this to EXCLUDE classes from the structural default (a class sits on
    /// the data/identity line and gets `==` only via an explicit
    /// `impl Equatable`); records and classes otherwise share
    /// `record_field_types`, so the field table alone cannot distinguish them.
    class_names: HashSet<String>,
    /// Variant payload types for locally-declared enums:
    /// enum_name → \[(variant_name, \[(component_label, type), …\]), …\].
    /// Component labels are field names for struct variants and `_0`-style
    /// indices for tuple payloads; unit variants contribute an empty list.
    /// Generic param references are stored SYMBOLICALLY as `Named(param)` (the
    /// same convention `record_field_types` uses for generic records) so the
    /// DQ29 structural-Equatable predicate can substitute instantiation
    /// arguments at the use site. Populated during `collect_sig` for
    /// `EnumDecl` nodes; imported enums are absent (their payload types do not
    /// cross the export ABI), which the predicate treats as conservatively
    /// conforming.
    enum_variant_payloads: HashMap<String, Vec<EnumVariantPayloadTypes>>,
    /// Monotonic node-id source for AIR nodes the checker *synthesizes* (today
    /// only the `for`-over-`Iterable` desugar, see
    /// [`TypeChecker::desugar_for_iterable`]). The checker is constructed
    /// without the session's `NodeIdGen`, so it mints its own ids from a high
    /// base (`SYNTH_ID_BASE`) chosen to sit far above the dense, zero-based range
    /// the lowerer assigns — synthesized nodes therefore never collide with real
    /// nodes for the `id`-equality checks codegen relies on (e.g. the
    /// desugared-method-call receiver-identity test in the generator).
    synth_id: std::cell::Cell<NodeId>,
    /// Per-checker counter that makes each synthesized `for`-loop iterator
    /// binding name (`__bock_iter_<n>`) unique, so nested desugared `for` loops
    /// do not shadow one another.
    synth_iter_var: std::cell::Cell<u32>,
    /// `(name, span)` pairs for which an undefined-name diagnostic has
    /// already been emitted. The AIR lowerer desugars a method call
    /// `recv.m(args)` into `Call { callee: FieldAccess(recv, m), args:
    /// [recv, args…] }`, **duplicating the receiver node** — so the same
    /// source expression is inferred twice (once inside the callee's
    /// `FieldAccess`, once as `args[0]`). For well-typed code the double
    /// inference is harmless, but an undefined receiver used to produce the
    /// same `E4002` twice at the identical span
    /// (Q-diag-effect-violation-errors). One root cause must emit one
    /// diagnostic (diagnostics-review rubric #6), so emission sites consult
    /// this set first.
    reported_undefined: HashSet<(String, Span)>,
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
            imported_trait_impls: Vec::new(),
            method_types: HashMap::new(),
            method_generic_params: HashMap::new(),
            effect_op_types: HashMap::new(),
            effect_components: HashMap::new(),
            record_field_types: HashMap::new(),
            record_generic_params: HashMap::new(),
            type_aliases: HashMap::new(),
            trait_method_types: HashMap::new(),
            type_var_bounds: HashMap::new(),
            class_names: HashSet::new(),
            enum_variant_payloads: HashMap::new(),
            synth_id: std::cell::Cell::new(SYNTH_ID_BASE),
            synth_iter_var: std::cell::Cell::new(0),
            reported_undefined: HashSet::new(),
        }
    }

    // ── TypeVarId allocation ─────────────────────────────────────────────────

    /// Allocate a fresh type-inference variable.
    fn fresh_var(&self) -> Type {
        Type::TypeVar(self.var_gen.next())
    }

    // ── Synthesized-node helpers (for the `for`-over-`Iterable` desugar) ──────

    /// Mint a fresh, collision-free [`NodeId`] for a synthesized AIR node.
    ///
    /// Ids are drawn monotonically from [`SYNTH_ID_BASE`], far above the
    /// lowerer's dense zero-based range, so a synthesized node's id never
    /// equals a real node's id (which codegen's receiver-identity check and the
    /// per-module item dedup rely on).
    fn next_synth_id(&self) -> NodeId {
        let id = self.synth_id.get();
        self.synth_id.set(id.wrapping_add(1));
        id
    }

    /// Build a synthesized AIR node carrying a fresh id, the given `span`, and
    /// the `scope_id` metadata downstream passes expect (copied from the `for`
    /// node so the synthesized subtree lives in the loop's lexical scope).
    fn synth_node(&self, span: Span, scope_id: i64, kind: NodeKind) -> AIRNode {
        let mut node = AIRNode::new(self.next_synth_id(), span, kind);
        node.metadata
            .insert("scope_id".to_string(), Value::Int(scope_id));
        node
    }

    /// Build a synthesized identifier-reference node for a local binding.
    fn synth_ident(&self, name: &str, span: Span, scope_id: i64) -> AIRNode {
        self.synth_node(
            span,
            scope_id,
            NodeKind::Identifier {
                name: bock_ast::Ident {
                    name: name.to_string(),
                    span,
                },
            },
        )
    }

    /// Build a synthesized zero-argument method call on `receiver`, in the SAME
    /// desugared shape the lowerer produces (`Call { callee: FieldAccess(recv,
    /// method), args: [self = recv] }`). The receiver is cloned into both the
    /// field-access object and the `self` arg with the *same* node id, matching
    /// the lowerer so codegen's receiver-identity check recognises the call as a
    /// method call rather than a field-closure invocation.
    fn synth_method_call(
        &self,
        receiver: AIRNode,
        method: &str,
        span: Span,
        scope_id: i64,
    ) -> AIRNode {
        let field_access = self.synth_node(
            span,
            scope_id,
            NodeKind::FieldAccess {
                object: Box::new(receiver.clone()),
                field: bock_ast::Ident {
                    name: method.to_string(),
                    span,
                },
            },
        );
        let self_arg = bock_air::AirArg {
            label: None,
            value: receiver,
        };
        self.synth_node(
            span,
            scope_id,
            NodeKind::Call {
                callee: Box::new(field_access),
                args: vec![self_arg],
                type_args: vec![],
            },
        )
    }

    /// Build a synthesized `Some`/`None`-style constructor pattern.
    fn synth_ctor_pat(
        &self,
        ctor: &str,
        fields: Vec<AIRNode>,
        span: Span,
        scope_id: i64,
    ) -> AIRNode {
        self.synth_node(
            span,
            scope_id,
            NodeKind::ConstructorPat {
                path: TypePath {
                    segments: vec![bock_ast::Ident {
                        name: ctor.to_string(),
                        span,
                    }],
                    span,
                },
                fields,
            },
        )
    }

    /// Rewrite a `for <pattern> in <iterable> { <body> }` whose `iterable`
    /// implements `Iterable` into the proven manual-drive shape, in place.
    ///
    /// The `node` (a [`NodeKind::For`]) is rewritten to:
    ///
    /// ```text
    /// {
    ///   let mut __bock_iter_N = <iterable>.iter();
    ///   loop {
    ///     match __bock_iter_N.next() {
    ///       Some(<pattern>) => <body>
    ///       None            => break
    ///     }
    ///   }
    /// }
    /// ```
    ///
    /// The user's `<pattern>`, `<iterable>`, and `<body>` are *moved* (not
    /// cloned) out of the original `For` node into the synthesized subtree.
    /// After rewriting, the caller infers the new subtree through the normal
    /// [`TypeChecker::infer_node`] path, so the `match`/`Some(pat)`/method-call
    /// nodes pick up exactly the metadata codegen needs (the `Optional[T]`
    /// payload typing, receiver-kind tags, copy-type marks). The synthesized
    /// `loop` is native on every target, and the user's `break`/`continue` land
    /// inside the `Some` arm body, targeting that loop.
    ///
    /// `iter_var` names the binding; passing a per-loop-unique name keeps nested
    /// desugared `for` loops from shadowing one another.
    fn desugar_for_iterable(
        &self,
        node: &mut AIRNode,
        pattern: AIRNode,
        iterable: AIRNode,
        body: AIRNode,
        iter_var: &str,
    ) {
        let span = node.span;
        let scope_id = node
            .metadata
            .get("scope_id")
            .and_then(|v| match v {
                Value::Int(i) => Some(*i),
                _ => None,
            })
            .unwrap_or(0);

        // `let mut __bock_iter_N = <iterable>.iter()`
        let iter_call = self.synth_method_call(iterable, "iter", span, scope_id);
        let let_pat = self.synth_node(
            span,
            scope_id,
            NodeKind::BindPat {
                name: bock_ast::Ident {
                    name: iter_var.to_string(),
                    span,
                },
                is_mut: true,
            },
        );
        let let_binding = self.synth_node(
            span,
            scope_id,
            NodeKind::LetBinding {
                is_mut: true,
                pattern: Box::new(let_pat),
                ty: None,
                value: Box::new(iter_call),
            },
        );

        // `match __bock_iter_N.next() { Some(<pattern>) => <body>; None => break }`
        let next_recv = self.synth_ident(iter_var, span, scope_id);
        let next_call = self.synth_method_call(next_recv, "next", span, scope_id);
        let some_pat = self.synth_ctor_pat("Some", vec![pattern], span, scope_id);
        let some_arm = self.synth_node(
            span,
            scope_id,
            NodeKind::MatchArm {
                pattern: Box::new(some_pat),
                guard: None,
                body: Box::new(body),
            },
        );
        let none_pat = self.synth_ctor_pat("None", vec![], span, scope_id);
        let break_node = self.synth_node(span, scope_id, NodeKind::Break { value: None });
        let none_arm = self.synth_node(
            span,
            scope_id,
            NodeKind::MatchArm {
                pattern: Box::new(none_pat),
                guard: None,
                body: Box::new(break_node),
            },
        );
        let match_node = self.synth_node(
            span,
            scope_id,
            NodeKind::Match {
                scrutinee: Box::new(next_call),
                arms: vec![some_arm, none_arm],
            },
        );

        // `loop { <match> }`
        let loop_body = self.synth_node(
            span,
            scope_id,
            NodeKind::Block {
                stmts: vec![match_node],
                tail: None,
            },
        );
        let loop_node = self.synth_node(
            span,
            scope_id,
            NodeKind::Loop {
                body: Box::new(loop_body),
            },
        );

        // `{ <let>; <loop> }` — replace the `for` node's kind in place.
        node.kind = NodeKind::Block {
            stmts: vec![let_binding, loop_node],
            tail: None,
        };
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
            node.metadata.insert("copy_type".into(), Value::Bool(true));
        }
        resolved
    }

    /// Look up the resolved type for `node_id` from the side-table.
    #[must_use]
    pub fn type_of(&self, id: NodeId) -> Option<&Type> {
        self.types.get(&id)
    }

    /// Stamp the receiver-kind annotation ([`RECV_KIND_META_KEY`]) onto a
    /// method-call `node` from the resolved `receiver_ty`.
    ///
    /// This is the checker→codegen lynchpin (see [`recv_kind_tag`]): at the
    /// method-resolution sites the checker already knows the receiver type, so
    /// it records the receiver *category* on the call node for codegen to read
    /// after the type side-table is dropped. No-ops when the receiver type maps
    /// to no tag (inference var, function type, …), leaving the call to its
    /// existing structural lowering. The receiver type is run through the
    /// current substitution first so a late-unified type var resolves.
    ///
    /// Bounded type variables get a bespoke tag the plain [`recv_kind_tag`]
    /// cannot produce (it has no access to the bounds table): when the resolved
    /// receiver is a `Type::TypeVar` carrying a trait bound — the receiver of
    /// `a.compare(b)` inside `max[T: Comparable](a, b)` — the tag is
    /// `"TraitBound:<Trait>"` (the first bound). This tells codegen the method
    /// dispatches through that trait rather than a concrete type. A new *value*
    /// of the existing `recv_kind` metadata key (same mechanism as
    /// `Primitive:…`/`User:…`/`Optional`), so it does not touch the AIR shape,
    /// the export ABI, or any visitor.
    fn stamp_recv_kind(&self, node: &mut AIRNode, receiver_ty: &Type) {
        let resolved = self.subst.apply(receiver_ty);
        let tag = match &resolved {
            Type::TypeVar(id) => self
                .type_var_bounds
                .get(id)
                .and_then(|bounds| bounds.first())
                .map(|trait_name| format!("TraitBound:{trait_name}")),
            _ => recv_kind_tag(&resolved),
        };
        if let Some(tag) = tag {
            node.metadata
                .insert(RECV_KIND_META_KEY.to_string(), Value::String(tag));
        }
    }

    // ── Getters for export collection ───────────────────────────────────────

    /// Record field types: record_name → [(field_name, field_type)].
    #[must_use]
    pub fn record_field_types(&self) -> &HashMap<String, Vec<(String, Type)>> {
        &self.record_field_types
    }

    /// Generic type parameter names for records: record_name → Vec\<param_name\>.
    #[must_use]
    pub fn record_generic_params(&self) -> &HashMap<String, Vec<String>> {
        &self.record_generic_params
    }

    /// Effect operation types: effect_name → [(op_name, fn_type)].
    #[must_use]
    pub fn effect_op_types(&self) -> &HashMap<String, Vec<(String, Type)>> {
        &self.effect_op_types
    }

    /// Component effects for composite effects: effect_name → Vec\<component_name\>.
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

    /// Where-clause trait bounds on a generic function's type parameters,
    /// keyed by the [`TypeVarId`] the parameter was assigned during signature
    /// collection.
    ///
    /// Returns `(var_id, [trait_name, …])` pairs — one entry per generic
    /// parameter that carries at least one bound. The `var_id` is the same id
    /// that appears as `?<id>` in the function's exported [`Type`] string, so
    /// the export ABI can encode the bound against the right type variable
    /// (Q-xmod-bounds). Returns an empty vec for an unknown or non-generic
    /// function, or one with no where-clause bounds.
    #[must_use]
    pub fn fn_where_bounds(&self, name: &str) -> Vec<(TypeVarId, Vec<String>)> {
        let Some(sig) = self.fn_sigs.get(name) else {
            return vec![];
        };
        // Map generic-param name → its TypeVarId (positional zip).
        let name_to_id: HashMap<&str, TypeVarId> = sig
            .generic_params
            .iter()
            .zip(sig.generic_var_ids.iter())
            .map(|(n, id)| (n.as_str(), *id))
            .collect();

        let mut out: Vec<(TypeVarId, Vec<String>)> = Vec::new();
        for clause in &sig.where_clause {
            let Some(&var_id) = name_to_id.get(clause.param.name.as_str()) else {
                continue; // bound on an unknown param — already diagnosed
            };
            let traits: Vec<String> = clause.bounds.iter().map(type_path_to_name).collect();
            if traits.is_empty() {
                continue;
            }
            // Merge bounds for the same param (multiple where-clauses or
            // inline + where) so the export carries the full set.
            if let Some(existing) = out.iter_mut().find(|(id, _)| *id == var_id) {
                for t in traits {
                    if !existing.1.contains(&t) {
                        existing.1.push(t);
                    }
                }
            } else {
                out.push((var_id, traits));
            }
        }
        out
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

    /// Record a trait impl declared in an imported module (Q-xmod-impl).
    ///
    /// `trait_args` is empty for a plain trait impl (`impl Comparable for B`)
    /// and non-empty for a parameterized one (`impl From[A] for B`). The
    /// recorded impls are folded into the freshly-built `impl_table` in
    /// [`TypeChecker::check_module`], so cross-module `.into()` resolution and
    /// cross-module where-clause bound satisfaction see them.
    pub fn register_imported_trait_impl(
        &mut self,
        trait_name: String,
        trait_args: Vec<Type>,
        target: Type,
    ) {
        self.imported_trait_impls
            .push((trait_name, trait_args, target));
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
    /// original [`TypeVarId`]s (e.g. `"Fn(?3) -> ?3"`). Without an `FnSig`
    /// entry the call-site instantiation logic in the `Call` handler is
    /// bypassed, causing the first call to bind those vars permanently.
    ///
    /// This method re-allocates fresh [`TypeVarId`]s, remaps the function
    /// type, stores the remapped type in `env`, and inserts a matching
    /// `FnSig` into `fn_sigs`.
    pub fn seed_imported_generic_fn(&mut self, name: &str, fn_ty: &FnType) -> Type {
        self.seed_imported_generic_fn_with_bounds(name, fn_ty, &[])
    }

    /// Like [`Self::seed_imported_generic_fn`] but also reconstructs the
    /// imported function's where-clause trait bounds so they are enforced at
    /// call sites in the importing module (Q-xmod-bounds).
    ///
    /// `bounds` is `(original_type_var_id, [trait_name, …])`, where the var id
    /// is the one encoded in the export ABI (the `?<id>` that appears in the
    /// exported type string). Each id is matched to the synthetic generic
    /// parameter (`T<position>`) created for it here, and a [`TypeConstraint`]
    /// is built so the call-site trait-bound check can enforce it.
    pub fn seed_imported_generic_fn_with_bounds(
        &mut self,
        name: &str,
        fn_ty: &FnType,
        bounds: &[(TypeVarId, Vec<String>)],
    ) -> Type {
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

        // Create synthetic generic param names. Synthetic param `T<i>`
        // corresponds to `original_ids[i]`.
        let generic_params: Vec<String> =
            (0..original_ids.len()).map(|i| format!("T{i}")).collect();

        // Reconstruct the where-clause: map each encoded `(original_var_id,
        // traits)` to the synthetic param name that the same id became, and
        // build a `TypeConstraint` keyed on that name. `check_trait_bounds_at_call`
        // pairs `clause.param.name` with `generic_params`/`generic_var_ids` by
        // name, so the constraint reaches the right fresh call-site var.
        let where_clause: Vec<TypeConstraint> = bounds
            .iter()
            .filter_map(|(orig_id, traits)| {
                let pos = original_ids.iter().position(|id| id == orig_id)?;
                if traits.is_empty() {
                    return None;
                }
                Some(TypeConstraint {
                    id: 0,
                    span: Span::dummy(),
                    param: bock_ast::Ident {
                        name: generic_params[pos].clone(),
                        span: Span::dummy(),
                    },
                    bounds: traits
                        .iter()
                        .map(|t| TypePath {
                            segments: t
                                .split('.')
                                .map(|seg| bock_ast::Ident {
                                    name: seg.to_string(),
                                    span: Span::dummy(),
                                })
                                .collect(),
                            span: Span::dummy(),
                        })
                        .collect(),
                })
            })
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
                    where_clause,
                },
            );
        }

        remapped
    }

    // ── Unification helper ───────────────────────────────────────────────────

    /// Try to unify `found` (the type the expression actually has) with
    /// `expected` (the type the surrounding context requires). On failure
    /// emit an `E4001` at `span` and return `Type::Error`.
    ///
    /// The argument orientation is part of the diagnostic contract: the
    /// message reads ``expected `T`, found `U``` with `T` taken from
    /// `expected` and `U` from `found`, and the conversion hint (when one
    /// exists) suggests the conversion that produces the **expected** type.
    /// Call sites must pass the established/required type as `expected`
    /// (for operand pairs, the left/first operand establishes the
    /// expectation). Types render in surface Bock syntax via [`Type`]'s
    /// `Display` — never `Debug`.
    fn unify_or_error(&mut self, found: &Type, expected: &Type, span: Span, context: &str) -> Type {
        let found = self.resolve_alias(&self.subst.apply(found));
        let expected = self.resolve_alias(&self.subst.apply(expected));
        // `unify` is symmetric for solving, but its error payloads describe
        // the first argument as `left`/`expected` — pass `expected` first so
        // arity errors (`expected a function taking N parameters, …`) read
        // with the right orientation.
        match unify(&expected, &found, &mut self.subst) {
            Ok(()) => self.subst.apply(&found),
            Err(e) => {
                let msg = match &e {
                    TypeError::Mismatch { .. } => {
                        format!(
                            "type mismatch in {context}: expected `{expected}`, found `{found}`"
                        )
                    }
                    other => format!("type mismatch in {context}: {other}"),
                };
                let diag = self.diags.error(E_TYPE_MISMATCH, msg, span);
                if let Some(hint) = conversion_hint(&found, &expected) {
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
        let (items, imports) = match &module.kind {
            NodeKind::Module { items, imports, .. } => (items.clone(), imports.clone()),
            _ => return,
        };

        // §12.2 / DQ8 (Q-import-reject): reject any `use` whose module path
        // carries neither a brace-list nor a wildcard — a bare
        // `use core.error`. Module-qualified access is deferred to v1.x; the
        // only two v1 import forms are the braced list and the wildcard.
        self.reject_bare_module_imports(&imports);

        // Build the trait-impl table from the module's `impl` blocks and wire
        // it into the checker so where-clause bounds are enforced at call
        // sites. Without a wired table, `check_trait_bounds_at_call` is a
        // no-op and bounds go unchecked.
        //
        // Order matters (Q1b sealing): `build_from` runs sealing on *user*
        // impls first; `register_canonical_conformances` then registers the
        // compiler's primitive conformances via `register_trait_impl_inner`,
        // which bypasses the sealing check, so the compiler can never reject
        // its own registration.
        let mut impl_table = ImplTable::build_from(module);
        crate::traits::register_canonical_conformances(&mut impl_table);
        // Canonical primitive conversions (`From`/`TryFrom` + blanket `Into`),
        // registered after conformances so `(5).into()`, `Float.from(3)`, and
        // `Int.try_from(s)` resolve uniformly with user conversions.
        crate::traits::register_canonical_conversions(&mut impl_table);
        // Q-xmod-impl: fold in trait impls declared in imported modules so
        // cross-module `.into()` (and `From`/`Into` resolution) and
        // cross-module where-clause bounds see them. Runs after the local +
        // canonical registration so a local impl always wins; the fold then
        // re-synthesizes the blanket `Into` from any imported `From`.
        if !self.imported_trait_impls.is_empty() {
            impl_table.fold_imported_impls(&self.imported_trait_impls);
        }
        // Surface coherence (`E4010`) and sealing (`E4011`) diagnostics
        // produced during table construction.
        self.diags.absorb(&impl_table.diags);
        self.impl_table = Some(impl_table);

        // Pass 1: collect signatures
        for item in &items {
            self.collect_sig(item);
        }

        // Pass 1b: §10.3 Layer-2 (Module) handlers. A module-level
        // `handle <Effect> with <handler>` installs that effect's operation
        // types into the module env so a bare op call anywhere in the module
        // type-checks without an enclosing `handling` block. Mirrors the
        // resolver's `inject_module_handle_operations`. Runs after `collect_sig`
        // (so `effect_op_types` is populated) and before item checking.
        {
            let mut visited = std::collections::HashSet::new();
            for item in &items {
                if let NodeKind::ModuleHandle { effect, .. } = &item.kind {
                    let ename = type_path_to_name(effect);
                    self.inject_effect_ops_into_env(&ename, &mut visited);
                }
            }
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

    /// §12.2 / DQ8 (Q-import-reject): emit `E4014` for every import whose items
    /// are [`ImportItems::Module`] — a `use` of a module path with neither a
    /// brace-list nor a wildcard (e.g. `use core.error`). v1 accepts only the
    /// braced form (`use core.error.{Error}`) and the discouraged wildcard
    /// (`use core.error.*`); module-qualified access is deferred to v1.x.
    fn reject_bare_module_imports(&mut self, imports: &[AIRNode]) {
        for import in imports {
            let NodeKind::ImportDecl { path, items } = &import.kind else {
                continue;
            };
            if !matches!(items, bock_ast::ImportItems::Module) {
                continue;
            }
            let path_str = path
                .segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join(".");
            self.diags
                .error(
                    E_BARE_MODULE_IMPORT,
                    format!(
                        "`use {path_str}` is not a v1 import form: a `use` must \
                         name what it imports with a brace-list or a wildcard"
                    ),
                    import.span,
                )
                .note(format!(
                    "import the names you need with the braced form, e.g. \
                     `use {path_str}.{{ /* names */ }}`"
                ))
                .note(
                    "module-qualified access (referring to symbols as \
                     `module.Symbol`) is deferred to v1.x",
                );
        }
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
            NodeKind::EnumDecl {
                name,
                variants,
                generic_params,
                ..
            } => {
                let enum_name = name.name.clone();

                // Extract generic param names.
                let gp_names: Vec<String> =
                    generic_params.iter().map(|g| g.name.name.clone()).collect();

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
                    let type_args: Vec<Type> = gp_names.iter().map(|n| gp_map[n].clone()).collect();
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

                // DQ29: record every variant's payload component types for the
                // structural-Equatable predicate (an enum conforms iff every
                // payload type of every variant conforms). Generic params are
                // stored SYMBOLICALLY as `Named(param)` — the convention
                // `record_field_types` already uses for generic records — so
                // the predicate can substitute the instantiation's type
                // arguments at the use site (the template type vars in
                // `gp_map` are reserved for constructor fn_sigs).
                let symbolic_gp_map: HashMap<String, Type> = gp_names
                    .iter()
                    .map(|n| (n.clone(), Type::Named(crate::NamedType { name: n.clone() })))
                    .collect();
                let mut payloads: Vec<EnumVariantPayloadTypes> = Vec::new();
                for variant in variants {
                    if let NodeKind::EnumVariant {
                        name: vname,
                        payload,
                    } = &variant.kind
                    {
                        let components: Vec<(String, Type)> = match payload {
                            EnumVariantPayload::Unit => vec![],
                            EnumVariantPayload::Tuple(param_nodes) => param_nodes
                                .iter()
                                .enumerate()
                                .map(|(i, p)| {
                                    (
                                        format!("_{i}"),
                                        self.air_type_node_to_type(p, &symbolic_gp_map),
                                    )
                                })
                                .collect(),
                            EnumVariantPayload::Struct(fields) => fields
                                .iter()
                                .map(|f| {
                                    (
                                        f.name.name.clone(),
                                        self.type_expr_to_type(&f.ty, &symbolic_gp_map),
                                    )
                                })
                                .collect(),
                        };
                        payloads.push((vname.name.clone(), components));
                    }
                }
                self.enum_variant_payloads
                    .insert(enum_name.clone(), payloads);

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
                                        let ty = self.type_expr_to_type(&f.ty, &gp_map);
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
                        generic_params: method_gps,
                        ..
                    } = &method.kind
                    {
                        let gp_map: HashMap<String, Type> = HashMap::new();

                        // Record the method's OWN type-param names (e.g. the `U`
                        // in `fn map[U](...)`) so the call site can substitute
                        // them with fresh inference vars
                        // (Q-checker-method-generic-call-infer). The type's own
                        // params (`T`) are pinned by the receiver and are NOT
                        // listed here.
                        let method_gp_names: Vec<String> =
                            method_gps.iter().map(|g| g.name.name.clone()).collect();
                        if !method_gp_names.is_empty() {
                            self.method_generic_params
                                .entry(target_name.clone())
                                .or_default()
                                .insert(name.name.clone(), method_gp_names);
                        }

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

                        // Substitute `Self` -> the impl's target type across the
                        // method signature (params + return). An explicit `Self`
                        // written in an impl method's own signature — e.g.
                        // `fn double(self) -> Self` or `fn combine(self, other:
                        // Self)` — lowers to `Type::Named("Self")`, which the
                        // trait-method resolution path substitutes but the impl
                        // method's own registered `FnSig` did not, yielding E4001
                        // at call sites (Named("Self") vs the concrete target).
                        // This mirrors the trait-method `self_params=["Self"]`
                        // substitution. Associated-type `Self::Output` is out of
                        // scope (parsed as a distinct type path, not `Self`).
                        let self_params = ["Self".to_string()];
                        let self_args = [target_ty.clone()];
                        let fn_ty = substitute_type_params(&fn_ty, &self_params, &self_args);

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

                let comp_names: Vec<String> = components.iter().map(type_path_to_name).collect();
                if !comp_names.is_empty() {
                    self.effect_components.insert(name.name.clone(), comp_names);
                }
            }
            NodeKind::RecordDecl {
                name,
                fields,
                generic_params,
                ..
            } => {
                let record_name = name.name.clone();
                let gp_names: Vec<String> =
                    generic_params.iter().map(|g| g.name.name.clone()).collect();
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
                    Type::Named(crate::NamedType { name: record_name }),
                );
            }
            NodeKind::TypeAlias { name, ty, .. } => {
                let underlying = self.air_type_node_to_type(ty, &HashMap::new());
                self.type_aliases.insert(name.name.clone(), underlying);
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

                // DQ29: classes are excluded from structural Equatable; record
                // the name so the predicate can tell a class apart from a
                // record (both populate `record_field_types`).
                self.class_names.insert(class_name.clone());

                // Register generic params if present.
                let gp_names: Vec<String> =
                    generic_params.iter().map(|g| g.name.name.clone()).collect();
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
                        generic_params: method_gps,
                        ..
                    } = &method.kind
                    {
                        let gp_map: HashMap<String, Type> = HashMap::new();

                        // Record the method's OWN type-param names so the call
                        // site can substitute them with fresh inference vars
                        // (Q-checker-method-generic-call-infer); see the
                        // `ImplBlock` branch for the rationale.
                        let method_gp_names: Vec<String> =
                            method_gps.iter().map(|g| g.name.name.clone()).collect();
                        if !method_gp_names.is_empty() {
                            self.method_generic_params
                                .entry(class_name.clone())
                                .or_default()
                                .insert(method_name.name.clone(), method_gp_names);
                        }

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
            NodeKind::TraitDecl { name, methods, .. } => {
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
            NodeKind::ImplBlock { .. } => {
                self.check_impl_block(node);
            }
            NodeKind::ClassDecl { .. } => {
                self.check_class_decl(node);
            }
            // Record/enum declarations carry no body to check, but DQ29 stamps
            // the structurally-Equatable ones for the Rust backend's
            // `PartialEq` derive (see `DERIVE_EQ_META_KEY`).
            NodeKind::RecordDecl { .. } | NodeKind::EnumDecl { .. } => {
                self.stamp_derive_structural_eq(node);
                self.record(node, Type::Primitive(PrimitiveType::Void));
            }
            // Other top-level items: record as Void for now.
            _ => {
                self.record(node, Type::Primitive(PrimitiveType::Void));
            }
        }
    }

    /// Stamp a `RecordDecl` / `EnumDecl` with [`DERIVE_EQ_META_KEY`] when the
    /// declared type conforms to `Equatable` structurally (DQ29) and declares
    /// no explicit `impl Equatable` (the impl suppresses the structural
    /// default — `==` routes through its `eq` instead, so the derive would
    /// pin the WRONG equality into containers).
    ///
    /// The probe runs on the bare `Named` type: a generic decl's symbolic
    /// `Named(param)` field placeholders are unknown to the predicate and thus
    /// conservatively conforming, which matches Rust's conditional derive
    /// semantics (`#[derive(PartialEq)]` on `Pair<A, B>` bounds each use site
    /// on `A: PartialEq, B: PartialEq` — rule 4's per-instantiation decision).
    fn stamp_derive_structural_eq(&mut self, node: &mut AIRNode) {
        let name = match &node.kind {
            NodeKind::RecordDecl { name, .. } | NodeKind::EnumDecl { name, .. } => {
                name.name.clone()
            }
            _ => return,
        };
        let named = Type::Named(crate::NamedType { name });
        if let Some(table) = self.impl_table.as_ref() {
            if resolve_impl(&TraitRef::new("Equatable"), &named, table).is_some() {
                return;
            }
        }
        let mut in_progress = HashSet::new();
        let mut path = Vec::new();
        if self
            .structural_equatable_witness(&named, &mut in_progress, &mut path)
            .is_none()
        {
            node.metadata
                .insert(DERIVE_EQ_META_KEY.to_string(), Value::Bool(true));
        }
    }

    /// Type-check every method **body** in an `impl` block.
    ///
    /// Mirrors [`Self::check_fn_decl`] per method, but establishes the impl
    /// context first: the impl's generic params become fresh type vars (with
    /// their bounds recorded), `Self` is mapped to the concrete target type,
    /// and `self` is bound in scope to that target. For a generic impl
    /// (`impl[T] Foo[T] { … }`) the target is a `Generic` whose args are those
    /// fresh vars, so field/method access through `record_generic_params`
    /// substitution resolves the same way it does at external call sites.
    ///
    /// Method signatures are already registered in `method_types` by
    /// [`Self::collect_sig`]; this pass only walks the bodies that pass missed,
    /// so type errors inside methods are reported and the checker's codegen
    /// metadata stamps (`recv_kind`, `list_concat`) reach method bodies.
    fn check_impl_block(&mut self, node: &mut AIRNode) {
        let (generic_params, target) = match &node.kind {
            NodeKind::ImplBlock {
                generic_params,
                target,
                ..
            } => (generic_params.clone(), target.clone()),
            _ => return,
        };

        let target_name = match &target.kind {
            NodeKind::TypeNamed { path, .. } => Some(type_path_to_name(path)),
            _ => None,
        };
        let Some(target_name) = target_name else {
            self.record(node, Type::Primitive(PrimitiveType::Void));
            return;
        };

        let (impl_gp_map, target_ty) = self.build_impl_context(&generic_params, &target_name);

        if let NodeKind::ImplBlock { methods, .. } = &mut node.kind {
            let mut methods = std::mem::take(methods);
            for method in methods.iter_mut() {
                self.check_method_body(method, &target_ty, &impl_gp_map);
            }
            if let NodeKind::ImplBlock { methods: slot, .. } = &mut node.kind {
                *slot = methods;
            }
        }

        self.record(node, Type::Primitive(PrimitiveType::Void));
    }

    /// Type-check every method **body** in a `class` declaration. See
    /// [`Self::check_impl_block`]; classes are the inherent-impl analogue with
    /// declared fields and optional base inheritance (already folded into
    /// `method_types`/`record_field_types` by [`Self::collect_sig`]).
    fn check_class_decl(&mut self, node: &mut AIRNode) {
        let (generic_params, class_name) = match &node.kind {
            NodeKind::ClassDecl {
                generic_params,
                name,
                ..
            } => (generic_params.clone(), name.name.clone()),
            _ => return,
        };

        let (impl_gp_map, target_ty) = self.build_impl_context(&generic_params, &class_name);

        if let NodeKind::ClassDecl { methods, .. } = &mut node.kind {
            let mut methods = std::mem::take(methods);
            for method in methods.iter_mut() {
                self.check_method_body(method, &target_ty, &impl_gp_map);
            }
            if let NodeKind::ClassDecl { methods: slot, .. } = &mut node.kind {
                *slot = methods;
            }
        }

        self.record(node, Type::Primitive(PrimitiveType::Void));
    }

    /// Build the per-method type context shared by impl and class bodies:
    /// a `gp_map` mapping each of the impl/class's generic params to a fresh
    /// type var (with inline trait bounds recorded) plus `Self` -> the concrete
    /// target, and the target type itself (`Generic` when the impl is generic,
    /// `Named` otherwise) to bind `self`.
    fn build_impl_context(
        &mut self,
        generic_params: &[GenericParam],
        target_name: &str,
    ) -> (HashMap<String, Type>, Type) {
        let mut gp_map: HashMap<String, Type> = generic_params
            .iter()
            .map(|g| (g.name.name.clone(), self.fresh_var()))
            .collect();

        // Record inline trait bounds (e.g. `impl[T: Show] Foo[T]`) on the
        // fresh type vars so method bodies can resolve trait methods on `T`.
        for gp in generic_params {
            if let Some(Type::TypeVar(id)) = gp_map.get(&gp.name.name) {
                let bound_names: Vec<String> = gp.bounds.iter().map(type_path_to_name).collect();
                if !bound_names.is_empty() {
                    self.type_var_bounds
                        .entry(*id)
                        .or_default()
                        .extend(bound_names);
                }
            }
        }

        let target_ty = if generic_params.is_empty() {
            Type::Named(crate::NamedType {
                name: target_name.to_string(),
            })
        } else {
            // Generic target: `Foo[T, U]` with the impl's params as args, so
            // field/method access resolves through `record_generic_params`.
            let args: Vec<Type> = generic_params
                .iter()
                .map(|g| gp_map[&g.name.name].clone())
                .collect();
            Type::Generic(GenericType {
                constructor: target_name.to_string(),
                args,
            })
        };

        // `Self` written anywhere in a method body or signature resolves to the
        // concrete target (mirrors the signature substitution in `collect_sig`).
        gp_map.insert("Self".to_string(), target_ty.clone());

        (gp_map, target_ty)
    }

    /// Type-check a single impl/class method body in place.
    ///
    /// `target_ty` is the concrete (or generic-instantiated) type the method is
    /// attached to; `self` is bound to it. `impl_gp_map` carries the impl/class
    /// generic params + `Self`; the method's own generic params are layered on
    /// top. This is the per-function template of [`Self::check_fn_decl`],
    /// extended with the impl context.
    fn check_method_body(
        &mut self,
        node: &mut AIRNode,
        target_ty: &Type,
        impl_gp_map: &HashMap<String, Type>,
    ) {
        let (generic_params, params, return_type, effect_clause, where_clause) =
            match node.kind.clone() {
                NodeKind::FnDecl {
                    generic_params,
                    params,
                    return_type,
                    effect_clause,
                    where_clause,
                    ..
                } => (
                    generic_params,
                    params,
                    return_type,
                    effect_clause,
                    where_clause,
                ),
                // Methods are always FnDecl; ignore anything else defensively.
                _ => return,
            };

        self.env.push_scope();

        // Start from the impl context (impl generic params + `Self`) and layer
        // the method's own generic params on top.
        let mut gp_map = impl_gp_map.clone();
        for gp in &generic_params {
            gp_map.insert(gp.name.name.clone(), self.fresh_var());
        }

        // Record trait bounds on the method's own type variables.
        for gp in &generic_params {
            if let Some(Type::TypeVar(id)) = gp_map.get(&gp.name.name) {
                let bound_names: Vec<String> = gp.bounds.iter().map(type_path_to_name).collect();
                if !bound_names.is_empty() {
                    self.type_var_bounds
                        .entry(*id)
                        .or_default()
                        .extend(bound_names);
                }
            }
        }
        for clause in &where_clause {
            if let Some(Type::TypeVar(id)) = gp_map.get(&clause.param.name) {
                let bound_names: Vec<String> =
                    clause.bounds.iter().map(type_path_to_name).collect();
                if !bound_names.is_empty() {
                    self.type_var_bounds
                        .entry(*id)
                        .or_default()
                        .extend(bound_names);
                }
            }
        }

        // Bind params. A `self` receiver (no annotation) binds to the target
        // type; everything else resolves through `gp_map` (so `Self` and the
        // impl/method generics map to the concrete instantiation).
        for p in &params {
            if let NodeKind::Param { pattern, ty, .. } = &p.kind {
                if let NodeKind::BindPat { name, .. } = &pattern.kind {
                    if name.name == "self" && ty.is_none() {
                        self.env.define("self".to_string(), target_ty.clone());
                        continue;
                    }
                    let pty = self.air_type_node_to_type(p.kind.param_ty_node(), &gp_map);
                    self.env.define(name.name.clone(), pty);
                } else if let Some(pat_name) = p.kind.param_pat_name() {
                    let pty = self.air_type_node_to_type(p.kind.param_ty_node(), &gp_map);
                    self.env.define(pat_name, pty);
                }
            }
        }

        let ret_ty = return_type
            .as_deref()
            .map(|n| self.air_type_node_to_type(n, &gp_map))
            .unwrap_or(Type::Primitive(PrimitiveType::Void));

        // Inject effect operation types from the method's `with` clause.
        {
            let mut visited = std::collections::HashSet::new();
            for effect_tp in &effect_clause {
                let ename = type_path_to_name(effect_tp);
                self.inject_effect_ops_into_env(&ename, &mut visited);
            }
        }

        self.check_where_clause(&where_clause, &gp_map, node.span);

        self.return_ty_stack.push(ret_ty.clone());
        if let NodeKind::FnDecl { body, .. } = &mut node.kind {
            self.check_node(body, &ret_ty);
        }
        self.return_ty_stack.pop();

        self.env.pop_scope();

        // Methods record Void as their item-level type (their signature already
        // lives in `method_types`); the body walk's purpose is diagnostics +
        // codegen metadata stamping, not a fresh signature.
        self.record(node, Type::Primitive(PrimitiveType::Void));
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
                let bound_names: Vec<String> = gp.bounds.iter().map(type_path_to_name).collect();
                if !bound_names.is_empty() {
                    self.type_var_bounds
                        .entry(*id)
                        .or_default()
                        .extend(bound_names);
                }
            }
        }
        for clause in &where_clause {
            if let Some(Type::TypeVar(id)) = gp_map.get(&clause.param.name) {
                let bound_names: Vec<String> =
                    clause.bounds.iter().map(type_path_to_name).collect();
                if !bound_names.is_empty() {
                    self.type_var_bounds
                        .entry(*id)
                        .or_default()
                        .extend(bound_names);
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
                // Exact (non-parameterized) lookup first; then fall back to
                // *arg-imprecise* satisfaction for a parameterized bound such
                // as `T: Into[U]`. The bound's type argument is dropped at
                // parse time (the `where` clause stores only the trait path),
                // so a parameterized bound is satisfied when the concrete type
                // implements the trait for *some* argument. This is the
                // documented v1 limitation (see the session PR notes).
                let concrete_key = crate::traits::type_key(&concrete_ty);
                let satisfied = resolve_impl(&trait_ref, &concrete_ty, impl_table).is_some()
                    || impl_table.has_any_param_trait_impl(&trait_name, &concrete_key);
                if !satisfied {
                    // DQ29 (§18.5): an `Equatable` bound is ALSO satisfied by
                    // structural conformance — a record/enum whose fields /
                    // payloads are all Equatable passes without an explicit
                    // impl, exactly as the `==` operator gate accepts it. A
                    // structurally non-Equatable type is rejected with the
                    // same witness-carrying diagnostic as the gate
                    // (E4015 instead of the generic bound error).
                    if trait_name == "Equatable" {
                        let mut in_progress = HashSet::new();
                        let mut path = Vec::new();
                        match self.structural_equatable_witness(
                            &concrete_ty,
                            &mut in_progress,
                            &mut path,
                        ) {
                            None => continue,
                            Some(witness) => {
                                let (detail, suggestion) =
                                    equatable_failure_wording(&concrete_key, &witness);
                                self.diags
                                    .error(
                                        E_NOT_EQUATABLE,
                                        format!(
                                            "type `{concrete_ty}` does not satisfy bound \
                                             `Equatable` required by function `{fn_name}` \
                                             — {detail}"
                                        ),
                                        span,
                                    )
                                    .note(suggestion);
                                continue;
                            }
                        }
                    }
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
                        // The lowerer's method-call desugar duplicates the
                        // receiver node (see `reported_undefined`), so the
                        // same undefined identifier can be inferred twice at
                        // one span. Emit once per `(name, span)`.
                        if self.reported_undefined.insert((name.clone(), span)) {
                            self.diags.error(
                                E_UNDEFINED_VAR,
                                format!("undefined variable `{name}`"),
                                span,
                            );
                        }
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
                let result = self.infer_binop(op, &lt, &rt, span);
                // `+` on `List[T]` operands is concatenation, not numeric addition.
                // Stamp the node so codegen lowers it to each target's concat idiom
                // rather than a native `+` (which fails on TS/Rust/Go arrays/slices
                // and silently string-concats on JS). The result *or* either operand
                // resolving to a concrete `List` is sufficient — a record-field
                // receiver (`self.items + [x]`) may leave the unified result type a
                // still-open var while the left operand is already a concrete
                // `List`, so checking the operands too closes that gap.
                if matches!(op, BinOp::Add) {
                    let is_list = |t: &Type| matches!(self.subst.apply(t), Type::Generic(g) if g.constructor == "List");
                    if is_list(&result) || is_list(&lt) || is_list(&rt) {
                        node.metadata
                            .insert(LIST_CONCAT_META_KEY.to_string(), Value::Bool(true));
                    }
                    // String `+` is concatenation. Stamp it so the Rust backend
                    // lowers it to `format!` (Rust has no `String + String`). The
                    // result *or* either operand resolving to `String` is
                    // sufficient (an operand may still be an open var while the
                    // other side is already concrete `String`).
                    let is_string = |t: &Type| {
                        matches!(self.subst.apply(t), Type::Primitive(PrimitiveType::String))
                    };
                    if is_string(&result) || is_string(&lt) || is_string(&rt) {
                        node.metadata
                            .insert(STRING_CONCAT_META_KEY.to_string(), Value::Bool(true));
                    }
                }
                // `/` and `%` on two *integer* operands are integer division /
                // remainder with the cross-target truncate-toward-zero,
                // dividend-sign, abort-on-zero semantics fixed by DQ23 (§3.6).
                // Stamp the node so codegen lowers it to that contract rather than
                // the target's native operator (JS `/` is float division; Python
                // `//` floors and `%` follows floor division). Both operands must
                // resolve to an integer primitive — a mixed `Int`/`Float` operand
                // pair is a §4.2 type error reported by `infer_binop`, not stamped.
                if matches!(op, BinOp::Div | BinOp::Rem) {
                    let is_int = |t: &Type| {
                        matches!(
                            self.subst.apply(t),
                            Type::Primitive(
                                PrimitiveType::Int
                                    | PrimitiveType::Int8
                                    | PrimitiveType::Int16
                                    | PrimitiveType::Int32
                                    | PrimitiveType::Int64
                                    | PrimitiveType::Int128
                                    | PrimitiveType::UInt8
                                    | PrimitiveType::UInt16
                                    | PrimitiveType::UInt32
                                    | PrimitiveType::UInt64
                            )
                        )
                    };
                    if is_int(&lt) && is_int(&rt) {
                        node.metadata
                            .insert(INT_ARITH_META_KEY.to_string(), Value::Bool(true));
                    }
                }
                // `<`/`>`/`<=`/`>=` on two **user** (`Named`) operands that
                // implement `Comparable` must be lowered through the type's
                // `compare(self, other)` rather than the target's native ordering
                // operator (which is broken on every backend for user values, see
                // `USER_COMPARE_META_KEY`). `infer_binop` already accepted the
                // comparison (`require_comparable_operand`); stamp the node so
                // codegen routes it through `compare`. Probe the left operand,
                // falling back to the right only when the left stayed an inference
                // variable — mirroring the gate's post-unify probe.
                if matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge) {
                    let probe = match self.subst.apply(&lt) {
                        Type::TypeVar(_) => &rt,
                        _ => &lt,
                    };
                    if self.is_user_comparable(probe) {
                        node.metadata
                            .insert(USER_COMPARE_META_KEY.to_string(), Value::Bool(true));
                    }
                }
                // `==`/`!=` on operands whose native target equality is wrong
                // (records/enums/collections/tuples, explicit `impl Equatable`,
                // bounded generics) are stamped with the equality lane codegen
                // must use (DQ29 — see `USER_EQ_META_KEY`). Same post-unify
                // probe as the ordering stamp above.
                if matches!(op, BinOp::Eq | BinOp::Ne) {
                    let probe = match self.subst.apply(&lt) {
                        Type::TypeVar(_) => &rt,
                        _ => &lt,
                    };
                    if let Some(kind) = self.user_eq_kind(probe) {
                        node.metadata.insert(
                            USER_EQ_META_KEY.to_string(),
                            Value::String(kind.to_string()),
                        );
                    }
                }
                result
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
                // §10.4 reserved surface: `Effect.handler(...)`. An effect
                // name is a *type*, not a value, so `Log` in value position
                // would otherwise fall through to a rule-less "undefined
                // variable" error. When the object is an unbound identifier
                // that names a known effect and the accessed member is
                // `handler`, report the actual rule (the lambda-handler
                // form is reserved until v1.x) instead — and suppress the
                // generic E4002 for the effect name (the lowerer's
                // method-call desugar also duplicates it as `args[0]`; see
                // `reported_undefined`).
                if field_name == "handler" {
                    if let NodeKind::FieldAccess { object, .. } = &node.kind {
                        if let NodeKind::Identifier { name } = &object.kind {
                            let effect_name = name.name.clone();
                            if self.env.lookup(&effect_name).is_none()
                                && (self.effect_op_types.contains_key(&effect_name)
                                    || self.effect_components.contains_key(&effect_name))
                            {
                                self.reported_undefined
                                    .insert((effect_name.clone(), object.span));
                                self.diags
                                    .error(
                                        E_RESERVED_LAMBDA_HANDLER,
                                        format!(
                                            "the lambda-handler form `{effect_name}.handler(...)` is reserved until v1.x"
                                        ),
                                        span,
                                    )
                                    .note(format!(
                                        "v1 supports one handler form: declare a record, `impl {effect_name} for <Record>`, then install it with `handle {effect_name} with <record>` (module level) or `handling ({effect_name} with <record>) {{ ... }}` (block level)"
                                    ));
                                return self.record(node, Type::Error);
                            }
                        }
                    }
                }
                let obj_ty = if let NodeKind::FieldAccess { object, .. } = &mut node.kind {
                    self.infer_node(object)
                } else {
                    unreachable!()
                };
                let obj_ty = self.subst.apply(&obj_ty);
                match &obj_ty {
                    Type::Error => Type::Error,
                    Type::Named(nt) => {
                        // Prefer a same-named *field* over a method in bare
                        // value position. A getter method whose name matches a
                        // field (`impl Error for SimpleError { fn message(self)
                        // -> String { self.message } }`) is idiomatic; reading
                        // `self.message` must yield the field's type, not the
                        // method's function type. Method *calls* still resolve
                        // the method type — the `Call` handler resolves a
                        // FieldAccess callee against `method_types` directly
                        // (see `resolve_user_method_fn_type`).
                        if let Some(fields) = self.record_field_types.get(&nt.name) {
                            if let Some((_, field_ty)) =
                                fields.iter().find(|(n, _)| n == &field_name)
                            {
                                return self.record(node, field_ty.clone());
                            }
                        }
                        // Look up method on the named type from inherent impls.
                        // Freshen the method's OWN type params per call site so
                        // they infer from the arguments
                        // (Q-checker-method-generic-call-infer).
                        if let Some(fn_ty) = self
                            .method_types
                            .get(&nt.name)
                            .and_then(|methods| methods.get(&field_name))
                            .cloned()
                        {
                            let resolved =
                                self.freshen_method_type_params(&nt.name, &field_name, fn_ty);
                            return self.record(node, resolved);
                        }
                        self.fresh_var()
                    }
                    Type::Generic(g) => {
                        // User-defined generic type: look up fields/methods by
                        // constructor name, substituting type params. Prefer a
                        // same-named *field* over a method in bare value
                        // position (see the `Named` case above for rationale).
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
                        if let Some(fn_ty) = self
                            .method_types
                            .get(&g.constructor)
                            .and_then(|methods| methods.get(&field_name))
                            .cloned()
                        {
                            // Pin the type's own params (`T`) to the receiver's
                            // concrete args, then freshen the method's OWN params
                            // (`U`) per call site so they infer from the
                            // arguments (Q-checker-method-generic-call-infer).
                            let resolved = if let Some(params) =
                                self.record_generic_params.get(&g.constructor)
                            {
                                substitute_type_params(&fn_ty, params, &g.args)
                            } else {
                                fn_ty
                            };
                            let resolved = self.freshen_method_type_params(
                                &g.constructor,
                                &field_name,
                                resolved,
                            );
                            return self.record(node, resolved);
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
                                if let Some(methods) =
                                    self.trait_method_types.get(trait_name).cloned()
                                {
                                    if let Some(fn_ty) = methods.get(&field_name) {
                                        let resolved =
                                            substitute_type_params(fn_ty, &self_params, &self_args);
                                        return self.record(node, resolved);
                                    }
                                }
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
                    Type::Primitive(_) => {
                        // Q-bridge (#104): consult canonical trait conformances
                        // first so e.g. `(1).compare(2)` types as
                        // `Fn(Int, Int) -> Ordering` rather than the intrinsic
                        // `compare -> Int` fallback. Falls through to the
                        // intrinsic method signatures for non-trait methods
                        // (`abs`, `to_string`, …) or when no conformance is in
                        // scope.
                        if let Some(fn_ty) =
                            self.resolve_primitive_canonical_method_fn_type(&obj_ty, &field_name)
                        {
                            fn_ty
                        } else if let Some(fn_ty) =
                            self.resolve_builtin_method_fn_type(&obj_ty, &field_name)
                        {
                            fn_ty
                        } else {
                            self.fresh_var()
                        }
                    }
                    _ => {
                        // Check built-in method signatures for Generic / Primitive types.
                        if let Some(fn_ty) =
                            self.resolve_builtin_method_fn_type(&obj_ty, &field_name)
                        {
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
                // Q-prim-assoc: a primitive associated-conversion call
                // (`Float.from(3)`, `Int.try_from(s)`) resolves against the
                // canonical primitive conversions, not the ordinary callee path
                // (the primitive type name is not a value binding). Handle it
                // first; `None` means "not such a call", so fall through.
                if let Some(result_ty) = self.try_resolve_primitive_conversion_call(node) {
                    return self.record(node, result_ty);
                }

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
                let mut callee_ty = if let NodeKind::Call { callee, .. } = &mut node.kind {
                    self.infer_node(callee)
                } else {
                    unreachable!()
                };

                // Receiver-type annotation (checker→codegen): a desugared method
                // call is `Call { callee: FieldAccess(recv, method), args:
                // [recv, …] }`. Inferring the callee above recorded the
                // receiver's type in the side-table, so stamp the call node with
                // the receiver category for codegen (see `RECV_KIND_META_KEY`).
                if let NodeKind::FieldAccess { object, field, .. } = &callee_clone.kind {
                    if let Some(recv_ty) = self.types.get(&object.id).cloned() {
                        self.stamp_recv_kind(node, &recv_ty);
                        // The FieldAccess handler prefers a same-named *field*
                        // over a method in value position, so a method call
                        // whose name collides with a field would otherwise see
                        // the (non-callable) field type here. In call-callee
                        // position the method takes precedence: re-resolve the
                        // method's function type from `method_types` and use it.
                        let recv_ty = self.subst.apply(&recv_ty);
                        // DQ22: `contains` is not a `Map` method. Map membership is
                        // `contains_key` (key) / `contains_value` (value); bare
                        // `contains` is `Set`-only (a Set has only elements, so it
                        // is unambiguous there). Reject `m.contains(...)` with a
                        // precise "did you mean `contains_key`?" suggestion rather
                        // than letting the unknown method resolve to a fresh type
                        // variable. NOT aliased to `contains_key`. Handled ahead of
                        // the general unknown-method check so the Map-specific
                        // wording (and the `contains_value` hint) wins.
                        let map_contains = field.name == "contains"
                            && matches!(&recv_ty, Type::Generic(g)
                                if g.constructor == "Map" && g.args.len() == 2);
                        if map_contains {
                            self.diags
                                .error(
                                    E_NO_SUCH_METHOD,
                                    "`contains` is not a method on `Map`; \
                                     did you mean `contains_key`?",
                                    field.span,
                                )
                                .note(
                                    "use `contains_key(k)` to test for a key \
                                     or `contains_value(v)` for a value; bare \
                                     `contains` is a `Set` method",
                                );
                        } else {
                            // Q-checker-unknown-method-concrete: a method that does
                            // not resolve on a concrete, closed-method-set receiver
                            // is an error (with a nearest-name suggestion) — not a
                            // silent fresh type variable. A no-op for §4.9
                            // `Flexible`/sketch receivers, inference vars, and user
                            // types whose definition is not in scope.
                            self.check_unknown_method_on_concrete(
                                &recv_ty,
                                &field.name,
                                field.span,
                            );
                        }
                        if !matches!(callee_ty, Type::Function(_)) {
                            if let Some(fn_ty) =
                                self.resolve_user_method_fn_type(&recv_ty, &field.name)
                            {
                                callee_ty = fn_ty;
                            }
                        }
                    }
                }

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
                let method_span = method.span;
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
                // Receiver-type annotation (checker→codegen): the AIR lowerer
                // desugars most method calls into the `Call(FieldAccess(…))`
                // form, but stamp a surviving `MethodCall` too so the annotation
                // is comprehensive regardless of lowering shape.
                self.stamp_recv_kind(node, &receiver_ty);
                // Q-checker-unknown-method-concrete: flag an unknown method on a
                // concrete receiver here too (mirrors the desugared `Call` path),
                // so a surviving `MethodCall` shape is covered. A no-op for §4.9
                // `Flexible`/sketch and other open receivers.
                self.check_unknown_method_on_concrete(&receiver_ty, &method_name, method_span);
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
                            let part_ty = self.infer_node(e);
                            // A `Bool`-typed `${expr}` part must stringify to the
                            // canonical lowercase `"true"`/`"false"` (§3.5). Python
                            // f-strings would otherwise print `True`/`False`; stamp
                            // the part node so the Python backend lowercases it. The
                            // part's resolved type is not otherwise reachable from
                            // codegen (it lives only in the dropped side-table).
                            if matches!(
                                self.subst.apply(&part_ty),
                                Type::Primitive(PrimitiveType::Bool)
                            ) {
                                e.metadata
                                    .insert(BOOL_STRINGIFY_META_KEY.to_string(), Value::Bool(true));
                            }
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
                // Orientation: the assignment target establishes the
                // expected type; the assigned value is the found type.
                self.unify_or_error(&vty, &tty, span, "assignment");
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
                // Orientation: the low bound establishes the expected type;
                // the high bound is the found type.
                self.unify_or_error(&hty, &lty, span, "range bounds");
                Type::Generic(GenericType {
                    constructor: "Range".into(),
                    args: vec![lty],
                })
            }

            // ── Loops ─────────────────────────────────────────────────────────
            NodeKind::For { .. } => {
                let node_span = node.span;
                // First, infer the iterable so we can classify it. The built-in
                // collections (`List`/`Range`, and `Map`/`Set` element typing
                // below) keep their native fast path; a *user* type that
                // implements `Iterable` is rewritten (§18.5 desugar) into the
                // proven `loop { match it.next() { Some(pat) => body; None =>
                // break } }` shape so it lowers through the already-native
                // loop/match codegen with no per-target `for`-over-user-type
                // support.
                let iter_ty = if let NodeKind::For { iterable, .. } = &mut node.kind {
                    self.infer_node(iterable)
                } else {
                    unreachable!()
                };
                let resolved_iter_ty = self.subst.apply(&iter_ty);

                let is_builtin_iterable = matches!(
                    &resolved_iter_ty,
                    Type::Generic(g)
                        if matches!(g.constructor.as_str(), "List" | "Range" | "Map" | "Set")
                );

                // Desugar only a *user* type (not a built-in collection) that
                // has a registered `Iterable` impl for some type argument.
                if !is_builtin_iterable {
                    let implements_iterable = self
                        .impl_table
                        .as_ref()
                        .map(|table| {
                            let key = crate::traits::type_key(&resolved_iter_ty);
                            resolve_impl(&TraitRef::new("Iterable"), &resolved_iter_ty, table)
                                .is_some()
                                || table.has_any_param_trait_impl("Iterable", &key)
                        })
                        .unwrap_or(false);

                    if implements_iterable {
                        // Move the user's pattern / iterable / body out of the
                        // `For` node into the synthesized subtree.
                        let (pattern, iterable, body) = if let NodeKind::For {
                            pattern,
                            iterable,
                            body,
                        } = &mut node.kind
                        {
                            (
                                std::mem::replace(
                                    pattern,
                                    Box::new(AIRNode::new(0, node_span, NodeKind::Placeholder)),
                                ),
                                std::mem::replace(
                                    iterable,
                                    Box::new(AIRNode::new(0, node_span, NodeKind::Placeholder)),
                                ),
                                std::mem::replace(
                                    body,
                                    Box::new(AIRNode::new(0, node_span, NodeKind::Placeholder)),
                                ),
                            )
                        } else {
                            unreachable!()
                        };

                        // Gensym a unique binding name so nested desugared `for`
                        // loops do not shadow one another.
                        let n = self.synth_iter_var.get();
                        self.synth_iter_var.set(n.wrapping_add(1));
                        let iter_var = format!("__bock_iter_{n}");

                        self.desugar_for_iterable(node, *pattern, *iterable, *body, &iter_var);
                        // Re-infer the rewritten subtree (now a `Block`) through
                        // the normal path, so the synthesized `match`/`Some(pat)`
                        // / method-call nodes pick up the element typing and the
                        // codegen metadata (Optional payload, receiver kind).
                        return self.infer_block(node);
                    }
                }

                // Built-in / fallback path: element-type the loop variable from
                // the iterable's generic argument (List/Range/Map/Set), else a
                // fresh var, exactly as before.
                self.env.push_scope();
                if let NodeKind::For {
                    pattern,
                    iterable: _,
                    body,
                } = &mut node.kind
                {
                    let elem_ty = match &resolved_iter_ty {
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
                    // §10.4 bare-op form: inject the handled effects' operation
                    // types into a fresh env scope so a bare op call inside the
                    // block (`log(...)`) type-checks. Mirrors the resolver's
                    // op injection in `resolve_handling`. The scope is popped
                    // after the body so the ops do not leak past the block.
                    let effect_names: Vec<String> = handlers
                        .iter()
                        .map(|hp| type_path_to_name(&hp.effect))
                        .collect();
                    self.env.push_scope();
                    let mut visited = std::collections::HashSet::new();
                    for ename in &effect_names {
                        self.inject_effect_ops_into_env(ename, &mut visited);
                    }
                    let ty = self.infer_node(body);
                    self.env.pop_scope();
                    ty
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

            // ── Check mode for `.into()` (return-type-driven conversion) ──────
            // A `receiver.into()` call lowers to
            // `Call { callee: FieldAccess(receiver, "into"), args: [self] }`.
            // In check mode the target type `U` comes from the expected type:
            // we look up the blanket/explicit `Into[U] for A` impl (where `A`
            // is the receiver type). On success the call's type is exactly `U`;
            // on failure we emit `E4012`. This is the inline resolution hook —
            // no obligation queue. If the expected type is not yet concrete
            // (no reachable annotation) we fall through to ordinary inference,
            // which keeps `.into()` usable only where a target type is known
            // (the documented v1 annotation-required limitation).
            NodeKind::Call { callee, args, .. }
                if args.len() == 1
                    && matches!(
                        &callee.kind,
                        NodeKind::FieldAccess { field, .. } if field.name == "into"
                    ) =>
            {
                let target = self.subst.apply(expected);
                // Infer the receiver (the desugared `self` argument).
                let receiver_ty = if let NodeKind::Call { args, .. } = &mut node.kind {
                    self.infer_node(&mut args[0].value)
                } else {
                    unreachable!()
                };
                let receiver_ty = self.subst.apply(&receiver_ty);

                // Only attempt conversion resolution when both the target and
                // the receiver are concrete enough to key the impl table. A
                // type-variable target means no reachable annotation — fall
                // through to generic inference.
                let resolvable = !matches!(target, Type::TypeVar(_) | Type::Error)
                    && !matches!(receiver_ty, Type::TypeVar(_) | Type::Error);
                if resolvable {
                    if let Some(table) = self.impl_table.as_ref() {
                        let trait_ref = TraitRef::parameterized("Into", vec![target.clone()]);
                        if resolve_impl(&trait_ref, &receiver_ty, table).is_some() {
                            self.record(node, target.clone());
                            return;
                        }
                        // No matching conversion: emit a precise diagnostic.
                        self.diags.error(
                            E_NO_CONVERSION,
                            format!(
                                "cannot convert `{}` into `{}` via `.into()`: no `From`/`Into`                                  impl relates these types",
                                crate::traits::type_key(&receiver_ty),
                                crate::traits::type_key(&target),
                            ),
                            span,
                        );
                        self.record(node, target.clone());
                        return;
                    }
                }
                // Fall through: no impl table or target not reachable.
                let inferred = self.infer_node(node);
                let expected = self.subst.apply(expected);
                self.unify_or_error(&inferred, &expected, span, "expression");
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
                // Orientation: the first (non-diverging) branch establishes
                // the expected type; the other branch is the found type.
                self.unify_or_error(b, a, span, "if-else branches")
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

    /// Resolve a primitive method-call return type via a *canonical* trait
    /// conformance, if one applies.
    ///
    /// Q-bridge (#104): primitives gain trait methods (`compare`, `eq`, …)
    /// through compiler-registered canonical conformances in `impl_table`.
    /// This helper fires only when **all** of the following hold:
    ///
    /// 1. the receiver is a primitive (checked by the caller),
    /// 2. some in-scope trait (in `trait_method_types`) declares `method`,
    /// 3. a canonical conformance for that trait is registered for the
    ///    receiver in `impl_table`.
    ///
    /// When matched, returns the trait method's declared return type with the
    /// `Self` type mapped to the concrete receiver. Returns `None` (fall
    /// through to the intrinsic arms) when no such conformance is in scope —
    /// preserving behavior for code that never imports the core trait.
    fn resolve_primitive_canonical_method_return(
        &self,
        receiver_ty: &Type,
        method: &str,
    ) -> Option<Type> {
        let impl_table = self.impl_table.as_ref()?;

        // Find an in-scope trait that declares `method` AND has a canonical
        // conformance registered for this receiver. Iterating `trait_method_types`
        // keeps the lookup gated on the trait actually being imported (cond. 2/3).
        for (trait_name, methods) in &self.trait_method_types {
            let Some(Type::Function(fn_ty)) = methods.get(method) else {
                continue;
            };
            let trait_ref = TraitRef::new(trait_name);
            if resolve_impl(&trait_ref, receiver_ty, impl_table).is_none() {
                continue;
            }
            // Map the trait method's declared return type, substituting the
            // `Self` placeholder with the concrete receiver type. The return
            // type is otherwise already concrete (e.g. `Ordering`, `Bool`).
            let self_params = ["Self".to_string()];
            let self_args = [receiver_ty.clone()];
            return Some(substitute_type_params(&fn_ty.ret, &self_params, &self_args));
        }
        None
    }

    /// Resolve the full *function* type of a primitive method call via a
    /// canonical trait conformance, if one applies.
    ///
    /// Q-bridge (#104): the AIR lowers `(1).compare(2)` to
    /// `Call(FieldAccess(1, "compare"), [1, 2])`, so the `FieldAccess` handler
    /// resolves the method's whole function type (receiver as the first
    /// parameter). This mirrors [`Self::resolve_primitive_canonical_method_return`]
    /// but returns the full `Fn(Self, …) -> Ret` type with every `Self`
    /// occurrence (params *and* return) mapped to the concrete receiver — so
    /// `(1).compare(2)` types as `Fn(Int, Int) -> Ordering` and the call
    /// yields `Ordering`, matchable against its variants.
    ///
    /// Gating matches the return-type helper: fires only when the receiver is
    /// primitive, an in-scope trait declares `method`, and a canonical
    /// conformance for that trait is registered for the receiver. Falls
    /// through (returns `None`) otherwise, preserving the intrinsic fast path.
    fn resolve_primitive_canonical_method_fn_type(
        &self,
        receiver_ty: &Type,
        method: &str,
    ) -> Option<Type> {
        let impl_table = self.impl_table.as_ref()?;
        for (trait_name, methods) in &self.trait_method_types {
            let Some(fn_ty @ Type::Function(_)) = methods.get(method) else {
                continue;
            };
            let trait_ref = TraitRef::new(trait_name);
            if resolve_impl(&trait_ref, receiver_ty, impl_table).is_none() {
                continue;
            }
            let self_params = ["Self".to_string()];
            let self_args = [receiver_ty.clone()];
            return Some(substitute_type_params(fn_ty, &self_params, &self_args));
        }
        None
    }

    /// Resolve the full *function* type of a user-defined method (registered in
    /// `method_types`) on a receiver type, with the type's generic params
    /// substituted to the receiver's concrete arguments.
    ///
    /// Used by the `Call` handler so a method *call* whose name collides with a
    /// same-named record field still resolves the method (the `FieldAccess`
    /// handler prefers the field in bare value position; this restores the
    /// method type when the FieldAccess is a call callee).
    ///
    /// The method's OWN type parameters (e.g. the `U` in
    /// `Box[T].map[U](f: Fn(T) -> U) -> Box[U]`) are replaced with *fresh*
    /// inference variables per call site, so they are inferred from the call
    /// arguments — the method-level analogue of free-function call inference
    /// (Q-checker-method-generic-call-infer). The receiver pins the type's own
    /// params (`T`); only the method's own params (`U`) are freshened here.
    fn resolve_user_method_fn_type(&self, receiver_ty: &Type, method: &str) -> Option<Type> {
        let receiver_ty = self.subst.apply(receiver_ty);
        let (type_name, fn_ty) = match &receiver_ty {
            Type::Named(nt) => {
                let fn_ty = self
                    .method_types
                    .get(&nt.name)
                    .and_then(|m| m.get(method))
                    .cloned()?;
                (nt.name.clone(), fn_ty)
            }
            Type::Generic(g) => {
                let fn_ty = self
                    .method_types
                    .get(&g.constructor)
                    .and_then(|m| m.get(method))
                    .cloned()?;
                // Pin the type's own params (`T`) to the receiver's concrete args.
                let fn_ty = if let Some(params) = self.record_generic_params.get(&g.constructor) {
                    substitute_type_params(&fn_ty, params, &g.args)
                } else {
                    fn_ty
                };
                (g.constructor.clone(), fn_ty)
            }
            _ => return None,
        };
        Some(self.freshen_method_type_params(&type_name, method, fn_ty))
    }

    /// Q-prim-assoc: resolve the **primitive** associated-conversion call form
    /// `Prim.from(x)` / `Prim.try_from(x)` (e.g. `Float.from(3)`,
    /// `Int.try_from(s)`), returning the call's result type when it resolves
    /// against a canonical primitive conversion.
    ///
    /// The lowerer represents `Type.method(args)` as a `Call` whose callee is
    /// `FieldAccess(Identifier(Type), method)` stamped with
    /// [`bock_air::lower::ASSOC_CALL_META_KEY`] (no `self` prepended). For a
    /// *user* type the `Identifier(Type)` infers to a `Named` type and the
    /// `FieldAccess`/`method_types` path resolves the impl's `from`/`try_from`.
    /// A *primitive* type name (`Int`/`Float`/`String`/`Char`/…) is not bound
    /// in the value env, so that path would emit `E4002 undefined variable`.
    /// This hook intercepts those calls and resolves them against the canonical
    /// primitive conversions registered by
    /// [`crate::traits::register_canonical_conversions`]:
    ///
    /// - `Prim.from(x)` resolves `From[typeof(x)] for Prim` and yields `Prim`.
    /// - `Prim.try_from(x)` resolves `TryFrom[typeof(x)] for Prim` and yields
    ///   `Result[Prim, ConvertError]`.
    ///
    /// Returns `Some(result_ty)` on a successful resolution (after inferring the
    /// argument so its node is typed for codegen). Returns `None` when the call
    /// is not a primitive associated `from`/`try_from` (let the ordinary Call
    /// path handle it). When the callee *is* a primitive `from`/`try_from` but
    /// no canonical conversion relates the argument type to the target, emits
    /// `E4012` and returns `Some(Type::Error)` so the call is not double-reported
    /// by the generic path.
    fn try_resolve_primitive_conversion_call(&mut self, node: &mut AIRNode) -> Option<Type> {
        if !is_associated_call_node(node) {
            return None;
        }
        // Destructure the callee shape: FieldAccess(Identifier(P), method).
        let (target_prim, method, method_span) = {
            let NodeKind::Call { callee, .. } = &node.kind else {
                return None;
            };
            let NodeKind::FieldAccess { object, field } = &callee.kind else {
                return None;
            };
            let NodeKind::Identifier { name } = &object.kind else {
                return None;
            };
            let prim = name_to_primitive(&name.name)?;
            let method = field.name.clone();
            if method != "from" && method != "try_from" {
                return None;
            }
            (prim, method, field.span)
        };
        let target_ty = Type::Primitive(target_prim);

        // Infer the sole conversion argument (its node must be typed for codegen).
        let arg_ty = {
            let NodeKind::Call { args, .. } = &mut node.kind else {
                return None;
            };
            if args.len() != 1 {
                // A primitive `from`/`try_from` takes exactly one source value.
                return None;
            }
            self.infer_node(&mut args[0].value)
        };
        let arg_ty = self.subst.apply(&arg_ty);

        // An unresolved / error argument can't key the impl table; defer to the
        // generic path rather than risk a spurious E4012.
        if matches!(arg_ty, Type::TypeVar(_) | Type::Error) {
            return None;
        }

        let trait_name = if method == "from" { "From" } else { "TryFrom" };
        let resolves = self
            .impl_table
            .as_ref()
            .map(|table| {
                let trait_ref = TraitRef::parameterized(trait_name, vec![arg_ty.clone()]);
                resolve_impl(&trait_ref, &target_ty, table).is_some()
            })
            .unwrap_or(false);

        if resolves {
            let result_ty = if method == "from" {
                target_ty
            } else {
                // `TryFrom::try_from` returns `Result[Self, ConvertError]`.
                Type::Result(
                    Box::new(target_ty),
                    Box::new(Type::Named(crate::NamedType {
                        name: "ConvertError".to_string(),
                    })),
                )
            };
            return Some(result_ty);
        }

        // The callee is a primitive `from`/`try_from`, but no canonical
        // conversion relates the argument type to the target primitive. Reject
        // cleanly with `E4012` (mirrors the `.into()` no-conversion diagnostic).
        self.diags.error(
            E_NO_CONVERSION,
            format!(
                "cannot convert `{}` to `{}` via `{}.{}()`: no canonical `{}` \
                 conversion relates these types",
                crate::traits::type_key(&arg_ty),
                crate::traits::type_key(&target_ty),
                crate::traits::type_key(&target_ty),
                method,
                trait_name,
            ),
            method_span,
        );
        Some(Type::Error)
    }

    /// Replace a method's OWN generic type parameters with fresh inference
    /// variables (Q-checker-method-generic-call-infer).
    ///
    /// A method like `fn map[U](...)` registers its own param names (`["U"]`) in
    /// `method_generic_params`. Those names survive in the stored method type as
    /// `Named("U")` placeholders. At each call site they must become *fresh*
    /// inference variables so the method's own params unify against the call
    /// arguments independently per call — exactly as `instantiate_and_check`
    /// freshens a free function's type params. The receiver has already pinned
    /// the type's own params before this runs, so only the method's own params
    /// remain to be freshened.
    fn freshen_method_type_params(&self, type_name: &str, method: &str, fn_ty: Type) -> Type {
        let Some(names) = self
            .method_generic_params
            .get(type_name)
            .and_then(|m| m.get(method))
        else {
            return fn_ty;
        };
        if names.is_empty() {
            return fn_ty;
        }
        let fresh: Vec<Type> = names.iter().map(|_| self.fresh_var()).collect();
        substitute_type_params(&fn_ty, names, &fresh)
    }

    /// Conversion methods that are resolved by dedicated machinery (the
    /// `.into()` inline hook and the `From`/`TryFrom` impl table), *not* the
    /// per-receiver built-in method matches. The unknown-method check must
    /// never flag these — they legitimately resolve on receivers whose closed
    /// method set does not list them.
    const CONVERSION_METHODS: &'static [&'static str] = &["into", "from", "try_from"];

    /// Q-checker-unknown-method-concrete: returns `true` when `method` resolves
    /// on `receiver_ty` through *any* path the checker knows — built-in
    /// intrinsics, canonical primitive trait conformances, user inherent/trait
    /// impls (`method_types`), record/class field-closures (a `field()` call),
    /// or the conversion hooks. Used to decide whether an unknown-method
    /// diagnostic is warranted on a concrete receiver.
    fn method_is_resolvable(&self, receiver_ty: &Type, method: &str) -> bool {
        let receiver_ty = self.subst.apply(receiver_ty);

        // Conversion methods resolve through dedicated machinery.
        if Self::CONVERSION_METHODS.contains(&method) {
            return true;
        }

        // Built-in intrinsic method (List/Map/Set/String/Int/…/Optional/Result).
        if self
            .resolve_builtin_method_fn_type(&receiver_ty, method)
            .is_some()
        {
            return true;
        }

        // Primitive canonical-trait conformance (`compare`/`eq`/`to_string`/…),
        // gated on the trait actually being in scope.
        if matches!(receiver_ty, Type::Primitive(_))
            && self
                .resolve_primitive_canonical_method_fn_type(&receiver_ty, method)
                .is_some()
        {
            return true;
        }

        // User type: inherent/trait-impl method, or a same-named field-closure.
        let user_name = match &receiver_ty {
            Type::Named(nt) => Some(&nt.name),
            Type::Generic(g) => Some(&g.constructor),
            _ => None,
        };
        if let Some(name) = user_name {
            if self
                .method_types
                .get(name)
                .is_some_and(|m| m.contains_key(method))
            {
                return true;
            }
            if self
                .record_field_types
                .get(name)
                .is_some_and(|fs| fs.iter().any(|(n, _)| n == method))
            {
                return true;
            }
        }

        // Trait *default* methods: a concrete type that implements a trait
        // inherits every default method the trait declares but the impl did not
        // override. Such methods live in `trait_method_types` (the trait's
        // signatures), not in the type's `method_types`, so check every trait
        // the receiver implements — and that trait's supertraits — for a
        // declaration of `method`. This keeps inherited defaults (e.g.
        // `Eq::not_equals` calling the required `equals`) resolvable.
        if self.type_implements_trait_method(&receiver_ty, method) {
            return true;
        }

        false
    }

    /// Q-checker-unknown-method-concrete: returns `true` when `receiver_ty`
    /// implements some trait (directly, or via a supertrait of one it
    /// implements) that declares `method` in [`Self::trait_method_types`]. This
    /// covers inherited trait *default* methods, which are not registered in the
    /// type's own `method_types`.
    fn type_implements_trait_method(&self, receiver_ty: &Type, method: &str) -> bool {
        let Some(table) = self.impl_table.as_ref() else {
            return false;
        };
        let key = crate::traits::type_key(receiver_ty);
        for entry in table.entries() {
            if entry.type_key != key {
                continue;
            }
            let Some(trait_ref) = &entry.trait_ref else {
                continue;
            };
            // The directly-implemented trait, plus its supertraits.
            if self
                .trait_method_types
                .get(&trait_ref.name)
                .is_some_and(|m| m.contains_key(method))
            {
                return true;
            }
            for supertrait in table.all_supertraits(&trait_ref.name) {
                if self
                    .trait_method_types
                    .get(&supertrait)
                    .is_some_and(|m| m.contains_key(method))
                {
                    return true;
                }
            }
        }
        false
    }

    /// Q-checker-unknown-method-concrete: the candidate method names for a
    /// **concrete, closed-method-set** receiver — used both to gate the
    /// unknown-method diagnostic (a `None` result means the receiver is not a
    /// closed concrete type, so no diagnostic) and to compute a nearest-name
    /// suggestion.
    ///
    /// Returns `None` for receivers whose method set is *open* or not fully
    /// known at this point:
    /// - `Type::TypeVar` — an unresolved inference variable; methods may resolve
    ///   once it is unified (and bounded-trait methods apply).
    /// - `Type::Flexible` — §4.9 sketch-mode narrowing resolves methods
    ///   aggressively by design; the diagnostic must never leak here.
    /// - `Type::Error` — poison; already diagnosed.
    /// - `Type::Function` / `Type::Tuple` / `Type::Refined` — no method surface.
    /// - a `Named`/`Generic` user type whose definition is not in scope (no
    ///   `record_field_types`/`method_types` entry) — its method set is unknown,
    ///   so suppress rather than risk a false positive.
    fn concrete_closed_method_names(&self, receiver_ty: &Type) -> Option<Vec<String>> {
        let receiver_ty = self.subst.apply(receiver_ty);
        match &receiver_ty {
            // Closed built-in receivers.
            Type::Primitive(p) if !matches!(p, PrimitiveType::Void | PrimitiveType::Never) => {
                let mut names = self.builtin_method_names(&receiver_ty);
                // Canonical primitive trait methods that are in scope.
                for methods in self.trait_method_types.values() {
                    for m in methods.keys() {
                        if self
                            .resolve_primitive_canonical_method_fn_type(&receiver_ty, m)
                            .is_some()
                        {
                            names.push(m.clone());
                        }
                    }
                }
                Some(names)
            }
            Type::Optional(_) | Type::Result(_, _) => Some(self.builtin_method_names(&receiver_ty)),
            Type::Generic(g) if matches!(g.constructor.as_str(), "List" | "Map" | "Set") => {
                let mut names = self.builtin_method_names(&receiver_ty);
                // A user `impl` on a built-in generic (rare) contributes too.
                if let Some(m) = self.method_types.get(&g.constructor) {
                    names.extend(m.keys().cloned());
                }
                Some(names)
            }
            // Known user types (definition in scope): the closed set is the
            // registered methods plus the record/class fields.
            Type::Named(nt) => {
                if !self.record_field_types.contains_key(&nt.name)
                    && !self.method_types.contains_key(&nt.name)
                {
                    return None;
                }
                let mut names: Vec<String> = self
                    .method_types
                    .get(&nt.name)
                    .map(|m| m.keys().cloned().collect())
                    .unwrap_or_default();
                if let Some(fs) = self.record_field_types.get(&nt.name) {
                    names.extend(fs.iter().map(|(n, _)| n.clone()));
                }
                Some(names)
            }
            Type::Generic(g) => {
                if !self.record_field_types.contains_key(&g.constructor)
                    && !self.method_types.contains_key(&g.constructor)
                {
                    return None;
                }
                let mut names: Vec<String> = self
                    .method_types
                    .get(&g.constructor)
                    .map(|m| m.keys().cloned().collect())
                    .unwrap_or_default();
                if let Some(fs) = self.record_field_types.get(&g.constructor) {
                    names.extend(fs.iter().map(|(n, _)| n.clone()));
                }
                Some(names)
            }
            // Open / non-concrete receivers — never flag.
            _ => None,
        }
    }

    /// The built-in (intrinsic) method names for a closed built-in receiver,
    /// drawn from the union of the two intrinsic resolution tables (some methods
    /// live only in the return-type table — e.g. `display` — and some only in
    /// the fn-type table — e.g. `map`/`fold`/`zip`).
    fn builtin_method_names(&self, receiver_ty: &Type) -> Vec<String> {
        const ALL_BUILTIN_METHODS: &[&str] = &[
            // collections / iteration
            "len",
            "length",
            "count",
            "byte_len",
            "is_empty",
            "contains",
            "contains_key",
            "first",
            "last",
            "find",
            "get",
            "index_of",
            "push",
            "append",
            "pop",
            "insert",
            "remove",
            "concat",
            "clear",
            "reverse",
            "sort",
            "dedup",
            "flatten",
            "take",
            "skip",
            "slice",
            "filter",
            "map",
            "map_values",
            "flat_map",
            "fold",
            "reduce",
            "for_each",
            "any",
            "all",
            "enumerate",
            "zip",
            "join",
            "to_set",
            "to_list",
            "keys",
            "values",
            "entries",
            "set",
            "delete",
            "merge",
            "add",
            "union",
            "intersection",
            "difference",
            "symmetric_difference",
            "is_subset",
            "is_superset",
            "is_disjoint",
            // string
            "starts_with",
            "ends_with",
            "regex_match",
            "to_upper",
            "to_lower",
            "trim",
            "trim_start",
            "trim_end",
            "substring",
            "replace",
            "repeat",
            "pad_start",
            "pad_end",
            "format",
            "regex_replace",
            "regex_find",
            "split",
            "chars",
            "bytes",
            "char_at",
            // scalar
            "abs",
            "min",
            "max",
            "clamp",
            "shift_left",
            "shift_right",
            "to_float",
            "to_int",
            "floor",
            "ceil",
            "round",
            "sqrt",
            "is_nan",
            "is_infinite",
            "negate",
            "is_alpha",
            "is_digit",
            "is_whitespace",
            "compare",
            "hash_code",
            "equals",
            "to_string",
            "display",
            // optional / result
            "is_some",
            "is_none",
            "unwrap",
            "unwrap_or",
            "is_ok",
            "is_err",
            "map_err",
        ];
        ALL_BUILTIN_METHODS
            .iter()
            .filter(|m| self.method_is_resolvable(receiver_ty, m))
            .map(|m| (*m).to_string())
            .collect()
    }

    /// Q-checker-unknown-method-concrete: emit `E4013` when `method` does not
    /// resolve on a **concrete, closed-method-set** receiver, with a nearest-name
    /// suggestion when one exists. A no-op for open / non-concrete receivers
    /// (inference vars, §4.9 `Flexible` sketch types, the `Error` sentinel, and
    /// user types whose definition is not in scope).
    ///
    /// `span` should be the method-name span so the diagnostic underlines the
    /// offending method.
    fn check_unknown_method_on_concrete(&mut self, receiver_ty: &Type, method: &str, span: Span) {
        // Conversion methods resolve elsewhere; never flag.
        if Self::CONVERSION_METHODS.contains(&method) {
            return;
        }
        // Resolves through some path → fine.
        if self.method_is_resolvable(receiver_ty, method) {
            return;
        }
        // Only flag concrete, closed-method-set receivers.
        let Some(candidates) = self.concrete_closed_method_names(receiver_ty) else {
            return;
        };

        let receiver_ty = self.subst.apply(receiver_ty);
        let recv_desc = describe_receiver_type(&receiver_ty);
        let diag = self.diags.error(
            E_NO_SUCH_METHOD,
            format!("no method `{method}` on `{recv_desc}`"),
            span,
        );
        if let Some(suggestion) = nearest_method_name(method, &candidates) {
            diag.note(format!("did you mean `{suggestion}`?"));
        }
    }

    /// Resolve the return type of a method call on a known receiver type.
    ///
    /// Returns a concrete type when the receiver type and method name
    /// identify a well-known built-in method; falls back to a fresh type
    /// variable otherwise.
    fn resolve_method_return_type(&self, receiver_ty: &Type, method: &str) -> Type {
        let receiver_ty = self.subst.apply(receiver_ty);

        // Q-bridge (#104): for a primitive receiver, consult the canonical
        // trait conformances registered in `impl_table` *before* the intrinsic
        // `match`. If a registered conformance's trait declares `method`,
        // return that trait method's declared return type (with `Self` mapped
        // to the concrete receiver). This makes e.g. `(1).compare(2)` resolve
        // to `Ordering` (not the intrinsic `Int` fallback) and `a.eq(b)` to
        // `Bool`, uniformly with user types. Non-trait intrinsics (`abs`,
        // `to_string`, …) and code that never imports the core trait fall
        // through to the intrinsic arms below.
        if matches!(receiver_ty, Type::Primitive(_)) {
            if let Some(ty) = self.resolve_primitive_canonical_method_return(&receiver_ty, method) {
                return ty;
            }
        }

        match &receiver_ty {
            Type::Error => Type::Error,
            // List[T] methods
            Type::Generic(g) if g.constructor == "List" && g.args.len() == 1 => {
                let elem_ty = &g.args[0];
                match method {
                    "len" | "length" | "count" => Type::Primitive(PrimitiveType::Int),
                    "first" | "last" | "find" | "get" => Type::Optional(Box::new(elem_ty.clone())),
                    "index_of" => Type::Optional(Box::new(Type::Primitive(PrimitiveType::Int))),
                    "contains" | "is_empty" | "any" | "all" => Type::Primitive(PrimitiveType::Bool),
                    // DQ18: `push`/`append` are in-place mutators — they require
                    // a `mut` receiver (enforced in `ownership.rs`) and return
                    // `Void`. Functional list-building stays on `+`/`concat`.
                    "push" | "append" => Type::Primitive(PrimitiveType::Void),
                    "pop" | "insert" | "remove" | "concat" | "reverse" | "sort" | "filter"
                    | "dedup" | "take" | "skip" | "flat_map" | "slice" | "flatten" => {
                        receiver_ty.clone()
                    }
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
                "len" | "length" | "count" | "byte_len" => Type::Primitive(PrimitiveType::Int),
                "contains" | "starts_with" | "ends_with" | "is_empty" | "regex_match" => {
                    Type::Primitive(PrimitiveType::Bool)
                }
                "to_upper" | "to_lower" | "trim" | "trim_start" | "trim_end" | "reverse"
                | "slice" | "substring" | "replace" | "to_string" | "display" | "repeat"
                | "pad_start" | "pad_end" | "format" | "regex_replace" | "join" => {
                    Type::Primitive(PrimitiveType::String)
                }
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
                "char_at" => Type::Optional(Box::new(Type::Primitive(PrimitiveType::Char))),
                _ => self.fresh_var(),
            },
            // Int methods
            Type::Primitive(PrimitiveType::Int) => match method {
                "abs" | "min" | "max" | "clamp" | "shift_left" | "shift_right" | "compare"
                | "hash_code" => Type::Primitive(PrimitiveType::Int),
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
                    "contains" | "is_empty" | "is_subset" | "is_superset" | "is_disjoint" => {
                        Type::Primitive(PrimitiveType::Bool)
                    }
                    "add"
                    | "remove"
                    | "union"
                    | "intersection"
                    | "difference"
                    | "symmetric_difference"
                    | "filter"
                    | "map" => receiver_ty.clone(),
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
                    "len" | "length" | "count" => {
                        mk(r, vec![], Type::Primitive(PrimitiveType::Int))
                    }
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
                    // DQ18: `push`/`append` mutate in place and return `Void`.
                    "push" | "append" => {
                        mk(r, vec![elem.clone()], Type::Primitive(PrimitiveType::Void))
                    }
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
                    "reverse" | "sort" | "dedup" | "flatten" => mk(r, vec![], receiver_ty.clone()),
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
                        let pair =
                            Type::Tuple(vec![Type::Primitive(PrimitiveType::Int), elem.clone()]);
                        mk(
                            r,
                            vec![],
                            Type::Generic(GenericType {
                                constructor: "List".into(),
                                args: vec![pair],
                            }),
                        )
                    }
                    "zip" => {
                        let f = self.fresh_var();
                        let other_list = Type::Generic(GenericType {
                            constructor: "List".into(),
                            args: vec![f.clone()],
                        });
                        let pair = Type::Tuple(vec![elem.clone(), f]);
                        mk(
                            r,
                            vec![other_list],
                            Type::Generic(GenericType {
                                constructor: "List".into(),
                                args: vec![pair],
                            }),
                        )
                    }
                    "join" => mk(
                        r,
                        vec![Type::Primitive(PrimitiveType::String)],
                        Type::Primitive(PrimitiveType::String),
                    ),
                    "to_set" => mk(
                        r,
                        vec![],
                        Type::Generic(GenericType {
                            constructor: "Set".into(),
                            args: vec![elem.clone()],
                        }),
                    ),
                    _ => None,
                }
            }
            Type::Generic(g) if g.constructor == "Map" && g.args.len() == 2 => {
                let key = &g.args[0];
                let val = &g.args[1];
                let r = &receiver_ty;
                match method {
                    "len" | "length" | "count" => {
                        mk(r, vec![], Type::Primitive(PrimitiveType::Int))
                    }
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
                        mk(
                            r,
                            vec![cb],
                            Type::Generic(GenericType {
                                constructor: "Map".into(),
                                args: vec![key.clone(), u],
                            }),
                        )
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
                    "len" | "length" | "count" => {
                        mk(r, vec![], Type::Primitive(PrimitiveType::Int))
                    }
                    "is_empty" => mk(r, vec![], Type::Primitive(PrimitiveType::Bool)),
                    "contains" => mk(r, vec![elem.clone()], Type::Primitive(PrimitiveType::Bool)),
                    "add" | "remove" => mk(r, vec![elem.clone()], receiver_ty.clone()),
                    "union" | "intersection" | "difference" | "symmetric_difference" => {
                        mk(r, vec![receiver_ty.clone()], receiver_ty.clone())
                    }
                    "is_subset" | "is_superset" | "is_disjoint" => mk(
                        r,
                        vec![receiver_ty.clone()],
                        Type::Primitive(PrimitiveType::Bool),
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
                    "to_list" => mk(
                        r,
                        vec![],
                        Type::Generic(GenericType {
                            constructor: "List".into(),
                            args: vec![elem.clone()],
                        }),
                    ),
                    _ => None,
                }
            }
            Type::Primitive(PrimitiveType::String) => {
                let r = &receiver_ty;
                let str_ty = Type::Primitive(PrimitiveType::String);
                let int_ty = Type::Primitive(PrimitiveType::Int);
                match method {
                    "len" | "length" | "count" | "byte_len" => mk(r, vec![], int_ty),
                    "is_empty" => mk(r, vec![], Type::Primitive(PrimitiveType::Bool)),
                    "contains" | "starts_with" | "ends_with" => mk(
                        r,
                        vec![str_ty.clone()],
                        Type::Primitive(PrimitiveType::Bool),
                    ),
                    "regex_match" => mk(
                        r,
                        vec![str_ty.clone()],
                        Type::Primitive(PrimitiveType::Bool),
                    ),
                    "to_upper" | "to_lower" | "trim" | "trim_start" | "trim_end" | "reverse"
                    | "to_string" | "display" => mk(r, vec![], str_ty),
                    "repeat" => mk(r, vec![Type::Primitive(PrimitiveType::Int)], str_ty),
                    "slice" | "substring" => mk(
                        r,
                        vec![
                            Type::Primitive(PrimitiveType::Int),
                            Type::Primitive(PrimitiveType::Int),
                        ],
                        str_ty,
                    ),
                    "replace" | "regex_replace" => {
                        mk(r, vec![str_ty.clone(), str_ty.clone()], str_ty)
                    }
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
                    "min" | "max" | "shift_left" | "shift_right" | "compare" => {
                        mk(r, vec![int_ty.clone()], int_ty)
                    }
                    "clamp" => mk(r, vec![int_ty.clone(), int_ty.clone()], int_ty),
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
                    "abs" | "floor" | "ceil" | "round" | "sqrt" => mk(r, vec![], float_ty),
                    "min" | "max" => mk(r, vec![float_ty.clone()], float_ty),
                    "clamp" => mk(r, vec![float_ty.clone(), float_ty.clone()], float_ty),
                    "to_int" => mk(r, vec![], Type::Primitive(PrimitiveType::Int)),
                    "to_string" | "display" => {
                        mk(r, vec![], Type::Primitive(PrimitiveType::String))
                    }
                    "is_nan" | "is_infinite" | "equals" => {
                        mk(r, vec![], Type::Primitive(PrimitiveType::Bool))
                    }
                    "compare" | "hash_code" => mk(r, vec![], Type::Primitive(PrimitiveType::Int)),
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
                    "is_some" | "is_none" => mk(r, vec![], Type::Primitive(PrimitiveType::Bool)),
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
                    "is_ok" | "is_err" => mk(r, vec![], Type::Primitive(PrimitiveType::Bool)),
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

    /// §18.5 operator gating: require a `<`/`>`/`<=`/`>=` operand to be
    /// `Comparable`.
    ///
    /// The gate fires only for a **user** (`Type::Named`) operand whose type is
    /// resolved and provably *not* `Comparable` in the current `impl_table`. It
    /// is intentionally conservative everywhere else:
    ///
    /// - **No `impl_table`** (e.g. a unit-test checker, or pre-module setup):
    ///   skipped, mirroring the `where`-clause bound check, which cannot prove
    ///   non-conformance without the table.
    /// - **Inference variables / `Flexible` / `Error`:** skipped — the operand
    ///   type is not yet concrete, so a bounded generic param (`T: Comparable`)
    ///   reaches `compare` via its where-clause obligation, not this gate.
    /// - **Primitives / generics / tuples / functions:** the canonical
    ///   conformances registered in `impl_table` decide; a primitive that *is*
    ///   `Comparable` (Int, Float, String, Char, sized numerics) passes, and one
    ///   that is not (e.g. `Bool`) is rejected here, matching §18.5's matrix.
    ///
    /// On failure it emits [`E_WHERE_CLAUSE`] — the trait-bound error code —
    /// with a message suggesting `impl Comparable`.
    fn require_comparable_operand(&mut self, operand: &Type, span: Span) {
        let resolved = self.subst.apply(operand);
        // Only gate concrete operands; leave inference vars / sketch types /
        // poison untouched so bounded generics and error recovery are unharmed.
        match &resolved {
            Type::TypeVar(_) | Type::Flexible(_) | Type::Error => return,
            _ => {}
        }
        let impl_table = match self.impl_table.as_ref() {
            Some(t) => t,
            None => return, // no table → cannot prove non-conformance.
        };
        let trait_ref = TraitRef::new("Comparable");
        if resolve_impl(&trait_ref, &resolved, impl_table).is_none() {
            let key = crate::traits::type_key(&resolved);
            // A primitive that is not `Comparable` (only `Bool`, per the §18.5
            // sealed-conformance matrix) cannot be given an `impl` — core trait
            // conformances for primitives are sealed — so point at the newtype
            // escape hatch instead of suggesting an impossible `impl`.
            let suggestion = if matches!(resolved, Type::Primitive(_)) {
                format!(
                    "`{key}` is not `Comparable` (the `(core trait, primitive)` \
                     conformances are sealed); wrap it in a newtype with its own \
                     `impl Comparable`"
                )
            } else {
                format!("implement `Comparable` for `{key}`")
            };
            self.diags.error(
                E_WHERE_CLAUSE,
                format!(
                    "type `{key}` does not implement `Comparable`; the \
                     `<`/`>`/`<=`/`>=` operators require it — {suggestion}"
                ),
                span,
            );
        }
    }

    /// True when `operand` resolves to a **user** (`Named` record / class) type
    /// that implements `Comparable` in the current `impl_table`.
    ///
    /// This is the codegen-routing companion of [`require_comparable_operand`]:
    /// once the gate has accepted an ordering comparison, this answers whether the
    /// operands are a *user* `Comparable` type, so the body pass can stamp the
    /// `BinaryOp` node with [`USER_COMPARE_META_KEY`] (the operator must be lowered
    /// through `compare`, not the broken native `<`). Primitives — which the
    /// canonical `impl_table` also marks `Comparable` — are intentionally excluded:
    /// their native ordering operator already works on every target. Inference
    /// variables / flexible / poison types and the absence of an `impl_table`
    /// likewise return `false` (a bounded generic `T: Comparable` lowers through
    /// the trait-bound bridge, not this stamp).
    fn is_user_comparable(&self, operand: &Type) -> bool {
        let resolved = self.subst.apply(operand);
        let Type::Named(_) = &resolved else {
            return false;
        };
        let Some(impl_table) = self.impl_table.as_ref() else {
            return false;
        };
        let trait_ref = TraitRef::new("Comparable");
        resolve_impl(&trait_ref, &resolved, impl_table).is_some()
    }

    // ── DQ29: structural Equatable (§18.5) ───────────────────────────────────

    /// DQ29 (§18.5): decide whether `ty` conforms to `Equatable`, returning
    /// `None` when it does and the poisoning [`NonEquatableWitness`] when it
    /// does not.
    ///
    /// The decision is **structural and on-demand** (computed at the use site;
    /// no conditional trait-table entries):
    ///
    /// 1. **Explicit impl wins** — any type with a resolvable `impl Equatable`
    ///    conforms outright (the structural rules below are the compiler-provided
    ///    default, suppressed by the impl).
    /// 2. **Primitives** — decided by the canonical sealed conformances in
    ///    `impl_table` (all v1 scalars conform; `Void` conforms vacuously).
    /// 3. **Records** — conform iff every field type conforms (recursively).
    /// 4. **Enums** — conform iff every payload type of every variant conforms.
    /// 5. **Compound built-ins** — `List[T]`/`Set[T]`/`Optional[T]` iff `T`;
    ///    `Map[K, V]` iff `K` and `V`; `Result[T, E]` iff `T` and `E`; tuples
    ///    iff all components.
    /// 6. **Generic user types** — instantiate conditionally: the constructor's
    ///    declared field/payload types are checked with the instantiation's
    ///    type arguments substituted for the symbolic `Named(param)`
    ///    placeholders.
    /// 7. **Classes** — never conform structurally (data/identity line); only
    ///    rule 1 admits them.
    /// 8. **`Fn` types** — never conform (the poisoning leaf).
    ///
    /// Unknowns are **conservatively conforming**: unsolved type vars /
    /// flexible (sketch) types / `Error`, and `Named` types whose structure
    /// this checker cannot see (imported enums' payloads do not cross the
    /// export ABI; imported classes are not distinguishable from records).
    /// The gate only rejects what it can *prove* non-Equatable — mirroring
    /// [`Self::require_comparable_operand`]'s conservatism.
    ///
    /// Recursive types terminate co-inductively: a type currently being
    /// checked (`in_progress`) is assumed conforming, so `record Tree { kids:
    /// List[Tree] }` resolves to whatever its non-recursive leaves decide.
    fn structural_equatable_witness(
        &self,
        ty: &Type,
        in_progress: &mut HashSet<String>,
        path: &mut Vec<String>,
    ) -> Option<NonEquatableWitness> {
        let resolved = self.subst.apply(ty);
        let witness_here = |path: &[String], class_name: Option<String>| {
            Some(NonEquatableWitness {
                path: path.to_vec(),
                leaf: resolved.clone(),
                class_name,
            })
        };
        match &resolved {
            // Unknowns: conservatively conforming (cannot prove otherwise).
            Type::TypeVar(_) | Type::Flexible(_) | Type::Error => None,
            // The poisoning leaf: function types have no equality.
            Type::Function(_) => witness_here(path, None),
            Type::Primitive(p) => {
                // `Void` is a vacuous unit — always equal to itself.
                if matches!(p, PrimitiveType::Void) {
                    return None;
                }
                let table = self.impl_table.as_ref()?;
                if resolve_impl(&TraitRef::new("Equatable"), &resolved, table).is_some() {
                    None
                } else {
                    witness_here(path, None)
                }
            }
            Type::Named(n) => {
                // Explicit impl wins (rule 1) — including impls folded in from
                // imported modules.
                if let Some(table) = self.impl_table.as_ref() {
                    if resolve_impl(&TraitRef::new("Equatable"), &resolved, table).is_some() {
                        return None;
                    }
                }
                // Co-inductive assumption for recursive types.
                if !in_progress.insert(n.name.clone()) {
                    return None;
                }
                let result = if self.class_names.contains(&n.name) {
                    // Rule 7: classes are excluded from the structural default.
                    witness_here(path, Some(n.name.clone()))
                } else if let Some(variants) = self.enum_variant_payloads.get(&n.name) {
                    self.enum_payloads_witness(
                        &variants.clone(),
                        &HashMap::new(),
                        in_progress,
                        path,
                    )
                } else if let Some(fields) = self.record_field_types.get(&n.name) {
                    self.record_fields_witness(&fields.clone(), &HashMap::new(), in_progress, path)
                } else {
                    // Unknown structure (imported enum/class, opaque type):
                    // conservatively conforming.
                    None
                };
                in_progress.remove(&n.name);
                result
            }
            Type::Generic(g) => {
                match (g.constructor.as_str(), g.args.as_slice()) {
                    // Rule 5: compound built-ins compose conditionally.
                    ("List" | "Set", [elem]) => {
                        path.push("[..]".to_string());
                        let w = self.structural_equatable_witness(elem, in_progress, path);
                        path.pop();
                        w
                    }
                    ("Map", [key, value]) => {
                        path.push("[key]".to_string());
                        if let Some(w) = self.structural_equatable_witness(key, in_progress, path) {
                            path.pop();
                            return Some(w);
                        }
                        path.pop();
                        path.push("[value]".to_string());
                        let w = self.structural_equatable_witness(value, in_progress, path);
                        path.pop();
                        w
                    }
                    // Rule 6: generic user types instantiate conditionally.
                    _ => {
                        if let Some(table) = self.impl_table.as_ref() {
                            if resolve_impl(&TraitRef::new("Equatable"), &resolved, table).is_some()
                            {
                                return None;
                            }
                        }
                        let key = crate::traits::type_key(&resolved);
                        if !in_progress.insert(key.clone()) {
                            return None;
                        }
                        let subst_map: HashMap<String, Type> = self
                            .record_generic_params
                            .get(&g.constructor)
                            .map(|params| {
                                params.iter().cloned().zip(g.args.iter().cloned()).collect()
                            })
                            .unwrap_or_default();
                        let result = if let Some(variants) =
                            self.enum_variant_payloads.get(&g.constructor)
                        {
                            self.enum_payloads_witness(
                                &variants.clone(),
                                &subst_map,
                                in_progress,
                                path,
                            )
                        } else if let Some(fields) = self.record_field_types.get(&g.constructor) {
                            if self.class_names.contains(&g.constructor) {
                                witness_here(path, Some(g.constructor.clone()))
                            } else {
                                self.record_fields_witness(
                                    &fields.clone(),
                                    &subst_map,
                                    in_progress,
                                    path,
                                )
                            }
                        } else {
                            // Unknown constructor: conservatively conforming.
                            None
                        };
                        in_progress.remove(&key);
                        result
                    }
                }
            }
            Type::Tuple(elems) => {
                for (i, elem) in elems.iter().enumerate() {
                    path.push(i.to_string());
                    if let Some(w) = self.structural_equatable_witness(elem, in_progress, path) {
                        path.pop();
                        return Some(w);
                    }
                    path.pop();
                }
                None
            }
            Type::Optional(inner) => {
                path.push("[..]".to_string());
                let w = self.structural_equatable_witness(inner, in_progress, path);
                path.pop();
                w
            }
            Type::Result(ok, err) => {
                path.push("[ok]".to_string());
                if let Some(w) = self.structural_equatable_witness(ok, in_progress, path) {
                    path.pop();
                    return Some(w);
                }
                path.pop();
                path.push("[err]".to_string());
                let w = self.structural_equatable_witness(err, in_progress, path);
                path.pop();
                w
            }
            Type::Refined(base, _) => self.structural_equatable_witness(base, in_progress, path),
        }
    }

    /// Probe every field of a record (or class admitted via explicit impl
    /// elsewhere) for structural Equatable conformance, substituting
    /// `subst_map` for symbolic `Named(param)` placeholders first (rule 6).
    fn record_fields_witness(
        &self,
        fields: &[(String, Type)],
        subst_map: &HashMap<String, Type>,
        in_progress: &mut HashSet<String>,
        path: &mut Vec<String>,
    ) -> Option<NonEquatableWitness> {
        for (fname, fty) in fields {
            let fty = substitute_named_params(fty, subst_map);
            path.push(fname.clone());
            if let Some(w) = self.structural_equatable_witness(&fty, in_progress, path) {
                path.pop();
                return Some(w);
            }
            path.pop();
        }
        None
    }

    /// Probe every payload component of every enum variant for structural
    /// Equatable conformance (rule 4), substituting `subst_map` for symbolic
    /// `Named(param)` placeholders first (rule 6).
    fn enum_payloads_witness(
        &self,
        variants: &[EnumVariantPayloadTypes],
        subst_map: &HashMap<String, Type>,
        in_progress: &mut HashSet<String>,
        path: &mut Vec<String>,
    ) -> Option<NonEquatableWitness> {
        for (vname, components) in variants {
            for (label, cty) in components {
                let cty = substitute_named_params(cty, subst_map);
                path.push(format!("{vname}.{label}"));
                if let Some(w) = self.structural_equatable_witness(&cty, in_progress, path) {
                    path.pop();
                    return Some(w);
                }
                path.pop();
            }
        }
        None
    }

    /// §18.5 operator gating (DQ29): require an `==`/`!=` operand to be
    /// `Equatable`, mirroring [`Self::require_comparable_operand`]'s shape but
    /// deciding conformance with the structural predicate
    /// ([`Self::structural_equatable_witness`]) instead of an impl lookup
    /// alone.
    ///
    /// Conservative skips match the Comparable gate: no `impl_table`, or an
    /// operand that is still an inference variable / flexible / poison, emit
    /// nothing (a bounded generic `T: Equatable` reaches `==` via its
    /// where-clause obligation, not this gate).
    ///
    /// On failure it emits [`E_NOT_EQUATABLE`] naming the offending field path
    /// and leaf type, with a note suggesting the fix.
    fn require_equatable_operand(&mut self, operand: &Type, span: Span) {
        let resolved = self.subst.apply(operand);
        match &resolved {
            Type::TypeVar(_) | Type::Flexible(_) | Type::Error => return,
            _ => {}
        }
        if self.impl_table.is_none() {
            return; // no table → cannot prove non-conformance.
        }
        let mut in_progress = HashSet::new();
        let mut path = Vec::new();
        if let Some(witness) =
            self.structural_equatable_witness(&resolved, &mut in_progress, &mut path)
        {
            let key = crate::traits::type_key(&resolved);
            let (detail, suggestion) = equatable_failure_wording(&key, &witness);
            self.diags
                .error(
                    E_NOT_EQUATABLE,
                    format!(
                        "type `{resolved}` does not implement `Equatable`; the `==`/`!=` \
                         operators require it — {detail}"
                    ),
                    span,
                )
                .note(suggestion);
        }
    }

    /// Classify an `==`/`!=` operand for the [`USER_EQ_META_KEY`] codegen
    /// stamp. Returns `None` when the native operator is already correct on
    /// every target (primitives, unknowns without an `Equatable` bound, and
    /// anything the gate rejected).
    fn user_eq_kind(&self, operand: &Type) -> Option<&'static str> {
        let resolved = self.subst.apply(operand);
        match &resolved {
            Type::TypeVar(id) => {
                // Inside a generic fn body: `a == b` on a bounded param. Only
                // an Equatable-implying bound warrants the JS/TS deep-equality
                // routing ("generic"); an unbounded var stays native.
                let bounds = self.type_var_bounds.get(id)?;
                if bounds.iter().any(|b| b == "Equatable" || b == "Comparable") {
                    Some("generic")
                } else {
                    None
                }
            }
            Type::Flexible(_) | Type::Error | Type::Primitive(_) | Type::Function(_) => None,
            Type::Named(_) => {
                // Explicit impl (rule 1) → route through the user's `eq`.
                if let Some(table) = self.impl_table.as_ref() {
                    if resolve_impl(&TraitRef::new("Equatable"), &resolved, table).is_some() {
                        return Some("impl");
                    }
                }
                // Structural record/enum (a class without an impl is rejected
                // by the gate; stamping is moot on an erroring program).
                if self.type_needs_deep_eq(&resolved, &mut HashSet::new()) {
                    Some("deep")
                } else {
                    Some("structural")
                }
            }
            Type::Generic(_) | Type::Tuple(_) | Type::Optional(_) | Type::Result(_, _) => {
                if self.type_needs_deep_eq(&resolved, &mut HashSet::new()) {
                    Some("deep")
                } else {
                    Some("structural")
                }
            }
            Type::Refined(base, _) => self.user_eq_kind(base),
        }
    }

    /// True when equality over `ty` (transitively) involves a collection —
    /// `List`/`Map`/`Set`, or an `Optional`/`Result` wrapper — so Go must
    /// route `==` through its deep-equality runtime helper (native `==` on
    /// slices/maps is a compile error). Walks record fields and enum payloads
    /// with the same co-inductive guard as the conformance probe.
    fn type_needs_deep_eq(&self, ty: &Type, in_progress: &mut HashSet<String>) -> bool {
        let resolved = self.subst.apply(ty);
        match &resolved {
            Type::Generic(g) => match (g.constructor.as_str(), g.args.as_slice()) {
                ("List" | "Set" | "Map", _) => true,
                _ => {
                    let key = crate::traits::type_key(&resolved);
                    if !in_progress.insert(key.clone()) {
                        return false;
                    }
                    let subst_map: HashMap<String, Type> = self
                        .record_generic_params
                        .get(&g.constructor)
                        .map(|params| params.iter().cloned().zip(g.args.iter().cloned()).collect())
                        .unwrap_or_default();
                    let deep =
                        if let Some(variants) = self.enum_variant_payloads.get(&g.constructor) {
                            variants.clone().iter().any(|(_, components)| {
                                components.iter().any(|(_, cty)| {
                                    self.type_needs_deep_eq(
                                        &substitute_named_params(cty, &subst_map),
                                        in_progress,
                                    )
                                })
                            })
                        } else if let Some(fields) = self.record_field_types.get(&g.constructor) {
                            fields.clone().iter().any(|(_, fty)| {
                                self.type_needs_deep_eq(
                                    &substitute_named_params(fty, &subst_map),
                                    in_progress,
                                )
                            })
                        } else {
                            false
                        };
                    in_progress.remove(&key);
                    deep
                }
            },
            Type::Optional(_) | Type::Result(_, _) => true,
            Type::Named(n) => {
                if !in_progress.insert(n.name.clone()) {
                    return false;
                }
                let deep = if let Some(variants) = self.enum_variant_payloads.get(&n.name) {
                    variants.clone().iter().any(|(_, components)| {
                        components
                            .iter()
                            .any(|(_, cty)| self.type_needs_deep_eq(cty, in_progress))
                    })
                } else if let Some(fields) = self.record_field_types.get(&n.name) {
                    fields
                        .clone()
                        .iter()
                        .any(|(_, fty)| self.type_needs_deep_eq(fty, in_progress))
                } else {
                    false
                };
                in_progress.remove(&n.name);
                deep
            }
            Type::Tuple(elems) => elems
                .iter()
                .any(|e| self.type_needs_deep_eq(e, in_progress)),
            Type::Refined(base, _) => self.type_needs_deep_eq(base, in_progress),
            _ => false,
        }
    }

    fn infer_binop(&mut self, op: BinOp, lt: &Type, rt: &Type, span: Span) -> Type {
        match op {
            // Arithmetic: operands and result are numeric.
            // Orientation: the left operand establishes the expected type;
            // the right operand is the found type.
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem | BinOp::Pow => {
                self.unify_or_error(rt, lt, span, "arithmetic operands");
                self.subst.apply(lt)
            }

            // Comparison: operands must unify; result is Bool.
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                self.unify_or_error(rt, lt, span, "comparison operands");
                // §18.5 trait-language integration: the ordering operators
                // (`<`, `>`, `<=`, `>=`) require `impl Comparable` for a
                // *user* (Named) operand. Primitives are gated by the canonical
                // conformances in `impl_table`; generic type variables are gated
                // by their `where`-clause bounds. `==`/`!=` gate behind
                // `Equatable` the same way (DQ29), with records/enums/compound
                // built-ins conforming STRUCTURALLY — see
                // `require_equatable_operand`.
                //
                // `unify_or_error` above already required the operands to
                // share a type, so a single gate check on the (post-unify)
                // left operand covers both sides without double-reporting.
                // Fall back to the right operand only when the left stayed
                // an inference variable (e.g. an open var unified *into* a
                // concrete right-hand type).
                let probe = match self.subst.apply(lt) {
                    Type::TypeVar(_) => rt,
                    _ => lt,
                };
                if matches!(op, BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge) {
                    self.require_comparable_operand(probe, span);
                } else if matches!(op, BinOp::Eq | BinOp::Ne) {
                    self.require_equatable_operand(probe, span);
                }
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
                self.unify_or_error(rt, lt, span, "bitwise operands");
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
            NodeKind::TypeSelf => {
                // Inside an impl/class method body the context maps `Self` to
                // the concrete target (see `build_impl_context`); honor it so a
                // `-> Self` return or `other: Self` param resolves to the target
                // type. Outside that context (e.g. trait declarations) `Self`
                // stays an abstract `Named("Self")` placeholder.
                if let Some(ty) = gp_map.get("Self") {
                    ty.clone()
                } else {
                    Type::Named(crate::NamedType {
                        name: "Self".into(),
                    })
                }
            }
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
    fn type_expr_to_type(&self, ty: &TypeExpr, gp_map: &HashMap<String, Type>) -> Type {
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
                        ("Optional", 1) => Type::Optional(Box::new(converted_args[0].clone())),
                        _ => Type::Generic(GenericType {
                            constructor: name,
                            args: converted_args,
                        }),
                    }
                }
            }
            TypeExpr::Tuple { elems, .. } => Type::Tuple(
                elems
                    .iter()
                    .map(|e| self.type_expr_to_type(e, gp_map))
                    .collect(),
            ),
            TypeExpr::Function { params, ret, .. } => {
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

// ─── DQ29 structural-Equatable support types ─────────────────────────────────

/// The witness a failed structural-Equatable probe returns: which leaf
/// poisoned the conformance, and where it sits.
///
/// `path` is the chain of steps from the probed type to the offending leaf:
/// record/class field names, `Variant._0`-style enum payload components,
/// `0`-style tuple indices, and `[..]` / `[key]` / `[value]` / `[ok]` /
/// `[err]` markers for collection/wrapper elements. Empty when the probed
/// type itself is the offending leaf. `leaf` is the non-Equatable type found
/// there. `class_name` is `Some` when the failure is a class without an
/// explicit `impl Equatable` (rule 7 — classes are excluded from the
/// structural default), which gets its own diagnostic wording.
struct NonEquatableWitness {
    path: Vec<String>,
    leaf: Type,
    class_name: Option<String>,
}

/// Replace symbolic `Named(param)` placeholders in `ty` with the concrete
/// types `subst_map` assigns them — the use-site instantiation step for
/// generic records/enums (DQ29 rule 6). Types without placeholders pass
/// through unchanged; an empty map is the identity.
fn substitute_named_params(ty: &Type, subst_map: &HashMap<String, Type>) -> Type {
    if subst_map.is_empty() {
        return ty.clone();
    }
    match ty {
        Type::Named(n) => subst_map
            .get(&n.name)
            .cloned()
            .unwrap_or_else(|| ty.clone()),
        Type::Generic(g) => Type::Generic(GenericType {
            constructor: g.constructor.clone(),
            args: g
                .args
                .iter()
                .map(|a| substitute_named_params(a, subst_map))
                .collect(),
        }),
        Type::Tuple(elems) => Type::Tuple(
            elems
                .iter()
                .map(|e| substitute_named_params(e, subst_map))
                .collect(),
        ),
        Type::Function(f) => Type::Function(FnType {
            params: f
                .params
                .iter()
                .map(|p| substitute_named_params(p, subst_map))
                .collect(),
            ret: Box::new(substitute_named_params(&f.ret, subst_map)),
            effects: f.effects.clone(),
        }),
        Type::Optional(inner) => {
            Type::Optional(Box::new(substitute_named_params(inner, subst_map)))
        }
        Type::Result(ok, err) => Type::Result(
            Box::new(substitute_named_params(ok, subst_map)),
            Box::new(substitute_named_params(err, subst_map)),
        ),
        Type::Refined(base, pred) => Type::Refined(
            Box::new(substitute_named_params(base, subst_map)),
            pred.clone(),
        ),
        _ => ty.clone(),
    }
}

/// Word a structural-Equatable failure for the [`E_NOT_EQUATABLE`] diagnostic:
/// returns `(detail, suggestion)` where `detail` finishes the "… requires it —"
/// sentence and `suggestion` is the trailing fix note.
///
/// `key` is the probed type's [`crate::traits::type_key`] rendering (used in
/// the suggestion); the witness decides the wording:
/// - class without impl → names the class and the exclusion rule;
/// - poisoned field/payload → names the field path and the leaf type
///   (machine-actionable per the diagnostics-review criterion);
/// - the probed type itself the leaf → names its kind (function type /
///   sealed primitive).
fn equatable_failure_wording(key: &str, witness: &NonEquatableWitness) -> (String, String) {
    if let Some(class_name) = &witness.class_name {
        let detail = if witness.path.is_empty() {
            format!(
                "`{class_name}` is a class, and classes are excluded from structural \
                 equality (data/identity line)"
            )
        } else {
            format!(
                "field `{}` is the class `{class_name}`, and classes are excluded from \
                 structural equality (data/identity line)",
                witness.path.join(".")
            )
        };
        return (
            detail,
            format!("implement `Equatable` for `{class_name}` or remove the comparison"),
        );
    }
    if witness.path.is_empty() {
        let detail = match &witness.leaf {
            Type::Function(_) => "function types have no equality".to_string(),
            Type::Primitive(_) => format!(
                "`{key}` has no canonical equality (the `(core trait, primitive)` \
                 conformances are sealed)"
            ),
            other => format!("`{other}` is not Equatable"),
        };
        let suggestion = match &witness.leaf {
            Type::Primitive(_) => format!(
                "wrap `{key}` in a newtype with its own `impl Equatable`, or remove the \
                 comparison"
            ),
            _ => "remove the comparison".to_string(),
        };
        return (detail, suggestion);
    }
    (
        format!(
            "field `{}` of type `{}` is not Equatable",
            witness.path.join("."),
            witness.leaf
        ),
        format!("implement `Equatable` for `{key}` or remove the comparison"),
    )
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
        Type::TypeVar(id) if !out.contains(id) => {
            out.push(*id);
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

/// Q-checker-unknown-method-concrete: a short, user-facing description of a
/// receiver type for the "no method `m` on `<type>`" diagnostic. Reuses
/// [`crate::traits::type_key`]'s human-readable encoding (e.g. `List[Int]`,
/// `Map[String, Int]`, `String`, `Point`).
fn describe_receiver_type(ty: &Type) -> String {
    crate::traits::type_key(ty)
}

/// Q-checker-unknown-method-concrete: the nearest candidate method name to
/// `target` by Levenshtein edit distance, used for the "did you mean `…`?"
/// suggestion. Returns `None` when no candidate is close enough (distance must
/// be at most a third of the longer name's length, and at most 3), so an
/// unrelated typo does not produce a misleading suggestion.
fn nearest_method_name(target: &str, candidates: &[String]) -> Option<String> {
    let mut best: Option<(usize, &String)> = None;
    for cand in candidates {
        if cand == target {
            continue;
        }
        let dist = levenshtein(target, cand);
        if best.is_none_or(|(d, _)| dist < d) {
            best = Some((dist, cand));
        }
    }
    let (dist, cand) = best?;
    let threshold = (target.len().max(cand.len()) / 3).clamp(1, 3);
    if dist <= threshold {
        Some(cand.clone())
    } else {
        None
    }
}

/// Levenshtein edit distance between two ASCII-ish identifier strings. Used by
/// [`nearest_method_name`] for the unknown-method suggestion.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr: Vec<usize> = vec![0; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(curr[j] + 1);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Map a built-in type name to its [`PrimitiveType`] variant, if any.
/// Q-prim-assoc: `true` when `node` is a `Call` the lowerer classified as an
/// **associated-function call** (`Type.method(args)` — no `self` prepended), via
/// the [`bock_air::lower::ASSOC_CALL_META_KEY`] stamp. The checker-side mirror of
/// `bock_codegen`'s `is_associated_call` (codegen is downstream, so the helper
/// cannot be shared); used to recognise the primitive `Prim.from`/`Prim.try_from`
/// conversion call shape.
fn is_associated_call_node(node: &AIRNode) -> bool {
    matches!(
        node.metadata.get(bock_air::lower::ASSOC_CALL_META_KEY),
        Some(Value::Bool(true))
    )
}

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

/// Suggest a conversion for common numeric/string primitive mismatches.
///
/// **Direction-aware**: the suggestion always names the conversion that
/// produces the **expected** type from the **found** value. When no such
/// conversion exists in Bock's surface, this returns `None` — a hint
/// suggesting the wrong-direction conversion is worse than no hint
/// (it would steer an agent's repair away from the type the context
/// requires).
fn conversion_hint(found: &Type, expected: &Type) -> Option<String> {
    let f = as_primitive(found)?;
    let e = as_primitive(expected)?;
    use PrimitiveType as P;
    let is_int = |p: &P| {
        matches!(
            p,
            P::Int
                | P::Int8
                | P::Int16
                | P::Int32
                | P::Int64
                | P::Int128
                | P::UInt8
                | P::UInt16
                | P::UInt32
                | P::UInt64
                | P::BigInt
        )
    };
    let is_float = |p: &P| matches!(p, P::Float | P::Float32 | P::Float64 | P::BigFloat);

    // Found an integer where `Float` is expected: `.to_float()` produces
    // exactly `Float` (only suggested for the unsized expected type).
    if is_int(&f) && e == P::Float {
        return Some(format!(
            "call `.to_float()` on the `{f}` value to produce the expected `Float`"
        ));
    }
    // Found a float where `Int` is expected: `.to_int()` produces `Int`.
    if is_float(&f) && e == P::Int {
        return Some(format!(
            "call `.to_int()` on the `{f}` value (truncates toward zero) to produce the expected `Int`"
        ));
    }
    // Found a `String` where a number is expected: a String is *parsed*,
    // not converted — `Int.try_from` / `Float.try_from` return a `Result`.
    if f == P::String && matches!(e, P::Int | P::Float) {
        return Some(format!(
            "a `String` is not implicitly converted; parse it with `{e}.try_from(...)` (returns a `Result` — handle the failure case)"
        ));
    }
    // Found any other primitive where `String` is expected: `.to_string()`.
    if e == P::String && f != P::String {
        return Some(format!(
            "call `.to_string()` on the `{f}` value to produce the expected `String`"
        ));
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

    // ── Diagnostic quality (Q-diag-e4001-message-quality /
    //    Q-diag-effect-violation-errors) ───────────────────────────────────

    /// E4001 must read ``expected `T`, found `U``` in surface syntax — never
    /// the doubled-prefix Debug leak `type mismatch: Primitive(String) vs
    /// Primitive(Int)`.
    #[test]
    fn type_mismatch_message_reads_expected_then_found() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let lit = str_lit(&gen);
        checker.check_expr(&lit, &Type::Primitive(PrimitiveType::Int));
        let diag = checker.diags.iter().next().expect("a diagnostic");
        assert_eq!(diag.code.to_string(), "E4001");
        assert!(
            diag.message.contains("expected `Int`, found `String`"),
            "message: {}",
            diag.message
        );
        assert!(
            !diag.message.contains("Primitive("),
            "message leaks Debug representation: {}",
            diag.message
        );
    }

    /// The E4001 conversion hint must suggest the conversion that produces
    /// the **expected** type. A `String` found where `Int` is expected is
    /// parsed (`Int.try_from`), NOT `.to_string()`-ed (which would convert
    /// the wrong operand and make the code wronger).
    #[test]
    fn type_mismatch_hint_for_expected_int_found_string_is_parse() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let lit = str_lit(&gen);
        checker.check_expr(&lit, &Type::Primitive(PrimitiveType::Int));
        let diag = checker.diags.iter().next().expect("a diagnostic");
        assert!(
            diag.notes.iter().any(|n| n.contains("Int.try_from")),
            "notes: {:?}",
            diag.notes
        );
        assert!(
            !diag.notes.iter().any(|n| n.contains(".to_string()")),
            "misleading wrong-direction hint: {:?}",
            diag.notes
        );
    }

    #[test]
    fn conversion_hint_is_direction_aware() {
        let int = Type::Primitive(PrimitiveType::Int);
        let float = Type::Primitive(PrimitiveType::Float);
        let string = Type::Primitive(PrimitiveType::String);
        let bool_t = Type::Primitive(PrimitiveType::Bool);

        // found Int, expected Float → convert the Int.
        let hint = conversion_hint(&int, &float).expect("hint");
        assert!(hint.contains(".to_float()"), "{hint}");
        // found Float, expected Int → convert the Float.
        let hint = conversion_hint(&float, &int).expect("hint");
        assert!(hint.contains(".to_int()"), "{hint}");
        // found Int, expected String → stringify the Int.
        let hint = conversion_hint(&int, &string).expect("hint");
        assert!(hint.contains(".to_string()"), "{hint}");
        // found String, expected Int/Float → parse, not `.to_string()`.
        let hint = conversion_hint(&string, &int).expect("hint");
        assert!(hint.contains("Int.try_from"), "{hint}");
        let hint = conversion_hint(&string, &float).expect("hint");
        assert!(hint.contains("Float.try_from"), "{hint}");
        // No determinable conversion → no hint (a wrong suggestion is
        // worse than none).
        assert_eq!(conversion_hint(&bool_t, &int), None);
        assert_eq!(conversion_hint(&string, &bool_t), None);
        // Sized float targets have no `.to_float()` shortcut → no hint.
        assert_eq!(
            conversion_hint(&int, &Type::Primitive(PrimitiveType::Float32)),
            None
        );
    }

    /// The lowerer's method-call desugar duplicates the receiver node, so an
    /// undefined name can be inferred twice at the identical span. One root
    /// cause → one diagnostic (rubric #6).
    #[test]
    fn undefined_variable_reported_once_per_name_and_span() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        // Two distinct nodes (distinct ids), same name and span — the shape
        // the desugar produces.
        let first = make_node(
            &gen,
            NodeKind::Identifier {
                name: ident("ghost"),
            },
        );
        let second = make_node(
            &gen,
            NodeKind::Identifier {
                name: ident("ghost"),
            },
        );
        checker.infer_expr(&first);
        checker.infer_expr(&second);
        assert_eq!(
            checker.diags.error_count(),
            1,
            "expected exactly one E4002 for one root cause"
        );
    }

    /// `Effect.handler(...)` (the v1.x-reserved lambda-handler surface) must
    /// report the actual rule as a single E6006 — not a doubled, rule-less
    /// `E4002 undefined variable` at the effect name.
    #[test]
    fn reserved_lambda_handler_reports_e6006_once() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        checker.insert_effect_op_types(
            "Log".into(),
            vec![(
                "log".into(),
                Type::Function(FnType {
                    params: vec![Type::Primitive(PrimitiveType::String)],
                    ret: Box::new(Type::Primitive(PrimitiveType::Void)),
                    effects: vec![],
                }),
            )],
        );
        let object = make_node(&gen, NodeKind::Identifier { name: ident("Log") });
        let field_access = make_node(
            &gen,
            NodeKind::FieldAccess {
                object: Box::new(object),
                field: ident("handler"),
            },
        );
        let ty = checker.infer_expr(&field_access);
        assert_eq!(ty, Type::Error);
        assert_eq!(checker.diags.error_count(), 1, "exactly one diagnostic");
        let diag = checker.diags.iter().next().expect("a diagnostic");
        assert_eq!(diag.code.to_string(), "E6006");
        assert!(
            diag.message.contains("`Log.handler(...)`")
                && diag.message.contains("reserved until v1.x"),
            "message: {}",
            diag.message
        );
        assert!(
            diag.notes.iter().any(|n| n.contains("impl Log for")),
            "note must state the supported v1 handler form: {:?}",
            diag.notes
        );
    }

    /// A `.handler` access on an ordinary undefined name (not an effect)
    /// still reports the generic undefined variable, not E6006.
    #[test]
    fn handler_field_on_non_effect_still_undefined_variable() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let object = make_node(
            &gen,
            NodeKind::Identifier {
                name: ident("NotAnEffect"),
            },
        );
        let field_access = make_node(
            &gen,
            NodeKind::FieldAccess {
                object: Box::new(object),
                field: ident("handler"),
            },
        );
        checker.infer_expr(&field_access);
        let diag = checker.diags.iter().next().expect("a diagnostic");
        assert_eq!(diag.code.to_string(), "E4002");
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

    // ── Operator gating: comparison on user types (§18.5, Q-list-operator-
    //    gating-user-types) ──────────────────────────────────────────────────

    /// A `<` on a user (Named) type whose definition does NOT `impl Comparable`
    /// must be rejected: §18.5 gates the comparison operators behind the trait.
    #[test]
    fn comparison_on_user_type_without_comparable_errors() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        // Impl table present, but `Point` does not implement Comparable.
        checker.impl_table = Some(make_impl_table(&[(
            "Comparable",
            Type::Primitive(PrimitiveType::Int),
        )]));

        checker.env.define(
            "a",
            Type::Named(crate::NamedType {
                name: "Point".into(),
            }),
        );
        checker.env.define(
            "b",
            Type::Named(crate::NamedType {
                name: "Point".into(),
            }),
        );
        let left = make_node(&gen, NodeKind::Identifier { name: ident("a") });
        let right = make_node(&gen, NodeKind::Identifier { name: ident("b") });
        let node = make_node(
            &gen,
            NodeKind::BinaryOp {
                op: BinOp::Lt,
                left: Box::new(left),
                right: Box::new(right),
            },
        );
        checker.infer_expr(&node);
        assert!(
            checker.diags.has_errors(),
            "expected error: Point does not implement Comparable"
        );
    }

    /// The same user type WITH `impl Comparable` checks clean under `<`.
    #[test]
    fn comparison_on_user_type_with_comparable_ok() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let point = Type::Named(crate::NamedType {
            name: "Point".into(),
        });
        checker.impl_table = Some(make_impl_table(&[("Comparable", point.clone())]));

        checker.env.define("a", point.clone());
        checker.env.define("b", point);
        let left = make_node(&gen, NodeKind::Identifier { name: ident("a") });
        let right = make_node(&gen, NodeKind::Identifier { name: ident("b") });
        let node = make_node(
            &gen,
            NodeKind::BinaryOp {
                op: BinOp::Gt,
                left: Box::new(left),
                right: Box::new(right),
            },
        );
        let ty = checker.infer_expr(&node);
        assert!(
            !checker.diags.has_errors(),
            "expected no errors: Point implements Comparable"
        );
        assert_eq!(ty, Type::Primitive(PrimitiveType::Bool));
    }

    /// Each of the four ordering operators is gated identically.
    #[test]
    fn all_ordering_operators_gated_on_user_types() {
        for op in [BinOp::Lt, BinOp::Le, BinOp::Gt, BinOp::Ge] {
            let gen = NodeIdGen::new();
            let mut checker = TypeChecker::new();
            checker.impl_table = Some(make_impl_table(&[(
                "Comparable",
                Type::Primitive(PrimitiveType::Int),
            )]));
            checker.env.define(
                "a",
                Type::Named(crate::NamedType {
                    name: "Widget".into(),
                }),
            );
            checker.env.define(
                "b",
                Type::Named(crate::NamedType {
                    name: "Widget".into(),
                }),
            );
            let left = make_node(&gen, NodeKind::Identifier { name: ident("a") });
            let right = make_node(&gen, NodeKind::Identifier { name: ident("b") });
            let node = make_node(
                &gen,
                NodeKind::BinaryOp {
                    op,
                    left: Box::new(left),
                    right: Box::new(right),
                },
            );
            checker.infer_expr(&node);
            assert!(
                checker.diags.has_errors(),
                "expected error for {op:?} on a non-Comparable user type"
            );
        }
    }

    /// Comparison on primitives still works without explicit gating fallout:
    /// `Int < Int` with the canonical conformances registered is accepted, and
    /// — to mirror the existing `infer_comparison_returns_bool` test — with no
    /// impl table at all the gate is skipped (cannot prove non-conformance).
    #[test]
    fn comparison_on_primitive_not_gated_when_conformant() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let mut table = ImplTable::new();
        crate::traits::register_canonical_conformances(&mut table);
        checker.impl_table = Some(table);

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
        assert!(
            !checker.diags.has_errors(),
            "Int is Comparable; `<` must be accepted"
        );
        assert_eq!(ty, Type::Primitive(PrimitiveType::Bool));
    }

    /// A bounded generic param (`T: Comparable`) compared with `<` must NOT be
    /// flagged: the operand type is a `TypeVar`/`TraitBound`, not a Named type,
    /// so the user-type gate does not apply (the where-clause check covers it).
    #[test]
    fn comparison_on_bounded_generic_param_not_gated() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        checker.impl_table = Some(make_impl_table(&[(
            "Comparable",
            Type::Primitive(PrimitiveType::Int),
        )]));
        // A fresh inference variable stands in for the bounded generic param.
        let tv = checker.fresh_var();
        checker.env.define("a", tv.clone());
        checker.env.define("b", tv);
        let left = make_node(&gen, NodeKind::Identifier { name: ident("a") });
        let right = make_node(&gen, NodeKind::Identifier { name: ident("b") });
        let node = make_node(
            &gen,
            NodeKind::BinaryOp {
                op: BinOp::Lt,
                left: Box::new(left),
                right: Box::new(right),
            },
        );
        checker.infer_expr(&node);
        assert!(
            !checker.diags.has_errors(),
            "comparison on an inference variable must not trigger the user-type gate"
        );
    }

    // ── User-comparison codegen stamp (USER_COMPARE_META_KEY,
    //    Q-user-comparison-codegen) ─────────────────────────────────────────────

    /// Build a `BinaryOp` over two operands both bound to `operand_ty`, run the
    /// body pass over it (`infer_node`, which mutates `node.metadata`), and return
    /// the node so a test can inspect its stamps.
    fn infer_binop_node(checker: &mut TypeChecker, op: BinOp, operand_ty: Type) -> AIRNode {
        let gen = NodeIdGen::new();
        checker.env.define("a", operand_ty.clone());
        checker.env.define("b", operand_ty);
        let left = make_node(&gen, NodeKind::Identifier { name: ident("a") });
        let right = make_node(&gen, NodeKind::Identifier { name: ident("b") });
        let mut node = make_node(
            &gen,
            NodeKind::BinaryOp {
                op,
                left: Box::new(left),
                right: Box::new(right),
            },
        );
        checker.infer_node(&mut node);
        node
    }

    /// Each ordering operator on a user `Comparable` type stamps the node so
    /// codegen routes the operator through `compare`.
    #[test]
    fn user_comparison_stamps_ordering_ops() {
        let point = Type::Named(crate::NamedType {
            name: "Point".into(),
        });
        for op in [BinOp::Lt, BinOp::Le, BinOp::Gt, BinOp::Ge] {
            let mut checker = TypeChecker::new();
            checker.impl_table = Some(make_impl_table(&[("Comparable", point.clone())]));
            let node = infer_binop_node(&mut checker, op, point.clone());
            assert_eq!(
                node.metadata.get(USER_COMPARE_META_KEY),
                Some(&bock_air::Value::Bool(true)),
                "{op:?} on a user Comparable type must be stamped"
            );
        }
    }

    /// A primitive ordering comparison is NOT stamped — native `<` already works
    /// on every target, so codegen must keep emitting it.
    #[test]
    fn primitive_comparison_not_stamped() {
        let mut checker = TypeChecker::new();
        let mut table = ImplTable::new();
        crate::traits::register_canonical_conformances(&mut table);
        checker.impl_table = Some(table);
        let node = infer_binop_node(&mut checker, BinOp::Lt, Type::Primitive(PrimitiveType::Int));
        assert!(
            !node.metadata.contains_key(USER_COMPARE_META_KEY),
            "primitive `<` must not carry the user-compare stamp"
        );
    }

    /// Equality (`==`) on a user type is the sibling Equatable lane — it must NOT
    /// be stamped by the comparison arm.
    #[test]
    fn user_equality_not_stamped_by_comparison_arm() {
        let point = Type::Named(crate::NamedType {
            name: "Point".into(),
        });
        let mut checker = TypeChecker::new();
        checker.impl_table = Some(make_impl_table(&[("Comparable", point.clone())]));
        let node = infer_binop_node(&mut checker, BinOp::Eq, point);
        assert!(
            !node.metadata.contains_key(USER_COMPARE_META_KEY),
            "`==` is the Equatable lane and must not carry the user-compare stamp"
        );
    }

    /// A user type that does NOT implement `Comparable` is not stamped (the
    /// comparison is also rejected by the gate; codegen never sees it, but the
    /// stamp must be absent regardless).
    #[test]
    fn non_comparable_user_type_not_stamped() {
        let point = Type::Named(crate::NamedType {
            name: "Point".into(),
        });
        let mut checker = TypeChecker::new();
        // Impl table present, but `Point` does NOT implement Comparable.
        checker.impl_table = Some(make_impl_table(&[(
            "Comparable",
            Type::Primitive(PrimitiveType::Int),
        )]));
        let node = infer_binop_node(&mut checker, BinOp::Lt, point);
        assert!(
            !node.metadata.contains_key(USER_COMPARE_META_KEY),
            "a non-Comparable user type must not be stamped"
        );
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

    // ── DQ18: `push`/`append` return Void ──────────────────────────────────

    #[test]
    fn method_call_list_push_returns_void() {
        // [1].push(2) → Void (DQ18: in-place mutator, value-less)
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let list = make_node(
            &gen,
            NodeKind::ListLiteral {
                elems: vec![int_lit(&gen)],
            },
        );
        let method_call = make_node(
            &gen,
            NodeKind::MethodCall {
                receiver: Box::new(list),
                method: ident("push"),
                type_args: vec![],
                args: vec![bock_air::AirArg {
                    label: None,
                    value: int_lit(&gen),
                }],
            },
        );
        let ty = checker.infer_expr(&method_call);
        assert_eq!(ty, Type::Primitive(PrimitiveType::Void));
    }

    #[test]
    fn method_call_list_append_returns_void() {
        // [1].append(2) → Void (append is the spelling alias for push)
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let list = make_node(
            &gen,
            NodeKind::ListLiteral {
                elems: vec![int_lit(&gen)],
            },
        );
        let method_call = make_node(
            &gen,
            NodeKind::MethodCall {
                receiver: Box::new(list),
                method: ident("append"),
                type_args: vec![],
                args: vec![bock_air::AirArg {
                    label: None,
                    value: int_lit(&gen),
                }],
            },
        );
        let ty = checker.infer_expr(&method_call);
        assert_eq!(ty, Type::Primitive(PrimitiveType::Void));
    }

    // ── DQ22: `contains` is not a `Map` method ─────────────────────────────

    /// Build a `{key: val}.<method>(arg)` call in the lowerer's desugared shape
    /// (`Call { callee: FieldAccess(map, method), args: [map, arg] }`). The `map`
    /// receiver is a single-entry `MapLiteral` so the checker resolves it to
    /// `Map[K, V]`; the `self` arg shares the field-access object's NodeId.
    fn desugared_map_method_call(
        gen: &NodeIdGen,
        method: &str,
        key: AIRNode,
        val: AIRNode,
        arg: AIRNode,
    ) -> AIRNode {
        let map = make_node(
            gen,
            NodeKind::MapLiteral {
                entries: vec![bock_air::AirMapEntry { key, value: val }],
            },
        );
        let map_self = map.clone();
        let callee = make_node(
            gen,
            NodeKind::FieldAccess {
                object: Box::new(map),
                field: ident(method),
            },
        );
        make_node(
            gen,
            NodeKind::Call {
                callee: Box::new(callee),
                type_args: vec![],
                args: vec![
                    bock_air::AirArg {
                        label: None,
                        value: map_self,
                    },
                    bock_air::AirArg {
                        label: None,
                        value: arg,
                    },
                ],
            },
        )
    }

    #[test]
    fn map_contains_is_rejected_with_suggestion() {
        // {"a": 1}.contains("a") → error: did you mean `contains_key`?
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let call = desugared_map_method_call(
            &gen,
            "contains",
            str_lit(&gen),
            int_lit(&gen),
            str_lit(&gen),
        );
        let _ = checker.infer_expr(&call);
        assert!(checker.diags.has_errors());
        let err = checker
            .diags
            .iter()
            .find(|d| d.code == E_NO_SUCH_METHOD)
            .expect("expected an E4013 Map-contains rejection");
        assert!(err.message.contains("contains_key"));
        assert!(!err.notes.is_empty(), "expected a suggestion note");
    }

    #[test]
    fn map_contains_key_still_resolves() {
        // {"a": 1}.contains_key("a") → Bool, no rejection.
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let call = desugared_map_method_call(
            &gen,
            "contains_key",
            str_lit(&gen),
            int_lit(&gen),
            str_lit(&gen),
        );
        let _ = checker.infer_expr(&call);
        assert!(
            !checker.diags.iter().any(|d| d.code == E_NO_SUCH_METHOD),
            "contains_key must not be rejected"
        );
    }

    // ── Q-checker-unknown-method-concrete ────────────────────────────────────

    /// Build the desugared method call for `[1].method(...)` on a `List[Int]`.
    fn desugared_list_method_call(gen: &NodeIdGen, method: &str, arg: Option<AIRNode>) -> AIRNode {
        let list = make_node(
            gen,
            NodeKind::ListLiteral {
                elems: vec![int_lit(gen)],
            },
        );
        let list_self = list.clone();
        let callee = make_node(
            gen,
            NodeKind::FieldAccess {
                object: Box::new(list),
                field: ident(method),
            },
        );
        let mut args = vec![bock_air::AirArg {
            label: None,
            value: list_self,
        }];
        if let Some(a) = arg {
            args.push(bock_air::AirArg {
                label: None,
                value: a,
            });
        }
        make_node(
            gen,
            NodeKind::Call {
                callee: Box::new(callee),
                type_args: vec![],
                args,
            },
        )
    }

    /// An unknown method on a concrete built-in receiver (`List[Int]`) is an
    /// `E4013` error, not a silent fresh type variable.
    #[test]
    fn list_unknown_method_is_rejected() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let call = desugared_list_method_call(&gen, "frobnicate", None);
        let _ = checker.infer_expr(&call);
        let err = checker
            .diags
            .iter()
            .find(|d| d.code == E_NO_SUCH_METHOD)
            .expect("expected an E4013 unknown-method rejection");
        assert!(err.message.contains("frobnicate"));
        assert!(err.message.contains("List[Int]"));
    }

    /// A near-name typo on a concrete receiver gets a "did you mean `…`?" note.
    #[test]
    fn list_unknown_method_suggests_nearest() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        // `lenght` is one transposition away from `length`.
        let call = desugared_list_method_call(&gen, "lenght", None);
        let _ = checker.infer_expr(&call);
        let err = checker
            .diags
            .iter()
            .find(|d| d.code == E_NO_SUCH_METHOD)
            .expect("expected an E4013 unknown-method rejection");
        assert!(
            err.notes.iter().any(|n| n.contains("length")),
            "expected a `did you mean `length`?` suggestion, got: {:?}",
            err.notes
        );
    }

    /// A real built-in method (List `map`) still resolves cleanly — the check
    /// fires only for genuinely-unknown methods.
    #[test]
    fn list_known_method_not_rejected() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let lambda = make_node(
            &gen,
            NodeKind::Lambda {
                params: vec![make_node(
                    &gen,
                    NodeKind::Param {
                        pattern: Box::new(make_node(
                            &gen,
                            NodeKind::BindPat {
                                name: ident("x"),
                                is_mut: false,
                            },
                        )),
                        ty: None,
                        default: None,
                    },
                )],
                body: Box::new(make_node(&gen, NodeKind::Identifier { name: ident("x") })),
            },
        );
        let call = desugared_list_method_call(&gen, "map", Some(lambda));
        let _ = checker.infer_expr(&call);
        assert!(
            !checker.diags.iter().any(|d| d.code == E_NO_SUCH_METHOD),
            "a known List method (`map`) must not be rejected"
        );
    }

    /// The `nearest_method_name` helper returns a suggestion only for a close
    /// candidate, and `None` for an unrelated name.
    #[test]
    fn nearest_method_name_thresholds() {
        let cands = vec!["length".to_string(), "len".to_string(), "push".to_string()];
        assert_eq!(
            nearest_method_name("lenght", &cands).as_deref(),
            Some("length")
        );
        assert_eq!(nearest_method_name("frobnicate", &cands), None);
    }

    // ── Q-import-reject (§12.2 / DQ8) ────────────────────────────────────────

    /// Build a `Module` node with a single `use <segments>` import carrying the
    /// given [`ImportItems`].
    fn module_with_import(
        gen: &NodeIdGen,
        segments: &[&str],
        items: bock_ast::ImportItems,
    ) -> AIRNode {
        let dummy = bock_errors::Span {
            file: bock_errors::FileId(0),
            start: 0,
            end: 0,
        };
        let import = make_node(
            gen,
            NodeKind::ImportDecl {
                path: bock_ast::ModulePath {
                    segments: segments
                        .iter()
                        .map(|s| bock_ast::Ident {
                            name: (*s).to_string(),
                            span: dummy,
                        })
                        .collect(),
                    span: dummy,
                },
                items,
            },
        );
        make_node(
            gen,
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![import],
                items: vec![],
            },
        )
    }

    /// A bare module import (`use core.error`, `ImportItems::Module`) is rejected
    /// with `E4014` pointing at the braced form.
    #[test]
    fn bare_module_import_is_rejected() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let mut module =
            module_with_import(&gen, &["core", "error"], bock_ast::ImportItems::Module);
        checker.check_module(&mut module);
        let err = checker
            .diags
            .iter()
            .find(|d| d.code == E_BARE_MODULE_IMPORT)
            .expect("expected an E4014 bare-module-import rejection");
        assert!(err.message.contains("core.error"));
        assert!(
            err.notes.iter().any(|n| n.contains("{")),
            "expected a braced-form suggestion note, got: {:?}",
            err.notes
        );
    }

    /// A braced import (`use core.error.{Error}`, `ImportItems::Named`) is NOT
    /// rejected.
    #[test]
    fn braced_import_not_rejected() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let named = bock_ast::ImportItems::Named(vec![bock_ast::ImportedName {
            name: bock_ast::Ident {
                name: "Error".to_string(),
                span: bock_errors::Span {
                    file: bock_errors::FileId(0),
                    start: 0,
                    end: 0,
                },
            },
            alias: None,
            span: bock_errors::Span {
                file: bock_errors::FileId(0),
                start: 0,
                end: 0,
            },
        }]);
        let mut module = module_with_import(&gen, &["core", "error"], named);
        checker.check_module(&mut module);
        assert!(
            !checker.diags.iter().any(|d| d.code == E_BARE_MODULE_IMPORT),
            "a braced import must not be rejected"
        );
    }

    /// A wildcard import (`use core.error.*`, `ImportItems::Glob`) is NOT
    /// rejected.
    #[test]
    fn wildcard_import_not_rejected() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        let mut module = module_with_import(&gen, &["core", "error"], bock_ast::ImportItems::Glob);
        checker.check_module(&mut module);
        assert!(
            !checker.diags.iter().any(|d| d.code == E_BARE_MODULE_IMPORT),
            "a wildcard import must not be rejected"
        );
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

    // ── impl-method `Self` substitution (Q-self-subst) ───────────────────

    /// Build an `impl <target> { <method>(self, <extra params>) -> <ret> }`
    /// AIR module node and return it ready for `collect_sig`.
    ///
    /// `extra_params` are `(name, type_node)` pairs appended after the
    /// untyped `self`; `ret` is the method's return-type node.
    fn impl_with_method(
        gen: &NodeIdGen,
        target: &str,
        method: &str,
        extra_params: Vec<(&str, AIRNode)>,
        ret: AIRNode,
    ) -> AIRNode {
        let self_pat = make_node(
            gen,
            NodeKind::BindPat {
                name: ident("self"),
                is_mut: false,
            },
        );
        let self_param = make_node(
            gen,
            NodeKind::Param {
                pattern: Box::new(self_pat),
                ty: None,
                default: None,
            },
        );
        let mut params = vec![self_param];
        for (pname, pty) in extra_params {
            let pat = make_node(
                gen,
                NodeKind::BindPat {
                    name: ident(pname),
                    is_mut: false,
                },
            );
            params.push(make_node(
                gen,
                NodeKind::Param {
                    pattern: Box::new(pat),
                    ty: Some(Box::new(pty)),
                    default: None,
                },
            ));
        }
        let body = make_node(
            gen,
            NodeKind::Block {
                stmts: vec![],
                tail: None,
            },
        );
        let method_node = make_node(
            gen,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: bock_ast::Visibility::Public,
                is_async: false,
                name: ident(method),
                generic_params: vec![],
                params,
                return_type: Some(Box::new(ret)),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        make_node(
            gen,
            NodeKind::ImplBlock {
                annotations: vec![],
                generic_params: vec![],
                trait_path: None,
                trait_args: vec![],
                target: Box::new(type_named_node(gen, target)),
                where_clause: vec![],
                methods: vec![method_node],
            },
        )
    }

    /// `impl Doubler { fn double(self) -> Self }` must register `double` with a
    /// concrete `Doubler` *return* type, not the un-substituted `Named("Self")`
    /// that previously leaked to call sites as E4001.
    #[test]
    fn impl_method_self_in_return_is_substituted() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();

        let self_ret = make_node(&gen, NodeKind::TypeSelf);
        let impl_node = impl_with_method(&gen, "Doubler", "double", vec![], self_ret);
        checker.collect_sig(&impl_node);

        let method_ty = checker
            .method_types
            .get("Doubler")
            .and_then(|m| m.get("double"))
            .expect("double should be registered on Doubler");
        let Type::Function(fn_ty) = method_ty else {
            panic!("expected a function type, got {method_ty:?}");
        };
        // `self` param resolves to the target type, and `-> Self` is now the
        // concrete target — no residual `Named("Self")` anywhere.
        let doubler = Type::Named(crate::NamedType {
            name: "Doubler".into(),
        });
        assert_eq!(*fn_ty.ret, doubler, "return `Self` should become Doubler");
        assert_eq!(fn_ty.params, vec![doubler]);
    }

    /// `impl Counter { fn combine(self, other: Self) -> Int }` must register
    /// `combine` with the `other` *parameter* typed as the concrete target,
    /// not the un-substituted `Named("Self")`.
    #[test]
    fn impl_method_self_in_param_is_substituted() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();

        let other_ty = make_node(&gen, NodeKind::TypeSelf);
        let int_ret = type_named_node(&gen, "Int");
        let impl_node = impl_with_method(
            &gen,
            "Counter",
            "combine",
            vec![("other", other_ty)],
            int_ret,
        );
        checker.collect_sig(&impl_node);

        let method_ty = checker
            .method_types
            .get("Counter")
            .and_then(|m| m.get("combine"))
            .expect("combine should be registered on Counter");
        let Type::Function(fn_ty) = method_ty else {
            panic!("expected a function type, got {method_ty:?}");
        };
        let counter = Type::Named(crate::NamedType {
            name: "Counter".into(),
        });
        // params: [self -> Counter, other: Self -> Counter]
        assert_eq!(fn_ty.params, vec![counter.clone(), counter]);
        assert_eq!(*fn_ty.ret, Type::Primitive(PrimitiveType::Int));
    }

    // ── impl/class method-body checking (Q-impl-body-typecheck) ───────────

    /// Build `impl <target> { fn <method>(self) -> <ret> { <tail> } }`, i.e. an
    /// inherent impl whose single method has a real (non-empty) body. Used to
    /// exercise the body-checking pass that `check_item` now performs.
    fn impl_with_bodied_method(
        gen: &NodeIdGen,
        target: &str,
        method: &str,
        ret: AIRNode,
        tail: AIRNode,
    ) -> AIRNode {
        let self_pat = make_node(
            gen,
            NodeKind::BindPat {
                name: ident("self"),
                is_mut: false,
            },
        );
        let self_param = make_node(
            gen,
            NodeKind::Param {
                pattern: Box::new(self_pat),
                ty: None,
                default: None,
            },
        );
        let body = make_node(
            gen,
            NodeKind::Block {
                stmts: vec![],
                tail: Some(Box::new(tail)),
            },
        );
        let method_node = make_node(
            gen,
            NodeKind::FnDecl {
                annotations: vec![],
                visibility: bock_ast::Visibility::Public,
                is_async: false,
                name: ident(method),
                generic_params: vec![],
                params: vec![self_param],
                return_type: Some(Box::new(ret)),
                effect_clause: vec![],
                where_clause: vec![],
                body: Box::new(body),
            },
        );
        make_node(
            gen,
            NodeKind::ImplBlock {
                annotations: vec![],
                generic_params: vec![],
                trait_path: None,
                trait_args: vec![],
                target: Box::new(type_named_node(gen, target)),
                where_clause: vec![],
                methods: vec![method_node],
            },
        )
    }

    /// A method body whose tail expression's type disagrees with the declared
    /// return type must now be reported — before the fix, `check_item` skipped
    /// `ImplBlock`, so the body was never walked and the mismatch was silent.
    #[test]
    fn impl_method_body_type_error_is_reported() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();

        // impl Widget { fn id(self) -> Int { "hello" } }  — String vs Int.
        let ret = type_named_node(&gen, "Int");
        let tail = str_lit(&gen);
        let impl_node = impl_with_bodied_method(&gen, "Widget", "id", ret, tail);
        let mut module = make_node(
            &gen,
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![impl_node],
            },
        );

        checker.check_module(&mut module);
        assert!(
            checker.diags.has_errors(),
            "expected a method-body type error, got none"
        );
    }

    /// A well-typed method body must still check clean (no false positive).
    #[test]
    fn impl_method_body_well_typed_is_clean() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();

        // impl Widget { fn name(self) -> String { "hello" } }
        let ret = type_named_node(&gen, "String");
        let tail = str_lit(&gen);
        let impl_node = impl_with_bodied_method(&gen, "Widget", "name", ret, tail);
        let mut module = make_node(
            &gen,
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![impl_node],
            },
        );

        checker.check_module(&mut module);
        assert!(
            !checker.diags.has_errors(),
            "well-typed method body should not error: {:?}",
            checker.diags.iter().collect::<Vec<_>>()
        );
    }

    /// A getter method whose name matches a record field (`fn message(self) ->
    /// String { self.message }`) must read the *field* in value position, not
    /// resolve `self.message` to the method's own function type (which would be
    /// `Fn(Self) -> String`, mismatching the `-> String` return). This is the
    /// `core.error` shape the body-checking pass first surfaced; the
    /// `FieldAccess` handler now prefers the same-named field.
    #[test]
    fn impl_getter_named_like_field_reads_the_field() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();

        // record Err { message: String }
        let field = bock_ast::RecordDeclField {
            id: gen.next(),
            span: span(),
            name: ident("message"),
            ty: TypeExpr::Named {
                id: gen.next(),
                span: span(),
                path: TypePath {
                    segments: vec![ident("String")],
                    span: span(),
                },
                args: vec![],
            },
            default: None,
        };
        let record_node = make_node(
            &gen,
            NodeKind::RecordDecl {
                annotations: vec![],
                visibility: bock_ast::Visibility::Public,
                name: ident("Err"),
                generic_params: vec![],
                fields: vec![field],
            },
        );

        // impl Err { fn message(self) -> String { self.message } }
        let self_ref = make_node(
            &gen,
            NodeKind::Identifier {
                name: ident("self"),
            },
        );
        let field_access = make_node(
            &gen,
            NodeKind::FieldAccess {
                object: Box::new(self_ref),
                field: ident("message"),
            },
        );
        let ret = type_named_node(&gen, "String");
        let impl_node = impl_with_bodied_method(&gen, "Err", "message", ret, field_access);

        let mut module = make_node(
            &gen,
            NodeKind::Module {
                path: None,
                annotations: vec![],
                imports: vec![],
                items: vec![record_node, impl_node],
            },
        );

        checker.check_module(&mut module);
        assert!(
            !checker.diags.has_errors(),
            "field-named getter should read the field, not the method: {:?}",
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

    /// Q-checker-unknown-method-concrete: an unknown method on a *concrete*
    /// receiver (here `Int`) is now an `E4013` error — the soundness hole where
    /// it silently resolved to a fresh type variable is closed. The result type
    /// is still a fresh var for error recovery, but the diagnostic fires.
    #[test]
    fn method_call_unknown_method_on_concrete_errors() {
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
        let _ = checker.infer_expr(&method_call);
        assert!(
            checker.diags.iter().any(|d| d.code == E_NO_SUCH_METHOD
                && d.message.contains("nonexistent")
                && d.message.contains("Int")),
            "unknown method on a concrete `Int` receiver must raise E4013"
        );
    }

    /// The new check must NOT fire when the receiver is an unresolved inference
    /// variable — methods may resolve once it is unified, and §4.9 sketch-mode
    /// narrowing resolves aggressively by design.
    #[test]
    fn method_call_unknown_method_on_typevar_does_not_error() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        // A bare identifier with no binding yields a fresh var receiver in
        // inference; build a MethodCall whose receiver is an unresolved var via
        // a lambda parameter (inferred to a fresh var, never unified).
        let lambda = make_node(
            &gen,
            NodeKind::Lambda {
                params: vec![make_node(
                    &gen,
                    NodeKind::Param {
                        pattern: Box::new(make_node(
                            &gen,
                            NodeKind::BindPat {
                                name: ident("x"),
                                is_mut: false,
                            },
                        )),
                        ty: None,
                        default: None,
                    },
                )],
                body: Box::new(make_node(
                    &gen,
                    NodeKind::MethodCall {
                        receiver: Box::new(make_node(
                            &gen,
                            NodeKind::Identifier { name: ident("x") },
                        )),
                        method: ident("whatever"),
                        type_args: vec![],
                        args: vec![],
                    },
                )),
            },
        );
        let _ = checker.infer_expr(&lambda);
        assert!(
            !checker.diags.iter().any(|d| d.code == E_NO_SUCH_METHOD),
            "an unknown method on an unresolved type-var receiver must NOT error"
        );
    }

    // ── Receiver-type annotation (checker → codegen) ─────────────────────────

    #[test]
    fn recv_kind_tag_maps_each_category() {
        use crate::NamedType;
        assert_eq!(
            recv_kind_tag(&Type::Primitive(PrimitiveType::Int)).as_deref(),
            Some("Primitive:Int")
        );
        assert_eq!(
            recv_kind_tag(&Type::Primitive(PrimitiveType::Float)).as_deref(),
            Some("Primitive:Float")
        );
        assert_eq!(
            recv_kind_tag(&Type::Primitive(PrimitiveType::String)).as_deref(),
            Some("Primitive:String")
        );
        assert_eq!(
            recv_kind_tag(&Type::Optional(Box::new(Type::Primitive(
                PrimitiveType::Int
            ))))
            .as_deref(),
            Some("Optional")
        );
        assert_eq!(
            recv_kind_tag(&Type::Result(
                Box::new(Type::Primitive(PrimitiveType::Int)),
                Box::new(Type::Primitive(PrimitiveType::String)),
            ))
            .as_deref(),
            Some("Result")
        );
        assert_eq!(
            recv_kind_tag(&Type::Generic(GenericType {
                constructor: "List".into(),
                args: vec![Type::Primitive(PrimitiveType::Int)],
            }))
            .as_deref(),
            Some("List")
        );
        assert_eq!(
            recv_kind_tag(&Type::Named(NamedType {
                name: "Point".into(),
            }))
            .as_deref(),
            Some("User:Point")
        );
        // No tag for inference vars / function types.
        assert_eq!(recv_kind_tag(&Type::TypeVar(0)), None);
    }

    /// Build the desugared method-call shape the lowerer produces for
    /// `recv.method(args)`: `Call { callee: FieldAccess(recv, method),
    /// args: [recv, ...args] }`. The receiver node is shared (same id) between
    /// the field-access object and the first (self) argument.
    fn desugared_method_call(
        gen: &NodeIdGen,
        receiver: AIRNode,
        method: &str,
        extra_args: Vec<AIRNode>,
    ) -> AIRNode {
        let field_access = make_node(
            gen,
            NodeKind::FieldAccess {
                object: Box::new(receiver.clone()),
                field: ident(method),
            },
        );
        let mut args = vec![bock_air::AirArg {
            label: None,
            value: receiver,
        }];
        for a in extra_args {
            args.push(bock_air::AirArg {
                label: None,
                value: a,
            });
        }
        make_node(
            gen,
            NodeKind::Call {
                callee: Box::new(field_access),
                type_args: vec![],
                args,
            },
        )
    }

    /// Register a `Comparable { compare(self, Self) -> Ordering }` /
    /// `Equatable { eq(self, Self) -> Bool }` model + an `impl_table` granting
    /// the named primitive both conformances, mirroring the canonical
    /// primitive-bridge wiring.
    fn with_primitive_comparable(checker: &mut TypeChecker, prim: PrimitiveType) {
        let self_ty = Type::Named(crate::NamedType {
            name: "Self".into(),
        });
        let mut comparable = HashMap::new();
        comparable.insert(
            "compare".to_string(),
            Type::Function(FnType {
                params: vec![self_ty.clone(), self_ty.clone()],
                ret: Box::new(Type::Named(crate::NamedType {
                    name: "Ordering".into(),
                })),
                effects: vec![],
            }),
        );
        checker.insert_trait_method_types("Comparable".to_string(), comparable);
        let mut equatable = HashMap::new();
        equatable.insert(
            "eq".to_string(),
            Type::Function(FnType {
                params: vec![self_ty.clone(), self_ty.clone()],
                ret: Box::new(Type::Primitive(PrimitiveType::Bool)),
                effects: vec![],
            }),
        );
        checker.insert_trait_method_types("Equatable".to_string(), equatable);
        checker.impl_table = Some(make_impl_table(&[
            ("Comparable", Type::Primitive(prim.clone())),
            ("Equatable", Type::Primitive(prim)),
        ]));
    }

    #[test]
    fn stamps_recv_kind_on_primitive_compare() {
        // (1).compare(2) → desugared Call. The checker resolves the receiver as
        // Int and stamps `recv_kind = "Primitive:Int"` on the call node.
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        with_primitive_comparable(&mut checker, PrimitiveType::Int);

        let mut call = desugared_method_call(&gen, int_lit(&gen), "compare", vec![int_lit(&gen)]);
        let ty = checker.infer_node(&mut call);
        // Resolves to the trait's declared return (Ordering), not the intrinsic.
        assert_eq!(
            ty,
            Type::Named(crate::NamedType {
                name: "Ordering".into()
            })
        );
        assert_eq!(
            call.metadata.get(RECV_KIND_META_KEY),
            Some(&Value::String("Primitive:Int".to_string())),
            "expected recv_kind stamped on the compare call node"
        );
    }

    #[test]
    fn stamps_recv_kind_on_primitive_eq_and_to_string() {
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();
        with_primitive_comparable(&mut checker, PrimitiveType::Int);

        let mut eq_call = desugared_method_call(&gen, int_lit(&gen), "eq", vec![int_lit(&gen)]);
        checker.infer_node(&mut eq_call);
        assert_eq!(
            eq_call.metadata.get(RECV_KIND_META_KEY),
            Some(&Value::String("Primitive:Int".to_string())),
        );

        // `.to_string()` is an intrinsic (not a trait method), but the receiver
        // kind is still stamped so codegen can lower it.
        let mut ts_call = desugared_method_call(&gen, int_lit(&gen), "to_string", vec![]);
        checker.infer_node(&mut ts_call);
        assert_eq!(
            ts_call.metadata.get(RECV_KIND_META_KEY),
            Some(&Value::String("Primitive:Int".to_string())),
        );
    }

    #[test]
    fn stamps_recv_kind_optional_and_list() {
        // The annotation is comprehensive: it also serves the P1-c consumer
        // (Optional/Result method dispatch). An `Int?` receiver → "Optional";
        // a `List[Int]` receiver → "List".
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();

        // Bind a variable `o: Int?` and call `o.unwrap_or(0)`.
        checker.env.define(
            "o",
            Type::Optional(Box::new(Type::Primitive(PrimitiveType::Int))),
        );
        let o_ref = make_node(&gen, NodeKind::Identifier { name: ident("o") });
        let mut opt_call = desugared_method_call(&gen, o_ref, "unwrap_or", vec![int_lit(&gen)]);
        checker.infer_node(&mut opt_call);
        assert_eq!(
            opt_call.metadata.get(RECV_KIND_META_KEY),
            Some(&Value::String("Optional".to_string())),
        );

        checker.env.define(
            "xs",
            Type::Generic(GenericType {
                constructor: "List".into(),
                args: vec![Type::Primitive(PrimitiveType::Int)],
            }),
        );
        let xs_ref = make_node(&gen, NodeKind::Identifier { name: ident("xs") });
        let mut list_call = desugared_method_call(&gen, xs_ref, "len", vec![]);
        checker.infer_node(&mut list_call);
        assert_eq!(
            list_call.metadata.get(RECV_KIND_META_KEY),
            Some(&Value::String("List".to_string())),
        );
    }

    /// Build a binary-op node `left <op> right`.
    fn binop_node(gen: &NodeIdGen, op: BinOp, left: AIRNode, right: AIRNode) -> AIRNode {
        make_node(
            gen,
            NodeKind::BinaryOp {
                op,
                left: Box::new(left),
                right: Box::new(right),
            },
        )
    }

    /// An integer literal of a *sized* type (e.g. `42_i32` → `Int32`).
    fn sized_int_lit(gen: &NodeIdGen, suffix: &str) -> AIRNode {
        make_node(
            gen,
            NodeKind::Literal {
                lit: Literal::Int(format!("42_{suffix}")),
            },
        )
    }

    #[test]
    fn stamps_int_arith_on_integer_div_and_rem() {
        // `17 / 5` and `17 % 5` — both operands `Int` — get the `int_arith` stamp
        // so codegen lowers them to DQ23's truncate-toward-zero / dividend-sign
        // semantics (§3.6).
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();

        let mut div = binop_node(&gen, BinOp::Div, int_lit(&gen), int_lit(&gen));
        checker.infer_node(&mut div);
        assert_eq!(
            div.metadata.get(INT_ARITH_META_KEY),
            Some(&Value::Bool(true)),
            "expected int_arith stamped on Int / Int",
        );

        let mut rem = binop_node(&gen, BinOp::Rem, int_lit(&gen), int_lit(&gen));
        checker.infer_node(&mut rem);
        assert_eq!(
            rem.metadata.get(INT_ARITH_META_KEY),
            Some(&Value::Bool(true)),
            "expected int_arith stamped on Int % Int",
        );
    }

    #[test]
    fn stamps_int_arith_on_sized_integer_div() {
        // "All sized integer types divide the same way" (DQ23): a `Int32 / Int32`
        // is stamped just like `Int / Int`.
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();

        let mut div = binop_node(
            &gen,
            BinOp::Div,
            sized_int_lit(&gen, "i32"),
            sized_int_lit(&gen, "i32"),
        );
        checker.infer_node(&mut div);
        assert_eq!(
            div.metadata.get(INT_ARITH_META_KEY),
            Some(&Value::Bool(true)),
            "expected int_arith stamped on Int32 / Int32",
        );

        // UInt64 too.
        let mut udiv = binop_node(
            &gen,
            BinOp::Div,
            sized_int_lit(&gen, "u64"),
            sized_int_lit(&gen, "u64"),
        );
        checker.infer_node(&mut udiv);
        assert_eq!(
            udiv.metadata.get(INT_ARITH_META_KEY),
            Some(&Value::Bool(true)),
            "expected int_arith stamped on UInt64 / UInt64",
        );
    }

    #[test]
    fn no_int_arith_stamp_on_float_div_or_addition() {
        // Float division is IEEE true division — NOT integer division — so it is
        // not stamped. And `+` (even on integers) is never integer division.
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();

        let mut fdiv = binop_node(&gen, BinOp::Div, float_lit(&gen), float_lit(&gen));
        checker.infer_node(&mut fdiv);
        assert!(
            !fdiv.metadata.contains_key(INT_ARITH_META_KEY),
            "Float / Float must not be stamped int_arith",
        );

        let mut add = binop_node(&gen, BinOp::Add, int_lit(&gen), int_lit(&gen));
        checker.infer_node(&mut add);
        assert!(
            !add.metadata.contains_key(INT_ARITH_META_KEY),
            "Int + Int is not integer division",
        );
    }

    #[test]
    fn stamps_bool_stringify_on_bool_interpolation_part() {
        // A `Bool`-typed `${expr}` part is stamped so the Python backend prints
        // the canonical lowercase `true`/`false` (§3.5). A non-Bool part is not.
        let gen = NodeIdGen::new();
        let mut checker = TypeChecker::new();

        let mut interp = make_node(
            &gen,
            NodeKind::Interpolation {
                parts: vec![
                    bock_air::AirInterpolationPart::Expr(Box::new(bool_lit(&gen, true))),
                    bock_air::AirInterpolationPart::Expr(Box::new(int_lit(&gen))),
                ],
            },
        );
        checker.infer_node(&mut interp);
        let NodeKind::Interpolation { parts } = &interp.kind else {
            panic!("expected interpolation");
        };
        let bock_air::AirInterpolationPart::Expr(bool_part) = &parts[0] else {
            panic!("expected expr part 0");
        };
        assert_eq!(
            bool_part.metadata.get(BOOL_STRINGIFY_META_KEY),
            Some(&Value::Bool(true)),
            "expected bool_stringify stamped on the Bool interpolation part",
        );
        let bock_air::AirInterpolationPart::Expr(int_part) = &parts[1] else {
            panic!("expected expr part 1");
        };
        assert!(
            !int_part.metadata.contains_key(BOOL_STRINGIFY_META_KEY),
            "Int interpolation part must not be stamped bool_stringify",
        );
    }
}
