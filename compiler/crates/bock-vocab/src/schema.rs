//! Serde types for the vocabulary JSON schema.
//!
//! The JSON layout is consumed by the VS Code extension and any other
//! tooling that wants a stable, versioned view of the language. The shape
//! is intentionally documentation-oriented: each entry carries enough
//! metadata (purpose, examples, spec references) to populate hover text
//! or a language-reference page without additional lookups.

use serde::{Deserialize, Serialize};

/// Top-level vocabulary emitted for an Bock compiler version.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Vocab {
    /// Compiler version (matches the `version` field of the workspace).
    pub version: String,
    /// Language-level vocabulary: keywords, operators, annotations.
    pub language: LanguageVocab,
    /// Standard library vocabulary: modules, types, methods.
    pub stdlib: StdlibVocab,
    /// Diagnostic codes and their documentation.
    pub diagnostics: DiagnosticsVocab,
    /// Tooling surface: commands, targets, AI providers.
    pub tooling: ToolingVocab,
}

// ─── Language ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LanguageVocab {
    pub keywords: Vec<Keyword>,
    pub operators: Vec<Operator>,
    pub annotations: Vec<Annotation>,
    pub strictness_levels: Vec<StrictnessLevel>,
    pub primitive_types: Vec<PrimitiveType>,
    pub prelude_types: Vec<Symbol>,
    pub prelude_functions: Vec<Symbol>,
    pub prelude_traits: Vec<Symbol>,
    pub prelude_constructors: Vec<Symbol>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Keyword {
    pub name: String,
    pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Operator {
    pub symbol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub precedence: Option<u8>,
    pub associativity: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Annotation {
    pub name: String,
    pub params: String,
    pub purpose: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StrictnessLevel {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrimitiveType {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub kind: String,
    pub signature: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
}

// ─── Stdlib ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StdlibVocab {
    pub modules: Vec<Module>,
    /// Flat list of receiver types that have registered methods, with the
    /// method names per receiver. Mirrors the [`bock_interp::BuiltinRegistry`]
    /// dispatch table.
    pub builtin_methods: Vec<BuiltinMethodGroup>,
    /// Flat list of registered global builtin functions.
    pub builtin_globals: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Module {
    pub path: String,
    pub types: Vec<Symbol>,
    pub functions: Vec<Symbol>,
    pub effects: Vec<Symbol>,
    pub traits: Vec<Symbol>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuiltinMethodGroup {
    /// Receiver type name (e.g. `"Int"`, `"List"`).
    pub receiver: String,
    pub methods: Vec<String>,
}

// ─── Diagnostics ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiagnosticsVocab {
    pub codes: Vec<DiagnosticCode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiagnosticCode {
    pub code: String,
    pub severity: String,
    pub summary: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bad_example: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub good_example: Option<String>,
    pub spec_refs: Vec<String>,
    pub related_codes: Vec<String>,
}

// ─── Tooling ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolingVocab {
    pub targets: Vec<Target>,
    pub ai_providers: Vec<String>,
    pub commands: Vec<Command>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Target {
    pub id: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Command {
    pub name: String,
    pub summary: String,
}
