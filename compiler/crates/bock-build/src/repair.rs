//! Codegen feedback loop (§17.7).
//!
//! The pipeline:
//!
//! 1. Compile already-generated target code via [`ToolchainRegistry`].
//! 2. On failure — if a provider is configured and strictness allows —
//!    call [`AiProvider::repair`] with the compiler error + AIR node.
//! 3. Gate the repair on confidence (pinned cache replay bypasses, as
//!    in D.5). Accepted repairs are retried against the compiler; if
//!    they succeed, emit a build-scope [`Decision`].
//! 4. Extract any [`CandidateRule`] on the repair response into the
//!    local [`RuleCache`]. A second build can then hit the rule first
//!    and skip the AI round-trip entirely.
//! 5. Cap retries ([`RepairConfig::max_attempts`]; default 2). After
//!    the cap the last compiler error is returned.
//!
//! This module does **not** drive initial generation — D.5's
//! `AiSynthesisDriver` already does that. It owns the *post-generation*
//! feedback loop: take already-produced code + the AIR node it came
//! from, and reconcile it with the target compiler.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bock_ai::{
    compute_key, node_kind_name, AiProvider, CandidateRule, Decision, DecisionType, ManifestError,
    ManifestWriter, RepairRequest, Rule, RuleCache, TargetProfile,
};
use bock_air::AIRNode;
use bock_types::Strictness;
use chrono::Utc;

use crate::toolchain::{CompilationResult, ToolchainError, ToolchainRegistry};

// ─── Configuration ───────────────────────────────────────────────────────────

/// Runtime knobs for a single module's repair pass.
#[derive(Debug, Clone)]
pub struct RepairConfig {
    /// Maximum repair retries per node before giving up (default 2).
    ///
    /// Caps infinite loops where repair keeps producing non-compiling
    /// code; the final compiler error is returned when exceeded.
    pub max_attempts: usize,
    /// Minimum confidence to accept a repair (§17.4 / §17.8).
    pub confidence_threshold: f64,
    /// Strictness for the current compilation. Gates rule auto-apply
    /// vs. pinned-only per §17.7.
    pub strictness: Strictness,
    /// Module path written into decision records.
    pub module_path: PathBuf,
}

impl Default for RepairConfig {
    fn default() -> Self {
        Self {
            max_attempts: 2,
            confidence_threshold: 0.75,
            strictness: Strictness::Development,
            module_path: PathBuf::new(),
        }
    }
}

// ─── Outcome ─────────────────────────────────────────────────────────────────

/// Result of a repair pass over a single generated snippet.
#[derive(Debug, Clone, PartialEq)]
pub enum RepairOutcome {
    /// Compiler accepted the original code on the first try — no repair
    /// was attempted.
    FirstTrySuccess {
        /// The code that compiled as-is.
        code: String,
    },
    /// Repair produced working code after one or more iterations.
    Repaired {
        /// The fixed code the compiler accepted.
        code: String,
        /// Number of repair attempts used (>= 1).
        attempts: usize,
        /// `true` if the repair response carried a candidate rule that
        /// was persisted to the [`RuleCache`].
        rule_added: bool,
    },
    /// The last repair attempt was rejected by the confidence gate and
    /// no further attempts were allowed.
    RejectedLowConfidence {
        /// Confidence reported by the provider.
        confidence: f64,
        /// The last compiler error we couldn't repair past.
        compiler_error: String,
    },
    /// Every attempt was made and the compiler still rejected the code.
    Exhausted {
        /// Number of repair attempts made.
        attempts: usize,
        /// The last compiler error encountered.
        compiler_error: String,
    },
    /// No provider was configured, so repair was skipped.
    NoProvider {
        /// Original compiler error.
        compiler_error: String,
    },
    /// Production strictness forbade calling the provider.
    ProductionBlocked {
        /// Original compiler error.
        compiler_error: String,
    },
    /// The provider returned an error.
    ProviderError {
        /// Human-readable provider error message.
        message: String,
    },
}

impl RepairOutcome {
    /// Returns the working code, if the outcome has one.
    #[must_use]
    pub fn accepted_code(&self) -> Option<&str> {
        match self {
            Self::FirstTrySuccess { code } | Self::Repaired { code, .. } => Some(code),
            _ => None,
        }
    }

    /// `true` if the repair loop produced working code (first-try or
    /// after at least one repair).
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(self, Self::FirstTrySuccess { .. } | Self::Repaired { .. })
    }
}

// ─── Errors ──────────────────────────────────────────────────────────────────

