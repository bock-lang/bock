//! HTTP-backed provider implementations (§17.8).
//!
//! Each submodule implements [`AiProvider`](crate::provider::AiProvider)
//! for a specific API. Providers are wired into the factory in
//! [`crate::make_provider`].

pub mod anthropic;
pub mod openai_compat;

pub use anthropic::AnthropicProvider;
pub use openai_compat::OpenAiCompatProvider;
