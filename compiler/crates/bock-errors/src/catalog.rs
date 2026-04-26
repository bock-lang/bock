//! Catalog of diagnostic codes emitted by the compiler.
//!
//! Source crates emit diagnostics with inline numeric codes (e.g. `E1001`).
//! This catalog is the single queryable registry of those codes together
//! with their human-facing metadata — used by editor extensions, the
//! vocabulary emitter, and online documentation.
//!
//! The catalog is deliberately separate from emission sites: keeping it
//! centralized means tools can render it without depending on the internal
//! structure of every compiler pass. When a new code is introduced at an
//! emission site, add an entry here too.

use crate::Severity;

/// Metadata for a single diagnostic code.
pub struct DiagnosticCodeInfo {
    /// Zero-padded code string (e.g. `"E1001"`).
    pub code: &'static str,
    /// Severity classification.
    pub severity: Severity,
    /// Short single-sentence summary shown by editors.
    pub summary: &'static str,
    /// Longer description (optional; markdown permitted).
    pub description: &'static str,
    /// Spec section references (e.g. `&["§6.1", "§17.4"]`).
    pub spec_refs: &'static [&'static str],
}

/// The full catalog of diagnostic codes.
///
/// Codes are grouped by crate of origin:
/// - `1xxx` — lexer (`bock-lexer`) and name resolution (`bock-air`)
/// - `2xxx` — parser (`bock-parser`)
/// - `4xxx` — type checker (`bock-types/checker`)
/// - `5xxx` — ownership (`bock-types/ownership`)
/// - `6xxx` — effects (`bock-types/effects`)
/// - `7xxx` — capabilities (`bock-types/capabilities`)
#[must_use]
pub fn diagnostic_catalog() -> Vec<DiagnosticCodeInfo> {
    vec![
        // ── Lexer (1xxx) ────────────────────────────────────────────────
        DiagnosticCodeInfo {
            code: "E1001",
            severity: Severity::Error,
            summary: "Unexpected character in source.",
            description: "The lexer encountered a character it does not recognize as part of any valid token.",
            spec_refs: &["§1"],
        },
        DiagnosticCodeInfo {
            code: "E1002",
            severity: Severity::Error,
            summary: "Unterminated string literal.",
            description: "A string literal was opened but never closed before end of input.",
            spec_refs: &["§1.3"],
        },
        DiagnosticCodeInfo {
            code: "E1003",
            severity: Severity::Error,
            summary: "Invalid escape sequence in string.",
            description: "An escape sequence in a string literal is not one of the recognized forms.",
            spec_refs: &["§1.3"],
        },
        DiagnosticCodeInfo {
            code: "E1004",
            severity: Severity::Error,
            summary: "Invalid character literal.",
            description: "A character literal is empty, contains more than one character, or has an invalid escape.",
            spec_refs: &["§1.3"],
        },
        DiagnosticCodeInfo {
            code: "E1005",
            severity: Severity::Error,
            summary: "Invalid digit for numeric literal.",
            description: "A digit was found that is outside the range of the declared numeric base.",
            spec_refs: &["§1.3"],
        },
        DiagnosticCodeInfo {
            code: "E1006",
            severity: Severity::Error,
            summary: "Unterminated block comment.",
            description: "A `/* ... */` block comment was opened but never closed before end of input.",
            spec_refs: &["§1.2"],
        },
        // ── Name resolution (also 1xxx) ─────────────────────────────────
        DiagnosticCodeInfo {
            code: "W1001",
            severity: Severity::Warning,
            summary: "Unused import.",
            description: "An import was declared but never referenced.",
            spec_refs: &["§10"],
        },
        // Note: E1001 overlaps with the lexer code above — historically
        // the resolver reuses the slot for "undefined name" at a different
        // phase. Tooling should disambiguate by pass context.
        DiagnosticCodeInfo {
            code: "E1005 (module)",
            severity: Severity::Error,
            summary: "Module not found.",
            description: "An imported module could not be located by the module registry.",
            spec_refs: &["§10"],
        },
        DiagnosticCodeInfo {
            code: "E1006 (module)",
            severity: Severity::Error,
            summary: "Symbol not found in module.",
            description: "A `use` path names a module that exists but does not export the requested symbol.",
            spec_refs: &["§10"],
        },
        DiagnosticCodeInfo {
            code: "E1007",
            severity: Severity::Error,
            summary: "Symbol is not visible.",
            description: "The referenced symbol exists but is private; declare it `public` to export it.",
            spec_refs: &["§10"],
        },
        // ── Parser (2xxx) ───────────────────────────────────────────────
        DiagnosticCodeInfo {
            code: "E2000",
            severity: Severity::Error,
            summary: "Parse error at top level.",
            description: "The parser encountered unexpected input while reading a top-level item.",
            spec_refs: &["§11"],
        },
        DiagnosticCodeInfo {
            code: "E2001",
            severity: Severity::Error,
            summary: "Unexpected token.",
            description: "The next token did not match any alternative at the current grammar position.",
            spec_refs: &["§11"],
        },
        DiagnosticCodeInfo {
            code: "E2002",
            severity: Severity::Error,
            summary: "Missing expected token.",
            description: "The parser required a specific token here and found something else.",
            spec_refs: &["§11"],
        },
        DiagnosticCodeInfo {
            code: "E2010",
            severity: Severity::Error,
            summary: "Invalid declaration.",
            description: "A top-level declaration has malformed structure.",
            spec_refs: &["§4"],
        },
        DiagnosticCodeInfo {
            code: "E2020",
            severity: Severity::Error,
            summary: "Invalid expression.",
            description: "An expression could not be parsed due to malformed structure.",
            spec_refs: &["§5"],
        },
        DiagnosticCodeInfo {
            code: "E2021",
            severity: Severity::Error,
            summary: "Invalid pattern.",
            description: "A pattern in a `match` or `let` binding could not be parsed.",
            spec_refs: &["§7"],
        },
        DiagnosticCodeInfo {
            code: "E2022",
            severity: Severity::Error,
            summary: "Invalid type expression.",
            description: "A type annotation could not be parsed as a valid type expression.",
            spec_refs: &["§2"],
        },
        DiagnosticCodeInfo {
            code: "E2030",
            severity: Severity::Error,
            summary: "Invalid lambda parameter list.",
            description: "Lambda parameters must be parenthesized; single-identifier forms are not accepted.",
            spec_refs: &["§5"],
        },
        DiagnosticCodeInfo {
            code: "E2040",
            severity: Severity::Error,
            summary: "Invalid generic parameter list.",
            description: "A generic parameter list is malformed.",
            spec_refs: &["§4.5"],
        },
        DiagnosticCodeInfo {
            code: "E2050",
            severity: Severity::Error,
            summary: "Invalid use declaration.",
            description: "A `use` import is malformed.",
            spec_refs: &["§10"],
        },
        DiagnosticCodeInfo {
            code: "E2060",
            severity: Severity::Error,
            summary: "Invalid attribute / annotation.",
            description: "An `@annotation` or `#attribute` could not be parsed.",
            spec_refs: &["§4.7"],
        },
        DiagnosticCodeInfo {
            code: "E2070",
            severity: Severity::Error,
            summary: "Invalid match arm.",
            description: "A match arm is malformed; each arm is `pattern => expression`.",
            spec_refs: &["§7"],
        },
        DiagnosticCodeInfo {
            code: "E2090",
            severity: Severity::Error,
            summary: "Invalid effect declaration.",
            description: "An `effect` declaration or `with` clause is malformed.",
            spec_refs: &["§8"],
        },
        // ── Type checker (4xxx) ────────────────────────────────────────
        DiagnosticCodeInfo {
            code: "E4001",
            severity: Severity::Error,
            summary: "Type mismatch.",
            description: "Expected and actual types do not unify.",
            spec_refs: &["§2"],
        },
        DiagnosticCodeInfo {
            code: "E4002",
            severity: Severity::Error,
            summary: "Undefined variable.",
            description: "A name referenced in an expression has no binding in scope.",
            spec_refs: &["§2"],
        },
        DiagnosticCodeInfo {
            code: "E4003",
            severity: Severity::Error,
            summary: "Arity mismatch in call.",
            description: "The number of arguments does not match the callee's parameter count.",
            spec_refs: &["§5"],
        },
        DiagnosticCodeInfo {
            code: "E4004",
            severity: Severity::Error,
            summary: "Value is not callable.",
            description: "An expression of non-function type was used in a call position.",
            spec_refs: &["§5"],
        },
        DiagnosticCodeInfo {
            code: "E4005",
            severity: Severity::Error,
            summary: "`where` clause predicate failed.",
            description: "A refined-type predicate could not be satisfied.",
            spec_refs: &["§2"],
        },
        DiagnosticCodeInfo {
            code: "E4010",
            severity: Severity::Error,
            summary: "Overlapping trait implementations.",
            description: "Two `impl` blocks apply to the same type and violate coherence.",
            spec_refs: &["§4.4"],
        },
        // ── Ownership (5xxx) ───────────────────────────────────────────
        DiagnosticCodeInfo {
            code: "E5001",
            severity: Severity::Error,
            summary: "Use after move.",
            description: "A value was used after it had been moved into another binding or call.",
            spec_refs: &["§3"],
        },
        DiagnosticCodeInfo {
            code: "E5002",
            severity: Severity::Error,
            summary: "Mutable borrow of non-mut binding.",
            description: "The callee takes a `mut` borrow but the binding was not declared `mut`.",
            spec_refs: &["§3"],
        },
        DiagnosticCodeInfo {
            code: "E5003",
            severity: Severity::Error,
            summary: "Value moved inside loop.",
            description: "The loop body moves a value captured from outside, which would move it more than once.",
            spec_refs: &["§3"],
        },
        // ── Effects (6xxx) ─────────────────────────────────────────────
        DiagnosticCodeInfo {
            code: "E6001",
            severity: Severity::Error,
            summary: "Undeclared effect.",
            description: "A function uses an algebraic effect that is not in its declared `with` clause.",
            spec_refs: &["§8"],
        },
        DiagnosticCodeInfo {
            code: "W6002",
            severity: Severity::Warning,
            summary: "Public function has undeclared effect (development mode).",
            description: "A public function uses an effect that is not in its declared `with` clause. Promotes to error in production strictness.",
            spec_refs: &["§8"],
        },
        DiagnosticCodeInfo {
            code: "E6003",
            severity: Severity::Error,
            summary: "Propagated effect not declared.",
            description: "A called function has an effect that the caller does not declare or handle.",
            spec_refs: &["§8"],
        },
        DiagnosticCodeInfo {
            code: "W6004",
            severity: Severity::Warning,
            summary: "Public function propagates undeclared effect (development mode).",
            description: "A public function calls a function whose effects escape its declared clause.",
            spec_refs: &["§8"],
        },
        // ── Capabilities (7xxx) ────────────────────────────────────────
        DiagnosticCodeInfo {
            code: "E7001",
            severity: Severity::Error,
            summary: "Missing capability.",
            description: "A function requires a capability that is not granted by the caller.",
            spec_refs: &["§9"],
        },
        DiagnosticCodeInfo {
            code: "W7002",
            severity: Severity::Warning,
            summary: "Public function requires uninstalled capability (development mode).",
            description: "A public function's required capability is not present in the caller's capability set.",
            spec_refs: &["§9"],
        },
        DiagnosticCodeInfo {
            code: "E7003",
            severity: Severity::Error,
            summary: "Propagated capability not declared.",
            description: "A callee requires a capability the caller has not declared.",
            spec_refs: &["§9"],
        },
        DiagnosticCodeInfo {
            code: "W7004",
            severity: Severity::Warning,
            summary: "Public function propagates uninstalled capability.",
            description: "A public function calls into code requiring capabilities it has not declared.",
            spec_refs: &["§9"],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_non_empty() {
        assert!(!diagnostic_catalog().is_empty());
    }

    #[test]
    fn codes_have_prefix() {
        for info in diagnostic_catalog() {
            let first = info.code.chars().next().unwrap();
            assert!(
                matches!(first, 'E' | 'W'),
                "code {:?} must start with E or W",
                info.code
            );
        }
    }
}
