//! Tier 1 AI synthesis — selective invocation, confidence gating, and
//! decision recording (§17.2, §17.3, §17.4).
//!
//! The synthesis layer augments the rule-based Tier 2 backends with AI
//! generation at capability-gap points (§17.6) and target-flagged
//! constructs (`TargetProfile::ai_hints`). It is the infrastructure half
//! of "AI-first with deterministic fallback":
//!
//! 1. Walk the AIR module and identify nodes that warrant AI synthesis.
//!    Trivial constructs (literals, arithmetic, direct calls, …) are
//!    classified as `None` by [`crate::profile::classify_node`] and
//!    bypass AI entirely — per §17.2 (Q3 amended, 2026-04-20).
//! 2. For each flagged node, call the provider's `generate` mode.
//!    Confidence gates acceptance (default `0.75`); pinned cache replays
//!    (§17.8) bypass the threshold.
//! 3. Run the deterministic verifier (§17.3) on accepted output.
//!    Verification lives in this crate — it never goes through the AI
//!    provider.
//! 4. Record the accepted choice as a build-scope decision (§17.4)
//!    routed to `.bock/decisions/build/`.
//! 5. On rejection, provider error, or verification failure, fall
//!    through to Tier 2 rule-based generation (preserved guarantee).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bock_air::{AIRNode, NodeKind};
use bock_ai::{
    compute_key, node_kind_name, AiCache, AiError, AiProvider, Decision, DecisionType,
    GenerateRequest, GenerateResponse, ManifestWriter, ModuleContext, RuleCache,
    StrictnessPolicy,
};
use bock_types::{AIRModule, Strictness};
use chrono::Utc;

use crate::profile::{classify_node, TargetProfile};

// ─── Configuration ───────────────────────────────────────────────────────────

/// Runtime knobs for a single AI-augmented module compilation.
#[derive(Debug, Clone)]
pub struct SynthesisConfig {
    /// Minimum AI confidence for auto-acceptance (default `0.75`, §17.4).
    pub confidence_threshold: f64,
    /// Fall back to Tier 2 on provider error or low confidence.
    pub deterministic_fallback: bool,
    /// Graduated strictness level for the current compilation.
    pub strictness: Strictness,
    /// Auto-pin accepted decisions at `development` strictness.
    pub auto_pin: bool,
    /// Canonical module path written into each decision record.
    pub module_path: PathBuf,
}

impl Default for SynthesisConfig {
    fn default() -> Self {
        Self {
            confidence_threshold: 0.75,
            deterministic_fallback: true,
            strictness: Strictness::Development,
            auto_pin: false,
            module_path: PathBuf::new(),
        }
    }
}

// ─── Outcome ─────────────────────────────────────────────────────────────────

/// Result of synthesizing a single flagged node.
#[derive(Debug, Clone, PartialEq)]
pub enum SynthesisOutcome {
    /// AI produced code that cleared the confidence threshold (or was
    /// replayed from the pinned cache) and passed verification.
    Accepted {
        /// The synthesized target code snippet.
        code: String,
        /// Confidence attached by the provider.
        confidence: f64,
        /// `true` when the response came from the content-addressed cache
        /// — treated as pinned replay per §17.8 (bypasses threshold).
        from_cache: bool,
    },
    /// A cached codegen rule (§17.7) matched this node's kind and was
    /// applied deterministically — the AI was never called.
    RuleApplied {
        /// The code produced by applying the rule's template.
        code: String,
        /// Identifier of the rule in the local [`RuleCache`].
        rule_id: String,
        /// The [`bock_air::NodeKind`] discriminant the rule matched.
        node_kind: String,
        /// Confidence attached to the rule at extraction time.
        confidence: f64,
    },
    /// AI produced code but confidence was below the threshold.
    RejectedLowConfidence {
        /// Confidence reported by the provider.
        confidence: f64,
    },
    /// AI produced code but it failed the deterministic verifier (§17.3).
    RejectedVerification {
        /// The reason verification failed.
        error: String,
    },
    /// Provider call failed (transport, auth, etc.). Tier 2 handles the node.
    ProviderError {
        /// The underlying AI error message.
        message: String,
    },
    /// Production strictness required a pinned decision but none was
    /// available. The caller decides whether to error or fall through.
    ProductionUnpinned,
}

