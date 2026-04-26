//! Anthropic Messages API native provider (§17.8).
//!
//! Unlike [`super::openai_compat::OpenAiCompatProvider`], this provider
//! targets features the OpenAI Chat Completions protocol cannot
//! express cleanly:
//!
//! - a top-level `system` field (not a role-tagged message),
//! - structured `content` arrays (`text`, `thinking`, `tool_use`),
//! - the `x-api-key` / `anthropic-version` header pair,
//! - extended thinking (reasoning surfaced as a dedicated content block),
//! - `tool_use` with an enum-constrained input schema for Select mode,
//!   which enforces the closed-set constraint of §10.8 at the schema
//!   layer rather than through post-hoc text parsing.
//!
//! Retries, auth loading, and error mapping mirror the OpenAI-compatible
//! provider — the transport semantics are the same, only the wire
//! protocol differs.

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

/// Anthropic Messages API version header value. Pinned per §17.8 —
/// bumping this is a provider-level decision, not per-call.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Default upper bound on generated tokens per call. The Messages API
/// requires `max_tokens`; we pick a value large enough for compiler
/// output without being wasteful.
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// Name of the tool the Select mode uses to force a structured response.
const SELECT_TOOL_NAME: &str = "select_option";

/// Native Anthropic Messages API provider.
///
/// Construct with [`Self::new`] and dispatch via [`AiProvider`].
#[derive(Debug)]
pub struct AnthropicProvider {
    config: AiConfig,
    client: Client,
    api_key: String,
}

