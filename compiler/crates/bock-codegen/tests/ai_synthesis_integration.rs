//! Integration tests for Tier 1 AI codegen wiring (D.5).
//!
//! These exercise selective invocation, confidence gating, pinned
//! replay, deterministic fallback, and decision-manifest recording
//! per §17.2 / §17.4 / §17.8 and Q3 of the 2026-04-20 spec amendment.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use bock_ai::{
    AiCache, AiError, AiProvider, CandidateRule, GenerateRequest, GenerateResponse, ManifestWriter,
    ModuleContext, OptimizeRequest, OptimizeResponse, RepairRequest, RepairResponse, Rule,
    RuleCache, SelectRequest, SelectResponse,
};
use bock_air::{AIRNode, AirHandlerPair, EnumVariantPayload, NodeKind};
use bock_ast::{Ident, TypePath, Visibility};
use bock_codegen::CodeGenerator;
use bock_codegen::{
    needs_ai_synthesis, synthesize_and_flush, verify_generated, AiSynthesisDriver, JsGenerator,
    RsGenerator, SynthesisConfig, TargetProfile,
};
use bock_errors::{FileId, Span};
use bock_types::Strictness;

// ─── Test provider: configurable confidence + call counting ──────────────────

struct CountingProvider {
    confidence: f64,
    calls: AtomicUsize,
    fail: bool,
}

impl CountingProvider {
    fn new(confidence: f64) -> Self {
        Self {
            confidence,
            calls: AtomicUsize::new(0),
            fail: false,
        }
    }

