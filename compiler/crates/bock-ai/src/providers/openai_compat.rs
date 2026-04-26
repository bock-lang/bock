//! OpenAI Chat Completions-compatible provider (§17.8).
//!
//! Default HTTP-backed provider for all servers that speak the
//! OpenAI `/v1/chat/completions` protocol: OpenAI itself, Ollama,
//! vLLM, llama.cpp server, LM Studio, Together, Groq, Fireworks,
//! OpenRouter, etc.
//!
//! All four modes from D.1 (Generate, Repair, Optimize, Select) are
//! implemented on top of a single chat-completions call. Responses are
//! parsed out of markdown fenced code blocks (for code-producing modes)
//! and/or JSON payloads (for structured-response modes). Retries use
//! exponential backoff on [`AiError::Network`] and [`AiError::RateLimited`]
//! up to [`AiConfig::max_retries`].

use std::collections::HashMap;
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde_json::{json, Value as JsonValue};

use crate::config::AiConfig;
use crate::error::AiError;
use crate::provider::{validate_select_response, AiProvider};
use crate::request::{
    Alternative, CandidateRule, GenerateRequest, GenerateResponse, ModuleContext,
    OptimizationHint, OptimizeRequest, OptimizeResponse, RepairRequest, RepairResponse,
    SelectContext, SelectRequest, SelectResponse, TargetProfile,
};
use bock_air::AIRNode;

/// OpenAI Chat Completions-compatible provider.
///
/// Construct with [`Self::new`] and dispatch via [`AiProvider`].
#[derive(Debug)]
pub struct OpenAiCompatProvider {
    config: AiConfig,
    client: Client,
    api_key: Option<String>,
}

impl OpenAiCompatProvider {
    /// Creates a provider from an [`AiConfig`].
    ///
    /// Loads the API key from the environment variable named in
    /// `config.api_key_env`. If the variable is unset (or no variable is
    /// named), the constructor still succeeds when the endpoint looks
    /// local (`localhost`, `127.0.0.1`, `0.0.0.0`, `[::1]`) — many
    /// Bock-on-laptop setups use Ollama or llama.cpp without auth. For
    /// any other endpoint a missing key returns [`AiError::Auth`].
    ///
    /// # Errors
    /// - [`AiError::Auth`] — non-local endpoint and no API key loaded.
    /// - [`AiError::ProviderError`] — the HTTP client could not be built.
    pub fn new(config: AiConfig) -> Result<Self, AiError> {
        let api_key = config
            .api_key_env
            .as_deref()
            .and_then(|name| std::env::var(name).ok())
            .filter(|s| !s.is_empty());

        if api_key.is_none() && !is_local_endpoint(&config.endpoint) {
            return Err(AiError::Auth(format!(
                "no API key loaded (env var {:?} unset) and endpoint '{}' is not local",
                config.api_key_env, config.endpoint,
            )));
        }

        let client = Client::builder()
            .timeout(StdDuration::from_secs(config.timeout_seconds.max(1)))
            .build()
            .map_err(|e| {
                AiError::ProviderError(format!("failed to build reqwest client: {e}"))
            })?;

        Ok(Self {
            config,
            client,
            api_key,
        })
    }

    fn chat_url(&self) -> String {
        let base = self.config.endpoint.trim_end_matches('/');
        format!("{base}/chat/completions")
    }

