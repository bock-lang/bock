//! Core trait dispatch — maps language features to trait method calls.
//!
//! The interpreter's expression evaluator uses [`TraitDispatch`] to resolve
//! operators, `for..in`, and string interpolation to the corresponding trait
//! method names registered in the [`bock_interp::BuiltinRegistry`].
//!
//! ## Trait-to-Language-Feature Mapping
//!
//! | Language Feature         | Trait        | Method                   |
//! |--------------------------|-------------|--------------------------|
//! | `<`, `>`, `<=`, `>=`    | Comparable  | `compare() -> Ordering`  |
//! | `==`, `!=`               | Equatable   | `equals() -> Bool`       |
//! | `for x in collection`   | Iterable    | `iter() -> Iterator`     |
//! | `"${expr}"`             | Displayable | `display() -> String`    |
//! | `x + y` (non-primitive) | Add         | `add(other) -> Self`     |
//! | `x - y` (non-primitive) | Sub         | `sub(other) -> Self`     |
//! | `x * y` (non-primitive) | Mul         | `mul(other) -> Self`     |
//! | `x / y` (non-primitive) | Div         | `div(other) -> Self`     |
//! | `x % y` (non-primitive) | Rem         | `rem(other) -> Self`     |
//! | `Into[T]` / `From[T]`  | Into/From   | `into() -> T` / `from()` |

use std::collections::{HashMap, HashSet};

use bock_ast::BinOp;
use bock_interp::TypeTag;

/// The name of a trait method (e.g., `"compare"`, `"equals"`, `"iter"`).
pub type MethodName = &'static str;

/// Maps language features (operators, `for..in`, string interpolation) to
/// trait method names, checking whether a given type has the required trait
/// implementation registered.
///
/// Built-in types are pre-populated. User-defined types can be registered
/// at runtime via [`TraitDispatch::register_trait`].
pub struct TraitDispatch {
    /// Maps `BinOp` → trait method name.
    binop_methods: HashMap<BinOp, MethodName>,
    /// Types that implement each trait, keyed by method name.
    /// When a `(method_name, TypeTag)` pair is present, the type supports
    /// the corresponding trait dispatch.
    trait_impls: HashMap<MethodName, HashSet<TypeTag>>,
    /// Set of all recognised prelude trait names.
    ///
    /// Traits listed here won't produce "unknown trait" errors when
    /// referenced in `@derive` annotations or trait bounds.
    known_traits: HashSet<&'static str>,
    /// Maps trait name → primary method name(s) for that trait.
    trait_methods: HashMap<&'static str, Vec<MethodName>>,
}

impl Default for TraitDispatch {
    fn default() -> Self {
        Self::new()
    }
}

impl TraitDispatch {
    /// Create a new `TraitDispatch` with the default operator-to-method mappings
    /// and built-in type registrations.
    #[must_use]
    pub fn new() -> Self {
        let mut binop_methods = HashMap::new();

        // Comparison operators → Comparable.compare
        binop_methods.insert(BinOp::Lt, "compare");
        binop_methods.insert(BinOp::Le, "compare");
        binop_methods.insert(BinOp::Gt, "compare");
        binop_methods.insert(BinOp::Ge, "compare");

        // Equality operators → Equatable.equals
        binop_methods.insert(BinOp::Eq, "equals");
        binop_methods.insert(BinOp::Ne, "equals");

        // Arithmetic operators → trait methods
        binop_methods.insert(BinOp::Add, "add");
        binop_methods.insert(BinOp::Sub, "sub");
        binop_methods.insert(BinOp::Mul, "mul");
        binop_methods.insert(BinOp::Div, "div");
        binop_methods.insert(BinOp::Rem, "rem");

        let mut dispatch = Self {
            binop_methods,
            trait_impls: HashMap::new(),
            known_traits: HashSet::new(),
            trait_methods: HashMap::new(),
        };

        // Register built-in trait implementations
        dispatch.register_builtins();

        // Register all prelude trait names
        dispatch.register_prelude_traits();

        dispatch
    }