// ─── Stats ───────────────────────────────────────────────────────────────────

/// Aggregate counters across a synthesis pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SynthesisStats {
    /// Total AIR nodes walked.
    pub total_nodes: usize,
    /// Nodes flagged by `classify_node` + `ai_hints`.
    pub flagged_nodes: usize,
    /// AI calls actually issued (flagged nodes when a provider was present).
    pub ai_calls: usize,
    /// Responses accepted (including pinned replay).
    pub accepted: usize,
    /// Accepted responses that came from the cache.
    pub cache_hits: usize,
    /// Rejected because confidence < threshold.
    pub rejected_low_confidence: usize,
    /// Rejected because verification (§17.3) failed.
    pub rejected_verification: usize,
    /// Provider returned an error.
    pub provider_errors: usize,
    /// Fallback to Tier 2 was triggered.
    pub fallback_triggered: usize,
    /// Production-strictness unpinned rejections.
    pub production_unpinned: usize,
    /// Flagged nodes served by the [`RuleCache`] before any AI call.
    pub rule_applied: usize,
}

// ─── needs_ai_synthesis ──────────────────────────────────────────────────────

/// Returns `true` only when the node is flagged by `target.ai_hints` and
/// matches a non-trivial [`crate::profile::NodeKindHint`]. Trivial
/// constructs (literals, arithmetic, direct calls, variable bindings)
/// always return `false` — the Q3 guarantee from the 2026-04-20 spec
/// amendment.
#[must_use]
pub fn needs_ai_synthesis(target: &TargetProfile, node: &AIRNode) -> bool {
    let Some(hint) = classify_node(node) else {
        return false;
    };
    target.ai_hints.contains(&hint)
}

// ─── Verification (§17.3) ────────────────────────────────────────────────────

/// Deterministic, provider-free verification of generated target code.
///
/// Minimum bar for D.5: non-empty + balanced brackets outside string
/// literals / line comments. Full per-target parser integration is future
/// work — it would live in `TargetProfile` and invoke each target's
/// toolchain when `bock build --verify` is on.
///
/// Python is indentation-sensitive and doesn't carry `{}`, so bracket
/// balancing is skipped for that target; only the emptiness check runs.
///
/// # Errors
/// Returns `Err(message)` with a human-readable reason when verification
/// fails.
pub fn verify_generated(target_id: &str, code: &str) -> Result<(), String> {
    if code.trim().is_empty() {
        return Err("generated code is empty".into());
    }
    if target_id == "python" || target_id == "py" {
        return Ok(());
    }
    check_bracket_balance(code)
}

fn check_bracket_balance(code: &str) -> Result<(), String> {
    let mut stack: Vec<char> = Vec::new();
    let mut chars = code.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => skip_until(&mut chars, '"'),
            '\'' => skip_until(&mut chars, '\''),
            '/' if chars.peek() == Some(&'/') => {
                for next in chars.by_ref() {
                    if next == '\n' {
                        break;
                    }
                }
            }
            '(' | '[' | '{' => stack.push(c),
            ')' => match stack.pop() {
                Some('(') => {}
                _ => return Err("unbalanced `)`".into()),
            },
            ']' => match stack.pop() {
                Some('[') => {}
                _ => return Err("unbalanced `]`".into()),
            },
            '}' => match stack.pop() {
                Some('{') => {}
                _ => return Err("unbalanced `}`".into()),
            },
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err(format!("unclosed `{}`", stack.last().unwrap()));
    }
    Ok(())
}

fn skip_until(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, delim: char) {
    while let Some(next) = chars.next() {
        if next == '\\' {
            chars.next();
        } else if next == delim {
            return;
        }
    }
}

// ─── Synthesis driver ────────────────────────────────────────────────────────

