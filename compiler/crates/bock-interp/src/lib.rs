// Value has interior mutability (Rc<RefCell<IteratorKind>>) but iterator
// values are never used as map/set keys (ordering panics at runtime).
#![allow(clippy::mutable_key_type)]
//! Bock interp — tree-walking interpreter for executing Bock AIR programs

pub mod builtins;
pub mod env;
pub mod error;
pub mod interp;
pub mod pattern;
pub mod value;

pub use builtins::{
    BuiltinFn, BuiltinRegistry, CallbackInvoker, HigherOrderBuiltinFn, NoOpInvoker, TypeTag,
};
pub use env::{EffectStack, Environment};
pub use error::RuntimeError;
pub use interp::Interpreter;
pub use pattern::match_pattern;
pub use value::{
    BockString, ChannelHandle, EnumValue, FnValue, FutureHandle, IteratorKind, IteratorNext,
    IteratorValue, OrdF64, RecordValue, Value,
};