    /// Register all built-in type trait implementations.
    fn register_builtins(&mut self) {
        // ── Comparable (compare) ────────────────────────────────────────
        for ty in [
            TypeTag::Int,
            TypeTag::Float,
            TypeTag::Bool,
            TypeTag::String,
            TypeTag::Char,
            TypeTag::List,
            TypeTag::Map,
            TypeTag::Set,
        ] {
            self.register_trait(ty, "compare");
        }

        // ── Equatable (equals) ──────────────────────────────────────────
        for ty in [
            TypeTag::Int,
            TypeTag::Float,
            TypeTag::Bool,
            TypeTag::String,
            TypeTag::Char,
            TypeTag::List,
            TypeTag::Map,
            TypeTag::Set,
        ] {
            self.register_trait(ty, "equals");
        }

        // ── Displayable (display) ───────────────────────────────────────
        for ty in [
            TypeTag::Int,
            TypeTag::Float,
            TypeTag::Bool,
            TypeTag::String,
            TypeTag::Char,
            TypeTag::List,
            TypeTag::Map,
            TypeTag::Set,
            TypeTag::Optional,
            TypeTag::Result,
        ] {
            self.register_trait(ty, "display");
        }

        // ── Iterable (iter) ────────────────────────────────────────────
        for ty in [TypeTag::List, TypeTag::Set, TypeTag::Map, TypeTag::Range] {
            self.register_trait(ty, "iter");
        }

        // ── Add ─────────────────────────────────────────────────────────
        for ty in [TypeTag::Int, TypeTag::Float, TypeTag::String] {
            self.register_trait(ty, "add");
        }

        // ── Sub ─────────────────────────────────────────────────────────
        for ty in [TypeTag::Int, TypeTag::Float] {
            self.register_trait(ty, "sub");
        }

        // ── Mul ─────────────────────────────────────────────────────────
        for ty in [TypeTag::Int, TypeTag::Float] {
            self.register_trait(ty, "mul");
        }

        // ── Div ─────────────────────────────────────────────────────────
        for ty in [TypeTag::Int, TypeTag::Float] {
            self.register_trait(ty, "div");
        }

        // ── Rem ─────────────────────────────────────────────────────────
        for ty in [TypeTag::Int, TypeTag::Float] {
            self.register_trait(ty, "rem");
        }

        // ── Hashable (hash_code) ────────────────────────────────────────
        for ty in [
            TypeTag::Int,
            TypeTag::Float,
            TypeTag::Bool,
            TypeTag::String,
            TypeTag::Char,
            TypeTag::List,
            TypeTag::Map,
            TypeTag::Set,
        ] {
            self.register_trait(ty, "hash_code");
        }

        // ── From/Into ───────────────────────────────────────────────────
        // Int → Float conversion
        self.register_trait(TypeTag::Int, "into");
        self.register_trait(TypeTag::Int, "from");
        self.register_trait(TypeTag::Float, "from");
        self.register_trait(TypeTag::String, "from");

        // ── Default (default) ────────────────────────────────────────────
        for ty in [
            TypeTag::Int,
            TypeTag::Float,
            TypeTag::Bool,
            TypeTag::String,
            TypeTag::Char,
        ] {
            self.register_trait(ty, "default");
        }
    }

    /// Register all prelude trait names so they are recognised by the type
    /// system and don't produce "unknown trait" errors.
    fn register_prelude_traits(&mut self) {
        // Existing traits
        self.register_known_trait("Comparable", &["compare"]);
        self.register_known_trait("Equatable", &["equals"]);
        self.register_known_trait("Hashable", &["hash_code"]);
        self.register_known_trait("Displayable", &["display"]);
        self.register_known_trait("Iterable", &["iter"]);
        self.register_known_trait("Add", &["add"]);
        self.register_known_trait("Sub", &["sub"]);
        self.register_known_trait("Mul", &["mul"]);
        self.register_known_trait("Div", &["div"]);
        self.register_known_trait("Rem", &["rem"]);
        self.register_known_trait("Into", &["into"]);
        self.register_known_trait("From", &["from"]);

        // New prelude traits (F2.16)
        self.register_known_trait("Default", &["default"]);
        self.register_known_trait("Serializable", &[]);
        self.register_known_trait("Cloneable", &[]);
        self.register_known_trait("TryFrom", &[]);
        self.register_known_trait("Collectable", &[]);
    }