/// Fatal errors that prevent the repair pipeline from making a decision.
///
/// These are distinct from [`RepairOutcome`] variants: an outcome like
/// `Exhausted` means the pipeline ran normally but compilation kept
/// failing, whereas these errors mean the pipeline itself couldn't run
/// (e.g., the manifest couldn't be flushed).
#[derive(Debug, thiserror::Error)]
pub enum RepairError {
    /// Manifest write failed.
    #[error("manifest error: {0}")]
    Manifest(#[from] ManifestError),
    /// Rule cache write failed.
    #[error("rule cache error: {0}")]
    Rules(#[from] bock_ai::RuleCacheError),
    /// I/O error writing or reading the candidate code for toolchain
    /// invocation.
    #[error("I/O error during repair: {0}")]
    Io(#[from] std::io::Error),
    /// An unexpected toolchain error that isn't a compilation failure —
    /// e.g., a missing toolchain binary.
    #[error("toolchain error: {0}")]
    Toolchain(#[from] ToolchainError),
}

// ─── Pipeline ────────────────────────────────────────────────────────────────

/// Wires together the toolchain, AI provider, rule cache, and manifest
/// writer into a single repair pass per AIR node.
///
/// The pipeline is cheap to construct and safe to share across threads;
/// the provider and manifest writer are behind `Arc`.
pub struct RepairPipeline {
    provider: Option<Arc<dyn AiProvider>>,
    rules: Option<RuleCache>,
    manifest: Option<Arc<Mutex<ManifestWriter>>>,
    toolchain: Arc<ToolchainRegistry>,
    config: RepairConfig,
}

impl RepairPipeline {
    /// Constructs a pipeline with no AI provider. Every failing compile
    /// ends as [`RepairOutcome::NoProvider`]; the toolchain still runs.
    #[must_use]
    pub fn without_provider(toolchain: Arc<ToolchainRegistry>, config: RepairConfig) -> Self {
        Self {
            provider: None,
            rules: None,
            manifest: None,
            toolchain,
            config,
        }
    }

    /// Constructs a fully wired pipeline.
    #[must_use]
    pub fn new(
        provider: Arc<dyn AiProvider>,
        rules: Option<RuleCache>,
        manifest: Option<Arc<Mutex<ManifestWriter>>>,
        toolchain: Arc<ToolchainRegistry>,
        config: RepairConfig,
    ) -> Self {
        Self {
            provider: Some(provider),
            rules,
            manifest,
            toolchain,
            config,
        }
    }

    /// Borrow the active config.
    #[must_use]
    pub fn config(&self) -> &RepairConfig {
        &self.config
    }

    /// Run the repair loop for one generated snippet.
    ///
    /// Writes `code` (and each repaired candidate) to `source_path` so
    /// the target toolchain can read it. Returns the outcome; the
    /// caller decides whether to fail the build or keep going.
    ///
    /// # Errors
    /// Returns [`RepairError`] for filesystem failures writing the
    /// candidate source, missing toolchains (anything other than
    /// `InvocationFailed`), manifest write failures, and rule cache
    /// write failures.
    pub async fn run(
        &self,
        target: &TargetProfile,
        node: &AIRNode,
        initial_code: String,
        source_path: &Path,
    ) -> Result<RepairOutcome, RepairError> {
        let target_id = target.id.clone();

        // First compile attempt against the generator's code.
        write_candidate(source_path, &initial_code)?;
        match self.toolchain.invoke(&target_id, source_path, false) {
            Ok(_) => return Ok(RepairOutcome::FirstTrySuccess { code: initial_code }),
            Err(ToolchainError::InvocationFailed { .. }) => { /* fall through to repair */ }
            Err(other) => return Err(other.into()),
        }

        // Read first error.
        let mut compiler_error =
            invocation_error(&self.toolchain.invoke(&target_id, source_path, false));

        // No provider → no repair path.
        let Some(provider) = self.provider.clone() else {
            return Ok(RepairOutcome::NoProvider { compiler_error });
        };

        // Production strictness forbids unpinned AI repair calls. The
        // guard mirrors D.5's `ProductionUnpinned` logic: at production
        // the only acceptable path is a pre-pinned rule, which the
        // caller was expected to try first via `RuleCache::lookup`.
        if matches!(self.config.strictness, Strictness::Production) {
            return Ok(RepairOutcome::ProductionBlocked { compiler_error });
        }

        let mut current_code = initial_code;
        let mut attempts: usize = 0;
        let mut rule_added = false;

        while attempts < self.config.max_attempts {
            attempts += 1;

            let request = RepairRequest {
                original_code: current_code.clone(),
                compiler_error: compiler_error.clone(),
                node: node.clone(),
                target: target.clone(),
            };
            let response = match provider.repair(&request).await {
                Ok(r) => r,
                Err(e) => {
                    return Ok(RepairOutcome::ProviderError {
                        message: format!("{e}"),
                    });
                }
            };

            if response.confidence < self.config.confidence_threshold {
                // Persist the compiler error we stopped on. Don't retry
                // — the model has told us it isn't confident.
                return Ok(RepairOutcome::RejectedLowConfidence {
                    confidence: response.confidence,
                    compiler_error,
                });
            }

            // Try the repaired code.
            write_candidate(source_path, &response.fixed_code)?;
            match self.toolchain.invoke(&target_id, source_path, false) {
                Ok(_) => {
                    // Compile succeeded. Record the repair decision,
                    // maybe persist the candidate rule, and return.
                    self.record_repair(node, target, &response, &compiler_error)?;
                    if let Some(candidate) = response.candidate_rule.as_ref() {
                        if let Some(rules) = &self.rules {
                            let rule = persist_rule(rules, candidate, node, response.confidence)?;
                            rule_added = true;
                            self.record_rule_applied(node, target, &rule)?;
                        }
                    }
                    return Ok(RepairOutcome::Repaired {
                        code: response.fixed_code,
                        attempts,
                        rule_added,
                    });
                }
                Err(ToolchainError::InvocationFailed { .. }) => {
                    // Roll forward: keep the attempted code and the new
                    // error for the next iteration.
                    current_code = response.fixed_code;
                    compiler_error =
                        invocation_error(&self.toolchain.invoke(&target_id, source_path, false));
                }
                Err(other) => return Err(other.into()),
            }
        }

        Ok(RepairOutcome::Exhausted {
            attempts,
            compiler_error,
        })
    }

    fn record_repair(
        &self,
        node: &AIRNode,
        target: &TargetProfile,
        response: &bock_ai::RepairResponse,
        original_error: &str,
    ) -> Result<(), ManifestError> {
        let Some(manifest) = &self.manifest else {
            return Ok(());
        };
        let mut mw = manifest.lock().expect("manifest writer mutex poisoned");

        let provider_id = self
            .provider
            .as_ref()
            .map_or_else(|| "deterministic".into(), |p| p.model_id());
        let id = decision_id("repair", node, target);
        mw.record(Decision {
            id,
            module: self.config.module_path.clone(),
            target: Some(target.id.clone()),
            decision_type: DecisionType::Repair,
            choice: response.fixed_code.clone(),
            alternatives: Vec::new(),
            reasoning: Some(format!(
                "compiler error: {}; fixed by AI repair ({})",
                summarize(original_error),
                response
                    .reasoning
                    .as_deref()
                    .unwrap_or("no reasoning supplied")
            )),
            model_id: provider_id,
            confidence: response.confidence,
            pinned: false,
            pin_reason: None,
            pinned_at: None,
            pinned_by: None,
            superseded_by: None,
            timestamp: Utc::now(),
        });
        Ok(())
    }

    fn record_rule_applied(
        &self,
        node: &AIRNode,
        target: &TargetProfile,
        rule: &Rule,
    ) -> Result<(), ManifestError> {
        let Some(manifest) = &self.manifest else {
            return Ok(());
        };
        let mut mw = manifest.lock().expect("manifest writer mutex poisoned");

        let provider_id = self
            .provider
            .as_ref()
            .map_or_else(|| "deterministic".into(), |p| p.model_id());
        let id = decision_id(&format!("rule:{}", rule.id), node, target);
        mw.record(Decision {
            id,
            module: self.config.module_path.clone(),
            target: Some(target.id.clone()),
            decision_type: DecisionType::RuleApplied,
            choice: format!("rule {} matched pattern {}", rule.id, rule.node_kind),
            alternatives: Vec::new(),
            reasoning: Some(format!(
                "candidate rule extracted from repair; future {} nodes may skip AI",
                rule.node_kind
            )),
            model_id: provider_id,
            confidence: rule.confidence,
            pinned: rule.pinned,
            pin_reason: rule.pinned.then(|| "manual".into()),
            pinned_at: rule.pinned.then(Utc::now),
            pinned_by: rule.pinned.then(|| "rule-author".into()),
            superseded_by: None,
            timestamp: Utc::now(),
        });
        Ok(())
    }
}

// ─── Rule-cache-first pre-AI hook ────────────────────────────────────────────

/// Outcome of the rule-cache pre-AI hook in D.6 step 3.
#[derive(Debug, Clone, PartialEq)]
pub enum RuleLookupOutcome {
    /// A cached rule matched and was applied; no AI call needed.
    Applied {
        /// The rule that matched.
        rule: Rule,
        /// The code produced by applying the template.
        code: String,
    },
    /// No rule matched for this node kind; the caller should fall
    /// through to Tier 1 AI synthesis.
    Miss,
    /// Production strictness required a pinned rule and none was
    /// found; the caller decides how to handle.
    MissNeedsPin,
}

/// Apply the rule cache *before* an AI call.
///
/// Loads rules for `target_id`, filters by `production_only_pinned`,
/// and returns the highest-priority match or a miss. On a hit, the
/// caller records the [`DecisionType::RuleApplied`] decision so
/// reviewers can see that the rule covered this node without AI.
///
/// # Errors
/// Returns [`bock_ai::RuleCacheError`] on I/O or parse failure.
pub fn try_apply_rule(
    rules: &RuleCache,
    target_id: &str,
    node: &AIRNode,
    strictness: Strictness,
) -> Result<RuleLookupOutcome, bock_ai::RuleCacheError> {
    let production_only = matches!(strictness, Strictness::Production);
    let Some(rule) = rules.lookup(target_id, node, production_only)? else {
        return Ok(if production_only {
            RuleLookupOutcome::MissNeedsPin
        } else {
            RuleLookupOutcome::Miss
        });
    };
    let code = apply_template(&rule.template, node);
    Ok(RuleLookupOutcome::Applied { rule, code })
}

/// v1 template application: return the template verbatim.
///
/// The rule format is TBD per §17.7; a real interpolation engine
/// (substituting `{{ scrutinee }}`, `{{ arms }}`, etc. from the node's
/// children) is out of scope for D.6. Callers should treat the
/// returned string as the rule's generated code for the current node.
#[must_use]
pub fn apply_template(template: &str, _node: &AIRNode) -> String {
    template.to_string()
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn write_candidate(source_path: &Path, code: &str) -> std::io::Result<()> {
    if let Some(parent) = source_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(source_path, code)
}

fn invocation_error(result: &Result<CompilationResult, ToolchainError>) -> String {
    match result {
        Ok(_) => "compilation unexpectedly succeeded".into(),
        Err(ToolchainError::InvocationFailed {
            stdout,
            stderr,
            exit_code,
            ..
        }) => {
            let diag = if stderr.is_empty() { stdout } else { stderr };
            format!(
                "exit {}: {}",
                exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".into()),
                summarize(diag)
            )
        }
        Err(e) => format!("{e}"),
    }
}

fn summarize(error: &str) -> String {
    // Trim noisy trailing whitespace and truncate to a single line-ish
    // excerpt for inclusion in decision records.
    let trimmed = error.trim();
    if trimmed.len() <= 512 {
        return trimmed.into();
    }
    let mut s = String::with_capacity(515);
    s.push_str(&trimmed[..512]);
    s.push_str("...");
    s
}

fn persist_rule(
    rules: &RuleCache,
    candidate: &CandidateRule,
    node: &AIRNode,
    confidence: f64,
) -> Result<Rule, bock_ai::RuleCacheError> {
    let kind = node_kind_name(&node.kind);
    let rule = Rule::from_candidate(candidate, kind, confidence);
    rules.insert(&rule)?;
    Ok(rule)
}

fn decision_id(prefix: &str, node: &AIRNode, target: &TargetProfile) -> String {
    #[derive(serde::Serialize)]
    struct Keyed<'a> {
        prefix: &'a str,
        target: &'a str,
        node_debug: String,
    }
    let keyed = Keyed {
        prefix,
        target: &target.id,
        node_debug: format!("{node:?}"),
    };
    compute_key(&keyed).unwrap_or_else(|_| format!("{prefix}-{}", node.id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bock_air::{NodeIdGen, NodeKind};
    use bock_errors::Span;

    fn dummy_node() -> AIRNode {
        let gen = NodeIdGen::new();
        AIRNode::new(
            gen.next(),
            Span::dummy(),
            NodeKind::Block {
                stmts: Vec::new(),
                tail: None,
            },
        )
    }

    fn js_target() -> TargetProfile {
        TargetProfile {
            id: "js".into(),
            display_name: "JavaScript".into(),
            capabilities: Default::default(),
            conventions: Default::default(),
        }
    }

    #[test]
    fn accepted_code_reports_working_outcome() {
        let ok = RepairOutcome::FirstTrySuccess { code: "x".into() };
        assert_eq!(ok.accepted_code(), Some("x"));
        assert!(ok.is_success());

        let rep = RepairOutcome::Repaired {
            code: "y".into(),
            attempts: 1,
            rule_added: false,
        };
        assert_eq!(rep.accepted_code(), Some("y"));
        assert!(rep.is_success());

        let bad = RepairOutcome::NoProvider {
            compiler_error: "boom".into(),
        };
        assert_eq!(bad.accepted_code(), None);
        assert!(!bad.is_success());
    }

    #[test]
    fn summarize_truncates_long_errors() {
        let long = "x".repeat(1000);
        let out = summarize(&long);
        assert!(out.len() <= 515);
        assert!(out.ends_with("..."));
    }

    #[test]
    fn summarize_short_errors_unchanged() {
        let out = summarize("  short error  ");
        assert_eq!(out, "short error");
    }

    #[test]
    fn apply_template_returns_template_verbatim() {
        let code = apply_template("switch(x){}", &dummy_node());
        assert_eq!(code, "switch(x){}");
    }

    #[test]
    fn try_apply_rule_misses_with_empty_cache() {
        let dir = tempfile::tempdir().unwrap();
        let rules = RuleCache::new(dir.path());
        let outcome = try_apply_rule(&rules, "js", &dummy_node(), Strictness::Development).unwrap();
        assert_eq!(outcome, RuleLookupOutcome::Miss);
    }

    #[test]
    fn try_apply_rule_hits_matching_kind() {
        let dir = tempfile::tempdir().unwrap();
        let rules = RuleCache::new(dir.path());
        let candidate = CandidateRule {
            target_id: "js".into(),
            pattern: "empty block".into(),
            template: "() => {}".into(),
            priority: 1,
        };
        let rule = Rule::from_candidate(&candidate, "Block", 0.9);
        rules.insert(&rule).unwrap();

        let outcome = try_apply_rule(&rules, "js", &dummy_node(), Strictness::Sketch).unwrap();
        match outcome {
            RuleLookupOutcome::Applied { rule: r, code } => {
                assert_eq!(r.node_kind, "Block");
                assert_eq!(code, "() => {}");
            }
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    #[test]
    fn try_apply_rule_reports_miss_needs_pin_in_production() {
        let dir = tempfile::tempdir().unwrap();
        let rules = RuleCache::new(dir.path());
        let candidate = CandidateRule {
            target_id: "js".into(),
            pattern: "empty block".into(),
            template: "() => {}".into(),
            priority: 1,
        };
        let rule = Rule::from_candidate(&candidate, "Block", 0.9);
        // not pinned — production should skip it
        rules.insert(&rule).unwrap();

        let outcome = try_apply_rule(&rules, "js", &dummy_node(), Strictness::Production).unwrap();
        assert_eq!(outcome, RuleLookupOutcome::MissNeedsPin);
    }

    #[test]
    fn pipeline_without_provider_returns_no_provider() {
        use std::path::PathBuf;
        // use a fake target whose toolchain will fail to locate the binary
        let mut registry = ToolchainRegistry::new();
        registry.register(crate::toolchain::ToolchainSpec {
            target_id: "fake".into(),
            display_name: "Fake".into(),
            binary_name: "not_a_real_binary_repair_xyz".into(),
            version_args: vec!["--version".into()],
            compile_command: "not_a_real_binary_repair_xyz".into(),
            compile_args: vec![],
            install_hint: "n/a".into(),
        });
        let toolchain = Arc::new(registry);
        let pipeline = RepairPipeline::without_provider(toolchain, RepairConfig::default());
        // Calling run with a NotFound error should bubble up as a
        // RepairError::Toolchain (not an InvocationFailed), which the
        // pipeline escalates. This verifies the pre-condition we rely on
        // in the no-provider branch.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("out.js");
        let target = TargetProfile {
            id: "fake".into(),
            display_name: "Fake".into(),
            capabilities: Default::default(),
            conventions: Default::default(),
        };
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = rt.block_on(pipeline.run(&target, &dummy_node(), "x".into(), &src));
        assert!(result.is_err(), "expected NotFound escalation");
        let _ = PathBuf::new();
        let _ = js_target();
    }
}