/// Machinery driving a single-module AI synthesis pass.
///
/// Holds an optional provider (behind a trait object), an optional
/// content-addressed response cache, an optional manifest writer, and
/// the synthesis configuration. The driver is constructed once per
/// build and reused across modules.
pub struct AiSynthesisDriver {
    provider: Option<Arc<dyn AiProvider>>,
    cache: Option<AiCache>,
    manifest: Option<Arc<Mutex<ManifestWriter>>>,
    rule_cache: Option<RuleCache>,
    config: SynthesisConfig,
}

impl AiSynthesisDriver {
    /// Constructs a driver with no provider — every flagged node falls
    /// through to Tier 2. Useful for `--deterministic` builds and for
    /// projects that haven't configured an `[ai]` section.
    #[must_use]
    pub fn deterministic(config: SynthesisConfig) -> Self {
        Self {
            provider: None,
            cache: None,
            manifest: None,
            rule_cache: None,
            config,
        }
    }

    /// Constructs a driver backed by `provider`, optionally with a
    /// response cache and a manifest writer.
    #[must_use]
    pub fn new(
        provider: Arc<dyn AiProvider>,
        cache: Option<AiCache>,
        manifest: Option<Arc<Mutex<ManifestWriter>>>,
        config: SynthesisConfig,
    ) -> Self {
        Self {
            provider: Some(provider),
            cache,
            manifest,
            rule_cache: None,
            config,
        }
    }

    /// Attach a [`RuleCache`] consulted before any AI call (§17.7).
    ///
    /// On a rule hit the driver applies the template deterministically
    /// and records a `RuleApplied` decision instead of calling the
    /// provider — saving tokens on already-learned patterns. Intended
    /// for builder-style composition with [`Self::new`] or
    /// [`Self::deterministic`].
    #[must_use]
    pub fn with_rule_cache(mut self, rules: RuleCache) -> Self {
        self.rule_cache = Some(rules);
        self
    }

    /// Access the configured rule cache, if any.
    #[must_use]
    pub fn rule_cache(&self) -> Option<&RuleCache> {
        self.rule_cache.as_ref()
    }

    /// Access the configured manifest writer, if any.
    #[must_use]
    pub fn manifest(&self) -> Option<&Arc<Mutex<ManifestWriter>>> {
        self.manifest.as_ref()
    }

    /// Borrow the active config (for diagnostics / tests).
    #[must_use]
    pub fn config(&self) -> &SynthesisConfig {
        &self.config
    }

    /// Runs a full synthesis pass over `module`, respecting the
    /// target profile's `ai_hints` and the driver's configuration.
    ///
    /// # Errors
    /// Only returns an error for manifest I/O failures. Every other
    /// failure is recorded in [`SynthesisStats`] so the caller can
    /// continue to Tier 2 rule-based generation.
    pub async fn synthesize_module(
        &self,
        module: &AIRModule,
        target: &TargetProfile,
        ctx: &ModuleContext,
    ) -> Result<SynthesisStats, bock_ai::ManifestError> {
        let mut stats = SynthesisStats::default();

        // Short path: no provider → deterministic only.
        if self.provider.is_none() {
            walk_module(module, &mut |n| {
                stats.total_nodes += 1;
                if needs_ai_synthesis(target, n) {
                    stats.flagged_nodes += 1;
                    stats.fallback_triggered += 1;
                }
            });
            return Ok(stats);
        }

        // Collect flagged nodes first so we can drive async calls
        // sequentially (cache hits → determinism), then count totals.
        let mut flagged: Vec<AIRNode> = Vec::new();
        walk_module(module, &mut |n| {
            stats.total_nodes += 1;
            if needs_ai_synthesis(target, n) {
                stats.flagged_nodes += 1;
                flagged.push(n.clone());
            }
        });

        for node in &flagged {
            let outcome = self.synthesize_one(node, target, ctx).await;
            self.account_outcome(&outcome, &mut stats);
            match &outcome {
                SynthesisOutcome::Accepted {
                    code,
                    confidence,
                    from_cache,
                } => {
                    self.record_decision(node, target, code, *confidence, *from_cache)?;
                }
                SynthesisOutcome::RuleApplied {
                    code,
                    rule_id,
                    node_kind,
                    confidence,
                } => {
                    self.record_rule_applied(
                        node, target, code, rule_id, node_kind, *confidence,
                    )?;
                }
                _ => {}
            }
        }

        Ok(stats)
    }

