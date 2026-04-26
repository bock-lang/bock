//! Integration tests for [`OpenAiCompatProvider`] against a wiremock-hosted
//! OpenAI-compatible endpoint.
//!
//! Exercises each of the four D.1 modes (Generate, Repair, Optimize, Select)
//! plus a retry-on-rate-limit scenario. No network calls and no real API
//! key — set `BOCK_AI_TEST_KEY` in the environment to opt in to a live
//! test in a future package; this file deliberately skips that path.

use std::collections::HashMap;

use bock_ai::{
    AiConfig, AiError, AiProvider, GenerateRequest, ModuleContext, OpenAiCompatProvider,
    OptimizationHint, OptimizeRequest, RepairRequest, SelectContext, SelectOption,
    SelectRequest, TargetProfile,
};
use bock_air::{AIRNode, NodeIdGen, NodeKind};
use bock_errors::Span;
use bock_types::Strictness;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

fn config_for(server: &MockServer, model: &str) -> AiConfig {
    AiConfig {
        provider: "openai-compatible".into(),
        endpoint: format!("{}/v1", server.uri()),
        model: model.into(),
        api_key_env: Some("__BOCK_AI_WIREMOCK_KEY__".into()),
        max_retries: 2,
        timeout_seconds: 5,
        ..AiConfig::default()
    }
}

fn provider_for(server: &MockServer, model: &str) -> OpenAiCompatProvider {
    // Non-local endpoint: set a fake key in the env so construction succeeds.
    std::env::set_var("__BOCK_AI_WIREMOCK_KEY__", "sk-fake");
    OpenAiCompatProvider::new(config_for(server, model)).expect("provider builds")
}

fn chat_response(content: &str) -> serde_json::Value {
    json!({
        "id": "chatcmpl-fake",
        "object": "chat.completion",
        "choices": [
            {
                "index": 0,
                "finish_reason": "stop",
                "message": {"role": "assistant", "content": content},
            }
        ]
    })
}

#[tokio::test]
async fn generate_against_wiremock() {
    let server = MockServer::start().await;
    let content = "```javascript\nfunction add(a,b){return a+b;}\n```\n\
        {\"confidence\": 0.88, \"reasoning\": \"direct mapping\"}";
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer sk-fake"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(content)))
        .mount(&server)
        .await;

    let provider = provider_for(&server, "gpt-4o");
    let req = GenerateRequest {
        node: dummy_node(),
        target: dummy_target(),
        module_context: ModuleContext::default(),
        prior_decisions: Vec::new(),
        strictness: Strictness::Development,
    };
    let resp = provider.generate(&req).await.expect("generate ok");
    assert!(resp.code.contains("function add"));
    assert!((resp.confidence - 0.88).abs() < 1e-9);
}

#[tokio::test]
async fn repair_against_wiremock() {
    let server = MockServer::start().await;
    let content = r#"{"fixed_code": "let x = 1;", "confidence": 0.93, "reasoning": "added rhs"}"#;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(content)))
        .mount(&server)
        .await;

    let provider = provider_for(&server, "gpt-4o");
    let req = RepairRequest {
        original_code: "let x =;".into(),
        compiler_error: "unexpected token".into(),
        node: dummy_node(),
        target: dummy_target(),
    };
    let resp = provider.repair(&req).await.expect("repair ok");
    assert_eq!(resp.fixed_code, "let x = 1;");
    assert!((resp.confidence - 0.93).abs() < 1e-9);
}

#[tokio::test]
async fn optimize_against_wiremock() {
    let server = MockServer::start().await;
    let content = "```\nreturn 1\n```\n{\"confidence\": 0.71, \
        \"improvements\": [\"dropped semicolon\"]}";
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(content)))
        .mount(&server)
        .await;

    let provider = provider_for(&server, "gpt-4o");
    let req = OptimizeRequest {
        working_code: "return 1;".into(),
        node: dummy_node(),
        target: dummy_target(),
        optimization_hints: vec![OptimizationHint::CodeSize],
    };
    let resp = provider.optimize(&req).await.expect("optimize ok");
    assert_eq!(resp.optimized_code, "return 1");
    assert_eq!(resp.improvements, vec!["dropped semicolon".to_string()]);
}

#[tokio::test]
async fn select_against_wiremock_rejects_out_of_set() {
    let server = MockServer::start().await;
    // Model returns an id that wasn't in the offered options.
    let content = r#"{"selected_id": "escalate", "confidence": 0.9}"#;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(content)))
        .mount(&server)
        .await;

    let provider = provider_for(&server, "gpt-4o");
    let req = SelectRequest {
        options: vec![
            SelectOption {
                id: "retry".into(),
                description: "retry".into(),
            },
            SelectOption {
                id: "fallback".into(),
                description: "fallback".into(),
            },
        ],
        context: SelectContext::default(),
        rationale_prompt: "pick one".into(),
    };
    let err = provider.select(&req).await.unwrap_err();
    assert!(
        matches!(err, AiError::InvalidResponse(_)),
        "closed-set enforced: {err:?}"
    );
}

#[tokio::test]
async fn select_against_wiremock_accepts_valid_id() {
    let server = MockServer::start().await;
    let content = r#"{"selected_id": "retry", "confidence": 0.8, "reasoning": "transient"}"#;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(content)))
        .mount(&server)
        .await;

    let provider = provider_for(&server, "gpt-4o");
    let req = SelectRequest {
        options: vec![
            SelectOption {
                id: "retry".into(),
                description: "retry".into(),
            },
            SelectOption {
                id: "fallback".into(),
                description: "fallback".into(),
            },
        ],
        context: SelectContext::default(),
        rationale_prompt: "pick one".into(),
    };
    let resp = provider.select(&req).await.expect("select ok");
    assert_eq!(resp.selected_id, "retry");
}

#[tokio::test]
async fn retries_on_rate_limit_then_succeeds() {
    let server = MockServer::start().await;

    // First response: 429 rate-limit.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(429).set_body_string("slow down"))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Subsequent responses: success.
    let content = "```\nok\n```\n{\"confidence\": 0.6}";
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_response(content)))
        .mount(&server)
        .await;

    let provider = provider_for(&server, "gpt-4o");
    let req = GenerateRequest {
        node: dummy_node(),
        target: dummy_target(),
        module_context: ModuleContext::default(),
        prior_decisions: Vec::new(),
        strictness: Strictness::Development,
    };
    let resp = provider.generate(&req).await.expect("retry succeeds");
    assert_eq!(resp.code, "ok");
}

#[tokio::test]
async fn auth_error_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
        .expect(1)
        .mount(&server)
        .await;

    let provider = provider_for(&server, "gpt-4o");
    let req = GenerateRequest {
        node: dummy_node(),
        target: dummy_target(),
        module_context: ModuleContext::default(),
        prior_decisions: Vec::new(),
        strictness: Strictness::Development,
    };
    let err = provider.generate(&req).await.unwrap_err();
    assert!(matches!(err, AiError::Auth(_)));
    // The `.expect(1)` assertion on drop verifies no retry occurred.
}
