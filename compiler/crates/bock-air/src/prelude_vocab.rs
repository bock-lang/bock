//! Prelude vocabulary — names that are always in scope without an explicit
//! `use`. Mirrors the seed lists in [`crate::resolve::SymbolTable::seed_prelude`].
//!
//! Exposed so that tooling (vocab emitter, editor extensions, documentation)
//! can render the prelude without re-hardcoding it. The resolver's internal
//! constants remain the authoritative definition; these `pub` constants
//! simply forward them so both sources stay trivially in sync at compile
//! time (a single list edit touches both).

/// Prelude functions — global callables always available.
pub const PRELUDE_FUNCTIONS: &[&str] = &[
    "print",
    "println",
    "debug",
    "assert",
    "expect",
    "todo",
    "unreachable",
    "sleep",
    "spawn",
];

/// Prelude types — builtin type names.
pub const PRELUDE_TYPES: &[&str] = &[
    "Int", "Float", "Bool", "String", "Char", "Void", "Never", "Optional", "Result",
    "List", "Map", "Set", "Fn", "Duration", "Instant", "Channel",
];

/// Prelude enum variant constructors (Optional, Result).
pub const PRELUDE_CONSTRUCTORS: &[&str] = &["Some", "None", "Ok", "Err"];

/// Prelude traits — standard-library traits always available.
pub const PRELUDE_TRAITS: &[&str] = &[
    "Comparable",
    "Equatable",
    "Hashable",
    "Displayable",
    "Iterator",
    "Iterable",
    "Into",
    "From",
];

/// Primitive type names.
pub const PRIMITIVE_TYPES: &[&str] = &[
    "Int", "Float", "Bool", "String", "Char", "Void", "Never",
];