    /// Register a trait name and its associated methods in the known-traits
    /// table.
    fn register_known_trait(&mut self, name: &'static str, methods: &[MethodName]) {
        self.known_traits.insert(name);
        self.trait_methods.insert(name, methods.to_vec());
    }

    /// Register a trait implementation for a user-defined or custom type.
    ///
    /// This allows extending the trait dispatch at runtime when user types
    /// implement core traits (via `@derive` or explicit `impl` blocks).
    pub fn register_trait(&mut self, type_tag: TypeTag, method: MethodName) {
        self.trait_impls.entry(method).or_default().insert(type_tag);
    }

    /// Check whether `name` is a recognised prelude trait.
    #[must_use]
    pub fn is_known_trait(&self, name: &str) -> bool {
        self.known_traits.contains(name)
    }

    /// Return the method names associated with a known trait.
    ///
    /// Returns an empty slice for stub traits that have no methods yet.
    #[must_use]
    pub fn trait_method_names(&self, trait_name: &str) -> &[MethodName] {
        self.trait_methods
            .get(trait_name)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Return all known prelude trait names.
    #[must_use]
    pub fn known_trait_names(&self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = self.known_traits.iter().copied().collect();
        names.sort_unstable();
        names
    }

    /// Resolve a binary operator to the trait method name to call, if the
    /// given type implements the required trait.
    ///
    /// Returns the method name (e.g., `"compare"`, `"equals"`, `"add"`) or
    /// `None` if the type doesn't implement the trait for that operator.
    #[must_use]
    pub fn resolve_binop(&self, op: BinOp, lhs_type: TypeTag) -> Option<MethodName> {
        let method = self.binop_methods.get(&op)?;
        if self.has_trait(lhs_type, method) {
            Some(method)
        } else {
            None
        }
    }

    /// Resolve the `for..in` feature — returns `"iter"` if the type is Iterable.
    #[must_use]
    pub fn resolve_for_in(&self, collection_type: TypeTag) -> Option<MethodName> {
        if self.has_trait(collection_type, "iter") {
            Some("iter")
        } else {
            None
        }
    }

    /// Resolve string interpolation `${}` — returns `"display"` if the type
    /// is Displayable.
    #[must_use]
    pub fn resolve_display(&self, type_tag: TypeTag) -> Option<MethodName> {
        if self.has_trait(type_tag, "display") {
            Some("display")
        } else {
            None
        }
    }

    /// Resolve From/Into conversion — returns the method name if the type
    /// implements the conversion trait.
    #[must_use]
    pub fn resolve_conversion(
        &self,
        type_tag: TypeTag,
        direction: ConversionDirection,
    ) -> Option<MethodName> {
        let method = match direction {
            ConversionDirection::Into => "into",
            ConversionDirection::From => "from",
        };
        if self.has_trait(type_tag, method) {
            Some(method)
        } else {
            None
        }
    }

    /// Check whether a type has a specific trait method registered.
    #[must_use]
    pub fn has_trait(&self, type_tag: TypeTag, method: &str) -> bool {
        self.trait_impls
            .get(method)
            .is_some_and(|types| types.contains(&type_tag))
    }

    /// List all types that implement a given trait method.
    #[must_use]
    pub fn types_implementing(&self, method: &str) -> Vec<TypeTag> {
        self.trait_impls
            .get(method)
            .map(|types| types.iter().copied().collect())
            .unwrap_or_default()
    }
}

/// Direction for From/Into conversion dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversionDirection {
    /// `value.into()` — convert source to target type.
    Into,
    /// `Type.from(value)` — construct target from source.
    From,
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn dispatch() -> TraitDispatch {
        TraitDispatch::new()
    }

    // ── Comparable ──────────────────────────────────────────────────────

    #[test]
    fn comparable_resolves_lt_for_int() {
        let d = dispatch();
        assert_eq!(d.resolve_binop(BinOp::Lt, TypeTag::Int), Some("compare"));
    }

    #[test]
    fn comparable_resolves_ge_for_string() {
        let d = dispatch();
        assert_eq!(d.resolve_binop(BinOp::Ge, TypeTag::String), Some("compare"));
    }

    #[test]
    fn comparable_resolves_gt_for_float() {
        let d = dispatch();
        assert_eq!(d.resolve_binop(BinOp::Gt, TypeTag::Float), Some("compare"));
    }

