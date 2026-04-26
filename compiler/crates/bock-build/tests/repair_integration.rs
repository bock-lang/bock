//! End-to-end tests for the §17.7 codegen feedback loop (D.6).
//!
//! These tests exercise [`RepairPipeline`] against a real target
//! toolchain (Node.js `node --check`). They skip automatically when
//! `node` is not on PATH so CI can still run the crate's unit tests in
//! minimal environments.

#![cfg(test)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use bock_ai::{
    AiError, AiProvider, CandidateRule, GenerateRequest, GenerateResponse, ManifestWriter,
    ModuleContext, OptimizeRequest, OptimizeResponse, RepairRequest, RepairResponse, Rule,
    RuleCache, SelectRequest, SelectResponse, TargetProfile,
};
use bock_air::{AIRNode, NodeIdGen, NodeKind};
use bock_build::{RepairConfig, RepairOutcome, RepairPipeline, ToolchainRegistry};
use bock_errors::Span;
use bock_types::Strictness;

// ─── Provider that generates broken code first, correct on repair ────────────

struct SwitchingProvider {
    generate_calls: AtomicUsize,
    repair_calls: AtomicUsize,
    repair_confidence: f64,
    emit_candidate: bool,
    fixed_code: String,
    broken_code: String,
}

impl SwitchingProvider {
    fn new(broken: &str, fixed: &str, repair_confidence: f64, emit_candidate: bool) -> Self {
        Self {
            generate_calls: AtomicUsize::new(0),
            repair_calls: AtomicUsize::new(0),
            repair_confidence,
            emit_candidate,
            fixed_code: fixed.into(),
            broken_code: broken.into(),
        }
    }