    async fn chat(&self, system: &str, user: &str) -> Result<String, AiError> {
        let body = json!({
            "model": self.config.model,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
        });

        let mut attempt: u32 = 0;
        loop {
            match self.call_once(&body).await {
                Ok(s) => return Ok(s),
                Err(e) if attempt < self.config.max_retries && is_retryable(&e) => {
                    let delay = StdDuration::from_millis(backoff_ms(attempt));
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn call_once(&self, body: &JsonValue) -> Result<String, AiError> {
        let mut req = self.client.post(self.chat_url()).json(body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }

        let resp = req.send().await.map_err(map_send_error)?;
        let status = resp.status();
        if status.is_success() {
            let value: JsonValue = resp.json().await.map_err(|e| {
                AiError::InvalidResponse(format!("response body not JSON: {e}"))
            })?;
            extract_message_content(&value)
        } else {
            let body_text = resp.text().await.unwrap_or_default();
            Err(map_http_status(status, &body_text))
        }
    }
}

#[async_trait]
impl AiProvider for OpenAiCompatProvider {
    async fn generate(
        &self,
        request: &GenerateRequest,
    ) -> Result<GenerateResponse, AiError> {
        let (system, user) = build_generate_messages(request);
        let content = self.chat(&system, &user).await?;
        parse_generate_content(&content)
    }

    async fn repair(&self, request: &RepairRequest) -> Result<RepairResponse, AiError> {
        let (system, user) = build_repair_messages(request);
        let content = self.chat(&system, &user).await?;
        parse_repair_content(&content)
    }

    async fn optimize(
        &self,
        request: &OptimizeRequest,
    ) -> Result<OptimizeResponse, AiError> {
        let (system, user) = build_optimize_messages(request);
        let content = self.chat(&system, &user).await?;
        parse_optimize_content(&content)
    }

    async fn select(&self, request: &SelectRequest) -> Result<SelectResponse, AiError> {
        let (system, user) = build_select_messages(request);
        let content = self.chat(&system, &user).await?;
        let response = parse_select_content(&content)?;
        validate_select_response(&request.options, &response)?;
        Ok(response)
    }

    fn model_id(&self) -> String {
        format!("openai-compatible:{}", self.config.model)
    }
}

// ─── Endpoint / retry / error mapping helpers ────────────────────────────────

fn is_local_endpoint(endpoint: &str) -> bool {
    let lower = endpoint.to_ascii_lowercase();
    lower.contains("localhost")
        || lower.contains("127.0.0.1")
        || lower.contains("0.0.0.0")
        || lower.contains("[::1]")
}

fn is_retryable(e: &AiError) -> bool {
    matches!(e, AiError::Network(_) | AiError::RateLimited(_))
}

fn backoff_ms(attempt: u32) -> u64 {
    // 100ms, 200ms, 400ms, 800ms, ... capped at ~6.4s.
    let exp = attempt.min(6);
    100u64.saturating_mul(1u64 << exp)
}

fn map_send_error(e: reqwest::Error) -> AiError {
    if e.is_timeout() {
        AiError::Timeout(e.to_string())
    } else if e.is_connect() {
        AiError::Network(format!("connect: {e}"))
    } else {
        AiError::Network(e.to_string())
    }
}

fn map_http_status(status: StatusCode, body: &str) -> AiError {
    let snippet = truncate(body, 400);
    let code = status.as_u16();
    match code {
        401 | 403 => AiError::Auth(format!("HTTP {code}: {snippet}")),
        429 => AiError::RateLimited(format!("HTTP {code}: {snippet}")),
        500..=599 => AiError::ProviderError(format!("HTTP {code}: {snippet}")),
        _ => AiError::ProviderError(format!("HTTP {code}: {snippet}")),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut boundary = max;
        while boundary > 0 && !s.is_char_boundary(boundary) {
            boundary -= 1;
        }
        format!("{}…", &s[..boundary])
    }
}

fn extract_message_content(resp: &JsonValue) -> Result<String, AiError> {
    resp.get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            AiError::InvalidResponse("response missing choices[0].message.content".into())
        })
}

// ─── Prompt construction ─────────────────────────────────────────────────────

const GENERATE_SYSTEM: &str = "\
You are the code-generation backend of the Bock language compiler. \
Translate the given Bock AIR (Bock Intermediate Representation) node into idiomatic target-language code that:
- preserves Bock semantics exactly,
- follows the conventions listed for the target,
- stays consistent with any prior decisions provided.

Respond with exactly one markdown fenced code block containing ONLY the target-language code. \
After the closing fence, on a new line, emit a single JSON object with these fields:
  \"confidence\":   number in [0.0, 1.0]
  \"reasoning\":    short string explaining your choice (may be omitted)
  \"alternatives\": array of {\"label\", \"reasoning\", \"confidence\"} (may be omitted)";

const REPAIR_SYSTEM: &str = "\
You are the repair backend of the Bock compiler. The compiler generated \
code for a target language, but the target toolchain rejected it. Produce \
a fixed version that compiles and preserves the AIR semantics.

Respond with exactly one JSON object with these fields:
  \"fixed_code\":   fixed target-language code (required)
  \"confidence\":   number in [0.0, 1.0] (required)
  \"reasoning\":    short string (optional)
  \"rule_pattern\": null, or an object {\"target_id\", \"pattern\", \"template\", \"priority\"} \
describing a rule candidate that would generalize this repair for future identical AIR shapes (optional).";

const OPTIMIZE_SYSTEM: &str = "\
You are the optimization backend of the Bock compiler. The given target \
code already compiles and is semantically correct. Rewrite it to be more \
idiomatic / performant / smaller per the hints provided, without changing \
observable behavior.

Respond with exactly one markdown fenced code block containing the rewritten code. \
After the closing fence, on a new line, emit a JSON object with fields:
  \"confidence\":   number in [0.0, 1.0]
  \"improvements\": array of short strings describing each change (optional)
  \"reasoning\":    short string (optional)";

const SELECT_SYSTEM: &str = "\
You are a classifier. You MUST pick exactly one id from the provided closed \
set of options — inventing a new id is a hard error. Base your choice on \
the context and the rationale prompt supplied by the caller.

Respond with exactly one JSON object with these fields:
  \"selected_id\": id of the chosen option (must match one of the options verbatim)
  \"confidence\":  number in [0.0, 1.0]
  \"reasoning\":   short string (optional)";

fn build_generate_messages(req: &GenerateRequest) -> (String, String) {
    let mut user = String::new();
    user.push_str(&render_target(&req.target));
    user.push_str(&render_module_context(&req.module_context));

    if !req.prior_decisions.is_empty() {
        user.push_str("\nPrior decisions:\n");
        for d in &req.prior_decisions {
            user.push_str(&format!("  {} = {}\n", d.decision, d.choice));
        }
    }

    user.push_str(&format!("\nStrictness: {:?}\n", req.strictness));
    user.push_str(&format!("\nAIR node:\n{}\n", render_air_node(&req.node)));

    (GENERATE_SYSTEM.to_string(), user)
}

fn build_repair_messages(req: &RepairRequest) -> (String, String) {
    let mut user = String::new();
    user.push_str(&render_target(&req.target));
    user.push_str("\nOriginal failing code:\n");
    user.push_str("```\n");
    user.push_str(req.original_code.trim_end_matches('\n'));
    user.push_str("\n```\n");
    user.push_str("\nCompiler error:\n");
    user.push_str(req.compiler_error.trim_end_matches('\n'));
    user.push('\n');
    user.push_str(&format!("\nAIR node:\n{}\n", render_air_node(&req.node)));

    (REPAIR_SYSTEM.to_string(), user)
}

fn build_optimize_messages(req: &OptimizeRequest) -> (String, String) {
    let mut user = String::new();
    user.push_str(&render_target(&req.target));

    if !req.optimization_hints.is_empty() {
        user.push_str("\nOptimization hints:\n");
        for hint in &req.optimization_hints {
            user.push_str(&format!("  - {}\n", render_hint(hint)));
        }
    }

    user.push_str("\nWorking code:\n");
    user.push_str("```\n");
    user.push_str(req.working_code.trim_end_matches('\n'));
    user.push_str("\n```\n");
    user.push_str(&format!("\nAIR node:\n{}\n", render_air_node(&req.node)));

    (OPTIMIZE_SYSTEM.to_string(), user)
}

fn build_select_messages(req: &SelectRequest) -> (String, String) {
    let mut user = String::new();

    user.push_str("Options (choose exactly one id):\n");
    for opt in &req.options {
        user.push_str(&format!("  [{}] {}\n", opt.id, opt.description));
    }

    user.push_str(&render_select_context(&req.context));
    user.push_str(&format!("\nQuestion: {}\n", req.rationale_prompt));

    (SELECT_SYSTEM.to_string(), user)
}

fn render_target(t: &TargetProfile) -> String {
    let mut s = format!("Target: {} ({})\n", t.id, t.display_name);
    s.push_str(&render_map("Target capabilities", &t.capabilities));
    s.push_str(&render_map("Target conventions", &t.conventions));
    s
}

fn render_module_context(m: &ModuleContext) -> String {
    let mut s = String::new();
    if !m.module_path.is_empty() {
        s.push_str(&format!("\nModule: {}\n", m.module_path));
    }
    if !m.imports.is_empty() {
        s.push_str("Imports:\n");
        let mut imports = m.imports.clone();
        imports.sort();
        for i in imports {
            s.push_str(&format!("  {i}\n"));
        }
    }
    if !m.siblings.is_empty() {
        s.push_str("Siblings:\n");
        let mut siblings = m.siblings.clone();
        siblings.sort();
        for i in siblings {
            s.push_str(&format!("  {i}\n"));
        }
    }
    if !m.annotations.is_empty() {
        s.push_str("Annotations:\n");
        let mut ann = m.annotations.clone();
        ann.sort();
        for a in ann {
            s.push_str(&format!("  {a}\n"));
        }
    }
    s
}

fn render_select_context(c: &SelectContext) -> String {
    let mut s = String::new();
    if let Some(err) = &c.error {
        s.push_str(&format!("\nError: {err}\n"));
    }
    if !c.annotations.is_empty() {
        s.push_str("Annotations:\n");
        let mut ann = c.annotations.clone();
        ann.sort();
        for a in ann {
            s.push_str(&format!("  {a}\n"));
        }
    }
    if !c.history.is_empty() {
        s.push_str("History (recent similar decisions):\n");
        for h in &c.history {
            s.push_str(&format!("  {h}\n"));
        }
    }
    s.push_str(&render_map("Metadata", &c.metadata));
    s
}

fn render_map(name: &str, m: &HashMap<String, String>) -> String {
    if m.is_empty() {
        return String::new();
    }
    let mut entries: Vec<(&String, &String)> = m.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut s = format!("{name}:\n");
    for (k, v) in entries {
        s.push_str(&format!("  {k}: {v}\n"));
    }
    s
}

fn render_hint(hint: &OptimizationHint) -> String {
    match hint {
        OptimizationHint::Performance => "performance".into(),
        OptimizationHint::Idiomatic => "idiomatic".into(),
        OptimizationHint::CodeSize => "code size".into(),
        OptimizationHint::Custom(s) => format!("custom: {s}"),
    }
}

fn render_air_node(node: &AIRNode) -> String {
    // AIR doesn't derive Serialize; its Debug output is stable enough for a
    // prompt and is what keeps this function deterministic.
    format!("{node:#?}")
}

// ─── Response parsing ────────────────────────────────────────────────────────

fn parse_generate_content(content: &str) -> Result<GenerateResponse, AiError> {
    let (code, tail) = split_code_and_tail(content);
    let code = code.ok_or_else(|| {
        AiError::InvalidResponse(
            "generate response missing markdown code block".into(),
        )
    })?;
    let confidence = extract_confidence(tail.as_ref());
    let reasoning = extract_string(tail.as_ref(), "reasoning");
    let alternatives = extract_alternatives(tail.as_ref());
    Ok(GenerateResponse {
        code,
        confidence,
        reasoning,
        alternatives,
    })
}

fn parse_optimize_content(content: &str) -> Result<OptimizeResponse, AiError> {
    let (code, tail) = split_code_and_tail(content);
    let code = code.ok_or_else(|| {
        AiError::InvalidResponse(
            "optimize response missing markdown code block".into(),
        )
    })?;
    let confidence = extract_confidence(tail.as_ref());
    let reasoning = extract_string(tail.as_ref(), "reasoning");
    let improvements = tail
        .as_ref()
        .and_then(|t| t.get("improvements"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(OptimizeResponse {
        optimized_code: code,
        confidence,
        improvements,
        reasoning,
    })
}

fn parse_repair_content(content: &str) -> Result<RepairResponse, AiError> {
    let json = parse_json_object(content)?;
    let fixed_code = json
        .get("fixed_code")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            AiError::InvalidResponse("repair response missing 'fixed_code'".into())
        })?
        .to_string();
    let confidence = extract_confidence(Some(&json));
    let reasoning = extract_string(Some(&json), "reasoning");
    let candidate_rule = json
        .get("rule_pattern")
        .filter(|v| !v.is_null())
        .and_then(parse_candidate_rule);
    Ok(RepairResponse {
        fixed_code,
        confidence,
        candidate_rule,
        reasoning,
    })
}

fn parse_select_content(content: &str) -> Result<SelectResponse, AiError> {
    let json = parse_json_object(content)?;
    let selected_id = json
        .get("selected_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            AiError::InvalidResponse("select response missing 'selected_id'".into())
        })?
        .to_string();
    let confidence = extract_confidence(Some(&json));
    let reasoning = extract_string(Some(&json), "reasoning");
    Ok(SelectResponse {
        selected_id,
        confidence,
        reasoning,
    })
}

fn parse_candidate_rule(v: &JsonValue) -> Option<CandidateRule> {
    Some(CandidateRule {
        target_id: v.get("target_id")?.as_str()?.to_string(),
        pattern: v.get("pattern")?.as_str()?.to_string(),
        template: v.get("template")?.as_str()?.to_string(),
        priority: v
            .get("priority")
            .and_then(|p| p.as_i64())
            .map(|n| n as i32)
            .unwrap_or(0),
    })
}

fn extract_confidence(v: Option<&JsonValue>) -> f64 {
    v.and_then(|v| v.get("confidence"))
        .and_then(|c| c.as_f64())
        .map(|f| f.clamp(0.0, 1.0))
        .unwrap_or(0.5)
}

fn extract_string(v: Option<&JsonValue>, key: &str) -> Option<String> {
    v.and_then(|v| v.get(key))
        .and_then(|s| s.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn extract_alternatives(v: Option<&JsonValue>) -> Vec<Alternative> {
    v.and_then(|v| v.get("alternatives"))
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|entry| {
                    Some(Alternative {
                        label: entry.get("label")?.as_str()?.to_string(),
                        reasoning: entry
                            .get("reasoning")
                            .and_then(|r| r.as_str())
                            .filter(|s| !s.is_empty())
                            .map(str::to_string),
                        confidence: entry
                            .get("confidence")
                            .and_then(|c| c.as_f64())
                            .unwrap_or(0.5)
                            .clamp(0.0, 1.0),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Splits a model response into `(code, trailing JSON)`.
///
/// Returns `(None, None)` when no fenced code block is present so the
/// caller can decide whether to treat the whole response as code or to
/// fail. `tail` is `None` when nothing JSON-shaped follows the block.
fn split_code_and_tail(content: &str) -> (Option<String>, Option<JsonValue>) {
    let Some(code_start) = content.find("```") else {
        return (None, None);
    };
    let after_open = &content[code_start + 3..];
    let line_end = after_open.find('\n').unwrap_or(0);
    let body_start = code_start + 3 + line_end + if after_open.is_empty() { 0 } else { 1 };
    let body = &content[body_start..];
    let Some(close_offset) = body.find("```") else {
        return (None, None);
    };
    let code = body[..close_offset].trim_end_matches('\n').to_string();
    let tail_raw = body[close_offset + 3..].trim();
    let tail = if tail_raw.is_empty() {
        None
    } else {
        extract_first_json_object(tail_raw)
    };
    (Some(code), tail)
}

fn parse_json_object(content: &str) -> Result<JsonValue, AiError> {
    let trimmed = content.trim();
    if let Ok(v) = serde_json::from_str::<JsonValue>(trimmed) {
        if v.is_object() {
            return Ok(v);
        }
    }
    // Unwrap a fenced block if present.
    if let (Some(code), _) = split_code_and_tail(content) {
        if let Ok(v) = serde_json::from_str::<JsonValue>(code.trim()) {
            if v.is_object() {
                return Ok(v);
            }
        }
    }
    // Last resort: find the first balanced {...} slice.
    extract_first_json_object(trimmed).ok_or_else(|| {
        AiError::InvalidResponse("response did not contain a JSON object".into())
    })
}

fn extract_first_json_object(s: &str) -> Option<JsonValue> {
    let bytes = s.as_bytes();
    let start = bytes.iter().position(|&b| b == b'{')?;
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escape = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                in_string = false;
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    let slice = &s[start..=i];
                    return serde_json::from_str(slice).ok();
                }
            }
            _ => {}
        }
    }
    None
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::{DecisionRef, SelectOption};
    use bock_air::{NodeIdGen, NodeKind};
    use bock_errors::Span;
    use bock_types::Strictness;

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
        let mut caps = HashMap::new();
        caps.insert("memory_model".into(), "GC".into());
        caps.insert("async".into(), "promises".into());
        let mut conv = HashMap::new();
        conv.insert("naming".into(), "camelCase".into());
        TargetProfile {
            id: "js".into(),
            display_name: "JavaScript".into(),
            capabilities: caps,
            conventions: conv,
        }
    }

    fn local_config(endpoint: &str, model: &str) -> AiConfig {
        AiConfig {
            provider: "openai-compatible".into(),
            endpoint: endpoint.into(),
            model: model.into(),
            api_key_env: None,
            max_retries: 0,
            timeout_seconds: 5,
            ..AiConfig::default()
        }
    }

    // ── Construction ────────────────────────────────────────────────────

    #[test]
    fn new_rejects_non_local_endpoint_without_api_key() {
        let cfg = AiConfig {
            provider: "openai-compatible".into(),
            endpoint: "https://api.example.com/v1".into(),
            model: "gpt-4o".into(),
            api_key_env: Some("__BOCK_AI_TEST_DEFINITELY_UNSET__".into()),
            ..AiConfig::default()
        };
        let err = OpenAiCompatProvider::new(cfg).unwrap_err();
        assert!(matches!(err, AiError::Auth(_)));
    }

    #[test]
    fn new_accepts_local_endpoint_without_key() {
        let cfg = local_config("http://localhost:11434/v1", "llama3");
        let p = OpenAiCompatProvider::new(cfg).expect("local endpoint ok");
        assert_eq!(p.model_id(), "openai-compatible:llama3");
    }

    #[test]
    fn new_accepts_api_key_from_env() {
        std::env::set_var("__BOCK_AI_TEST_KEY_OK__", "sk-fake");
        let cfg = AiConfig {
            provider: "openai-compatible".into(),
            endpoint: "https://api.example.com/v1".into(),
            model: "gpt-4o".into(),
            api_key_env: Some("__BOCK_AI_TEST_KEY_OK__".into()),
            ..AiConfig::default()
        };
        let p = OpenAiCompatProvider::new(cfg).expect("api key loaded");
        assert_eq!(p.model_id(), "openai-compatible:gpt-4o");
        std::env::remove_var("__BOCK_AI_TEST_KEY_OK__");
    }

    #[test]
    fn chat_url_appends_endpoint() {
        let p = OpenAiCompatProvider::new(local_config("http://localhost:8080/v1", "m"))
            .unwrap();
        assert_eq!(p.chat_url(), "http://localhost:8080/v1/chat/completions");

        let p2 = OpenAiCompatProvider::new(local_config("http://127.0.0.1:8080/v1/", "m"))
            .unwrap();
        assert_eq!(p2.chat_url(), "http://127.0.0.1:8080/v1/chat/completions");
    }

    // ── Prompt construction ─────────────────────────────────────────────

    #[test]
    fn generate_prompt_is_deterministic() {
        let req = GenerateRequest {
            node: dummy_node(),
            target: dummy_target(),
            module_context: ModuleContext {
                module_path: "src/m.bock".into(),
                imports: vec!["log.Debug".into(), "log.Info".into()],
                siblings: vec!["other_fn".into()],
                annotations: vec!["@domain(net)".into()],
            },
            prior_decisions: vec![DecisionRef {
                decision: "async_runtime".into(),
                choice: "tokio".into(),
            }],
            strictness: Strictness::Development,
        };
        let (sys_a, user_a) = build_generate_messages(&req);
        let (sys_b, user_b) = build_generate_messages(&req);
        assert_eq!(sys_a, sys_b);
        assert_eq!(user_a, user_b);
        assert!(user_a.contains("Target: js (JavaScript)"));
        assert!(user_a.contains("memory_model: GC"));
        assert!(user_a.contains("Strictness: Development"));
        assert!(user_a.contains("async_runtime = tokio"));
        assert!(user_a.contains("AIR node:"));
    }

    #[test]
    fn repair_prompt_includes_original_and_error() {
        let req = RepairRequest {
            original_code: "let x = ;".into(),
            compiler_error: "unexpected token".into(),
            node: dummy_node(),
            target: dummy_target(),
        };
        let (sys, user) = build_repair_messages(&req);
        assert!(sys.contains("repair backend"));
        assert!(user.contains("let x = ;"));
        assert!(user.contains("unexpected token"));
    }

    #[test]
    fn optimize_prompt_lists_hints() {
        let req = OptimizeRequest {
            working_code: "return 1;".into(),
            node: dummy_node(),
            target: dummy_target(),
            optimization_hints: vec![
                OptimizationHint::Performance,
                OptimizationHint::Custom("inline".into()),
            ],
        };
        let (_sys, user) = build_optimize_messages(&req);
        assert!(user.contains("performance"));
        assert!(user.contains("custom: inline"));
        assert!(user.contains("return 1;"));
    }

    #[test]
    fn select_prompt_lists_options() {
        let req = SelectRequest {
            options: vec![
                SelectOption {
                    id: "retry".into(),
                    description: "retry with backoff".into(),
                },
                SelectOption {
                    id: "fallback".into(),
                    description: "use deterministic fallback".into(),
                },
            ],
            context: SelectContext {
                error: Some("503 Service Unavailable".into()),
                ..SelectContext::default()
            },
            rationale_prompt: "Which recovery?".into(),
        };
        let (_sys, user) = build_select_messages(&req);
        assert!(user.contains("[retry] retry with backoff"));
        assert!(user.contains("[fallback]"));
        assert!(user.contains("503"));
        assert!(user.contains("Which recovery?"));
    }

    // ── Response parsing ────────────────────────────────────────────────

    #[test]
    fn generate_parses_code_and_tail() {
        let content = "Sure!\n```javascript\nfunction f(){return 1;}\n```\n\
            {\"confidence\": 0.82, \"reasoning\": \"idiomatic\", \
             \"alternatives\": [{\"label\": \"arrow\", \"confidence\": 0.4}]}";
        let r = parse_generate_content(content).expect("parsed");
        assert!(r.code.contains("function f"));
        assert!((r.confidence - 0.82).abs() < 1e-9);
        assert_eq!(r.reasoning.as_deref(), Some("idiomatic"));
        assert_eq!(r.alternatives.len(), 1);
        assert_eq!(r.alternatives[0].label, "arrow");
    }

    #[test]
    fn generate_defaults_when_tail_missing() {
        let content = "```js\nconst x = 1;\n```";
        let r = parse_generate_content(content).expect("parsed");
        assert_eq!(r.code, "const x = 1;");
        assert!((r.confidence - 0.5).abs() < 1e-9);
        assert!(r.reasoning.is_none());
        assert!(r.alternatives.is_empty());
    }

    #[test]
    fn generate_rejects_response_without_code_block() {
        let content = "I couldn't produce code.";
        let err = parse_generate_content(content).unwrap_err();
        assert!(matches!(err, AiError::InvalidResponse(_)));
    }

    #[test]
    fn repair_parses_fields() {
        let content = "{\"fixed_code\": \"let x = 1;\", \"confidence\": 0.91, \
            \"reasoning\": \"added rhs\", \"rule_pattern\": null}";
        let r = parse_repair_content(content).expect("parsed");
        assert_eq!(r.fixed_code, "let x = 1;");
        assert!((r.confidence - 0.91).abs() < 1e-9);
        assert_eq!(r.reasoning.as_deref(), Some("added rhs"));
        assert!(r.candidate_rule.is_none());
    }

    #[test]
    fn repair_parses_candidate_rule() {
        let content = r#"{
            "fixed_code": "x",
            "confidence": 0.7,
            "rule_pattern": {
                "target_id": "js",
                "pattern": "Block{}",
                "template": "{}",
                "priority": 5
            }
        }"#;
        let r = parse_repair_content(content).expect("parsed");
        let rule = r.candidate_rule.expect("rule present");
        assert_eq!(rule.target_id, "js");
        assert_eq!(rule.priority, 5);
    }

    #[test]
    fn repair_accepts_fenced_json() {
        let content = "```json\n{\"fixed_code\": \"ok\", \"confidence\": 0.6}\n```";
        let r = parse_repair_content(content).expect("parsed");
        assert_eq!(r.fixed_code, "ok");
    }

    #[test]
    fn repair_rejects_missing_fixed_code() {
        let content = "{\"confidence\": 0.5}";
        let err = parse_repair_content(content).unwrap_err();
        assert!(matches!(err, AiError::InvalidResponse(_)));
    }

    #[test]
    fn optimize_parses_improvements() {
        let content = "```\nreturn 1\n```\n{\"confidence\": 0.8, \
            \"improvements\": [\"removed semicolon\", \"inlined var\"]}";
        let r = parse_optimize_content(content).expect("parsed");
        assert_eq!(r.optimized_code, "return 1");
        assert_eq!(r.improvements.len(), 2);
        assert_eq!(r.improvements[0], "removed semicolon");
    }

    #[test]
    fn select_parses_fields() {
        let content = "{\"selected_id\": \"retry\", \"confidence\": 0.72, \
            \"reasoning\": \"transient\"}";
        let r = parse_select_content(content).expect("parsed");
        assert_eq!(r.selected_id, "retry");
        assert!((r.confidence - 0.72).abs() < 1e-9);
    }

    #[test]
    fn select_response_with_extra_prose() {
        let content =
            "Here's my pick:\n{\"selected_id\": \"fallback\", \"confidence\": 0.9}\nThanks!";
        let r = parse_select_content(content).expect("parsed");
        assert_eq!(r.selected_id, "fallback");
    }

    #[test]
    fn select_rejects_malformed_json() {
        let content = "not even close to JSON";
        let err = parse_select_content(content).unwrap_err();
        assert!(matches!(err, AiError::InvalidResponse(_)));
    }

    #[test]
    fn confidence_clamped_to_unit_interval() {
        let content =
            "```\nx\n```\n{\"confidence\": 1.5, \"reasoning\": \"overeager\"}";
        let r = parse_generate_content(content).expect("parsed");
        assert!((r.confidence - 1.0).abs() < 1e-9);
    }

    // ── Closed-set validation in select() ───────────────────────────────

    #[tokio::test]
    async fn select_via_provider_rejects_out_of_set_id() {
        // Use a local endpoint so we can build the provider, then
        // exercise validate-after-parse by hand via a fake response path.
        // (We can't hit chat() here without network, but parse_select_content
        // + validate_select_response is the composed pipeline.)
        let options = vec![
            SelectOption {
                id: "retry".into(),
                description: "retry".into(),
            },
            SelectOption {
                id: "fallback".into(),
                description: "fallback".into(),
            },
        ];
        let resp = parse_select_content(
            "{\"selected_id\": \"escalate\", \"confidence\": 0.9}",
        )
        .unwrap();
        let err = validate_select_response(&options, &resp).unwrap_err();
        assert!(matches!(err, AiError::InvalidResponse(_)));
    }

    // ── Error mapping ───────────────────────────────────────────────────

    #[test]
    fn map_http_status_covers_each_branch() {
        assert!(matches!(
            map_http_status(StatusCode::UNAUTHORIZED, "nope"),
            AiError::Auth(_)
        ));
        assert!(matches!(
            map_http_status(StatusCode::FORBIDDEN, "nope"),
            AiError::Auth(_)
        ));
        assert!(matches!(
            map_http_status(StatusCode::TOO_MANY_REQUESTS, "slow"),
            AiError::RateLimited(_)
        ));
        assert!(matches!(
            map_http_status(StatusCode::INTERNAL_SERVER_ERROR, "boom"),
            AiError::ProviderError(_)
        ));
        assert!(matches!(
            map_http_status(StatusCode::BAD_REQUEST, "?"),
            AiError::ProviderError(_)
        ));
    }

    #[test]
    fn retryable_classification() {
        assert!(is_retryable(&AiError::Network("x".into())));
        assert!(is_retryable(&AiError::RateLimited("429".into())));
        assert!(!is_retryable(&AiError::Auth("401".into())));
        assert!(!is_retryable(&AiError::Timeout("slow".into())));
        assert!(!is_retryable(&AiError::InvalidResponse("nope".into())));
    }

    #[test]
    fn backoff_is_exponential_and_capped() {
        assert_eq!(backoff_ms(0), 100);
        assert_eq!(backoff_ms(1), 200);
        assert_eq!(backoff_ms(2), 400);
        // Capped at attempt == 6.
        assert_eq!(backoff_ms(6), 100 * 64);
        assert_eq!(backoff_ms(100), 100 * 64);
    }

    #[test]
    fn local_endpoint_detector() {
        assert!(is_local_endpoint("http://localhost:11434"));
        assert!(is_local_endpoint("http://127.0.0.1:8000/v1"));
        assert!(is_local_endpoint("http://0.0.0.0:1234"));
        assert!(is_local_endpoint("http://[::1]:1234"));
        assert!(!is_local_endpoint("https://api.openai.com/v1"));
    }

    #[test]
    fn extract_message_content_reads_path() {
        let resp = json!({
            "choices": [
                {"message": {"content": "hello"}}
            ]
        });
        assert_eq!(extract_message_content(&resp).unwrap(), "hello");
    }

    #[test]
    fn extract_message_content_errors_when_missing() {
        let resp = json!({"choices": []});
        assert!(matches!(
            extract_message_content(&resp).unwrap_err(),
            AiError::InvalidResponse(_)
        ));
    }
}