    #[test]
    fn comparable_resolves_le_for_list() {
        let d = dispatch();
        assert_eq!(d.resolve_binop(BinOp::Le, TypeTag::List), Some("compare"));
    }

    #[test]
    fn comparable_none_for_function() {
        let d = dispatch();
        assert_eq!(d.resolve_binop(BinOp::Lt, TypeTag::Function), None);
    }

    // ── Equatable ───────────────────────────────────────────────────────

    #[test]
    fn equatable_resolves_eq_for_int() {
        let d = dispatch();
        assert_eq!(d.resolve_binop(BinOp::Eq, TypeTag::Int), Some("equals"));
    }

    #[test]
    fn equatable_resolves_ne_for_bool() {
        let d = dispatch();
        assert_eq!(d.resolve_binop(BinOp::Ne, TypeTag::Bool), Some("equals"));
    }

    #[test]
    fn equatable_none_for_iterator() {
        let d = dispatch();
        assert_eq!(d.resolve_binop(BinOp::Eq, TypeTag::Iterator), None);
    }

    // ── Iterable ────────────────────────────────────────────────────────

    #[test]
    fn iterable_resolves_for_list() {
        let d = dispatch();
        assert_eq!(d.resolve_for_in(TypeTag::List), Some("iter"));
    }

    #[test]
    fn iterable_resolves_for_set() {
        let d = dispatch();
        assert_eq!(d.resolve_for_in(TypeTag::Set), Some("iter"));
    }

    #[test]
    fn iterable_resolves_for_map() {
        let d = dispatch();
        assert_eq!(d.resolve_for_in(TypeTag::Map), Some("iter"));
    }

    #[test]
    fn iterable_resolves_for_range() {
        let d = dispatch();
        assert_eq!(d.resolve_for_in(TypeTag::Range), Some("iter"));
    }

    #[test]
    fn iterable_none_for_int() {
        let d = dispatch();
        assert_eq!(d.resolve_for_in(TypeTag::Int), None);
    }

    // ── Displayable ─────────────────────────────────────────────────────

    #[test]
    fn displayable_resolves_for_int() {
        let d = dispatch();
        assert_eq!(d.resolve_display(TypeTag::Int), Some("display"));
    }

    #[test]
    fn displayable_resolves_for_string() {
        let d = dispatch();
        assert_eq!(d.resolve_display(TypeTag::String), Some("display"));
    }

    #[test]
    fn displayable_resolves_for_optional() {
        let d = dispatch();
        assert_eq!(d.resolve_display(TypeTag::Optional), Some("display"));
    }

    #[test]
    fn displayable_none_for_function() {
        let d = dispatch();
        assert_eq!(d.resolve_display(TypeTag::Function), None);
    }

    // ── Add / arithmetic operators ──────────────────────────────────────

    #[test]
    fn add_resolves_for_int() {
        let d = dispatch();
        assert_eq!(d.resolve_binop(BinOp::Add, TypeTag::Int), Some("add"));
    }

    #[test]
    fn add_resolves_for_string() {
        let d = dispatch();
        assert_eq!(d.resolve_binop(BinOp::Add, TypeTag::String), Some("add"));
    }

    #[test]
    fn sub_resolves_for_float() {
        let d = dispatch();
        assert_eq!(d.resolve_binop(BinOp::Sub, TypeTag::Float), Some("sub"));
    }

    #[test]
    fn mul_resolves_for_int() {
        let d = dispatch();
        assert_eq!(d.resolve_binop(BinOp::Mul, TypeTag::Int), Some("mul"));
    }

    #[test]
    fn add_none_for_bool() {
        let d = dispatch();
        assert_eq!(d.resolve_binop(BinOp::Add, TypeTag::Bool), None);
    }

    // ── From/Into ───────────────────────────────────────────────────────

    #[test]
    fn into_resolves_for_int() {
        let d = dispatch();
        assert_eq!(
            d.resolve_conversion(TypeTag::Int, ConversionDirection::Into),
            Some("into")
        );
    }

    #[test]
    fn from_resolves_for_float() {
        let d = dispatch();
        assert_eq!(
            d.resolve_conversion(TypeTag::Float, ConversionDirection::From),
            Some("from")
        );
    }

