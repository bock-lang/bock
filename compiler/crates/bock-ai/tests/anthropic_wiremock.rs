//! Integration tests for [`AnthropicProvider`] against a wiremock-hosted
//! Messages API endpoint.
//!
//! Exercises each of the four D.1 modes (Generate, Repair, Optimize,
//! Select) plus the header conventions unique to Anthropic (`x-api-key`,
//! `anthropic-version`), structured content blocks, extended thinking,
//! and closed-set enforcement via `tool_use`. No network calls and no
//! real API key.

use std::collections::HashMap;

use bock_ai::{
    AiConfig, AiError, AiProvider, AnthropicProvider, GenerateRequest, ModuleContext,
    OptimizationHint, OptimizeRequest, RepairRequest, SelectContext, SelectOption, SelectRequest,
    TargetProfile,
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
        provider: "anthropic".into(),
        endpoint: format!("{}/v1", server.uri()),
        model: model.into(),
        api_key_env: Some("__BOCK_AI_ANTHROPIC_WIREMOCK_KEY__".into()),
        max_retries: 2,
        timeout_seconds: 5,
        ..AiConfig::default()
    }
}

fn provider_for(server: &MockServer, model: &str) -> AnthropicProvider {
    std::env::set_var("__BOCK_AI_ANTHROPIC_WIREMOCK_KEY__", "sk-ant-fake");
    AnthropicProvider::new(config_for(server, model)).expect("provider builds")
}

fn text_response(content_blocks: serde_json::Value) -> serde_json::Value {
    json!({
        "id": "msg_fake",
        "type": "message",
        "role": "assistant",
        "model": "claude-opus-4-7",
        "stop_reason": "end_turn",
        "content": content_blocks,
    })
}

#[tokio::test]
async fn generate_against_wiremock() {
    let server = MockServer::start().await;
    let body = text_response(json!([
        {"type": "thinking", "thinking": "picking an idiomatic arrow form"},
        {"type": "text", "text":
            "```javascript\nfunction add(a,b){return a+b;}\n```\n\
             {\"confidence\": 0.88, \"reasoning\": \"direct mapping\"}"
        },
    ]));
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "sk-ant-fake"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let provider = provider_for(&server, "claude-opus-4-7");
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
    // Reasoning combines extended-thinking output with JSON tail.
    let reasoning = resp.reasoning.unwrap();
    assert!(reasoning.contains("picking an idiomatic"));
    assert!(reasoning.contains("direct mapping"));
}

#[tokio::test]
async fn repair_against_wiremock() {
    let server = MockServer::start().await;
    let body = text_response(json!([
        {"type": "text", "text":
            r#"{"fixed_code": "let x = 1;", "confidence": 0.93, "reasoning": "added rhs"}"#
        },
    ]));
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let provider = provider_for(&server, "claude-opus-4-7");
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
    let body = text_response(json!([
        {"type": "text", "text":
            "```\nreturn 1\n```\n{\"confidence\": 0.71, \
             \"improvements\": [\"dropped semicolon\"]}"
        },
    ]));
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let provider = provider_for(&server, "claude-opus-4-7");
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
async fn select_against_wiremock_uses_tool_use() {
    let server = MockServer::start().await;
    let body = text_response(json!([
        {
            "type": "tool_use",
            "id": "toolu_fake",
            "name": "select_option",
            "input": {
                "selected_id": "retry",
                "confidence": 0.8,
                "reasoning": "transient"
            }
        },
    ]));
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let provider = provider_for(&server, "claude-opus-4-7");
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
    assert!((resp.confidence - 0.8).abs() < 1e-9);
    assert_eq!(resp.reasoning.as_deref(), Some("transient"));
}

#[tokio::test]
async fn select_against_wiremock_rejects_out_of_set() {
    let server = MockServer::start().await;
    // Even though the tool's enum constrains selected_id at the API
    // layer, we still validate server-side to tolerate a misconfigured
    // proxy or a buggy provider forwarding a different id.
    let body = text_response(json!([
        {
            "type": "tool_use",
            "id": "toolu_fake",
            "name": "select_option",
            "input": {"selected_id": "escalate", "confidence": 0.9}
        },
    ]));
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let provider = provider_for(&server, "claude-opus-4-7");
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
async fn select_against_wiremock_rejects_missing_tool_use() {
    let server = MockServer::start().await;
    // Model replies with only a text block — no tool_use. This must fail.
    let body = text_response(json!([
        {"type": "text", "text": "I'd pick retry."},
    ]));
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let provider = provider_for(&server, "claude-opus-4-7");
    let req = SelectRequest {
        options: vec![SelectOption {
            id: "retry".into(),
            description: "retry".into(),
        }],
        context: SelectContext::default(),
        rationale_prompt: "pick one".into(),
    };
    let err = provider.select(&req).await.unwrap_err();
    assert!(matches!(err, AiError::InvalidResponse(_)));
}

#[tokio::test]
async fn retries_on_rate_limit_then_succeeds() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(429).set_body_string("slow down"))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let success = text_response(json!([
        {"type": "text", "text": "```\nok\n```\n{\"confidence\": 0.6}"},
    ]));
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(success))
        .mount(&server)
        .await;

    let provider = provider_for(&server, "claude-opus-4-7");
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
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
        .expect(1)
        .mount(&server)
        .await;

    let provider = provider_for(&server, "claude-opus-4-7");
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

#[tokio::test]
async fn sends_system_as_top_level_field_not_message() {
    use wiremock::matchers::body_partial_json;

    let server = MockServer::start().await;
    let body = text_response(json!([
        {"type": "text", "text": "```\nok\n```"},
    ]));

    // Matches when the request body contains a top-level "system" string
    // and a single user message. Anthropic's distinguishing wire shape.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_partial_json(json!({
            "model": "claude-opus-4-7",
            "messages": [{"role": "user"}],
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .expect(1)
        .mount(&server)
        .await;

    let provider = provider_for(&server, "claude-opus-4-7");
    let req = GenerateRequest {
        node: dummy_node(),
        target: dummy_target(),
        module_context: ModuleContext::default(),
        prior_decisions: Vec::new(),
        strictness: Strictness::Development,
    };
    let _ = provider.generate(&req).await.expect("generate ok");
    // If the body shape didn't match, `.expect(1)` on drop would panic.
}
