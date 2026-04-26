//! Bock build — incremental build system orchestrating the full compilation pipeline.
//!
//! This crate provides:
//! - **Module dependency graph** construction from parsed AST imports
//! - **Content hashing** (SHA-256) for change detection
//! - **Minimal rebuild set** computation (changed modules + transitive dependents)
//! - **Build cache** persistence in `.bock/cache/`
//! - **Toolchain detection and invocation** for target compilation

pub mod cache;
pub mod content_hash;
pub mod dep_graph;
pub mod rebuild;
pub mod repair;
pub mod toolchain;

pub use cache::{BuildCache, CacheError};
pub use content_hash::{ContentHash, HashManifest};
pub use dep_graph::{DepGraph, ModuleId};
pub use rebuild::{compute_rebuild_set, ordered_rebuild_set};
pub use repair::{
    apply_template, try_apply_rule, RepairConfig, RepairError, RepairOutcome, RepairPipeline,
    RuleLookupOutcome,
};
pub use toolchain::{
    CompilationResult, DetectedToolchain, ToolchainError, ToolchainRegistry, ToolchainReport,
    ToolchainSpec,
};