    #[test]
    fn from_resolves_for_string() {
        let d = dispatch();
        assert_eq!(
            d.resolve_conversion(TypeTag::String, ConversionDirection::From),
            Some("from")
        );
    }

    #[test]
    fn into_none_for_void() {
        let d = dispatch();
        assert_eq!(
            d.resolve_conversion(TypeTag::Void, ConversionDirection::Into),
            None
        );
    }

    // ── Extensibility (user-defined types) ──────────────────────────────

    #[test]
    fn register_custom_comparable() {
        let mut d = dispatch();
        // Simulate a user type implementing Comparable via Record
        d.register_trait(TypeTag::Record, "compare");
        assert_eq!(d.resolve_binop(BinOp::Lt, TypeTag::Record), Some("compare"));
        assert_eq!(d.resolve_binop(BinOp::Ge, TypeTag::Record), Some("compare"));
    }

    #[test]
    fn register_custom_equatable() {
        let mut d = dispatch();
        d.register_trait(TypeTag::Record, "equals");
        assert_eq!(d.resolve_binop(BinOp::Eq, TypeTag::Record), Some("equals"));
        assert_eq!(d.resolve_binop(BinOp::Ne, TypeTag::Record), Some("equals"));
    }

    #[test]
    fn register_custom_iterable() {
        let mut d = dispatch();
        d.register_trait(TypeTag::Record, "iter");
        assert_eq!(d.resolve_for_in(TypeTag::Record), Some("iter"));
    }

    #[test]
    fn register_custom_displayable() {
        let mut d = dispatch();
        d.register_trait(TypeTag::Record, "display");
        assert_eq!(d.resolve_display(TypeTag::Record), Some("display"));
    }

    #[test]
    fn register_custom_add() {
        let mut d = dispatch();
        d.register_trait(TypeTag::Record, "add");
        assert_eq!(d.resolve_binop(BinOp::Add, TypeTag::Record), Some("add"));
    }

    #[test]
    fn register_custom_from_into() {
        let mut d = dispatch();
        d.register_trait(TypeTag::Record, "into");
        d.register_trait(TypeTag::Record, "from");
        assert_eq!(
            d.resolve_conversion(TypeTag::Record, ConversionDirection::Into),
            Some("into")
        );
        assert_eq!(
            d.resolve_conversion(TypeTag::Record, ConversionDirection::From),
            Some("from")
        );
    }

    // ── has_trait / types_implementing ───────────────────────────────────

    #[test]
    fn has_trait_positive() {
        let d = dispatch();
        assert!(d.has_trait(TypeTag::Int, "compare"));
        assert!(d.has_trait(TypeTag::List, "iter"));
    }

    #[test]
    fn has_trait_negative() {
        let d = dispatch();
        assert!(!d.has_trait(TypeTag::Int, "iter"));
        assert!(!d.has_trait(TypeTag::Function, "compare"));
    }

    #[test]
    fn types_implementing_compare() {
        let d = dispatch();
        let types = d.types_implementing("compare");
        assert!(types.contains(&TypeTag::Int));
        assert!(types.contains(&TypeTag::Float));
        assert!(types.contains(&TypeTag::String));
        assert!(!types.contains(&TypeTag::Function));
    }

    #[test]
    fn types_implementing_unknown_method() {
        let d = dispatch();
        assert!(d.types_implementing("nonexistent").is_empty());
    }

    // ── Operator coverage ───────────────────────────────────────────────

    #[test]
    fn logical_ops_not_trait_dispatched() {
        let d = dispatch();
        // And/Or are handled natively, not via trait dispatch
        assert_eq!(d.resolve_binop(BinOp::And, TypeTag::Bool), None);
        assert_eq!(d.resolve_binop(BinOp::Or, TypeTag::Bool), None);
    }

    #[test]
    fn bitwise_ops_not_trait_dispatched() {
        let d = dispatch();
        assert_eq!(d.resolve_binop(BinOp::BitAnd, TypeTag::Int), None);
        assert_eq!(d.resolve_binop(BinOp::BitOr, TypeTag::Int), None);
    }

    // ── Prelude trait recognition (F2.16) ──────────────────────────────

