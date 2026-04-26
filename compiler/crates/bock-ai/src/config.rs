//! AI provider configuration, parsed from the `[ai]` section of `bock.project`.
//!
//! See §17.8 and Appendix A of the specification. API keys are never
//! stored in config — only the environment variable name that holds the
//! key is recorded.

use serde::{Deserialize, Serialize};

use crate::error::AiError;

/// Runtime configuration for the AI provider.
///
/// Populated from the `[ai]` section of `bock.project`. All fields have
/// spec-mandated defaults (see [`AiConfig::default`]) so a missing `[ai]`
/// section yields a usable (stub) provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiConfig {
    /// Provider identifier. Built-in values: `"openai-compatible"`,
    /// `"anthropic"`, `"stub"`. See §17.8.
    pub provider: String,

    /// Base URL of the provider's API endpoint. Empty for providers that
    /// do not require it (e.g., the stub provider).
    pub endpoint: String,

    /// Model identifier understood by the provider.
    pub model: String,

    /// Environment variable name holding the API key. Keys never appear
    /// in the project file.
    pub api_key_env: Option<String>,

    /// Fall back to Tier 2 rule-based generation when the provider is
    /// unavailable or confidence drops below `confidence_threshold`.
    pub deterministic_fallback: bool,

    /// Automatically pin AI decisions at `development` strictness.
    pub auto_pin: bool,

    /// Cache AI responses (content-addressed, keyed on request hash).
    pub cache: bool,

    /// Acceptance threshold for AI-generated output. Must lie in `0.0..=1.0`.
    /// Default is `0.75` per §17.4.
    pub confidence_threshold: f64,

    /// Maximum retry count for retryable transport failures.
    pub max_retries: u32,

    /// Per-request timeout, in seconds.
    pub timeout_seconds: u64,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            provider: "stub".into(),
            endpoint: String::new(),
            model: String::new(),
            api_key_env: None,
            deterministic_fallback: true,
            auto_pin: false,
            cache: true,
            confidence_threshold: 0.75,
            max_retries: 3,
            timeout_seconds: 30,
        }
    }
}

impl AiConfig {
    /// Parses an entire `bock.project` TOML document and returns the
    /// `[ai]` subsection (or [`AiConfig::default`] when absent).
    ///
    /// # Errors
    /// Returns [`AiError::InvalidResponse`] (re-purposed for config
    /// parse errors) when the document is not valid TOML or when the
    /// `[ai]` table contains unknown field types.
    pub fn from_project_toml(source: &str) -> Result<Self, AiError> {
        #[derive(Deserialize)]
        struct Wrapper {
            #[serde(default)]
            ai: Option<AiConfig>,
        }

        let wrapper: Wrapper = toml::from_str(source)
            .map_err(|e| AiError::InvalidResponse(format!("bock.project parse error: {e}")))?;

        Ok(wrapper.ai.unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_matches_spec() {
        let c = AiConfig::default();
        assert_eq!(c.provider, "stub");
        assert!(c.deterministic_fallback);
        assert!(!c.auto_pin);
        assert!(c.cache);
        assert!((c.confidence_threshold - 0.75).abs() < f64::EPSILON);
        assert_eq!(c.max_retries, 3);
        assert_eq!(c.timeout_seconds, 30);
        assert!(c.api_key_env.is_none());
    }

    #[test]
    fn from_project_toml_with_ai_section() {
        let src = r#"
[project]
name = "demo"

[ai]
provider = "openai-compatible"
endpoint = "https://api.example.com/v1"
model = "gpt-4o"
api_key_env = "AI_API_KEY"
confidence_threshold = 0.9
max_retries = 5
timeout_seconds = 45
cache = false
"#;
        let c = AiConfig::from_project_toml(src).expect("parse");
        assert_eq!(c.provider, "openai-compatible");
        assert_eq!(c.endpoint, "https://api.example.com/v1");
        assert_eq!(c.model, "gpt-4o");
        assert_eq!(c.api_key_env.as_deref(), Some("AI_API_KEY"));
        assert!((c.confidence_threshold - 0.9).abs() < f64::EPSILON);
        assert_eq!(c.max_retries, 5);
        assert_eq!(c.timeout_seconds, 45);
        assert!(!c.cache);
        // Unspecified fields take defaults.
        assert!(c.deterministic_fallback);
    }

    #[test]
    fn from_project_toml_without_ai_section_yields_default() {
        let src = r#"
[project]
name = "demo"
"#;
        let c = AiConfig::from_project_toml(src).expect("parse");
        assert_eq!(c, AiConfig::default());
    }

    #[test]
    fn from_project_toml_empty_ai_section_fills_defaults() {
        let src = r#"
[ai]
"#;
        let c = AiConfig::from_project_toml(src).expect("parse");
        assert_eq!(c, AiConfig::default());
    }

    #[test]
    fn from_project_toml_reports_invalid_toml() {
        let err = AiConfig::from_project_toml("not = valid = toml").unwrap_err();
        assert!(matches!(err, AiError::InvalidResponse(_)));
    }
}