    fn account_outcome(&self, outcome: &SynthesisOutcome, stats: &mut SynthesisStats) {
        match outcome {
            SynthesisOutcome::RuleApplied { .. } => {
                stats.rule_applied += 1;
            }
            SynthesisOutcome::Accepted {
                from_cache: true, ..
            } => {
                stats.ai_calls += 1;
                stats.accepted += 1;
                stats.cache_hits += 1;
            }
            SynthesisOutcome::Accepted { .. } => {
                stats.ai_calls += 1;
                stats.accepted += 1;
            }
            SynthesisOutcome::RejectedLowConfidence { .. } => {
                stats.ai_calls += 1;
                stats.rejected_low_confidence += 1;
                stats.fallback_triggered += 1;
            }
            SynthesisOutcome::RejectedVerification { .. } => {
                stats.ai_calls += 1;
                stats.rejected_verification += 1;
                stats.fallback_triggered += 1;
            }
            SynthesisOutcome::ProviderError { .. } => {
                stats.ai_calls += 1;
                stats.provider_errors += 1;
                if self.config.deterministic_fallback {
                    stats.fallback_triggered += 1;
                }
            }
            SynthesisOutcome::ProductionUnpinned => {
                stats.production_unpinned += 1;
                if self.config.deterministic_fallback {
                    stats.fallback_triggered += 1;
                }
            }
        }
    }

    async fn synthesize_one(
        &self,
        node: &AIRNode,
        target: &TargetProfile,
        ctx: &ModuleContext,
    ) -> SynthesisOutcome {
        // Per §17.7, try the local rule cache *before* any AI call so
        // already-learned patterns don't spend tokens. Lookup errors
        // are non-fatal: we fall through to Tier 1 on miss or I/O
        // error, preserving D.5's guarantee that the AI path is always
        // reachable for the caller.
        if let Some(rule) = self.lookup_rule(node, target) {
            return rule;
        }

        let request = build_request(node, target, ctx, self.config.strictness);
        let (response, from_cache) = match self.call_generate(&request).await {
            Ok(Some(pair)) => pair,
            Ok(None) => {
                // Production strictness + cache miss: provider was never
                // consulted (see `call_generate`). Surface as a distinct
                // outcome so the caller can fall back to Tier 2.
                return SynthesisOutcome::ProductionUnpinned;
            }
            Err(e) => {
                return SynthesisOutcome::ProviderError {
                    message: format!("{e}"),
                };
            }
        };

        let accept = from_cache || response.confidence >= self.config.confidence_threshold;
        if !accept {
            return SynthesisOutcome::RejectedLowConfidence {
                confidence: response.confidence,
            };
        }

        if let Err(err) = verify_generated(&target.id, &response.code) {
            return SynthesisOutcome::RejectedVerification { error: err };
        }

        SynthesisOutcome::Accepted {
            code: response.code,
            confidence: response.confidence,
            from_cache,
        }
    }

    fn lookup_rule(&self, node: &AIRNode, target: &TargetProfile) -> Option<SynthesisOutcome> {
        let cache = self.rule_cache.as_ref()?;
        let production_only = matches!(self.config.strictness, Strictness::Production);
        let rule = cache.lookup(&target.id, node, production_only).ok().flatten()?;
        Some(SynthesisOutcome::RuleApplied {
            code: rule.template.clone(),
            rule_id: rule.id.clone(),
            node_kind: rule.node_kind.clone(),
            confidence: rule.confidence,
        })
    }

