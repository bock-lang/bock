//! AI provider interface for the Bock transpilation pipeline (§17.8).
//!
//! This crate defines the [`AiProvider`] trait and its four interaction
//! modes — [`generate`](AiProvider::generate) (Tier 1),
//! [`repair`](AiProvider::repair) (§17.7 feedback loop),
//! [`optimize`](AiProvider::optimize) (Tier 3), and
//! [`select`](AiProvider::select) (§10.8 adaptive handler strategy
//! selection). Verification (§17.3) is deterministic and is **not**
//! part of this trait; it is owned by the target profile and
//! `bock-codegen`.
//!
//! The crate ships a [`StubProvider`] for tests and a [`make_provider`]
//! factory that dispatches on [`AiConfig::provider`]. Both HTTP-backed
//! providers — [`OpenAiCompatProvider`] (`"openai-compatible"`) and
//! [`AnthropicProvider`] (`"anthropic"`) — are linked.

pub mod cache;
pub mod caching_provider;
pub mod config;
pub mod decision;
pub mod error;
pub mod governance;
pub mod manifest;
pub mod provider;
pub mod providers;
pub mod request;
pub mod rules;

pub use cache::{compute_key, AiCache, CacheError, CacheStats};
pub use caching_provider::CachingProvider;
pub use config::AiConfig;
pub use decision::{Decision, DecisionType, ManifestScope};
pub use error::AiError;
pub use governance::{validate_production, StrictnessPolicy, UnpinnedEntry, UnpinnedReport};
pub use manifest::{ManifestError, ManifestWriter};
pub use provider::{validate_select_response, AiProvider};
pub use providers::{AnthropicProvider, OpenAiCompatProvider};
pub use request::{
    Alternative, CandidateRule, DecisionRef, GenerateRequest, GenerateResponse, ModuleContext,
    OptimizationHint, OptimizeRequest, OptimizeResponse, RepairRequest, RepairResponse,
    SelectContext, SelectOption, SelectRequest, SelectResponse, TargetProfile,
};
pub use rules::{compute_rule_id, node_kind_name, Provenance, Rule, RuleCache, RuleCacheError};

use async_trait::async_trait;

/// Deterministic test provider that returns canned responses for all
/// four interaction modes.
///
/// Used in unit tests, and as the default selection when
/// [`AiConfig::provider`] is `"stub"`. Never performs network I/O.
#[derive(Debug, Clone)]
pub struct StubProvider {
    model: String,
}

impl StubProvider {
    /// Creates a stub provider whose [`model_id`](AiProvider::model_id)
    /// is derived from `config.model` (defaulting to `"stub"` when empty).
    #[must_use]
    pub fn new(config: AiConfig) -> Self {
        let model = if config.model.is_empty() {
            "stub".into()
        } else {
            config.model
        };
        Self { model }
    }
}

impl Default for StubProvider {
    fn default() -> Self {
        Self::new(AiConfig::default())
    }
}

#[async_trait]
impl AiProvider for StubProvider {
    async fn generate(
        &self,
        request: &GenerateRequest,
    ) -> Result<GenerateResponse, AiError> {
        Ok(GenerateResponse {
            code: format!("// stub generate for target '{}'\n", request.target.id),
            confidence: 1.0,
            reasoning: Some("stub: deterministic canned response".into()),
            alternatives: Vec::new(),
        })
    }

    async fn repair(&self, request: &RepairRequest) -> Result<RepairResponse, AiError> {
        Ok(RepairResponse {
            fixed_code: request.original_code.clone(),
            confidence: 1.0,
            candidate_rule: None,
            reasoning: Some("stub: echo original code".into()),
        })
    }

    async fn optimize(
        &self,
        request: &OptimizeRequest,
    ) -> Result<OptimizeResponse, AiError> {
        Ok(OptimizeResponse {
            optimized_code: request.working_code.clone(),
            confidence: 1.0,
            improvements: Vec::new(),
            reasoning: Some("stub: no-op optimization".into()),
        })
    }

