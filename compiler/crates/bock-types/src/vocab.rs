//! Catalog of annotations and strictness levels recognized by the type system.
//!
//! Exposed so that tooling (vocab emitter, editor completion) can render
//! the full set without duplicating the list.

use crate::Strictness;

/// Metadata for a single annotation recognized by the compiler.
pub struct AnnotationInfo {
    /// Annotation name without the leading `@` (e.g. `"managed"`).
    pub name: &'static str,
    /// Comma-separated parameter names, or empty for zero-arg annotations.
    pub params: &'static str,
    /// One-line purpose summary.
    pub purpose: &'static str,
    /// Spec section reference, if any.
    pub spec_ref: Option<&'static str>,
}

/// Metadata for a strictness level.
pub struct StrictnessLevelInfo {
    /// Canonical lowercase name (matches `bock.project` and the CLI).
    pub name: &'static str,
    /// Short description.
    pub description: &'static str,
    /// Spec section reference, if any.
    pub spec_ref: Option<&'static str>,
}

/// The full set of annotations recognized by the compiler.
#[must_use]
pub fn annotations() -> Vec<AnnotationInfo> {
    vec![
        AnnotationInfo {
            name: "managed",
            params: "",
            purpose: "Suppress ownership and move-checking for the annotated item.",
            spec_ref: Some("§3.6"),
        },
        AnnotationInfo {
            name: "derive",
            params: "trait,...",
            purpose: "Auto-derive trait implementations for a record or enum.",
            spec_ref: Some("§4.7"),
        },
        AnnotationInfo {
            name: "test",
            params: "",
            purpose: "Mark a function as a unit test discovered by `bock test`.",
            spec_ref: Some("§16.2"),
        },
        AnnotationInfo {
            name: "requires",
            params: "capability,...",
            purpose: "Declare the capabilities this function requires.",
            spec_ref: Some("§9"),
        },
        AnnotationInfo {
            name: "performance",
            params: "max_latency, max_allocations",
            purpose: "Declare performance budgets enforced by the context pass.",
            spec_ref: Some("§9"),
        },
    ]
}

/// The full set of strictness levels supported by the compiler.
#[must_use]
pub fn strictness_levels() -> Vec<StrictnessLevelInfo> {
    vec![
        StrictnessLevelInfo {
            name: "sketch",
            description: "Lenient mode: effects and capabilities are inferred; no diagnostics.",
            spec_ref: Some("§8.4"),
        },
        StrictnessLevelInfo {
            name: "development",
            description: "Public items must declare effects; private items are not checked.",
            spec_ref: Some("§8.4"),
        },
        StrictnessLevelInfo {
            name: "production",
            description: "Every function must declare its effects and capabilities.",
            spec_ref: Some("§8.4"),
        },
    ]
}

/// Canonical lowercase name for a [`Strictness`] level.
#[must_use]
pub fn strictness_name(level: Strictness) -> &'static str {
    match level {
        Strictness::Sketch => "sketch",
        Strictness::Development => "development",
        Strictness::Production => "production",
    }
}