    async fn call_generate(
        &self,
        request: &GenerateRequest,
    ) -> Result<Option<(GenerateResponse, bool)>, AiError> {
        let provider = self
            .provider
            .as_ref()
            .ok_or_else(|| AiError::Unavailable("no provider configured".into()))?;

        // Cache lookup — canonical key over the request + model id.
        // Cache reads are always allowed; the governance gate only
        // blocks *new* AI calls.
        let cache_key = self.build_cache_key(provider.model_id(), request);
        if let Some(cache) = &self.cache {
            if let Some(resp) = cache.get::<_, GenerateResponse>(&cache_key) {
                return Ok(Some((resp, true)));
            }
        }

        // Governance (§17.6): production strictness forbids fresh AI
        // calls at build time. Return `None` so the caller falls back
        // to Tier 2 via `SynthesisOutcome::ProductionUnpinned` without
        // ever touching the provider.
        let policy = StrictnessPolicy::for_level(self.config.strictness);
        if !policy.allow_build_ai {
            return Ok(None);
        }

        let resp = provider.generate(request).await?;
        if let Some(cache) = &self.cache {
            let _ = cache.put(&cache_key, &resp);
        }
        Ok(Some((resp, false)))
    }

    fn build_cache_key(&self, model_id: String, request: &GenerateRequest) -> CacheKey {
        let prior: Vec<(String, String)> = request
            .prior_decisions
            .iter()
            .map(|d| (d.decision.clone(), d.choice.clone()))
            .collect();
        // Strictness is intentionally NOT part of the key — the cache
        // captures the AI's *decision* (what code to emit), not the
        // acceptance policy. A decision pinned under `development`
        // replays identically under `production`. See §17.8.
        CacheKey {
            mode: "generate",
            model_id,
            target_id: request.target.id.clone(),
            module_path: request.module_context.module_path.clone(),
            imports: request.module_context.imports.clone(),
            siblings: request.module_context.siblings.clone(),
            annotations: request.module_context.annotations.clone(),
            prior_decisions: prior,
            node_debug: format!("{:?}", request.node),
        }
    }

    fn record_rule_applied(
        &self,
        node: &AIRNode,
        target: &TargetProfile,
        _code: &str,
        rule_id: &str,
        rule_kind: &str,
        confidence: f64,
    ) -> Result<(), bock_ai::ManifestError> {
        let Some(manifest) = &self.manifest else {
            return Ok(());
        };
        let mut mw = manifest
            .lock()
            .expect("manifest writer mutex poisoned");

        let model_id = self
            .provider
            .as_ref()
            .map_or_else(|| "deterministic".into(), |p| p.model_id());
        let id = rule_decision_id(node, target, rule_id);
        mw.record(Decision {
            id,
            module: self.config.module_path.clone(),
            target: Some(target.id.clone()),
            decision_type: DecisionType::RuleApplied,
            choice: format!("rule {rule_id} matched pattern {rule_kind}"),
            alternatives: Vec::new(),
            reasoning: Some(format!(
                "local rule cache hit for {rule_kind}; no AI call issued"
            )),
            model_id,
            confidence,
            pinned: true,
            pin_reason: Some("rule-applied".into()),
            pinned_at: Some(Utc::now()),
            pinned_by: Some("rule-cache".into()),
            superseded_by: None,
            timestamp: Utc::now(),
        });
        Ok(())
    }

    fn record_decision(
        &self,
        node: &AIRNode,
        target: &TargetProfile,
        code: &str,
        confidence: f64,
        from_cache: bool,
    ) -> Result<(), bock_ai::ManifestError> {
        let Some(manifest) = &self.manifest else {
            return Ok(());
        };
        let mut mw = manifest
            .lock()
            .expect("manifest writer mutex poisoned");

        let id = decision_id(node, target);
        let policy = StrictnessPolicy::for_level(self.config.strictness);
        // Pinning sources (§17.6, §17.8):
        //   1. Cache hits are pinned replays.
        //   2. Production governance forces every fresh decision to pinned.
        //   3. Development respects the per-project `auto_pin` toggle.
        //   4. Sketch records fresh decisions unpinned.
        let pinned = from_cache
            || policy.auto_pin_default
            || (matches!(self.config.strictness, Strictness::Development) && self.config.auto_pin);
        let pin_reason = if from_cache {
            Some("cache-replay".into())
        } else if policy.auto_pin_default {
            Some("production-auto".into())
        } else if pinned {
            Some("auto-pin".into())
        } else {
            None
        };

        let model_id = self
            .provider
            .as_ref()
            .map_or_else(|| "deterministic".into(), |p| p.model_id());

        mw.record(Decision {
            id,
            module: self.config.module_path.clone(),
            target: Some(target.id.clone()),
            decision_type: DecisionType::Codegen,
            choice: code.into(),
            alternatives: Vec::new(),
            reasoning: None,
            model_id,
            confidence,
            pinned,
            pin_reason,
            pinned_at: pinned.then(Utc::now),
            pinned_by: pinned.then(|| "auto".into()),
            superseded_by: None,
            timestamp: Utc::now(),
        });
        Ok(())
    }
}

