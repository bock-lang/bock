//! Strictness governance (§17.6) — policy layer that decides what the
//! AI pipeline may do at each strictness level.
//!
//! The three level-specific helpers answer four concrete questions:
//!
//! 1. *May the AI be consulted at build time?* — gates
//!    [`AiSynthesisRunner`](crate) before issuing a
//!    [`generate`](crate::AiProvider::generate) call.
//! 2. *May the AI be consulted at runtime?* — gates adaptive effect
//!    handlers before issuing a [`select`](crate::AiProvider::select) call.
//! 3. *Should new decisions be auto-pinned?* — populates
//!    [`Decision::pinned`](crate::decision::Decision::pinned) at record time.
//! 4. *May unpinned decisions pass the pre-build manifest gate?* —
//!    drives the production validation step in `bock build`.
//!
//! Policy is intentionally data — each strictness resolves to a single
//! [`StrictnessPolicy`] so call sites can read the rule without
//! reconstructing the match. A build flag like `--strict` reuses the
//! same function by passing [`Strictness::Production`].
//!
//! Production-scope validation ([`validate_production`]) is the most
//! consequential piece: it is the one place that fails a build for
//! governance reasons, and its report format is part of the CLI
//! surface (`bock build` prints `UnpinnedReport::render_error`).

use crate::decision::{Decision, DecisionType, ManifestScope};
use bock_types::Strictness;

/// Policy snapshot for a single strictness level.
///
/// Pure data — `allow_build_ai`, `allow_runtime_ai`, `auto_pin_default`,
/// and `allow_unpinned_in_build` are the four switches the rest of the
/// pipeline consults. [`for_level`] is the only constructor so the
/// mapping from `Strictness` to policy is centralized here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StrictnessPolicy {
    /// The strictness level this policy describes.
    pub level: Strictness,
    /// May the transpiler call an AI provider during build?
    ///
    /// `true` for sketch and development. `false` for production — in
    /// production the pipeline may read cached/pinned decisions and
    /// apply deterministic rules, but must not issue new AI calls.
    pub allow_build_ai: bool,
    /// May an adaptive handler call the AI provider at runtime?
    ///
    /// `true` in sketch and development. `false` in production, where
    /// adaptive handlers degrade to pinned selections only.
    pub allow_runtime_ai: bool,
    /// Default value for [`Decision::pinned`](crate::decision::Decision::pinned)
    /// when recording a *fresh* AI decision at this level.
    ///
    /// Sketch auto-pins for reproducibility; development leaves
    /// decisions unpinned (flagged "needs review"); production never
    /// produces fresh unpinned decisions (any AI call is forbidden).
    pub auto_pin_default: bool,
    /// May a build manifest contain unpinned decisions at this level?
    ///
    /// `true` in sketch and development — they are normal there.
    /// `false` in production — the pre-build gate rejects them.
    pub allow_unpinned_in_build: bool,
}

impl StrictnessPolicy {
    /// Canonical policy for each strictness level.
    ///
    /// The mapping is spec-derived (§17.6, §10.8) and deliberately
    /// centralized so no call site rederives it.
    #[must_use]
    pub fn for_level(level: Strictness) -> Self {
        match level {
            Strictness::Sketch => Self {
                level,
                allow_build_ai: true,
                allow_runtime_ai: true,
                auto_pin_default: false,
                allow_unpinned_in_build: true,
            },
            Strictness::Development => Self {
                level,
                allow_build_ai: true,
                allow_runtime_ai: true,
                auto_pin_default: false,
                allow_unpinned_in_build: true,
            },
            Strictness::Production => Self {
                level,
                allow_build_ai: false,
                allow_runtime_ai: false,
                auto_pin_default: true,
                allow_unpinned_in_build: false,
            },
        }
    }
}

/// Per-decision entry in an [`UnpinnedReport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnpinnedEntry {
    /// Source module the decision was recorded against.
    pub module: String,
    /// Short human-readable decision kind (e.g. `"codegen"`).
    pub kind: &'static str,
    /// Short id displayed to the user (first 8 chars of the full id).
    pub short_id: String,
    /// Full id — what the user passes to `bock override`.
    pub full_id: String,
    /// One-line summary of the decision (e.g. `"JS async pattern"`).
    pub summary: String,
}

