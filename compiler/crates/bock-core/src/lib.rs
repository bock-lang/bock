// Value has interior mutability (Rc<RefCell<IteratorKind>>) but iterator
// values are never used as map/set keys (ordering panics at runtime).
#![allow(clippy::mutable_key_type)]
//! Bock core — core standard library primitives, collections, monadic types, iterators, and built-in operations.
//!
//! This crate populates the interpreter's [`BuiltinRegistry`] with the full
//! method suites for primitive types (Int, Float, Bool, String, Char),
//! collection types (List, Map, Set), monadic types (Optional, Result),
//! iterator protocol (lazy combinators: map, filter, take, skip, enumerate, zip, chain, collect),
//! and their core trait implementations (Comparable, Equatable, Hashable, Displayable).

pub mod adaptive;
pub mod collections;
pub mod iterator;
pub mod option_result;
pub mod primitives;
pub mod string_builder;
pub mod time;
pub mod traits;

// ── Stub modules for unimplemented core modules ─────────────────────────────
// These exist so that downstream code can reference the module paths without
// error, even though they have no functionality yet.
pub mod concurrency;
pub mod effect;
pub mod error;
pub mod math;
pub mod memory;
pub mod test;

#[cfg(test)]
mod callback_tests;

pub use traits::{ConversionDirection, TraitDispatch};

use bock_interp::BuiltinRegistry;

/// Register all primitive, collection, and monadic type methods and trait
/// implementations into the given [`BuiltinRegistry`].
///
/// Call this during interpreter initialization to make all built-in methods
/// available.
pub fn register_core(registry: &mut BuiltinRegistry) {
    // Primitives
    primitives::int::register(registry);
    primitives::float::register(registry);
    primitives::bool::register(registry);
    primitives::string::register(registry);
    primitives::char::register(registry);
    primitives::duration::register(registry);
    primitives::instant::register(registry);

    // Collections
    collections::list::register(registry);
    collections::map::register(registry);
    collections::set::register(registry);

    // Option & Result
    option_result::optional::register(registry);
    option_result::result::register(registry);

    // Iterator protocol
    iterator::register(registry);

    // StringBuilder
    string_builder::register(registry);

    // Time (Duration/Instant already registered above as primitives; sleep is prelude)
    time::register(registry);

    // Concurrency primitives: Channel, spawn
    concurrency::register(registry);
}