/// Drives synthesis once per module and flushes the manifest writer.
///
/// Convenience for tests and build pipelines that want the manifest
/// flushed at the end of each module.
///
/// # Errors
/// Returns any manifest I/O error surfaced by [`ManifestWriter::flush`].
pub async fn synthesize_and_flush(
    driver: &AiSynthesisDriver,
    module: &AIRModule,
    target: &TargetProfile,
    ctx: &ModuleContext,
) -> Result<SynthesisStats, bock_ai::ManifestError> {
    let stats = driver.synthesize_module(module, target, ctx).await?;
    if let Some(m) = driver.manifest() {
        let mut guard = m.lock().expect("manifest writer mutex poisoned");
        guard.flush()?;
    }
    Ok(stats)
}

// ─── Cache key ───────────────────────────────────────────────────────────────

#[derive(serde::Serialize)]
struct CacheKey {
    mode: &'static str,
    model_id: String,
    target_id: String,
    module_path: String,
    imports: Vec<String>,
    siblings: Vec<String>,
    annotations: Vec<String>,
    prior_decisions: Vec<(String, String)>,
    node_debug: String,
}

// ─── AIR walker ──────────────────────────────────────────────────────────────

/// Visits every AIR node in the module in deterministic pre-order.
fn walk_module<F: FnMut(&AIRNode)>(module: &AIRModule, f: &mut F) {
    walk_node(module, f);
}