/// Result of [`validate_production`]: a sorted list of unpinned build
/// decisions that would block a production build.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UnpinnedReport {
    /// Offending entries, in presentation order (module, then id).
    pub entries: Vec<UnpinnedEntry>,
}

impl UnpinnedReport {
    /// Whether the report is empty (no unpinned decisions).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Render the user-facing error body per the §17.6 UX spec.
    ///
    /// Output matches the prompt-spec format:
    /// ```text
    /// Error: 3 unpinned decisions in production mode
    ///
    ///   src/api/client.bock: codegen #abc12345 (JS async pattern)
    ///   ...
    ///
    /// Run `bock override <id>` to pin individual decisions, or
    /// `bock build --pin-all` in development mode to bulk pin, then
    /// commit .bock/decisions/build/ to version control.
    /// ```
    #[must_use]
    pub fn render_error(&self) -> String {
        let mut out = format!(
            "Error: {} unpinned decision{} in production mode\n\n",
            self.entries.len(),
            if self.entries.len() == 1 { "" } else { "s" }
        );
        for e in &self.entries {
            out.push_str(&format!(
                "  {}: {} #{} ({})\n",
                e.module, e.kind, e.short_id, e.summary
            ));
        }
        out.push('\n');
        out.push_str(
            "Run `bock override <id>` to pin individual decisions, or\n\
             `bock build --pin-all` in development mode to bulk pin, then\n\
             commit .bock/decisions/build/ to version control.\n",
        );
        out
    }
}

/// Scan `decisions` for unpinned build-scope entries.
///
/// Only build-scope decisions are considered — runtime decisions are
/// pinned separately and promoted to build via `bock override --promote`.
///
/// Returns an empty [`UnpinnedReport`] when every build decision is
/// pinned. The caller (e.g. `bock build --strict`) decides whether an
/// empty report means "proceed" or "nothing to do".
#[must_use]
pub fn validate_production(decisions: &[Decision]) -> UnpinnedReport {
    let mut entries: Vec<UnpinnedEntry> = decisions
        .iter()
        .filter(|d| d.decision_type.scope() == ManifestScope::Build && !d.pinned)
        .map(|d| UnpinnedEntry {
            module: d.module.display().to_string(),
            kind: kind_label(d.decision_type),
            short_id: short_id(&d.id),
            full_id: d.id.clone(),
            summary: summarize_choice(d),
        })
        .collect();
    entries.sort_by(|a, b| a.module.cmp(&b.module).then_with(|| a.full_id.cmp(&b.full_id)));
    UnpinnedReport { entries }
}

fn kind_label(t: DecisionType) -> &'static str {
    match t {
        DecisionType::Codegen => "codegen",
        DecisionType::Repair => "repair",
        DecisionType::Optimize => "optimize",
        DecisionType::RuleApplied => "rule_applied",
        DecisionType::HandlerChoice => "handler_choice",
        DecisionType::AdaptiveRecovery => "adaptive_recovery",
    }
}

fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Produce a one-line summary from a decision's `choice` / `reasoning`.
///
/// Prefers reasoning when present (AI providers typically supply a
/// human summary), falls back to the first line of `choice` (typically
/// the generated code snippet's first statement). Truncated to 60
/// chars so the error output stays readable.
fn summarize_choice(d: &Decision) -> String {
    let source = d.reasoning.as_deref().unwrap_or(&d.choice);
    let first_line = source.lines().next().unwrap_or("").trim();
    let truncated = if first_line.chars().count() > 60 {
        let mut s: String = first_line.chars().take(57).collect();
        s.push_str("...");
        s
    } else {
        first_line.to_string()
    };
    if truncated.is_empty() {
        kind_label(d.decision_type).to_string()
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decision::{Decision, DecisionType};
    use chrono::{DateTime, Utc};
    use std::path::PathBuf;

    fn decision(id: &str, module: &str, dt: DecisionType, pinned: bool) -> Decision {
        Decision {
            id: id.into(),
            module: PathBuf::from(module),
            target: Some("js".into()),
            decision_type: dt,
            choice: "let x = 1;".into(),
            alternatives: vec![],
            reasoning: Some("JS async pattern".into()),
            model_id: "stub:stub".into(),
            confidence: 1.0,
            pinned,
            pin_reason: pinned.then(|| "manual".into()),
            pinned_at: pinned.then(Utc::now),
            pinned_by: pinned.then(|| "alice".into()),
            superseded_by: None,
            timestamp: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
        }
    }

    #[test]
    fn sketch_allows_everything_and_does_not_auto_pin() {
        let p = StrictnessPolicy::for_level(Strictness::Sketch);
        assert!(p.allow_build_ai);
        assert!(p.allow_runtime_ai);
        assert!(!p.auto_pin_default);
        assert!(p.allow_unpinned_in_build);
    }

    #[test]
    fn development_allows_ai_but_does_not_auto_pin_by_default() {
        let p = StrictnessPolicy::for_level(Strictness::Development);
        assert!(p.allow_build_ai);
        assert!(p.allow_runtime_ai);
        assert!(!p.auto_pin_default);
        assert!(p.allow_unpinned_in_build);
    }

    #[test]
    fn production_forbids_ai_and_unpinned_entries() {
        let p = StrictnessPolicy::for_level(Strictness::Production);
        assert!(!p.allow_build_ai);
        assert!(!p.allow_runtime_ai);
        assert!(!p.allow_unpinned_in_build);
        // auto_pin_default is `true` but is only used by a successful
        // build; production never records a *fresh* decision.
        assert!(p.auto_pin_default);
    }

    #[test]
    fn validate_production_is_empty_when_all_pinned() {
        let ds = vec![
            decision("a1", "src/a.bock", DecisionType::Codegen, true),
            decision("b2", "src/b.bock", DecisionType::Repair, true),
        ];
        assert!(validate_production(&ds).is_empty());
    }

    #[test]
    fn validate_production_lists_each_unpinned_build_decision() {
        let ds = vec![
            decision("abc12345", "src/api/client.bock", DecisionType::Codegen, false),
            decision("def45678", "src/api/client.bock", DecisionType::Codegen, false),
            decision("ghi78901", "src/models/user.bock", DecisionType::Repair, false),
        ];
        let report = validate_production(&ds);
        assert_eq!(report.entries.len(), 3);
        // Sorted: client.bock (abc, def) before user.bock (ghi).
        assert_eq!(report.entries[0].module, "src/api/client.bock");
        assert_eq!(report.entries[0].short_id, "abc12345");
        assert_eq!(report.entries[0].kind, "codegen");
        assert_eq!(report.entries[2].module, "src/models/user.bock");
        assert_eq!(report.entries[2].kind, "repair");
    }

    #[test]
    fn validate_production_ignores_runtime_scope_and_pinned_entries() {
        let ds = vec![
            decision("rt1", "src/a.bock", DecisionType::AdaptiveRecovery, false),
            decision("pinned", "src/a.bock", DecisionType::Codegen, true),
            decision("loose", "src/a.bock", DecisionType::Codegen, false),
        ];
        let report = validate_production(&ds);
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].full_id, "loose");
    }

    #[test]
    fn render_error_matches_spec_shape() {
        let ds = vec![
            decision("abc12345", "src/api/client.bock", DecisionType::Codegen, false),
        ];
        let report = validate_production(&ds);
        let rendered = report.render_error();
        assert!(rendered.starts_with("Error: 1 unpinned decision in production mode"));
        assert!(rendered.contains("src/api/client.bock: codegen #abc12345"));
        assert!(rendered.contains("bock override"));
        assert!(rendered.contains("bock build --pin-all"));
    }

    #[test]
    fn render_error_pluralizes_when_many() {
        let ds = vec![
            decision("a", "src/a.bock", DecisionType::Codegen, false),
            decision("b", "src/b.bock", DecisionType::Codegen, false),
        ];
        let rendered = validate_production(&ds).render_error();
        assert!(rendered.starts_with("Error: 2 unpinned decisions in production mode"));
    }

    #[test]
    fn summarize_truncates_long_lines() {
        let mut d = decision("x", "src/x.bock", DecisionType::Codegen, false);
        d.reasoning = None;
        d.choice = "a".repeat(100);
        let s = summarize_choice(&d);
        assert!(s.chars().count() <= 60);
        assert!(s.ends_with("..."));
    }

    #[test]
    fn summarize_uses_first_line_of_choice_when_no_reasoning() {
        let mut d = decision("x", "src/x.bock", DecisionType::Codegen, false);
        d.reasoning = None;
        d.choice = "first line\nsecond line".into();
        assert_eq!(summarize_choice(&d), "first line");
    }
}
