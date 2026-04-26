//! Error types for the AI provider interface.
//!
//! All provider operations return `Result<_, AiError>`. The codegen
//! pipeline inspects the variant to decide whether to fall back to
//! Tier 2 rule-based generation (per §17.2) or fail the build.

use thiserror::Error;

/// Errors produced by an [`AiProvider`](crate::provider::AiProvider) call.
///
/// The variants capture the categories the codegen pipeline distinguishes
/// between when deciding whether to retry, fall back, or surface the
/// failure. They are intentionally transport-agnostic — concrete HTTP
/// status codes or SDK errors should be mapped into one of these.
#[derive(Debug, Clone, Error)]
pub enum AiError {
    /// Transport-level failure reaching the provider (DNS, connect, TLS,
    /// read/write errors). Safe to retry.
    #[error("AI provider network error: {0}")]
    Network(String),

    /// Authentication failed — bad API key, missing credential, revoked
    /// token. Not retryable without re-configuring.
    #[error("AI provider authentication failed: {0}")]
    Auth(String),

    /// The request did not complete within the configured timeout.
    #[error("AI provider request timed out: {0}")]
    Timeout(String),

    /// The provider returned a rate-limit response (HTTP 429 or equivalent).
    /// Callers may retry with backoff.
    #[error("AI provider rate limited: {0}")]
    RateLimited(String),

    /// The configured cost or token budget has been exhausted. Not retryable
    /// until the budget is replenished.
    #[error("AI provider budget exceeded: {0}")]
    BudgetExceeded(String),

    /// The provider returned an error response that does not fit a more
    /// specific variant (5xx, provider-side validation, model error).
    #[error("AI provider error: {0}")]
    ProviderError(String),

    /// The provider is temporarily unavailable — compiler should fall back
    /// to Tier 2 rule-based generation if `deterministic_fallback` is set.
    #[error("AI provider unavailable: {0}")]
    Unavailable(String),

    /// The provider returned a response that failed structural validation —
    /// for example, a `select()` response whose `selected_id` was not in
    /// the provided option set (see [`validate_select_response`]).
    #[error("invalid AI provider response: {0}")]
    InvalidResponse(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_format_distinct_messages() {
        let errs = [
            AiError::Network("dns".into()),
            AiError::Auth("401".into()),
            AiError::Timeout("30s".into()),
            AiError::RateLimited("429".into()),
            AiError::BudgetExceeded("cap".into()),
            AiError::ProviderError("500".into()),
            AiError::Unavailable("down".into()),
            AiError::InvalidResponse("bad id".into()),
        ];
        let msgs: Vec<String> = errs.iter().map(|e| format!("{e}")).collect();
        for (i, m) in msgs.iter().enumerate() {
            for (j, n) in msgs.iter().enumerate() {
                if i != j {
                    assert_ne!(m, n, "variants {i} and {j} produced identical messages");
                }
            }
        }
    }

    #[test]
    fn debug_output_is_serializable_to_string() {
        // Acceptance criterion: variants serializable for debug output.
        let e = AiError::ProviderError("boom".into());
        let dbg = format!("{e:?}");
        assert!(dbg.contains("ProviderError"));
        assert!(dbg.contains("boom"));
    }

    #[test]
    fn error_trait_implemented() {
        fn assert_error<E: std::error::Error>() {}
        assert_error::<AiError>();
    }
}