    fn repair_calls(&self) -> usize {
        self.repair_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl AiProvider for SwitchingProvider {
    async fn generate(
        &self,
        _request: &GenerateRequest,
    ) -> Result<GenerateResponse, AiError> {
        self.generate_calls.fetch_add(1, Ordering::SeqCst);
        Ok(GenerateResponse {
            code: self.broken_code.clone(),
            confidence: 0.9,
            reasoning: Some("test: broken on first generate".into()),
            alternatives: Vec::new(),
        })
    }

    async fn repair(&self, _request: &RepairRequest) -> Result<RepairResponse, AiError> {
        self.repair_calls.fetch_add(1, Ordering::SeqCst);
        let candidate = if self.emit_candidate {
            Some(CandidateRule {
                target_id: "js".into(),
                pattern: "broken → balanced".into(),
                template: self.fixed_code.clone(),
                priority: 5,
            })
        } else {
            None
        };
        Ok(RepairResponse {
            fixed_code: self.fixed_code.clone(),
            confidence: self.repair_confidence,
            candidate_rule: candidate,
            reasoning: Some("test: return balanced fixed code".into()),
        })
    }

    async fn optimize(
        &self,
        _request: &OptimizeRequest,
    ) -> Result<OptimizeResponse, AiError> {
        unreachable!("optimize not used in D.6 tests")
    }

    async fn select(&self, _request: &SelectRequest) -> Result<SelectResponse, AiError> {
        unreachable!("select not used in D.6 tests")
    }

    fn model_id(&self) -> String {
        "switching:test".into()
    }
}

// ─── Provider that always returns broken code on repair — for the retry cap ──

struct AlwaysBrokenRepairProvider {
    calls: AtomicUsize,
    broken_variants: Vec<String>,
}

impl AlwaysBrokenRepairProvider {
    fn new(variants: Vec<String>) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            broken_variants: variants,
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl AiProvider for AlwaysBrokenRepairProvider {
    async fn generate(
        &self,
        _request: &GenerateRequest,
    ) -> Result<GenerateResponse, AiError> {
        unreachable!("generate not used for this test")
    }

    async fn repair(&self, _request: &RepairRequest) -> Result<RepairResponse, AiError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let idx = n.min(self.broken_variants.len() - 1);
        Ok(RepairResponse {
            fixed_code: self.broken_variants[idx].clone(),
            confidence: 0.95,
            candidate_rule: None,
            reasoning: Some(format!("attempt {n}: still broken")),
        })
    }

    async fn optimize(
        &self,
        _request: &OptimizeRequest,
    ) -> Result<OptimizeResponse, AiError> {
        unreachable!("optimize not used in D.6 tests")
    }

    async fn select(&self, _request: &SelectRequest) -> Result<SelectResponse, AiError> {
        unreachable!("select not used in D.6 tests")
    }

    fn model_id(&self) -> String {
        "always-broken:test".into()
    }
}

// ─── AIR fixture ─────────────────────────────────────────────────────────────

fn dummy_block_node() -> AIRNode {
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

fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn guard_node() -> bool {
    if !node_available() {
        eprintln!("skipping test: `node` not available on PATH");
        return false;
    }
    true
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn generate_then_repair_produces_working_code() {
    if !guard_node() {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("out.js");

    // Broken: unbalanced `}`. Fixed: balanced function body.
    let broken = "function f() { return 1;\n";
    let fixed = "function f() { return 1; }\n";
    let provider = Arc::new(SwitchingProvider::new(broken, fixed, 0.9, true));

    let rules = RuleCache::new(dir.path());
    let manifest = Arc::new(Mutex::new(ManifestWriter::new(dir.path())));
    let toolchain = Arc::new(ToolchainRegistry::with_builtins());
    let pipeline = RepairPipeline::new(
        provider.clone(),
        Some(rules),
        Some(manifest.clone()),
        toolchain,
        RepairConfig {
            module_path: PathBuf::from("src/m.bock"),
            ..Default::default()
        },
    );

    let outcome = pipeline
        .run(&js_target(), &dummy_block_node(), broken.into(), &src)
        .await
        .expect("pipeline ran");

    match outcome {
        RepairOutcome::Repaired {
            code,
            attempts,
            rule_added,
        } => {
            assert_eq!(code, fixed);
            assert_eq!(attempts, 1);
            assert!(rule_added, "candidate rule should be persisted");
        }
        other => panic!("expected Repaired outcome, got {other:?}"),
    }
    assert_eq!(provider.repair_calls(), 1);

    // Flush manifest and inspect.
    manifest.lock().unwrap().flush().unwrap();
    let decisions_file = dir
        .path()
        .join(".bock/decisions/build/src/m.bock.json");
    let contents = std::fs::read_to_string(&decisions_file).unwrap();
    assert!(contents.contains("\"repair\""), "expected repair entry");
    assert!(
        contents.contains("\"rule_applied\""),
        "expected rule_applied entry"
    );

    // Rule cache directory populated for the target.
    let rules_dir = dir.path().join(".bock/rules/js");
    assert!(rules_dir.exists(), "rule cache dir missing");
    let entries: Vec<_> = std::fs::read_dir(&rules_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(
        !entries.is_empty(),
        "expected at least one rule JSON on disk"
    );
}

#[tokio::test]
async fn second_build_hits_rule_and_skips_ai_call() {
    if !guard_node() {
        return;
    }

    // Use the codegen driver for the "second build": a RuleCache seeded
    // with the template that would have been extracted from the prior
    // repair pass. We verify that AiSynthesisDriver short-circuits the
    // AI provider entirely.
    use bock_codegen::{
        synthesize_and_flush, AiSynthesisDriver, SynthesisConfig, TargetProfile as CodegenTarget,
    };

    let dir = tempfile::tempdir().unwrap();

    // Seed the rule cache like a prior repair would.
    let rules = RuleCache::new(dir.path());
    let candidate = CandidateRule {
        target_id: "js".into(),
        pattern: "Match → switch".into(),
        template: "switch(x) { /* arms */ }".into(),
        priority: 10,
    };
    rules
        .insert(&Rule::from_candidate(&candidate, "Match", 0.9))
        .unwrap();

    // Counting provider for this second build. It must not be called.
    struct NoCallsProvider;
    #[async_trait]
    impl AiProvider for NoCallsProvider {
        async fn generate(
            &self,
            _request: &GenerateRequest,
        ) -> Result<GenerateResponse, AiError> {
            panic!("AI must not be called when rule cache serves the node");
        }
        async fn repair(
            &self,
            _request: &RepairRequest,
        ) -> Result<RepairResponse, AiError> {
            panic!("repair not expected");
        }
        async fn optimize(
            &self,
            _request: &OptimizeRequest,
        ) -> Result<OptimizeResponse, AiError> {
            unreachable!()
        }
        async fn select(
            &self,
            _request: &SelectRequest,
        ) -> Result<SelectResponse, AiError> {
            unreachable!()
        }
        fn model_id(&self) -> String {
            "no-calls".into()
        }
    }

    let provider: Arc<dyn AiProvider> = Arc::new(NoCallsProvider);
    let manifest = Arc::new(Mutex::new(ManifestWriter::new(dir.path())));
    let config = SynthesisConfig {
        confidence_threshold: 0.75,
        deterministic_fallback: true,
        strictness: Strictness::Development,
        auto_pin: false,
        module_path: PathBuf::from("src/m.bock"),
    };
    let driver = AiSynthesisDriver::new(provider, None, Some(manifest.clone()), config)
        .with_rule_cache(rules);

    // Build a minimal AIR module with a Match to trigger the rule.
    let scrutinee = AIRNode::new(
        1,
        Span::dummy(),
        NodeKind::Identifier {
            name: bock_ast::Ident {
                name: "x".into(),
                span: Span::dummy(),
            },
        },
    );
    let match_node = AIRNode::new(
        2,
        Span::dummy(),
        NodeKind::Match {
            scrutinee: Box::new(scrutinee),
            arms: Vec::new(),
        },
    );
    let module = AIRNode::new(
        0,
        Span::dummy(),
        NodeKind::Module {
            path: None,
            annotations: Vec::new(),
            imports: Vec::new(),
            items: vec![match_node],
        },
    );

    let ctx = ModuleContext {
        module_path: "src/m.bock".into(),
        imports: Vec::new(),
        siblings: Vec::new(),
        annotations: Vec::new(),
    };

    let stats = synthesize_and_flush(&driver, &module, &CodegenTarget::javascript(), &ctx)
        .await
        .unwrap();

    assert_eq!(stats.flagged_nodes, 1);
    assert_eq!(stats.rule_applied, 1);
    assert_eq!(stats.ai_calls, 0);

    let decisions_file = dir
        .path()
        .join(".bock/decisions/build/src/m.bock.json");
    let contents = std::fs::read_to_string(&decisions_file).unwrap();
    assert!(
        contents.contains("\"rule_applied\""),
        "second build must log rule_applied"
    );
    assert!(!contents.contains("\"codegen\""));
}

#[tokio::test]
async fn repair_attempt_cap_prevents_infinite_loop() {
    if !guard_node() {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("out.js");

    // Provider keeps producing syntactically broken JS. We cap at 2
    // attempts; pipeline should return `Exhausted`.
    let variants = vec![
        "function f() { return 1;\n".into(),
        "function g() { return 2;\n".into(),
        "function h() { return 3;\n".into(),
    ];
    let provider = Arc::new(AlwaysBrokenRepairProvider::new(variants));

    let manifest = Arc::new(Mutex::new(ManifestWriter::new(dir.path())));
    let toolchain = Arc::new(ToolchainRegistry::with_builtins());
    let pipeline = RepairPipeline::new(
        provider.clone(),
        None,
        Some(manifest),
        toolchain,
        RepairConfig {
            max_attempts: 2,
            module_path: PathBuf::from("src/m.bock"),
            ..Default::default()
        },
    );

    let outcome = pipeline
        .run(
            &js_target(),
            &dummy_block_node(),
            "function f() {\n".into(),
            &src,
        )
        .await
        .expect("pipeline ran");

    match outcome {
        RepairOutcome::Exhausted {
            attempts,
            compiler_error,
        } => {
            assert_eq!(attempts, 2);
            assert!(!compiler_error.is_empty());
        }
        other => panic!("expected Exhausted, got {other:?}"),
    }
    assert_eq!(provider.calls(), 2, "attempts should be capped");
}

#[tokio::test]
async fn low_confidence_repair_is_rejected() {
    if !guard_node() {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("out.js");

    let broken = "function f() { return 1;\n";
    let fixed = "function f() { return 1; }\n";
    // Repair confidence below default threshold 0.75.
    let provider = Arc::new(SwitchingProvider::new(broken, fixed, 0.3, false));

    let toolchain = Arc::new(ToolchainRegistry::with_builtins());
    let pipeline = RepairPipeline::new(
        provider.clone(),
        None,
        None,
        toolchain,
        RepairConfig {
            module_path: PathBuf::from("src/m.bock"),
            ..Default::default()
        },
    );

    let outcome = pipeline
        .run(&js_target(), &dummy_block_node(), broken.into(), &src)
        .await
        .expect("pipeline ran");

    match outcome {
        RepairOutcome::RejectedLowConfidence { confidence, .. } => {
            assert!((confidence - 0.3).abs() < f64::EPSILON);
        }
        other => panic!("expected RejectedLowConfidence, got {other:?}"),
    }
    assert_eq!(
        provider.repair_calls(),
        1,
        "low-confidence response stops at first attempt"
    );
}

#[tokio::test]
async fn first_try_success_skips_repair() {
    if !guard_node() {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("out.js");

    // This should compile cleanly on the first try.
    let fine = "function f() { return 1; }\n";
    let provider = Arc::new(SwitchingProvider::new("unused", "unused", 0.9, false));

    let toolchain = Arc::new(ToolchainRegistry::with_builtins());
    let pipeline = RepairPipeline::new(
        provider.clone(),
        None,
        None,
        toolchain,
        RepairConfig::default(),
    );

    let outcome = pipeline
        .run(&js_target(), &dummy_block_node(), fine.into(), &src)
        .await
        .expect("pipeline ran");

    assert!(matches!(outcome, RepairOutcome::FirstTrySuccess { .. }));
    assert_eq!(
        provider.repair_calls(),
        0,
        "repair must not be called when first compile succeeds"
    );
}

#[tokio::test]
async fn production_strictness_blocks_provider_repair() {
    if !guard_node() {
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("out.js");

    let broken = "function f() { return 1;\n";
    let fixed = "function f() { return 1; }\n";
    let provider = Arc::new(SwitchingProvider::new(broken, fixed, 0.9, true));

    let toolchain = Arc::new(ToolchainRegistry::with_builtins());
    let pipeline = RepairPipeline::new(
        provider.clone(),
        None,
        None,
        toolchain,
        RepairConfig {
            strictness: Strictness::Production,
            module_path: PathBuf::from("src/m.bock"),
            ..Default::default()
        },
    );

    let outcome = pipeline
        .run(&js_target(), &dummy_block_node(), broken.into(), &src)
        .await
        .expect("pipeline ran");

    assert!(matches!(outcome, RepairOutcome::ProductionBlocked { .. }));
    assert_eq!(provider.repair_calls(), 0);
}
