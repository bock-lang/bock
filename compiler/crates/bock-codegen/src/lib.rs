//! Bock codegen — target-specific code generation from typed Bock AIR.
//!
//! This crate provides the target profile system and code generator framework.
//! Each supported transpilation target (JS, TS, Python, Rust, Go) is described
//! by a [`TargetProfile`] with a capability matrix, and target-specific code
//! generators implement the [`CodeGenerator`] trait.

pub mod ai_synthesis;
pub mod error;
pub mod gaps;
pub mod generator;
pub mod go;
pub mod js;
pub mod profile;
pub mod py;
pub mod rs;
pub mod ts;

// Re-export primary public API at crate root.
pub use ai_synthesis::{
    cache_at, needs_ai_synthesis, synthesize_and_flush, verify_generated, AiSynthesisDriver,
    SynthesisConfig, SynthesisOutcome, SynthesisStats,
};
pub use bock_ai::{Rule, RuleCache};
pub use error::CodegenError;
pub use gaps::{detect_gaps, CapabilityGap};
pub use generator::{
    CodeGenerator, GeneratedCode, OutputFile, SourceInfo, SourceMap, SourceMapEntry, SourceMapping,
};
pub use go::GoGenerator;
pub use js::JsGenerator;
pub use profile::{
    classify_node, AsyncModel, ErrorHandling, GenericsModel, IndentStyle, MemoryModel,
    NamingConvention, NodeKindHint, Support, TargetCapabilities, TargetConventions, TargetProfile,
};
pub use py::PyGenerator;
pub use rs::RsGenerator;
pub use ts::TsGenerator;
