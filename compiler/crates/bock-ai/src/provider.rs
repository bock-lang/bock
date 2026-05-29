//! The [`AiProvider`] trait and closed-set validation helper.
//!
//! §17.8 of the specification: a provider-agnostic interface with four
//! interaction modes (Generate, Repair, Optimize, Select). Verification
//! is **not** on this trait — it is always deterministic and lives on
//! the target profile / `bock-codegen`.

use async_trait::async_trait;

use crate::error::AiError;
use crate::request::{
    GenerateRequest, GenerateResponse, OptimizeRequest, OptimizeResponse, RepairRequest,
    RepairResponse, SelectOption, SelectRequest, SelectResponse,
};

/// Provider-agnostic interface to an AI model.
///
/// Implementations are responsible for transport, prompt construction,
/// response parsing, retries, and caching. The codegen pipeline calls
/// these methods and consumes the structured response — it does not
/// know (or care) which model or API is behind the trait.
///
/// Implementations of [`select`](Self::select) MUST guarantee that the
/// returned [`SelectResponse::selected_id`] is present in the request's
/// option set; [`validate_select_response`] is provided to make this
/// easy to enforce before returning `Ok`.
#[async_trait]
pub trait AiProvider: Send + Sync {
    /// Tier 1: generate target code from an AIR node (§17.2).
    ///
    /// # Errors
    /// Returns an [`AiError`] on transport failure, provider error, or
    /// an invalid response.
    async fn generate(&self, request: &GenerateRequest) -> Result<GenerateResponse, AiError>;

    /// Repair failing generated code using the compiler error (§17.7).
    ///
    /// # Errors
    /// Returns an [`AiError`] on transport failure, provider error, or
    /// an invalid response.
    async fn repair(&self, request: &RepairRequest) -> Result<RepairResponse, AiError>;

    /// Tier 3: optimize working code for performance or idiom (§17.2).
    ///
    /// # Errors
    /// Returns an [`AiError`] on transport failure, provider error, or
    /// an invalid response.
    async fn optimize(&self, request: &OptimizeRequest) -> Result<OptimizeResponse, AiError>;

    /// Select from a closed set of options (§10.8).
    ///
    /// Implementations MUST enforce the closed-set constraint —
    /// `selected_id` must be one of `request.options`. Implementations
    /// should call [`validate_select_response`] before returning `Ok`.
    ///
    /// # Errors
    /// Returns an [`AiError`] on transport failure, provider error, or
    /// when closed-set validation fails.
    async fn select(&self, request: &SelectRequest) -> Result<SelectResponse, AiError>;

    /// Stable identifier for decision manifests.
    ///
    /// Format: `"provider:model"`, e.g. `"openai-compatible:gpt-4o"` or
    /// `"anthropic:claude-opus-4-7"`. Must be stable across runs so
    /// pinned decisions can be replayed against the same model.
    fn model_id(&self) -> String;
}

/// Validates that a `select()` response identifies an option that was
/// actually offered. Built-in providers call this before returning `Ok`.
///
/// # Errors
/// Returns [`AiError::InvalidResponse`] when `response.selected_id`
/// is not the `id` of any option in `options`.
pub fn validate_select_response(
    options: &[SelectOption],
    response: &SelectResponse,
) -> Result<(), AiError> {
    if options.iter().any(|o| o.id == response.selected_id) {
        Ok(())
    } else {
        Err(AiError::InvalidResponse(format!(
            "selected_id '{}' not in options",
            response.selected_id
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> Vec<SelectOption> {
        vec![
            SelectOption {
                id: "retry".into(),
                description: "retry".into(),
            },
            SelectOption {
                id: "fallback".into(),
                description: "fallback".into(),
            },
        ]
    }

    #[test]
    fn validate_accepts_id_in_set() {
        let options = opts();
        let resp = SelectResponse {
            selected_id: "retry".into(),
            confidence: 0.9,
            reasoning: None,
        };
        validate_select_response(&options, &resp).expect("accepted");
    }

    #[test]
    fn validate_rejects_id_not_in_set() {
        let options = opts();
        let resp = SelectResponse {
            selected_id: "escalate".into(),
            confidence: 0.9,
            reasoning: None,
        };
        let err = validate_select_response(&options, &resp).unwrap_err();
        match err {
            AiError::InvalidResponse(msg) => {
                assert!(msg.contains("escalate"), "message missing id: {msg}");
            }
            other => panic!("expected InvalidResponse, got {other:?}"),
        }
    }

    #[test]
    fn validate_rejects_empty_option_set() {
        let options: Vec<SelectOption> = Vec::new();
        let resp = SelectResponse {
            selected_id: "anything".into(),
            confidence: 1.0,
            reasoning: None,
        };
        assert!(validate_select_response(&options, &resp).is_err());
    }
}
