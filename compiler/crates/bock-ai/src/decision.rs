//! Decision manifest record types (§17.4).
//!
//! Per the 2026-04-22 spec amendment, decisions are split into two
//! populations:
//!
//! * **Build decisions** — produced during compilation (codegen, repair,
//!   optimization, rule application). Stable artifacts of the build,
//!   committed to version control under `.bock/decisions/build/`.
//! * **Runtime decisions** — produced during execution (adaptive effect
//!   handler selection, §10.8). Environment-local, not committed,
//!   stored under `.bock/decisions/runtime/`.
//!
//! [`DecisionType::scope`] routes each variant to the correct
//! manifest. New variants must declare their scope explicitly so the
//! routing stays exhaustive.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A single decision recorded in the manifest.
///
/// `id` is the content hash of the originating request — the same value
/// the [`AiCache`](crate::cache::AiCache) uses for its key — so that
/// pinned decisions can be replayed deterministically.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    /// Content hash of the originating request.
    pub id: String,
    /// Source module the decision applies to.
    pub module: PathBuf,
    /// Target language identifier (e.g., `"rust"`). `None` for runtime
    /// decisions which are target-independent.
    pub target: Option<String>,
    /// What kind of decision this is — drives manifest routing via
    /// [`DecisionType::scope`].
    pub decision_type: DecisionType,
    /// Selected choice (e.g., `"tokio"`, generated code snippet, or
    /// strategy identifier).
    pub choice: String,
    /// Alternatives considered but not chosen.
    pub alternatives: Vec<String>,
    /// Optional free-form reasoning supplied by the provider.
    pub reasoning: Option<String>,
    /// Stable provider/model identifier (e.g., `"anthropic:claude-opus"`).
    pub model_id: String,
    /// Confidence in the choice, `0.0..=1.0` (§17.4).
    pub confidence: f64,
    /// Whether this decision is pinned (replayed identically on rebuild).
    pub pinned: bool,
    /// If pinned, why — `"cache-replay"`, `"auto-pin"`, `"manual"`, etc.
    pub pin_reason: Option<String>,
    /// When the decision was pinned, if at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_at: Option<DateTime<Utc>>,
    /// Who pinned the decision (free-form identifier — username or CI tag).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_by: Option<String>,
    /// If this entry was superseded by a later promotion, the id of the
    /// successor decision in the build manifest. Set on a runtime decision
    /// after `bock override --promote` copies it into the build scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
    /// When the decision was recorded.
    pub timestamp: DateTime<Utc>,
}

/// Categories of decisions recorded by the compiler and runtime.
///
/// Each variant has a fixed [`scope`](Self::scope), so manifest routing
/// is exhaustive and adding a variant forces the author to pick a
/// scope explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionType {
    // ── Build-scope ──────────────────────────────────────────────────
    /// Tier 1 generation (§17.2).
    Codegen,
    /// Repair pass following a target-compiler failure (§17.7).
    Repair,
    /// Tier 3 optimization pass (§17.2).
    Optimize,
    /// Application of a Tier 2 rule from the local rule cache (§17.7).
    RuleApplied,

    /// A runtime adaptive selection that has been promoted to a
    /// build-time pin via `bock override --promote` (§10.8). Routes
    /// to the build manifest so subsequent production builds replay
    /// the recovery strategy deterministically.
    HandlerChoice,

    // ── Runtime-scope ────────────────────────────────────────────────
    /// Adaptive recovery strategy selection (§10.8).
    AdaptiveRecovery,
}

impl DecisionType {
    /// Routing scope for this decision type.
    ///
    /// Exhaustive over the variants — adding a new variant is a
    /// compile error until its scope is named here.
    #[must_use]
    pub fn scope(&self) -> ManifestScope {
        match self {
            Self::AdaptiveRecovery => ManifestScope::Runtime,
            Self::Codegen
            | Self::Repair
            | Self::Optimize
            | Self::RuleApplied
            | Self::HandlerChoice => ManifestScope::Build,
        }
    }
}

/// Which manifest a decision belongs to.
///
/// Matches the directory split under `.bock/decisions/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestScope {
    /// Build-time decisions — committed to VCS.
    Build,
    /// Runtime decisions — local only.
    Runtime,
}

impl ManifestScope {
    /// Subdirectory name within `.bock/decisions/` for this scope.
    #[must_use]
    pub fn dir_name(self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Runtime => "runtime",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codegen_routes_to_build() {
        assert_eq!(DecisionType::Codegen.scope(), ManifestScope::Build);
        assert_eq!(DecisionType::Repair.scope(), ManifestScope::Build);
        assert_eq!(DecisionType::Optimize.scope(), ManifestScope::Build);
        assert_eq!(DecisionType::RuleApplied.scope(), ManifestScope::Build);
        assert_eq!(DecisionType::HandlerChoice.scope(), ManifestScope::Build);
    }

    #[test]
    fn adaptive_recovery_routes_to_runtime() {
        assert_eq!(
            DecisionType::AdaptiveRecovery.scope(),
            ManifestScope::Runtime
        );
    }

    #[test]
    fn manifest_scope_dir_names() {
        assert_eq!(ManifestScope::Build.dir_name(), "build");
        assert_eq!(ManifestScope::Runtime.dir_name(), "runtime");
    }

    #[test]
    fn decision_round_trips_through_json() {
        let d = Decision {
            id: "abc123".into(),
            module: PathBuf::from("src/lib.bock"),
            target: Some("rust".into()),
            decision_type: DecisionType::Codegen,
            choice: "tokio".into(),
            alternatives: vec!["async-std".into(), "smol".into()],
            reasoning: Some("axum requires tokio".into()),
            model_id: "anthropic:claude-opus".into(),
            confidence: 0.92,
            pinned: false,
            pin_reason: None,
            pinned_at: None,
            pinned_by: None,
            superseded_by: None,
            timestamp: DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
        };
        let s = serde_json::to_string(&d).expect("serialize");
        let d2: Decision = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(d, d2);
    }

    #[test]
    fn pin_metadata_round_trips() {
        let d = Decision {
            id: "abc123".into(),
            module: PathBuf::from("src/lib.bock"),
            target: Some("rust".into()),
            decision_type: DecisionType::Codegen,
            choice: "tokio".into(),
            alternatives: vec![],
            reasoning: None,
            model_id: "anthropic:claude-opus".into(),
            confidence: 0.92,
            pinned: true,
            pin_reason: Some("reviewed by @alice 2026-04-22".into()),
            pinned_at: Some(DateTime::<Utc>::from_timestamp(1_745_000_000, 0).unwrap()),
            pinned_by: Some("alice".into()),
            superseded_by: None,
            timestamp: DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
        };
        let s = serde_json::to_string(&d).expect("serialize");
        let d2: Decision = serde_json::from_str(&s).expect("deserialize");
        assert_eq!(d, d2);
    }

    #[test]
    fn missing_optional_fields_deserialize_as_none() {
        // Older manifest files written before the pin-metadata fields
        // existed must still parse — the serde defaults cover the gap.
        let json = r#"{
            "id": "x",
            "module": "src/lib.bock",
            "target": "rust",
            "decision_type": "codegen",
            "choice": "tokio",
            "alternatives": [],
            "reasoning": null,
            "model_id": "stub:stub",
            "confidence": 1.0,
            "pinned": false,
            "pin_reason": null,
            "timestamp": "2026-04-22T10:00:00Z"
        }"#;
        let d: Decision = serde_json::from_str(json).expect("backward-compatible parse");
        assert!(d.pinned_at.is_none());
        assert!(d.pinned_by.is_none());
        assert!(d.superseded_by.is_none());
    }
}