impl AnthropicProvider {
    /// Creates a provider from an [`AiConfig`].
    ///
    /// Unlike [`super::OpenAiCompatProvider::new`], the Anthropic API is
    /// always remote — there is no local-endpoint escape hatch. A missing
    /// API key therefore always returns [`AiError::Auth`].
    ///
    /// # Errors
    /// - [`AiError::Auth`] — `config.api_key_env` is unset, names no
    ///   environment variable, or that variable is empty.
    /// - [`AiError::ProviderError`] — the HTTP client could not be built.
    pub fn new(config: AiConfig) -> Result<Self, AiError> {
        let api_key = config
            .api_key_env
            .as_deref()
            .and_then(|name| std::env::var(name).ok())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                AiError::Auth(format!(
                    "no API key loaded (env var {:?} unset or empty)",
                    config.api_key_env,
                ))
            })?;

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

    fn messages_url(&self) -> String {
        let base = self.config.endpoint.trim_end_matches('/');
        format!("{base}/messages")
    }

    async fn send(&self, body: &JsonValue) -> Result<JsonValue, AiError> {
        let mut attempt: u32 = 0;
        loop {
            match self.call_once(body).await {
                Ok(v) => return Ok(v),
                Err(e) if attempt < self.config.max_retries && is_retryable(&e) => {
                    let delay = StdDuration::from_millis(backoff_ms(attempt));
                    tokio::time::sleep(delay).await;
                    attempt += 1;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn call_once(&self, body: &JsonValue) -> Result<JsonValue, AiError> {
        let resp = self
            .client
            .post(self.messages_url())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(body)
            .send()
            .await
            .map_err(map_send_error)?;

        let status = resp.status();
        if status.is_success() {
            resp.json::<JsonValue>().await.map_err(|e| {
                AiError::InvalidResponse(format!("response body not JSON: {e}"))
            })
        } else {
            let body_text = resp.text().await.unwrap_or_default();
            Err(map_http_status(status, &body_text))
        }
    }

    fn base_body(&self, system: &str, user: &str) -> JsonValue {
        json!({
            "model": self.config.model,
            "max_tokens": DEFAULT_MAX_TOKENS,
            "system": system,
            "messages": [
                {"role": "user", "content": user},
            ],
        })
    }
}

#[async_trait]
impl AiProvider for AnthropicProvider {
    async fn generate(
        &self,
        request: &GenerateRequest,
    ) -> Result<GenerateResponse, AiError> {
        let (system, user) = build_generate_messages(request);
        let value = self.send(&self.base_body(&system, &user)).await?;
        let blocks = extract_content_blocks(&value)?;
        parse_generate_blocks(&blocks)
    }

    async fn repair(&self, request: &RepairRequest) -> Result<RepairResponse, AiError> {
        let (system, user) = build_repair_messages(request);
        let value = self.send(&self.base_body(&system, &user)).await?;
        let blocks = extract_content_blocks(&value)?;
        parse_repair_blocks(&blocks)
    }

    async fn optimize(
        &self,
        request: &OptimizeRequest,
    ) -> Result<OptimizeResponse, AiError> {
        let (system, user) = build_optimize_messages(request);
        let value = self.send(&self.base_body(&system, &user)).await?;
        let blocks = extract_content_blocks(&value)?;
        parse_optimize_blocks(&blocks)
    }

    async fn select(&self, request: &SelectRequest) -> Result<SelectResponse, AiError> {
        let (system, user) = build_select_messages(request);
        let tool = select_tool_schema(&request.options);
        let body = json!({
            "model": self.config.model,
            "max_tokens": DEFAULT_MAX_TOKENS,
            "system": system,
            "messages": [
                {"role": "user", "content": user},
            ],
            "tools": [tool],
            "tool_choice": {"type": "tool", "name": SELECT_TOOL_NAME},
        });
        let value = self.send(&body).await?;
        let blocks = extract_content_blocks(&value)?;
        let response = parse_select_blocks(&blocks)?;
        validate_select_response(&request.options, &response)?;
        Ok(response)
    }

    fn model_id(&self) -> String {
        format!("anthropic:{}", self.config.model)
    }
}

// ─── Retry / error mapping helpers ───────────────────────────────────────────

fn is_retryable(e: &AiError) -> bool {
    matches!(e, AiError::Network(_) | AiError::RateLimited(_))
}

fn backoff_ms(attempt: u32) -> u64 {
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

// ─── Prompt construction ─────────────────────────────────────────────────────

const GENERATE_SYSTEM: &str = "\
You are the code-generation backend of the Bock language compiler. \
Translate the given Bock AIR (Bock Intermediate Representation) node into idiomatic target-language code that:
- preserves Bock semantics exactly,
- follows the conventions listed for the target,
- stays consistent with any prior decisions provided.

Emit your response as two content blocks:
  1. A single markdown fenced code block containing ONLY the target-language code.
  2. A short JSON object (in a second text block or after the code fence) with fields:
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

Emit your response as:
  1. A single markdown fenced code block containing the rewritten code.
  2. A JSON object with fields:
       \"confidence\":   number in [0.0, 1.0]
       \"improvements\": array of short strings describing each change (optional)
       \"reasoning\":    short string (optional)";

const SELECT_SYSTEM: &str = "\
You are a classifier. You MUST pick exactly one id from the provided closed \
set of options — inventing a new id is a hard error. Base your choice on \
the context and the rationale prompt supplied by the caller.

Call the `select_option` tool with the chosen id, your confidence, and a \
short rationale. Do not reply with free-form text.";

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
    format!("{node:#?}")
}

/// JSON schema fragment for the `select_option` tool's `input_schema`.
///
/// The `selected_id` field is an `enum` over exactly the offered option
/// ids; Anthropic's constrained sampling uses this to prevent the model
/// from returning an id outside the closed set. We still call
/// [`validate_select_response`] afterward as belt-and-suspenders.
fn select_tool_schema(options: &[crate::request::SelectOption]) -> JsonValue {
    let ids: Vec<&str> = options.iter().map(|o| o.id.as_str()).collect();
    json!({
        "name": SELECT_TOOL_NAME,
        "description": "Pick exactly one option id from the closed set.",
        "input_schema": {
            "type": "object",
            "properties": {
                "selected_id": {
                    "type": "string",
                    "enum": ids,
                    "description": "Chosen option id (must match one of the provided options)."
                },
                "confidence": {
                    "type": "number",
                    "description": "Confidence in the selection, in [0.0, 1.0]."
                },
                "reasoning": {
                    "type": "string",
                    "description": "Short rationale for the choice."
                }
            },
            "required": ["selected_id", "confidence"]
        }
    })
}

// ─── Response parsing ────────────────────────────────────────────────────────

/// Parsed view of the `content` array on a Messages API response.
///
/// Anthropic's response is `{ "content": [ {...}, {...}, ... ] }` where
/// each block has a discriminating `type`. We collect the kinds the
/// four modes care about; unknown kinds are silently dropped.
#[derive(Debug, Default)]
struct ContentBlocks {
    /// Concatenated text across all `text` blocks in order.
    text: String,
    /// Concatenated thinking across all `thinking` blocks in order.
    thinking: String,
    /// `input` payload of the first `tool_use` block, if any.
    tool_use_input: Option<JsonValue>,
}

fn extract_content_blocks(resp: &JsonValue) -> Result<ContentBlocks, AiError> {
    let arr = resp.get("content").and_then(|c| c.as_array()).ok_or_else(|| {
        AiError::InvalidResponse("response missing content array".into())
    })?;

    let mut blocks = ContentBlocks::default();
    for block in arr {
        let kind = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match kind {
            "text" => {
                if let Some(s) = block.get("text").and_then(|t| t.as_str()) {
                    if !blocks.text.is_empty() {
                        blocks.text.push('\n');
                    }
                    blocks.text.push_str(s);
                }
            }
            "thinking" => {
                if let Some(s) = block.get("thinking").and_then(|t| t.as_str()) {
                    if !blocks.thinking.is_empty() {
                        blocks.thinking.push('\n');
                    }
                    blocks.thinking.push_str(s);
                }
            }
            "tool_use" => {
                if blocks.tool_use_input.is_none() {
                    blocks.tool_use_input = block.get("input").cloned();
                }
            }
            _ => {}
        }
    }
    Ok(blocks)
}

fn parse_generate_blocks(blocks: &ContentBlocks) -> Result<GenerateResponse, AiError> {
    let (code, tail) = split_code_and_tail(&blocks.text);
    let code = code.ok_or_else(|| {
        AiError::InvalidResponse(
            "generate response missing markdown code block".into(),
        )
    })?;
    let confidence = extract_confidence(tail.as_ref());
    let reasoning = combine_reasoning(&blocks.thinking, tail.as_ref());
    let alternatives = extract_alternatives(tail.as_ref());
    Ok(GenerateResponse {
        code,
        confidence,
        reasoning,
        alternatives,
    })
}

fn parse_optimize_blocks(blocks: &ContentBlocks) -> Result<OptimizeResponse, AiError> {
    let (code, tail) = split_code_and_tail(&blocks.text);
    let code = code.ok_or_else(|| {
        AiError::InvalidResponse(
            "optimize response missing markdown code block".into(),
        )
    })?;
    let confidence = extract_confidence(tail.as_ref());
    let reasoning = combine_reasoning(&blocks.thinking, tail.as_ref());
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

fn parse_repair_blocks(blocks: &ContentBlocks) -> Result<RepairResponse, AiError> {
    let json = parse_json_object(&blocks.text)?;
    let fixed_code = json
        .get("fixed_code")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            AiError::InvalidResponse("repair response missing 'fixed_code'".into())
        })?
        .to_string();
    let confidence = extract_confidence(Some(&json));
    let reasoning = combine_reasoning(&blocks.thinking, Some(&json));
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

fn parse_select_blocks(blocks: &ContentBlocks) -> Result<SelectResponse, AiError> {
    let input = blocks.tool_use_input.as_ref().ok_or_else(|| {
        AiError::InvalidResponse(
            "select response missing tool_use block with select_option input".into(),
        )
    })?;

    let selected_id = input
        .get("selected_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            AiError::InvalidResponse("select tool_use missing 'selected_id'".into())
        })?
        .to_string();
    let confidence = extract_confidence(Some(input));
    let reasoning = combine_reasoning(&blocks.thinking, Some(input));
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

/// Merges extended-thinking output with whatever `reasoning` field the
/// model emitted in its JSON tail / tool input. Both sources are
/// optional — only populate the result when at least one is present.
fn combine_reasoning(thinking: &str, tail: Option<&JsonValue>) -> Option<String> {
    let json_reasoning = extract_string(tail, "reasoning");
    let thinking = thinking.trim();
    match (thinking.is_empty(), json_reasoning) {
        (true, None) => None,
        (true, Some(r)) => Some(r),
        (false, None) => Some(thinking.to_string()),
        (false, Some(r)) => Some(format!("{thinking}\n\n{r}")),
    }
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
    if let (Some(code), _) = split_code_and_tail(content) {
        if let Ok(v) = serde_json::from_str::<JsonValue>(code.trim()) {
            if v.is_object() {
                return Ok(v);
            }
        }
    }
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
        let mut conv = HashMap::new();
        conv.insert("naming".into(), "camelCase".into());
        TargetProfile {
            id: "js".into(),
            display_name: "JavaScript".into(),
            capabilities: caps,
            conventions: conv,
        }
    }

    fn remote_config(model: &str, env_var: &str) -> AiConfig {
        AiConfig {
            provider: "anthropic".into(),
            endpoint: "https://api.anthropic.com/v1".into(),
            model: model.into(),
            api_key_env: Some(env_var.into()),
            max_retries: 0,
            timeout_seconds: 5,
            ..AiConfig::default()
        }
    }

    // ── Construction ────────────────────────────────────────────────────

    #[test]
    fn new_rejects_missing_api_key() {
        let cfg = remote_config(
            "claude-opus-4-7",
            "__BOCK_AI_ANTHROPIC_DEFINITELY_UNSET__",
        );
        let err = AnthropicProvider::new(cfg).unwrap_err();
        assert!(matches!(err, AiError::Auth(_)));
    }

    #[test]
    fn new_rejects_missing_env_name() {
        let cfg = AiConfig {
            provider: "anthropic".into(),
            endpoint: "https://api.anthropic.com/v1".into(),
            model: "claude-opus-4-7".into(),
            api_key_env: None,
            ..AiConfig::default()
        };
        let err = AnthropicProvider::new(cfg).unwrap_err();
        assert!(matches!(err, AiError::Auth(_)));
    }

    #[test]
    fn new_accepts_api_key_from_env() {
        std::env::set_var("__BOCK_AI_ANTHROPIC_KEY_OK__", "sk-ant-fake");
        let cfg = remote_config("claude-opus-4-7", "__BOCK_AI_ANTHROPIC_KEY_OK__");
        let p = AnthropicProvider::new(cfg).expect("api key loaded");
        assert_eq!(p.model_id(), "anthropic:claude-opus-4-7");
        std::env::remove_var("__BOCK_AI_ANTHROPIC_KEY_OK__");
    }

    #[test]
    fn messages_url_appends_endpoint() {
        std::env::set_var("__BOCK_AI_ANTHROPIC_URL_OK__", "sk-ant");
        let p = AnthropicProvider::new(AiConfig {
            provider: "anthropic".into(),
            endpoint: "https://api.anthropic.com/v1/".into(),
            model: "claude-opus-4-7".into(),
            api_key_env: Some("__BOCK_AI_ANTHROPIC_URL_OK__".into()),
            ..AiConfig::default()
        })
        .unwrap();
        assert_eq!(p.messages_url(), "https://api.anthropic.com/v1/messages");
        std::env::remove_var("__BOCK_AI_ANTHROPIC_URL_OK__");
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
        let (sys, user) = build_select_messages(&req);
        assert!(sys.contains("select_option"));
        assert!(user.contains("[retry] retry with backoff"));
        assert!(user.contains("[fallback]"));
        assert!(user.contains("503"));
        assert!(user.contains("Which recovery?"));
    }

    // ── Select tool schema ─────────────────────────────────────────────

    #[test]
    fn select_tool_schema_enumerates_option_ids() {
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
        let schema = select_tool_schema(&options);
        assert_eq!(schema.get("name").and_then(|v| v.as_str()), Some(SELECT_TOOL_NAME));

        let enum_vals = schema
            .pointer("/input_schema/properties/selected_id/enum")
            .and_then(|v| v.as_array())
            .expect("enum present");
        let ids: Vec<&str> = enum_vals.iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(ids, vec!["retry", "fallback"]);

        let required = schema
            .pointer("/input_schema/required")
            .and_then(|v| v.as_array())
            .expect("required present");
        assert!(required
            .iter()
            .any(|v| v.as_str() == Some("selected_id")));
    }

    // ── Response parsing: content blocks ───────────────────────────────

    #[test]
    fn extract_blocks_separates_text_and_thinking() {
        let resp = json!({
            "content": [
                {"type": "thinking", "thinking": "step 1\nstep 2"},
                {"type": "text", "text": "```js\nconst x=1;\n```\n{\"confidence\": 0.7}"},
            ]
        });
        let blocks = extract_content_blocks(&resp).unwrap();
        assert!(blocks.thinking.contains("step 1"));
        assert!(blocks.text.contains("const x=1"));
        assert!(blocks.tool_use_input.is_none());
    }

    #[test]
    fn extract_blocks_captures_tool_use_input() {
        let resp = json!({
            "content": [
                {"type": "tool_use", "id": "t1", "name": SELECT_TOOL_NAME,
                 "input": {"selected_id": "retry", "confidence": 0.9}},
            ]
        });
        let blocks = extract_content_blocks(&resp).unwrap();
        let input = blocks.tool_use_input.expect("tool_use captured");
        assert_eq!(input["selected_id"], "retry");
    }

    #[test]
    fn extract_blocks_rejects_missing_content() {
        let resp = json!({"id": "msg_x"});
        let err = extract_content_blocks(&resp).unwrap_err();
        assert!(matches!(err, AiError::InvalidResponse(_)));
    }

    // ── Response parsing: generate ─────────────────────────────────────

    #[test]
    fn generate_parses_code_and_tail_with_thinking() {
        let blocks = ContentBlocks {
            text: "```javascript\nfunction f(){return 1;}\n```\n\
                   {\"confidence\": 0.82, \"reasoning\": \"idiomatic\", \
                    \"alternatives\": [{\"label\": \"arrow\", \"confidence\": 0.4}]}"
                .into(),
            thinking: "weighing options".into(),
            tool_use_input: None,
        };
        let r = parse_generate_blocks(&blocks).expect("parsed");
        assert!(r.code.contains("function f"));
        assert!((r.confidence - 0.82).abs() < 1e-9);
        let reasoning = r.reasoning.unwrap();
        assert!(reasoning.contains("weighing options"));
        assert!(reasoning.contains("idiomatic"));
        assert_eq!(r.alternatives.len(), 1);
    }

    #[test]
    fn generate_defaults_when_tail_missing() {
        let blocks = ContentBlocks {
            text: "```js\nconst x = 1;\n```".into(),
            thinking: String::new(),
            tool_use_input: None,
        };
        let r = parse_generate_blocks(&blocks).expect("parsed");
        assert_eq!(r.code, "const x = 1;");
        assert!((r.confidence - 0.5).abs() < 1e-9);
        assert!(r.reasoning.is_none());
    }

    #[test]
    fn generate_rejects_response_without_code_block() {
        let blocks = ContentBlocks {
            text: "I couldn't produce code.".into(),
            ..ContentBlocks::default()
        };
        let err = parse_generate_blocks(&blocks).unwrap_err();
        assert!(matches!(err, AiError::InvalidResponse(_)));
    }

    #[test]
    fn generate_surfaces_thinking_only_when_tail_absent() {
        let blocks = ContentBlocks {
            text: "```\nok\n```".into(),
            thinking: "reasoned about edge case".into(),
            tool_use_input: None,
        };
        let r = parse_generate_blocks(&blocks).expect("parsed");
        assert_eq!(r.reasoning.as_deref(), Some("reasoned about edge case"));
    }

    // ── Response parsing: repair ──────────────────────────────────────

    #[test]
    fn repair_parses_fields_and_rule() {
        let blocks = ContentBlocks {
            text: r#"{
                "fixed_code": "let x = 1;",
                "confidence": 0.91,
                "reasoning": "added rhs",
                "rule_pattern": {
                    "target_id": "js",
                    "pattern": "Block{}",
                    "template": "{}",
                    "priority": 5
                }
            }"#
            .into(),
            ..ContentBlocks::default()
        };
        let r = parse_repair_blocks(&blocks).expect("parsed");
        assert_eq!(r.fixed_code, "let x = 1;");
        assert_eq!(r.reasoning.as_deref(), Some("added rhs"));
        let rule = r.candidate_rule.expect("rule present");
        assert_eq!(rule.priority, 5);
    }

    #[test]
    fn repair_rejects_missing_fixed_code() {
        let blocks = ContentBlocks {
            text: r#"{"confidence": 0.5}"#.into(),
            ..ContentBlocks::default()
        };
        let err = parse_repair_blocks(&blocks).unwrap_err();
        assert!(matches!(err, AiError::InvalidResponse(_)));
    }

    // ── Response parsing: optimize ────────────────────────────────────

    #[test]
    fn optimize_parses_improvements() {
        let blocks = ContentBlocks {
            text: "```\nreturn 1\n```\n\
                   {\"confidence\": 0.8, \
                    \"improvements\": [\"removed semicolon\", \"inlined var\"]}"
                .into(),
            ..ContentBlocks::default()
        };
        let r = parse_optimize_blocks(&blocks).expect("parsed");
        assert_eq!(r.optimized_code, "return 1");
        assert_eq!(r.improvements.len(), 2);
    }

    // ── Response parsing: select via tool_use ─────────────────────────

    #[test]
    fn select_parses_tool_use_input() {
        let blocks = ContentBlocks {
            text: String::new(),
            thinking: String::new(),
            tool_use_input: Some(json!({
                "selected_id": "retry",
                "confidence": 0.72,
                "reasoning": "transient failure"
            })),
        };
        let r = parse_select_blocks(&blocks).expect("parsed");
        assert_eq!(r.selected_id, "retry");
        assert!((r.confidence - 0.72).abs() < 1e-9);
        assert_eq!(r.reasoning.as_deref(), Some("transient failure"));
    }

    #[test]
    fn select_rejects_missing_tool_use() {
        let blocks = ContentBlocks {
            text: "I think retry".into(),
            ..ContentBlocks::default()
        };
        let err = parse_select_blocks(&blocks).unwrap_err();
        assert!(matches!(err, AiError::InvalidResponse(_)));
    }

    #[test]
    fn select_rejects_tool_use_missing_selected_id() {
        let blocks = ContentBlocks {
            tool_use_input: Some(json!({"confidence": 0.9})),
            ..ContentBlocks::default()
        };
        let err = parse_select_blocks(&blocks).unwrap_err();
        assert!(matches!(err, AiError::InvalidResponse(_)));
    }

    // ── Closed-set validation at the pipeline level ───────────────────

    #[test]
    fn select_pipeline_validates_closed_set() {
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
        let blocks = ContentBlocks {
            tool_use_input: Some(json!({"selected_id": "escalate", "confidence": 0.9})),
            ..ContentBlocks::default()
        };
        let resp = parse_select_blocks(&blocks).expect("parses structurally");
        let err = validate_select_response(&options, &resp).unwrap_err();
        assert!(matches!(err, AiError::InvalidResponse(_)));
    }

    // ── Confidence clamping & reasoning combination ───────────────────

    #[test]
    fn confidence_clamped_to_unit_interval() {
        let blocks = ContentBlocks {
            text: "```\nx\n```\n{\"confidence\": 1.5, \"reasoning\": \"overeager\"}".into(),
            ..ContentBlocks::default()
        };
        let r = parse_generate_blocks(&blocks).expect("parsed");
        assert!((r.confidence - 1.0).abs() < 1e-9);
    }

    #[test]
    fn combine_reasoning_handles_all_source_combinations() {
        assert!(combine_reasoning("", None).is_none());
        assert_eq!(
            combine_reasoning("  ", None),
            None,
            "whitespace-only thinking drops"
        );
        assert_eq!(
            combine_reasoning("", Some(&json!({"reasoning": "json"}))).as_deref(),
            Some("json")
        );
        assert_eq!(
            combine_reasoning("think", None).as_deref(),
            Some("think")
        );
        let merged =
            combine_reasoning("think", Some(&json!({"reasoning": "json"}))).unwrap();
        assert!(merged.contains("think"));
        assert!(merged.contains("json"));
    }

    // ── Error mapping ─────────────────────────────────────────────────

    #[test]
    fn map_http_status_covers_each_branch() {
        assert!(matches!(
            map_http_status(StatusCode::UNAUTHORIZED, "nope"),
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
    }

    #[test]
    fn backoff_is_exponential_and_capped() {
        assert_eq!(backoff_ms(0), 100);
        assert_eq!(backoff_ms(1), 200);
        assert_eq!(backoff_ms(6), 100 * 64);
        assert_eq!(backoff_ms(100), 100 * 64);
    }
}
