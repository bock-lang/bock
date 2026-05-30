//! Prelude vocabulary — names that are always in scope without an explicit
//! `use`. Mirrors the seed lists in `SymbolTable::seed_prelude`.
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
///
/// `Ordering` is included because §18.2 lists it in the prelude (it is the
/// return type of `Comparable.compare`); its definition lives in
/// `core.compare` and is re-exported here name-wise.
pub const PRELUDE_TYPES: &[&str] = &[
    "Int", "Float", "Bool", "String", "Char", "Void", "Never", "Optional", "Result", "List", "Map",
    "Set", "Fn", "Duration", "Instant", "Channel", "Ordering",
];

/// Prelude enum variant constructors (`Optional`, `Result`, `Ordering`).
///
/// `Less`/`Equal`/`Greater` are the variants of `Ordering` (defined in
/// `core.compare`); §18.2 lists them in the prelude so user code can name them
/// without an import when implementing `compare` or matching a `cmp()` result.
pub const PRELUDE_CONSTRUCTORS: &[&str] =
    &["Some", "None", "Ok", "Err", "Less", "Equal", "Greater"];

/// Prelude traits — standard-library traits always available.
///
/// Mirrors the §18.2 core-trait list (`Comparable`, `Equatable`, `Hashable`,
/// `Displayable`, `Serializable`, `Cloneable`, `Default`, `Into`, `From`,
/// `Iterator`, `Iterable`) plus the defined-in-`core` convert/error traits
/// `TryFrom` (from `core.convert`) and `Error` (from `core.error`), which the
/// prelude re-exports so user code can name them without an import. Traits
/// whose definitions live in an embedded `core.*` module (`Comparable`,
/// `Equatable`, `Into`, `From`, `TryFrom`, `Displayable`, `Error`) are also
/// seeded with their full method signatures in the type checker via
/// `bock_types::seed_prelude`; the rest (`Hashable`, `Serializable`,
/// `Cloneable`, `Default`, `Iterator`, `Iterable`) are name-level only until
/// their `core.*` definitions ship.
pub const PRELUDE_TRAITS: &[&str] = &[
    "Comparable",
    "Equatable",
    "Hashable",
    "Displayable",
    "Serializable",
    "Cloneable",
    "Default",
    "Into",
    "From",
    "TryFrom",
    "Iterator",
    "Iterable",
    "Error",
];

/// Primitive type names.
pub const PRIMITIVE_TYPES: &[&str] = &["Int", "Float", "Bool", "String", "Char", "Void", "Never"];