    async fn select(&self, request: &SelectRequest) -> Result<SelectResponse, AiError> {
        let first = request.options.first().ok_or_else(|| {
            AiError::InvalidResponse("stub select: empty option set".into())
        })?;
        let response = SelectResponse {
            selected_id: first.id.clone(),
            confidence: 1.0,
            reasoning: Some("stub: first option".into()),
        };
        validate_select_response(&request.options, &response)?;
        Ok(response)
    }

    fn model_id(&self) -> String {
        format!("stub:{}", self.model)
    }
}

/// Constructs a provider from an [`AiConfig`].
///
/// Dispatches on [`AiConfig::provider`] via a static `match` — per Q7
/// of the 2026-04-20 spec amendment, no plugin loading. Both HTTP-backed
/// providers are linked: `"openai-compatible"` (D.3) and `"anthropic"`
/// (D.4). `"stub"` is always available for tests.
///
/// # Errors
/// Returns [`AiError::ProviderError`] when `config.provider` is not a
/// recognized identifier, [`AiError::Auth`] when a provider cannot load
/// its API key, or [`AiError::Unavailable`] when a provider is recognized
/// but not yet linked.
pub fn make_provider(config: AiConfig) -> Result<Box<dyn AiProvider>, AiError> {
    match config.provider.as_str() {
        "stub" => Ok(Box::new(StubProvider::new(config))),
        "openai-compatible" => Ok(Box::new(OpenAiCompatProvider::new(config)?)),
        "anthropic" => Ok(Box::new(AnthropicProvider::new(config)?)),
        other => Err(AiError::ProviderError(format!(
            "unknown provider: {other}"
        ))),
    }
}

/// The list of built-in provider identifiers accepted by [`make_provider`].
///
/// Intended for tooling (vocab emitter, CLI help rendering). The order
/// here is the preferred listing order.
#[must_use]
pub fn known_providers() -> &'static [&'static str] {
    &["openai-compatible", "anthropic", "stub"]
}

#[cfg(test)]
mod tests {
    use super::*;
    use bock_air::{AIRNode, NodeIdGen, NodeKind};
    use bock_errors::Span;
    use bock_types::Strictness;
    use std::collections::HashMap;

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

    fn dummy_target() -> TargetProfile {
        TargetProfile {
            id: "js".into(),
            display_name: "JavaScript".into(),
            capabilities: HashMap::new(),
            conventions: HashMap::new(),
        }
    }