fn walk_node<F: FnMut(&AIRNode)>(node: &AIRNode, f: &mut F) {
    f(node);
    match &node.kind {
        NodeKind::Module { imports, items, .. } => {
            for n in imports {
                walk_node(n, f);
            }
            for n in items {
                walk_node(n, f);
            }
        }
        NodeKind::FnDecl {
            params,
            return_type,
            body,
            ..
        } => {
            for p in params {
                walk_node(p, f);
            }
            if let Some(rt) = return_type {
                walk_node(rt, f);
            }
            walk_node(body, f);
        }
        NodeKind::ClassDecl { methods, .. } => {
            for m in methods {
                walk_node(m, f);
            }
        }
        NodeKind::TraitDecl { methods, .. } => {
            for m in methods {
                walk_node(m, f);
            }
        }
        NodeKind::ImplBlock { methods, .. } => {
            for m in methods {
                walk_node(m, f);
            }
        }
        NodeKind::EnumDecl { variants, .. } => {
            for v in variants {
                walk_node(v, f);
            }
        }
        NodeKind::EffectDecl { operations, .. } => {
            for op in operations {
                walk_node(op, f);
            }
        }
        NodeKind::Block { stmts, tail } => {
            for s in stmts {
                walk_node(s, f);
            }
            if let Some(t) = tail {
                walk_node(t, f);
            }
        }
        NodeKind::If {
            condition,
            then_block,
            else_block,
            ..
        } => {
            walk_node(condition, f);
            walk_node(then_block, f);
            if let Some(e) = else_block {
                walk_node(e, f);
            }
        }
        NodeKind::For {
            pattern,
            iterable,
            body,
        } => {
            walk_node(pattern, f);
            walk_node(iterable, f);
            walk_node(body, f);
        }
        NodeKind::While { condition, body } => {
            walk_node(condition, f);
            walk_node(body, f);
        }
        NodeKind::Loop { body } => walk_node(body, f),
        NodeKind::LetBinding {
            pattern, value, ty, ..
        } => {
            walk_node(pattern, f);
            walk_node(value, f);
            if let Some(t) = ty {
                walk_node(t, f);
            }
        }
        NodeKind::Match { scrutinee, arms } => {
            walk_node(scrutinee, f);
            for a in arms {
                walk_node(a, f);
            }
        }
        NodeKind::MatchArm {
            pattern,
            guard,
            body,
        } => {
            walk_node(pattern, f);
            if let Some(g) = guard {
                walk_node(g, f);
            }
            walk_node(body, f);
        }
        NodeKind::HandlingBlock { body, .. } => walk_node(body, f),
        NodeKind::BinaryOp { left, right, .. } => {
            walk_node(left, f);
            walk_node(right, f);
        }
        NodeKind::UnaryOp { operand, .. } => walk_node(operand, f),
        NodeKind::Call { callee, args, .. } => {
            walk_node(callee, f);
            for a in args {
                walk_node(&a.value, f);
            }
        }
        NodeKind::MethodCall { receiver, args, .. } => {
            walk_node(receiver, f);
            for a in args {
                walk_node(&a.value, f);
            }
        }
        NodeKind::Lambda { params, body } => {
            for p in params {
                walk_node(p, f);
            }
            walk_node(body, f);
        }
        NodeKind::Return { value } | NodeKind::Break { value } => {
            if let Some(v) = value {
                walk_node(v, f);
            }
        }
        NodeKind::Assign { target, value, .. } => {
            walk_node(target, f);
            walk_node(value, f);
        }
        NodeKind::FieldAccess { object, .. } => walk_node(object, f),
        NodeKind::Index { object, index } => {
            walk_node(object, f);
            walk_node(index, f);
        }
        NodeKind::Pipe { left, right } | NodeKind::Compose { left, right } => {
            walk_node(left, f);
            walk_node(right, f);
        }
        NodeKind::Await { expr } | NodeKind::Propagate { expr } => walk_node(expr, f),
        NodeKind::Move { expr } | NodeKind::Borrow { expr } | NodeKind::MutableBorrow { expr } => {
            walk_node(expr, f);
        }
        NodeKind::Guard {
            let_pattern,
            condition,
            else_block,
        } => {
            if let Some(p) = let_pattern {
                walk_node(p, f);
            }
            walk_node(condition, f);
            walk_node(else_block, f);
        }
        NodeKind::Param {
            pattern,
            ty,
            default,
        } => {
            walk_node(pattern, f);
            if let Some(t) = ty {
                walk_node(t, f);
            }
            if let Some(d) = default {
                walk_node(d, f);
            }
        }
        NodeKind::ListLiteral { elems }
        | NodeKind::SetLiteral { elems }
        | NodeKind::TupleLiteral { elems } => {
            for e in elems {
                walk_node(e, f);
            }
        }
        NodeKind::MapLiteral { entries } => {
            for e in entries {
                walk_node(&e.key, f);
                walk_node(&e.value, f);
            }
        }
        NodeKind::RecordConstruct { fields, spread, .. } => {
            for fld in fields {
                if let Some(v) = &fld.value {
                    walk_node(v, f);
                }
            }
            if let Some(s) = spread {
                walk_node(s, f);
            }
        }
        NodeKind::Range { lo, hi, .. } => {
            walk_node(lo, f);
            walk_node(hi, f);
        }
        NodeKind::ResultConstruct { value: Some(v), .. } => walk_node(v, f),
        NodeKind::TypeNamed { args, .. } => {
            for a in args {
                walk_node(a, f);
            }
        }
        NodeKind::TypeTuple { elems } => {
            for e in elems {
                walk_node(e, f);
            }
        }
        NodeKind::TypeFunction { params, ret, .. } => {
            for p in params {
                walk_node(p, f);
            }
            walk_node(ret, f);
        }
        NodeKind::TypeOptional { inner } => walk_node(inner, f),
        NodeKind::TypeAlias { ty, .. } => walk_node(ty, f),
        NodeKind::ConstDecl { ty, value, .. } => {
            walk_node(ty, f);
            walk_node(value, f);
        }
        NodeKind::ModuleHandle { handler, .. } => walk_node(handler, f),
        NodeKind::PropertyTest { body, .. } => walk_node(body, f),
        NodeKind::ConstructorPat { fields, .. } => {
            for fld in fields {
                walk_node(fld, f);
            }
        }
        NodeKind::RecordPat { fields, .. } => {
            for fld in fields {
                if let Some(p) = &fld.pattern {
                    walk_node(p, f);
                }
            }
        }
        NodeKind::TuplePat { elems } => {
            for e in elems {
                walk_node(e, f);
            }
        }
        NodeKind::ListPat { elems, rest } => {
            for e in elems {
                walk_node(e, f);
            }
            if let Some(r) = rest {
                walk_node(r, f);
            }
        }
        NodeKind::OrPat { alternatives } => {
            for a in alternatives {
                walk_node(a, f);
            }
        }
        NodeKind::GuardPat { pattern, guard } => {
            walk_node(pattern, f);
            walk_node(guard, f);
        }
        NodeKind::RangePat { lo, hi, .. } => {
            walk_node(lo, f);
            walk_node(hi, f);
        }
        _ => {}
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn build_request(
    node: &AIRNode,
    target: &TargetProfile,
    ctx: &ModuleContext,
    strictness: Strictness,
) -> GenerateRequest {
    GenerateRequest {
        node: node.clone(),
        target: flatten_profile(target),
        module_context: ctx.clone(),
        prior_decisions: Vec::new(),
        strictness,
    }
}

fn flatten_profile(target: &TargetProfile) -> bock_ai::TargetProfile {
    use std::collections::HashMap;
    let mut capabilities = HashMap::new();
    capabilities.insert(
        "memory_model".into(),
        format!("{}", target.capabilities.memory_model),
    );
    capabilities.insert(
        "async_model".into(),
        format!("{}", target.capabilities.async_model),
    );
    capabilities.insert(
        "generics".into(),
        format!("{}", target.capabilities.generics),
    );
    capabilities.insert(
        "pattern_matching".into(),
        format!("{}", target.capabilities.pattern_matching),
    );
    capabilities.insert(
        "algebraic_types".into(),
        format!("{}", target.capabilities.algebraic_types),
    );
    capabilities.insert(
        "string_interpolation".into(),
        format!("{}", target.capabilities.string_interpolation),
    );
    capabilities.insert("traits".into(), format!("{}", target.capabilities.traits));
    let mut conventions = HashMap::new();
    conventions.insert("naming".into(), format!("{}", target.conventions.naming));
    conventions.insert(
        "error_handling".into(),
        format!("{}", target.conventions.error_handling),
    );
    conventions.insert(
        "file_extension".into(),
        target.conventions.file_extension.clone(),
    );
    bock_ai::TargetProfile {
        id: target.id.clone(),
        display_name: target.display_name.clone(),
        capabilities,
        conventions,
    }
}

/// Decision id — stable hash of (target, node debug). Keeps manifest
/// lookups aligned with the content-addressed cache.
fn decision_id(node: &AIRNode, target: &TargetProfile) -> String {
    #[derive(serde::Serialize)]
    struct Keyed<'a> {
        target: &'a str,
        node_debug: String,
    }
    let keyed = Keyed {
        target: &target.id,
        node_debug: format!("{node:?}"),
    };
    compute_key(&keyed).unwrap_or_else(|_| format!("{:x}", node.id))
}

/// Decision id for a rule-applied entry — discriminated by rule id so
/// it never collides with a codegen decision for the same node.
fn rule_decision_id(node: &AIRNode, target: &TargetProfile, rule_id: &str) -> String {
    #[derive(serde::Serialize)]
    struct Keyed<'a> {
        kind: &'static str,
        target: &'a str,
        rule_id: &'a str,
        node_kind: &'a str,
        node_id: u32,
    }
    let keyed = Keyed {
        kind: "rule_applied",
        target: &target.id,
        rule_id,
        node_kind: node_kind_name(&node.kind),
        node_id: node.id,
    };
    compute_key(&keyed).unwrap_or_else(|_| format!("rule-{rule_id}-{:x}", node.id))
}

/// Convenience for callers that want to build a cache rooted at the
/// project directory without importing `bock_ai::AiCache`.
#[must_use]
pub fn cache_at(project_root: &Path) -> AiCache {
    AiCache::new(project_root)
}