    #[test]
    fn all_prelude_traits_recognized() {
        let d = dispatch();
        for name in [
            "Comparable",
            "Equatable",
            "Hashable",
            "Displayable",
            "Iterable",
            "Add",
            "Sub",
            "Mul",
            "Div",
            "Rem",
            "Into",
            "From",
            "Default",
            "Serializable",
            "Cloneable",
            "TryFrom",
            "Collectable",
        ] {
            assert!(
                d.is_known_trait(name),
                "trait `{name}` should be recognized"
            );
        }
    }

    #[test]
    fn unknown_trait_not_recognized() {
        let d = dispatch();
        assert!(!d.is_known_trait("NonExistentTrait"));
    }

    #[test]
    fn known_trait_names_includes_new_traits() {
        let d = dispatch();
        let names = d.known_trait_names();
        assert!(names.contains(&"Serializable"));
        assert!(names.contains(&"Cloneable"));
        assert!(names.contains(&"Default"));
        assert!(names.contains(&"TryFrom"));
        assert!(names.contains(&"Collectable"));
    }

    #[test]
    fn trait_method_names_for_default() {
        let d = dispatch();
        assert_eq!(d.trait_method_names("Default"), &["default"]);
    }

    #[test]
    fn trait_method_names_for_stub_traits() {
        let d = dispatch();
        // Stub traits have no methods registered yet
        assert!(d.trait_method_names("Serializable").is_empty());
        assert!(d.trait_method_names("Cloneable").is_empty());
        assert!(d.trait_method_names("TryFrom").is_empty());
        assert!(d.trait_method_names("Collectable").is_empty());
    }

    // ── Hashable trait ─────────────────────────────────────────────────

    #[test]
    fn hashable_recognized() {
        let d = dispatch();
        assert!(d.is_known_trait("Hashable"));
    }

    #[test]
    fn hashable_method_is_hash_code() {
        let d = dispatch();
        assert_eq!(d.trait_method_names("Hashable"), &["hash_code"]);
    }

    #[test]
    fn hashable_registered_for_int() {
        let d = dispatch();
        assert!(d.has_trait(TypeTag::Int, "hash_code"));
    }

    #[test]
    fn hashable_registered_for_string() {
        let d = dispatch();
        assert!(d.has_trait(TypeTag::String, "hash_code"));
    }

    #[test]
    fn hashable_registered_for_list() {
        let d = dispatch();
        assert!(d.has_trait(TypeTag::List, "hash_code"));
    }

    #[test]
    fn hashable_not_registered_for_function() {
        let d = dispatch();
        assert!(!d.has_trait(TypeTag::Function, "hash_code"));
    }

    // ── Default trait ──────────────────────────────────────────────────

    #[test]
    fn default_registered_for_int() {
        let d = dispatch();
        assert!(d.has_trait(TypeTag::Int, "default"));
    }

    #[test]
    fn default_registered_for_string() {
        let d = dispatch();
        assert!(d.has_trait(TypeTag::String, "default"));
    }

    #[test]
    fn default_registered_for_bool() {
        let d = dispatch();
        assert!(d.has_trait(TypeTag::Bool, "default"));
    }

    #[test]
    fn default_registered_for_float() {
        let d = dispatch();
        assert!(d.has_trait(TypeTag::Float, "default"));
    }

    #[test]
    fn default_registered_for_char() {
        let d = dispatch();
        assert!(d.has_trait(TypeTag::Char, "default"));
    }

    #[test]
    fn default_not_registered_for_function() {
        let d = dispatch();
        assert!(!d.has_trait(TypeTag::Function, "default"));
    }

    // ── Derive annotation acceptance ───────────────────────────────────

    #[test]
    fn derive_serializable_recognized() {
        // Verifies that `@derive(Serializable)` won't produce an
        // "unknown trait" error — the trait name is in the dispatch table.
        let d = dispatch();
        assert!(d.is_known_trait("Serializable"));
    }

    #[test]
    fn derive_cloneable_recognized() {
        let d = dispatch();
        assert!(d.is_known_trait("Cloneable"));
    }

    #[test]
    fn derive_default_recognized() {
        let d = dispatch();
        assert!(d.is_known_trait("Default"));
    }
}
