//! Request and response types for the four AI interaction modes
//! defined in §17.8: Generate, Repair, Optimize, and Select.
//!
//! All confidence values are `f64` in the range `0.0..=1.0` (see §17.4).
//! Providers are expected to populate reasoning and alternatives where
//! feasible so that decision manifest entries can be constructed from a
//! response without a second provider round-trip.

use std::collections::HashMap;

use bock_air::AIRNode;
use bock_types::Strictness;
use serde::{Deserialize, Serialize};

// ─── Shared helper types ─────────────────────────────────────────────────────

/// Minimal target profile view consumed by the provider interface.
///
/// Intentionally lightweight: it carries the identifying fields every
/// provider prompt needs (target id and display name) plus summary bags
/// so a richer profile (e.g., the capability matrix owned by
/// `bock-codegen`) can be flattened into textual context without
/// creating a crate dependency cycle.
///
/// D.5 will have `bock-codegen` construct one of these when calling
/// the provider.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TargetProfile {
    /// Short identifier: `"js"`, `"ts"`, `"python"`, `"rust"`, `"go"`, ...
    pub id: String,
    /// Human-readable display name (`"JavaScript"`, `"Rust"`).
    pub display_name: String,
    /// Flattened capability map (e.g., `"memory_model" -> "GC"`).
    pub capabilities: HashMap<String, String>,
    /// Flattened convention map (e.g., `"naming" -> "snake_case"`).
    pub conventions: HashMap<String, String>,
}

/// Per-module context fed to generation/repair/optimize calls.
///
/// Supplies the provider with enough surrounding information to produce
/// idiomatic target code without leaking the full project. Concrete
/// fields may grow in later phases.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModuleContext {
    /// Canonical module path (e.g., `"src/net/http_client.bock"`).
    pub module_path: String,
    /// Names and short signatures of imports visible in this module.
    pub imports: Vec<String>,
    /// Names of sibling definitions in the same module.
    pub siblings: Vec<String>,
    /// Project-level semantic annotations reaching this module
    /// (`@context`, `@domain`, `@security`).
    pub annotations: Vec<String>,
}

/// Reference to a prior decision that should bias the current call.
///
/// Lets providers stay consistent with already-made choices (e.g., same
/// async runtime, same JSON library) without re-examining every option.
/// Corresponds to a decision manifest entry per §17.4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRef {
    /// Decision key, e.g., `"async_runtime"`.
    pub decision: String,
    /// Selected value, e.g., `"tokio"`.
    pub choice: String,
}

/// Hints for the optimization pass (Tier 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptimizationHint {
    /// Prefer runtime performance.
    Performance,
    /// Prefer idiomatic target-language style.
    Idiomatic,
    /// Prefer smaller generated code size.
    CodeSize,
    /// Provider-specific hint carrying a free-form label.
    Custom(String),
}

/// An alternative considered by the provider but not chosen.
///
/// Populated into decision manifest entries so reviewers can see what
/// else the model weighed against the accepted choice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alternative {
    /// Short label identifying the alternative.
    pub label: String,
    /// Why this alternative was rejected.
    pub reasoning: Option<String>,
    /// Confidence assigned to this alternative.
    pub confidence: f64,
}

/// Candidate codegen rule emitted by a successful `repair` response.
///
/// See §17.7. The rule describes a pattern/template pair that, if
/// accepted, would let Tier 2 generate the correct code deterministically
/// for future AIR nodes of the same shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateRule {
    /// Target language the rule applies to (e.g., `"js"`).
    pub target_id: String,
    /// AIR pattern the rule matches, in a format TBD per §17.7.
    pub pattern: String,
    /// Code template to emit when the pattern matches.
    pub template: String,
    /// Priority for conflict resolution (higher wins).
    pub priority: i32,
}

// ─── Generate (Tier 1) ───────────────────────────────────────────────────────

/// Input to [`AiProvider::generate`](crate::provider::AiProvider::generate).
#[derive(Debug, Clone)]
pub struct GenerateRequest {
    /// AIR node to be translated.
    pub node: AIRNode,
    /// Target profile (language + capabilities + conventions).
    pub target: TargetProfile,
    /// Surrounding module context.
    pub module_context: ModuleContext,
    /// Previously accepted decisions the provider should stay consistent with.
    pub prior_decisions: Vec<DecisionRef>,
    /// Graduated strictness level for the current compilation.
    pub strictness: Strictness,
}

/// Response from [`AiProvider::generate`](crate::provider::AiProvider::generate).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenerateResponse {
    /// Target-language code produced for the AIR node.
    pub code: String,
    /// Confidence in the produced code (`0.0..=1.0`, see §17.4).
    pub confidence: f64,
    /// Optional free-form reasoning for decision manifest entries.
    pub reasoning: Option<String>,
    /// Alternatives considered but not chosen.
    pub alternatives: Vec<Alternative>,
}