    #[tokio::test]
    async fn stub_generate_returns_canned() {
        let p = StubProvider::default();
        let req = GenerateRequest {
            node: dummy_node(),
            target: dummy_target(),
            module_context: ModuleContext::default(),
            prior_decisions: Vec::new(),
            strictness: Strictness::Development,
        };
        let resp = p.generate(&req).await.expect("ok");
        assert!(resp.code.contains("js"));
        assert!((resp.confidence - 1.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn stub_repair_echoes_original() {
        let p = StubProvider::default();
        let req = RepairRequest {
            original_code: "let x = 1;".into(),
            compiler_error: "oops".into(),
            node: dummy_node(),
            target: dummy_target(),
        };
        let resp = p.repair(&req).await.expect("ok");
        assert_eq!(resp.fixed_code, "let x = 1;");
    }

    #[tokio::test]
    async fn stub_optimize_is_identity() {
        let p = StubProvider::default();
        let req = OptimizeRequest {
            working_code: "return 1;".into(),
            node: dummy_node(),
            target: dummy_target(),
            optimization_hints: vec![OptimizationHint::Performance],
        };
        let resp = p.optimize(&req).await.expect("ok");
        assert_eq!(resp.optimized_code, "return 1;");
    }

    #[tokio::test]
    async fn stub_select_returns_first_option() {
        let p = StubProvider::default();
        let req = SelectRequest {
            options: vec![
                SelectOption {
                    id: "a".into(),
                    description: "first".into(),
                },
                SelectOption {
                    id: "b".into(),
                    description: "second".into(),
                },
            ],
            context: SelectContext::default(),
            rationale_prompt: "pick one".into(),
        };
        let resp = p.select(&req).await.expect("ok");
        assert_eq!(resp.selected_id, "a");
    }

    #[tokio::test]
    async fn stub_select_fails_on_empty_options() {
        let p = StubProvider::default();
        let req = SelectRequest {
            options: Vec::new(),
            context: SelectContext::default(),
            rationale_prompt: "pick one".into(),
        };
        let err = p.select(&req).await.expect_err("should fail");
        assert!(matches!(err, AiError::InvalidResponse(_)));
    }

    #[test]
    fn stub_model_id_format() {
        let p = StubProvider::default();
        assert_eq!(p.model_id(), "stub:stub");

        let p2 = StubProvider::new(AiConfig {
            model: "custom".into(),
            ..AiConfig::default()
        });
        assert_eq!(p2.model_id(), "stub:custom");
    }

    #[test]
    fn factory_dispatches_to_stub() {
        let cfg = AiConfig {
            provider: "stub".into(),
            ..AiConfig::default()
        };
        let p = make_provider(cfg).expect("stub constructs");
        assert!(p.model_id().starts_with("stub:"));
    }

    #[test]
    fn factory_dispatches_to_openai_compatible() {
        let cfg = AiConfig {
            provider: "openai-compatible".into(),
            endpoint: "http://localhost:11434/v1".into(),
            model: "llama3".into(),
            ..AiConfig::default()
        };
        let p = make_provider(cfg).expect("openai-compatible constructs");
        assert_eq!(p.model_id(), "openai-compatible:llama3");
    }

    #[test]
    fn factory_openai_compatible_requires_key_for_remote() {
        let cfg = AiConfig {
            provider: "openai-compatible".into(),
            endpoint: "https://api.example.com/v1".into(),
            model: "gpt-4o".into(),
            api_key_env: Some("__BOCK_AI_TEST_UNSET_ENV_VAR__".into()),
            ..AiConfig::default()
        };
        let err = make_provider(cfg).err().expect("missing key");
        assert!(matches!(err, AiError::Auth(_)));
    }

    #[test]
    fn factory_dispatches_to_anthropic() {
        std::env::set_var("__BOCK_AI_FACTORY_ANTHROPIC_KEY__", "sk-ant-fake");
        let cfg = AiConfig {
            provider: "anthropic".into(),
            endpoint: "https://api.anthropic.com/v1".into(),
            model: "claude-opus-4-7".into(),
            api_key_env: Some("__BOCK_AI_FACTORY_ANTHROPIC_KEY__".into()),
            ..AiConfig::default()
        };
        let p = make_provider(cfg).expect("anthropic constructs");
        assert_eq!(p.model_id(), "anthropic:claude-opus-4-7");
        std::env::remove_var("__BOCK_AI_FACTORY_ANTHROPIC_KEY__");
    }

    #[test]
    fn factory_anthropic_requires_key() {
        let cfg = AiConfig {
            provider: "anthropic".into(),
            endpoint: "https://api.anthropic.com/v1".into(),
            model: "claude-opus-4-7".into(),
            api_key_env: Some("__BOCK_AI_ANTHROPIC_UNSET_ENV_VAR__".into()),
            ..AiConfig::default()
        };
        let err = make_provider(cfg).err().expect("missing key");
        assert!(matches!(err, AiError::Auth(_)));
    }

    #[test]
    fn factory_rejects_unknown_provider() {
        let cfg = AiConfig {
            provider: "made-up".into(),
            ..AiConfig::default()
        };
        let err = make_provider(cfg).err().expect("unknown");
        match err {
            AiError::ProviderError(m) => assert!(m.contains("made-up")),
            other => panic!("expected ProviderError, got {other:?}"),
        }
    }
}
