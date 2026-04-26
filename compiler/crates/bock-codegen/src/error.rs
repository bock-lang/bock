//! Codegen error types.

use std::fmt;

/// Errors that can occur during code generation.
#[derive(Debug, Clone)]
pub enum CodegenError {
    /// An AIR construct has no viable representation in the target.
    UnsupportedConstruct {
        /// Description of the construct (e.g., "algebraic_types").
        construct: String,
        /// Target that lacks support.
        target_id: String,
    },
    /// A required capability gap has no known synthesis strategy.
    NoSynthesisStrategy {
        /// The construct that needs synthesis.
        construct: String,
        /// Target that needs the synthesis.
        target_id: String,
    },
    /// Generic internal error.
    Internal(String),
}

impl fmt::Display for CodegenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedConstruct {
                construct,
                target_id,
            } => write!(
                f,
                "unsupported construct `{construct}` for target `{target_id}`"
            ),
            Self::NoSynthesisStrategy {
                construct,
                target_id,
            } => write!(
                f,
                "no synthesis strategy for `{construct}` on target `{target_id}`"
            ),
            Self::Internal(msg) => write!(f, "internal codegen error: {msg}"),
        }
    }
}

impl std::error::Error for CodegenError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_construct_display() {
        let err = CodegenError::UnsupportedConstruct {
            construct: "algebraic_types".into(),
            target_id: "js".into(),
        };
        let msg = format!("{err}");
        assert!(msg.contains("algebraic_types"));
        assert!(msg.contains("js"));
    }

    #[test]
    fn no_synthesis_display() {
        let err = CodegenError::NoSynthesisStrategy {
            construct: "pattern_matching".into(),
            target_id: "go".into(),
        };
        let msg = format!("{err}");
        assert!(msg.contains("pattern_matching"));
        assert!(msg.contains("go"));
    }

    #[test]
    fn internal_display() {
        let err = CodegenError::Internal("oops".into());
        assert!(format!("{err}").contains("oops"));
    }
}
