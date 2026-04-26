//! Bock AIR — Bock Intermediate Representation node definitions.
//!
//! All four AIR layers (S-AIR, T-AIR, C-AIR, TR-AIR) share the same
//! [`AIRNode`] / [`NodeKind`] types. Later compiler passes progressively fill
//! in the optional layer slots (`type_info`, `ownership`, `context`, `target`).
//!
//! # Module layout
//! - [`node`] — [`AIRNode`], [`NodeKind`], [`NodeIdGen`], auxiliary types.
//! - [`stubs`] — placeholder types for each layer slot.
//! - [`visitor`] — [`Visitor`] trait and `walk_*` helpers for tree traversal.

pub mod compose_context;
pub mod context;
pub mod lower;
pub mod node;
pub mod prelude_vocab;
pub mod registry;
pub mod resolve;
pub mod scope;
pub mod stubs;
pub mod validate_context;
pub mod verify_capabilities;
pub mod visitor;

// Re-export the most-used types at the crate root.
pub use compose_context::compose_context;
pub use context::interpret_context;
pub use lower::lower_module;
pub use node::{
    AIRNode, AirArg, AirHandlerPair, AirInterpolationPart, AirMapEntry, AirRecordField,
    AirRecordPatternField, EnumVariantPayload, NodeId, NodeIdGen, NodeKind, ResultVariant,
};
pub use registry::{
    EnumVariantExport, ExportDetail, ExportKind, ExportedSymbol, ModuleExports, ModuleId,
    ModuleRegistry, RegistryError,
};
pub use resolve::{
    resolve_names, resolve_names_with_registry, Binding, NameKind, ResolvedName, Scope, SymbolTable,
};
pub use scope::{
    build_scope_tree, Binding as ScopeBinding, Scope as ScopeNode, ScopeId, ScopeTree,
};
pub use stubs::{
    security_level_rank, BehavioralModifier, ByteSize, Capability, ContextBlock, ContextMarker,
    Duration, EffectRef, OwnershipInfo, OwnershipState, PerformanceBudget, SecurityInfo, SizeUnit,
    TargetInfo, TimeUnit, TypeInfo, TypeRef, Value, KNOWN_CAPABILITIES, SECURITY_LEVELS,
};
pub use validate_context::{validate_context, StrictnessLevel};
pub use verify_capabilities::{verify_capabilities, CompletenessReport, VerificationMode};
pub use visitor::Visitor;