    fn failing() -> Self {
        Self {
            confidence: 0.0,
            calls: AtomicUsize::new(0),
            fail: true,
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl AiProvider for CountingProvider {
    async fn generate(&self, request: &GenerateRequest) -> Result<GenerateResponse, AiError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(AiError::Unavailable("test: provider down".into()));
        }
        // Emit code that always verifies cleanly: non-empty, balanced.
        Ok(GenerateResponse {
            code: format!(
                "// synthesized for {}\n{{ /* body */ }}\n",
                request.target.id
            ),
            confidence: self.confidence,
            reasoning: Some("test".into()),
            alternatives: Vec::new(),
        })
    }

    async fn repair(&self, _request: &RepairRequest) -> Result<RepairResponse, AiError> {
        unreachable!("repair not used in D.5 tests")
    }

    async fn optimize(&self, _request: &OptimizeRequest) -> Result<OptimizeResponse, AiError> {
        unreachable!("optimize not used in D.5 tests")
    }

    async fn select(&self, _request: &SelectRequest) -> Result<SelectResponse, AiError> {
        unreachable!("select not used in D.5 tests")
    }

    fn model_id(&self) -> String {
        "counting:test".into()
    }
}

// ─── AIR fixture builders ────────────────────────────────────────────────────

fn span() -> Span {
    Span {
        file: FileId(0),
        start: 0,
        end: 0,
    }
}

fn ident(name: &str) -> Ident {
    Ident {
        name: name.into(),
        span: span(),
    }
}

fn node(id: u32, kind: NodeKind) -> AIRNode {
    AIRNode::new(id, span(), kind)
}

/// Module with a match expression (flagged by JS ai_hints).
fn module_with_match() -> AIRNode {
    let scrutinee = node(1, NodeKind::Identifier { name: ident("x") });
    let match_node = node(
        2,
        NodeKind::Match {
            scrutinee: Box::new(scrutinee),
            arms: vec![],
        },
    );
    node(
        0,
        NodeKind::Module {
            path: None,
            annotations: vec![],
            imports: vec![],
            items: vec![match_node],
        },
    )
}

/// Module with an enum declaration (flagged by JS ai_hints).
fn module_with_enum() -> AIRNode {
    let variant = node(
        2,
        NodeKind::EnumVariant {
            name: ident("A"),
            payload: EnumVariantPayload::Unit,
        },
    );
    let enum_decl = node(
        1,
        NodeKind::EnumDecl {
            annotations: vec![],
            visibility: Visibility::Public,
            name: ident("Color"),
            generic_params: vec![],
            variants: vec![variant],
        },
    );
    node(
        0,
        NodeKind::Module {
            path: None,
            annotations: vec![],
            imports: vec![],
            items: vec![enum_decl],
        },
    )
}

/// Module containing only a literal (trivial — should never hit AI).
fn module_trivial_only() -> AIRNode {
    let lit = node(
        1,
        NodeKind::Literal {
            lit: bock_ast::Literal::Int("42".into()),
        },
    );
    node(
        0,
        NodeKind::Module {
            path: None,
            annotations: vec![],
            imports: vec![],
            items: vec![lit],
        },
    )
}

/// Module with an effect handling block (flagged on every target per ai_hints).
fn module_with_handling() -> AIRNode {
    let handler = node(3, NodeKind::Identifier { name: ident("h") });
    let body = node(
        4,
        NodeKind::Block {
            stmts: vec![],
            tail: None,
        },
    );
    let handling = node(
        1,
        NodeKind::HandlingBlock {
            handlers: vec![AirHandlerPair {
                effect: TypePath {
                    segments: vec![ident("Log")],
                    span: span(),
                },
                handler: Box::new(handler),
            }],
            body: Box::new(body),
        },
    );
    node(
        0,
        NodeKind::Module {
            path: None,
            annotations: vec![],
            imports: vec![],
            items: vec![handling],
        },
    )
}

fn module_ctx(path: &str) -> ModuleContext {
    ModuleContext {
        module_path: path.into(),
        imports: Vec::new(),
        siblings: Vec::new(),
        annotations: Vec::new(),
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[test]
fn needs_ai_synthesis_trivial_bypasses_ai() {
    let js = TargetProfile::javascript();
    let lit = node(
        1,
        NodeKind::Literal {
            lit: bock_ast::Literal::Int("1".into()),
        },
    );
    assert!(!needs_ai_synthesis(&js, &lit));
}

#[test]
fn needs_ai_synthesis_flagged_for_js_match() {
    let js = TargetProfile::javascript();
    let m = node(
        1,
        NodeKind::Match {
            scrutinee: Box::new(node(2, NodeKind::Identifier { name: ident("x") })),
            arms: vec![],
        },
    );
    assert!(needs_ai_synthesis(&js, &m));
}

#[test]
fn needs_ai_synthesis_flagged_only_when_hinted() {
    // Rust does not flag Match — native support.
    let rust = TargetProfile::rust();
    let m = node(
        1,
        NodeKind::Match {
            scrutinee: Box::new(node(2, NodeKind::Identifier { name: ident("x") })),
            arms: vec![],
        },
    );
    assert!(!needs_ai_synthesis(&rust, &m));
}

#[test]
fn verify_accepts_balanced_js() {
    assert!(verify_generated("js", "function f() { return 1; }").is_ok());
}

#[test]
fn verify_rejects_unbalanced_js() {
    assert!(verify_generated("js", "function f() { return 1;").is_err());
}

#[test]
fn verify_rejects_empty() {
    assert!(verify_generated("js", "").is_err());
    assert!(verify_generated("js", "   \n  ").is_err());
}

#[test]
fn verify_python_skips_bracket_check() {
    assert!(verify_generated("python", "def f():\n    return 1\n").is_ok());
}

#[test]
fn trait_method_dispatches_through_ai_hints() {
    // CodeGenerator::needs_ai_synthesis default should match free fn.
    let gen = JsGenerator::new();
    let m = node(
        1,
        NodeKind::Match {
            scrutinee: Box::new(node(2, NodeKind::Identifier { name: ident("x") })),
            arms: vec![],
        },
    );
    assert!(gen.needs_ai_synthesis(&m));
    let lit = node(
        3,
        NodeKind::Literal {
            lit: bock_ast::Literal::Int("1".into()),
        },
    );
    assert!(!gen.needs_ai_synthesis(&lit));
}

#[test]
fn rust_trait_rejects_native_constructs() {
    let gen = RsGenerator::new();
    let m = node(
        1,
        NodeKind::Match {
            scrutinee: Box::new(node(2, NodeKind::Identifier { name: ident("x") })),
            arms: vec![],
        },
    );
    assert!(!gen.needs_ai_synthesis(&m));
}

// ── High-confidence acceptance (§17.4) ──────────────────────────────────────

#[tokio::test]
async fn high_confidence_accepted_and_recorded() {
    let provider = Arc::new(CountingProvider::new(0.9));
    let dir = tempfile::tempdir().unwrap();
    let manifest = Arc::new(Mutex::new(ManifestWriter::new(dir.path())));
    let cache = Some(AiCache::new(dir.path()));
    let config = SynthesisConfig {
        confidence_threshold: 0.75,
        deterministic_fallback: true,
        strictness: Strictness::Development,
        auto_pin: false,
        module_path: PathBuf::from("src/m.bock"),
    };
    let driver = AiSynthesisDriver::new(provider.clone(), cache, Some(manifest.clone()), config);

    let module = module_with_match();
    let target = TargetProfile::javascript();
    let ctx = module_ctx("src/m.bock");

    let stats = synthesize_and_flush(&driver, &module, &target, &ctx)
        .await
        .expect("synthesis ok");

    assert_eq!(stats.flagged_nodes, 1);
    assert_eq!(stats.ai_calls, 1);
    assert_eq!(stats.accepted, 1);
    assert_eq!(stats.fallback_triggered, 0);
    assert_eq!(provider.calls(), 1);

    // Manifest should have one codegen decision on disk.
    let build_dir = dir.path().join(".bock/decisions/build");
    let file = build_dir.join("src/m.bock.json");
    assert!(file.exists(), "manifest file missing: {file:?}");
    let content = std::fs::read_to_string(&file).unwrap();
    assert!(content.contains("\"codegen\""));
    assert!(content.contains("\"confidence\": 0.9"));
    assert!(!content.contains("\"pinned\": true"));
}

// ── Low-confidence fallback ─────────────────────────────────────────────────

#[tokio::test]
async fn low_confidence_triggers_fallback() {
    let provider = Arc::new(CountingProvider::new(0.5));
    let dir = tempfile::tempdir().unwrap();
    let manifest = Arc::new(Mutex::new(ManifestWriter::new(dir.path())));
    let config = SynthesisConfig {
        confidence_threshold: 0.75,
        deterministic_fallback: true,
        strictness: Strictness::Development,
        auto_pin: false,
        module_path: PathBuf::from("src/m.bock"),
    };
    let driver = AiSynthesisDriver::new(provider.clone(), None, Some(manifest.clone()), config);

    let module = module_with_match();
    let target = TargetProfile::javascript();
    let ctx = module_ctx("src/m.bock");

    let stats = synthesize_and_flush(&driver, &module, &target, &ctx)
        .await
        .expect("synthesis ok");

    assert_eq!(stats.flagged_nodes, 1);
    assert_eq!(stats.ai_calls, 1);
    assert_eq!(stats.accepted, 0);
    assert_eq!(stats.rejected_low_confidence, 1);
    assert_eq!(stats.fallback_triggered, 1);
    // No manifest file should be written (no decision recorded).
    let build_dir = dir.path().join(".bock/decisions/build");
    assert!(!build_dir.join("src/m.bock.json").exists());
}

// ── No-provider path ────────────────────────────────────────────────────────

#[tokio::test]
async fn no_provider_falls_through() {
    let config = SynthesisConfig {
        module_path: PathBuf::from("src/m.bock"),
        ..Default::default()
    };
    let driver = AiSynthesisDriver::deterministic(config);

    let module = module_with_match();
    let target = TargetProfile::javascript();
    let ctx = module_ctx("src/m.bock");

    let stats = driver
        .synthesize_module(&module, &target, &ctx)
        .await
        .expect("synthesis ok");

    assert_eq!(stats.flagged_nodes, 1);
    assert_eq!(stats.ai_calls, 0);
    assert_eq!(stats.accepted, 0);
    assert_eq!(stats.fallback_triggered, 1);
}

// ── Cache replay bypasses threshold ─────────────────────────────────────────

#[tokio::test]
async fn pinned_cache_replay_bypasses_threshold() {
    let dir = tempfile::tempdir().unwrap();

    // First build: high confidence → accepted + cached.
    {
        let provider = Arc::new(CountingProvider::new(0.9));
        let manifest = Arc::new(Mutex::new(ManifestWriter::new(dir.path())));
        let cache = Some(AiCache::new(dir.path()));
        let config = SynthesisConfig {
            confidence_threshold: 0.75,
            deterministic_fallback: true,
            strictness: Strictness::Development,
            auto_pin: false,
            module_path: PathBuf::from("src/m.bock"),
        };
        let driver = AiSynthesisDriver::new(provider.clone(), cache, Some(manifest), config);
        let module = module_with_match();
        let target = TargetProfile::javascript();
        let ctx = module_ctx("src/m.bock");
        let stats = synthesize_and_flush(&driver, &module, &target, &ctx)
            .await
            .unwrap();
        assert_eq!(stats.accepted, 1);
        assert_eq!(stats.cache_hits, 0);
        assert_eq!(provider.calls(), 1);
    }

    // Second build: provider now returns LOW confidence. Cache hit wins.
    {
        let provider = Arc::new(CountingProvider::new(0.1));
        let manifest = Arc::new(Mutex::new(ManifestWriter::new(dir.path())));
        let cache = Some(AiCache::new(dir.path()));
        let config = SynthesisConfig {
            confidence_threshold: 0.75,
            deterministic_fallback: true,
            strictness: Strictness::Development,
            auto_pin: false,
            module_path: PathBuf::from("src/m.bock"),
        };
        let driver = AiSynthesisDriver::new(provider.clone(), cache, Some(manifest), config);
        let module = module_with_match();
        let target = TargetProfile::javascript();
        let ctx = module_ctx("src/m.bock");
        let stats = synthesize_and_flush(&driver, &module, &target, &ctx)
            .await
            .unwrap();

        // Cache replay — provider never called, response treated as pinned.
        assert_eq!(provider.calls(), 0, "cache hit should skip provider");
        assert_eq!(stats.cache_hits, 1);
        assert_eq!(stats.accepted, 1);
        assert_eq!(stats.rejected_low_confidence, 0);
    }

    // Manifest should have two entries (one per build), the latter pinned.
    let manifest_file = dir.path().join(".bock/decisions/build/src/m.bock.json");
    let content = std::fs::read_to_string(&manifest_file).unwrap();
    // Count pinned=true entries.
    let pinned_count = content.matches("\"pinned\": true").count();
    assert!(
        pinned_count >= 1,
        "expected pinned replay entry in {content}"
    );
    assert!(content.contains("\"cache-replay\""));
}

// ── Provider error + fallback ───────────────────────────────────────────────

#[tokio::test]
async fn provider_error_triggers_fallback() {
    let provider = Arc::new(CountingProvider::failing());
    let dir = tempfile::tempdir().unwrap();
    let manifest = Arc::new(Mutex::new(ManifestWriter::new(dir.path())));
    let config = SynthesisConfig {
        deterministic_fallback: true,
        module_path: PathBuf::from("src/m.bock"),
        ..Default::default()
    };
    let driver = AiSynthesisDriver::new(provider, None, Some(manifest), config);

    let module = module_with_match();
    let target = TargetProfile::javascript();
    let ctx = module_ctx("src/m.bock");

    let stats = synthesize_and_flush(&driver, &module, &target, &ctx)
        .await
        .unwrap();

    assert_eq!(stats.provider_errors, 1);
    assert_eq!(stats.accepted, 0);
    assert_eq!(stats.fallback_triggered, 1);
}

// ── Trivial code never hits AI ──────────────────────────────────────────────

#[tokio::test]
async fn trivial_code_never_hits_ai() {
    let provider = Arc::new(CountingProvider::new(1.0));
    let dir = tempfile::tempdir().unwrap();
    let manifest = Arc::new(Mutex::new(ManifestWriter::new(dir.path())));
    let config = SynthesisConfig {
        module_path: PathBuf::from("src/m.bock"),
        ..Default::default()
    };
    let driver = AiSynthesisDriver::new(provider.clone(), None, Some(manifest), config);

    let module = module_trivial_only();
    let target = TargetProfile::javascript();
    let ctx = module_ctx("src/m.bock");

    let stats = driver
        .synthesize_module(&module, &target, &ctx)
        .await
        .unwrap();

    assert_eq!(stats.flagged_nodes, 0);
    assert_eq!(stats.ai_calls, 0);
    assert_eq!(stats.fallback_triggered, 0);
    assert_eq!(
        provider.calls(),
        0,
        "provider must not be called for literals"
    );
}

// ── Production strictness without pinned decisions ──────────────────────────

#[tokio::test]
async fn production_without_pin_is_unpinned_fallback() {
    let provider = Arc::new(CountingProvider::new(0.99));
    let dir = tempfile::tempdir().unwrap();
    let manifest = Arc::new(Mutex::new(ManifestWriter::new(dir.path())));
    let config = SynthesisConfig {
        confidence_threshold: 0.75,
        deterministic_fallback: true,
        strictness: Strictness::Production,
        auto_pin: false,
        module_path: PathBuf::from("src/m.bock"),
    };
    // No cache — guarantees no pinned decision available.
    let driver = AiSynthesisDriver::new(provider.clone(), None, Some(manifest), config);

    let module = module_with_match();
    let target = TargetProfile::javascript();
    let ctx = module_ctx("src/m.bock");

    let stats = synthesize_and_flush(&driver, &module, &target, &ctx)
        .await
        .unwrap();

    assert_eq!(stats.production_unpinned, 1);
    assert_eq!(stats.accepted, 0);
    assert_eq!(stats.fallback_triggered, 1);
}

// ── Production strictness WITH pinned decisions ─────────────────────────────

#[tokio::test]
async fn production_with_pinned_decision_replays() {
    let dir = tempfile::tempdir().unwrap();

    // Warm-up: development build populates the cache.
    {
        let provider = Arc::new(CountingProvider::new(0.9));
        let manifest = Arc::new(Mutex::new(ManifestWriter::new(dir.path())));
        let cache = Some(AiCache::new(dir.path()));
        let config = SynthesisConfig {
            strictness: Strictness::Development,
            module_path: PathBuf::from("src/m.bock"),
            ..Default::default()
        };
        let driver = AiSynthesisDriver::new(provider, cache, Some(manifest), config);
        let module = module_with_match();
        let target = TargetProfile::javascript();
        let ctx = module_ctx("src/m.bock");
        synthesize_and_flush(&driver, &module, &target, &ctx)
            .await
            .unwrap();
    }

    // Production build: cache replay delivers a pinned decision.
    {
        let provider = Arc::new(CountingProvider::new(0.01));
        let manifest = Arc::new(Mutex::new(ManifestWriter::new(dir.path())));
        let cache = Some(AiCache::new(dir.path()));
        let config = SynthesisConfig {
            strictness: Strictness::Production,
            deterministic_fallback: true,
            module_path: PathBuf::from("src/m.bock"),
            ..Default::default()
        };
        let driver = AiSynthesisDriver::new(provider.clone(), cache, Some(manifest), config);
        let module = module_with_match();
        let target = TargetProfile::javascript();
        let ctx = module_ctx("src/m.bock");

        let stats = synthesize_and_flush(&driver, &module, &target, &ctx)
            .await
            .unwrap();

        assert_eq!(stats.production_unpinned, 0);
        assert_eq!(stats.accepted, 1);
        assert_eq!(stats.cache_hits, 1);
        assert_eq!(provider.calls(), 0, "cache-served pin skips provider");
    }
}

// ── Enum declaration hit ────────────────────────────────────────────────────

#[tokio::test]
async fn enum_flagged_on_js_but_not_rust() {
    let js_provider = Arc::new(CountingProvider::new(0.9));
    let config = SynthesisConfig {
        module_path: PathBuf::from("src/m.bock"),
        ..Default::default()
    };
    let driver = AiSynthesisDriver::new(js_provider.clone(), None, None, config.clone());

    let module = module_with_enum();

    let stats = driver
        .synthesize_module(&module, &TargetProfile::javascript(), &module_ctx("m.bock"))
        .await
        .unwrap();
    assert!(stats.flagged_nodes >= 1, "js flags enum + variant");
    assert!(stats.ai_calls >= 1);

    // Rust — no enum flag.
    let rust_provider = Arc::new(CountingProvider::new(0.9));
    let driver2 = AiSynthesisDriver::new(rust_provider.clone(), None, None, config);
    let stats2 = driver2
        .synthesize_module(&module, &TargetProfile::rust(), &module_ctx("m.bock"))
        .await
        .unwrap();
    // Rust's ai_hints don't include EnumDecl, EnumVariant, or anything else
    // this module uses → no AI calls.
    assert_eq!(stats2.flagged_nodes, 0);
    assert_eq!(stats2.ai_calls, 0);
    assert_eq!(rust_provider.calls(), 0);
}

// ── D.6: rule cache is consulted before AI generation ──────────────────────

#[tokio::test]
async fn rule_cache_hit_skips_ai_and_records_rule_applied() {
    let provider = Arc::new(CountingProvider::new(0.9));
    let dir = tempfile::tempdir().unwrap();

    // Seed the rule cache so Match on JS is served deterministically.
    let rules = RuleCache::new(dir.path());
    let candidate = CandidateRule {
        target_id: "js".into(),
        pattern: "Match → switch".into(),
        template: "switch(x) { /* arms */ }".into(),
        priority: 10,
    };
    let rule = Rule::from_candidate(&candidate, "Match", 0.95);
    rules.insert(&rule).unwrap();

    let manifest = Arc::new(Mutex::new(ManifestWriter::new(dir.path())));
    let config = SynthesisConfig {
        confidence_threshold: 0.75,
        deterministic_fallback: true,
        strictness: Strictness::Development,
        auto_pin: false,
        module_path: PathBuf::from("src/m.bock"),
    };
    let driver = AiSynthesisDriver::new(provider.clone(), None, Some(manifest), config)
        .with_rule_cache(rules);

    let module = module_with_match();
    let target = TargetProfile::javascript();
    let ctx = module_ctx("src/m.bock");

    let stats = synthesize_and_flush(&driver, &module, &target, &ctx)
        .await
        .unwrap();

    assert_eq!(stats.flagged_nodes, 1);
    assert_eq!(stats.rule_applied, 1, "rule should serve this node");
    assert_eq!(stats.ai_calls, 0, "AI must not be called on rule hit");
    assert_eq!(provider.calls(), 0, "provider untouched");

    // Manifest should have a rule_applied decision, not codegen.
    let file = dir.path().join(".bock/decisions/build/src/m.bock.json");
    let content = std::fs::read_to_string(&file).unwrap();
    assert!(
        content.contains("\"rule_applied\""),
        "missing rule_applied entry: {content}"
    );
    assert!(!content.contains("\"codegen\""));
}

#[tokio::test]
async fn rule_cache_miss_still_calls_ai() {
    let provider = Arc::new(CountingProvider::new(0.9));
    let dir = tempfile::tempdir().unwrap();

    // Rule exists but for a different node kind — should miss.
    let rules = RuleCache::new(dir.path());
    let candidate = CandidateRule {
        target_id: "js".into(),
        pattern: "Call".into(),
        template: "call()".into(),
        priority: 1,
    };
    rules
        .insert(&Rule::from_candidate(&candidate, "Call", 0.8))
        .unwrap();

    let manifest = Arc::new(Mutex::new(ManifestWriter::new(dir.path())));
    let config = SynthesisConfig {
        confidence_threshold: 0.75,
        deterministic_fallback: true,
        strictness: Strictness::Development,
        auto_pin: false,
        module_path: PathBuf::from("src/m.bock"),
    };
    let driver = AiSynthesisDriver::new(provider.clone(), None, Some(manifest), config)
        .with_rule_cache(rules);

    let module = module_with_match();
    let target = TargetProfile::javascript();
    let ctx = module_ctx("src/m.bock");

    let stats = synthesize_and_flush(&driver, &module, &target, &ctx)
        .await
        .unwrap();

    assert_eq!(stats.rule_applied, 0);
    assert_eq!(stats.ai_calls, 1);
    assert_eq!(stats.accepted, 1);
    assert_eq!(provider.calls(), 1);
}

#[tokio::test]
async fn rule_cache_production_requires_pinned_rule() {
    let provider = Arc::new(CountingProvider::new(0.99));
    let dir = tempfile::tempdir().unwrap();

    // Unpinned rule — production should ignore it.
    let rules = RuleCache::new(dir.path());
    let candidate = CandidateRule {
        target_id: "js".into(),
        pattern: "Match".into(),
        template: "switch(x) {}".into(),
        priority: 1,
    };
    rules
        .insert(&Rule::from_candidate(&candidate, "Match", 0.9))
        .unwrap();

    let manifest = Arc::new(Mutex::new(ManifestWriter::new(dir.path())));
    let config = SynthesisConfig {
        confidence_threshold: 0.75,
        deterministic_fallback: true,
        strictness: Strictness::Production,
        auto_pin: false,
        module_path: PathBuf::from("src/m.bock"),
    };
    let driver = AiSynthesisDriver::new(provider.clone(), None, Some(manifest), config)
        .with_rule_cache(rules);

    let module = module_with_match();
    let target = TargetProfile::javascript();
    let ctx = module_ctx("src/m.bock");

    let stats = synthesize_and_flush(&driver, &module, &target, &ctx)
        .await
        .unwrap();

    // Unpinned rule skipped → production without a pinned codegen
    // decision either → unpinned fallback.
    assert_eq!(stats.rule_applied, 0);
    assert_eq!(stats.production_unpinned, 1);
}

// ── Handling block flagged across every target ──────────────────────────────

#[tokio::test]
async fn handling_block_flagged_on_every_target() {
    let module = module_with_handling();
    let ctx = module_ctx("src/m.bock");
    for target in TargetProfile::all_builtins() {
        let provider = Arc::new(CountingProvider::new(0.9));
        let config = SynthesisConfig {
            module_path: PathBuf::from("src/m.bock"),
            ..Default::default()
        };
        let driver = AiSynthesisDriver::new(provider.clone(), None, None, config);
        let stats = driver
            .synthesize_module(&module, &target, &ctx)
            .await
            .unwrap();
        assert!(
            stats.flagged_nodes >= 1,
            "target {} should flag handling block",
            target.id
        );
    }
}