// ─── Repair (§17.7) ──────────────────────────────────────────────────────────

/// Input to [`AiProvider::repair`](crate::provider::AiProvider::repair).
#[derive(Debug, Clone)]
pub struct RepairRequest {
    /// The code that failed target compilation or verification.
    pub original_code: String,
    /// Diagnostic message from the target compiler/verifier.
    pub compiler_error: String,
    /// The AIR node the failing code was generated from.
    pub node: AIRNode,
    /// Target profile under which the failure occurred.
    pub target: TargetProfile,
}

/// Response from [`AiProvider::repair`](crate::provider::AiProvider::repair).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepairResponse {
    /// Fixed target-language code.
    pub fixed_code: String,
    /// Confidence in the fix (`0.0..=1.0`).
    pub confidence: f64,
    /// Optional candidate rule distilled from the repair, to be proposed
    /// for merging into the local rule cache.
    pub candidate_rule: Option<CandidateRule>,
    /// Optional free-form reasoning.
    pub reasoning: Option<String>,
}

// ─── Optimize (Tier 3) ───────────────────────────────────────────────────────

/// Input to [`AiProvider::optimize`](crate::provider::AiProvider::optimize).
#[derive(Debug, Clone)]
pub struct OptimizeRequest {
    /// Target-language code that already compiles and verifies.
    pub working_code: String,
    /// The AIR node the code was generated from.
    pub node: AIRNode,
    /// Target profile.
    pub target: TargetProfile,
    /// Desired optimization directions.
    pub optimization_hints: Vec<OptimizationHint>,
}

/// Response from [`AiProvider::optimize`](crate::provider::AiProvider::optimize).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptimizeResponse {
    /// Improved target-language code (same semantics as input).
    pub optimized_code: String,
    /// Confidence in the optimization (`0.0..=1.0`).
    pub confidence: f64,
    /// Short descriptions of each observable improvement.
    pub improvements: Vec<String>,
    /// Optional free-form reasoning.
    pub reasoning: Option<String>,
}

// ─── Select (§10.8) ──────────────────────────────────────────────────────────

/// A single option in a closed-set selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectOption {
    /// Stable identifier. Must be unique within a request's option set.
    pub id: String,
    /// Human-readable description shown to the model for discrimination.
    pub description: String,
}

/// Context supplied alongside the option set for a `select()` call.
///
/// The recovery-context discipline of §10.8 applies: no AIR, no source,
/// no call stack. This struct is the sanctioned surface for classification
/// prompts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelectContext {
    /// Error text, if the selection is triggered by a failure.
    pub error: Option<String>,
    /// Semantic annotations reaching the decision site
    /// (`@context`, `@domain`, `@security`).
    pub annotations: Vec<String>,
    /// Recent similar decisions, bounded per §10.8 (10 items).
    pub history: Vec<String>,
    /// Free-form metadata (operation id, elapsed time, attempt count, ...).
    pub metadata: HashMap<String, String>,
}

/// Input to [`AiProvider::select`](crate::provider::AiProvider::select).
#[derive(Debug, Clone)]
pub struct SelectRequest {
    /// The closed set of options. `SelectResponse::selected_id` must be
    /// one of these.
    pub options: Vec<SelectOption>,
    /// Selection context.
    pub context: SelectContext,
    /// Natural-language description of what is being selected.
    pub rationale_prompt: String,
}

/// Response from [`AiProvider::select`](crate::provider::AiProvider::select).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectResponse {
    /// Identifier of the chosen option. MUST be a value present in the
    /// request's option set — callers should verify with
    /// [`crate::provider::validate_select_response`].
    pub selected_id: String,
    /// Confidence in the selection (`0.0..=1.0`).
    pub confidence: f64,
    /// Optional free-form reasoning.
    pub reasoning: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_profile_default_is_empty() {
        let p = TargetProfile::default();
        assert!(p.id.is_empty());
        assert!(p.capabilities.is_empty());
    }

    #[test]
    fn alternative_fields() {
        let alt = Alternative {
            label: "async-std".into(),
            reasoning: Some("less ecosystem momentum".into()),
            confidence: 0.3,
        };
        assert_eq!(alt.label, "async-std");
        assert!(alt.confidence < 0.5);
    }

    #[test]
    fn select_option_equality() {
        let a = SelectOption {
            id: "retry".into(),
            description: "retry with exponential backoff".into(),
        };
        let b = SelectOption {
            id: "retry".into(),
            description: "retry with exponential backoff".into(),
        };
        assert_eq!(a, b);
    }
}

// Confidence is expressed throughout this module as `f64` in the range
// `0.0..=1.0`. See §17.4 of bock-spec.md — raw float, no wrapper.
