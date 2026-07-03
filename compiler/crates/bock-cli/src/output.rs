//! Shared machinery for the CLI's machine-readable output (`--format json`).
//!
//! `bock check`, `bock test`, and `bock inspect` accept
//! `--format <human|json>`. In json mode a command's stdout carries exactly
//! one JSON document, serialized from the same structured values the human
//! renderer consumes ([`Diagnostic`] and friends) — never re-parsed from
//! rendered text. The document shape is a public machine contract (CI, the
//! LSP, and the planned `bock mcp` server consume it): it changes only
//! additively, with [`FORMAT_VERSION`] bumped on any breaking change.
//!
//! Every document shares one envelope — `format_version` / `command` /
//! `outcome` / `summary` — plus one per-command payload array
//! (`diagnostics`, `tests`, or `decisions`). The per-command documents are
//! built where the command lives (`check.rs`, `test.rs`, `inspect.rs`); this
//! module holds the pieces they share.

use bock_errors::{Diagnostic, Severity};

/// The `format_version` stamped on every `--format json` document. Bumped
/// only on a breaking change to the document shape; additive fields do not
/// bump it, so consumers must ignore unknown fields.
pub const FORMAT_VERSION: u32 = 1;

/// Output format for commands with a machine-readable mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    /// Human-readable rendering (the default).
    Human,
    /// One machine-readable JSON document on stdout.
    Json,
}

/// Stable lowercase severity names used in JSON output.
#[must_use]
pub fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
        Severity::Hint => "hint",
    }
}

/// Serialize one [`Diagnostic`] to the machine-contract shape:
/// `severity` / `code` / `message` / `span {file, start, end, line, col}` /
/// `suggestion`.
///
/// `file` and `source` locate the span: `source` is the content backing the
/// span's byte offsets, used for the 1-based `line`/`col` projection (the
/// same span shape `bock inspect air --json` established, plus `file`).
/// Diagnostics with no backing file — e.g. a module cycle that cannot be
/// pinned to a specific `use` edge — serialize with `"file": null` and the
/// dummy span's `1:1`.
///
/// `suggestion` carries the diagnostic's fix-hint notes joined into one
/// string, or `null` when it has none.
#[must_use]
pub fn diagnostic_json(
    diag: &Diagnostic,
    file: Option<&str>,
    source: Option<&str>,
) -> serde_json::Value {
    let (line, col) = source
        .map(|s| byte_to_line_col(s, diag.span.start))
        .unwrap_or((1, 1));
    let suggestion = if diag.notes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(diag.notes.join("\n"))
    };
    serde_json::json!({
        "severity": severity_name(diag.severity),
        "code": diag.code.to_string(),
        "message": diag.message,
        "span": {
            "file": file,
            "start": diag.span.start,
            "end": diag.span.end,
            "line": line,
            "col": col,
        },
        "suggestion": suggestion,
    })
}

/// Print one JSON document to stdout — the single stdout write a
/// `--format json` command performs.
pub fn print_document(doc: &serde_json::Value) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(doc)?);
    Ok(())
}

/// Convert a byte offset into `source` to a 1-indexed `(line, column)`, with
/// the column counting Unicode scalar values (characters), not bytes.
///
/// Mirrors `bock_source::SourceFile::line_col` for content held as a `&str`;
/// kept here so span rendering does not require a `SourceFile`. Offsets past
/// the end clamp to the end of the file.
#[must_use]
pub fn byte_to_line_col(source: &str, offset: usize) -> (usize, usize) {
    let clamped = offset.min(source.len());
    let prefix = &source[..clamped];
    let line = prefix.bytes().filter(|&b| b == b'\n').count() + 1;
    let line_start = prefix.rfind('\n').map_or(0, |i| i + 1);
    let col = prefix[line_start..].chars().count() + 1;
    (line, col)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bock_errors::{DiagnosticCode, FileId, Span};

    #[test]
    fn byte_to_line_col_counts_chars_not_bytes() {
        // ASCII: identity-ish, 1-indexed.
        let s = "abc\ndef";
        assert_eq!(byte_to_line_col(s, 0), (1, 1));
        assert_eq!(byte_to_line_col(s, 2), (1, 3));
        assert_eq!(byte_to_line_col(s, 4), (2, 1)); // first char of line 2

        // Multibyte: 'é' is 2 bytes — the column must count it as one char.
        let m = "fée x"; // f(0) é(1-2) e(3) ' '(4) x(5)
        assert_eq!(byte_to_line_col(m, 5), (1, 5)); // 'x' is column 5, not 6

        // Past the end clamps.
        assert_eq!(byte_to_line_col(m, 999), (1, 6));

        // Multi-line content, matching SourceFile::line_col semantics.
        let content = "ab\ncd\n";
        assert_eq!(byte_to_line_col(content, 3), (2, 1));
        assert_eq!(byte_to_line_col(content, 4), (2, 2));
        assert_eq!(byte_to_line_col(content, 999), (3, 1));
    }

    fn diag(notes: &[&str]) -> Diagnostic {
        Diagnostic {
            severity: Severity::Error,
            code: DiagnosticCode {
                prefix: 'E',
                number: 4002,
            },
            message: "undefined variable `bad`".into(),
            span: Span {
                file: FileId(1),
                start: 9,
                end: 12,
            },
            labels: vec![],
            notes: notes.iter().map(|n| n.to_string()).collect(),
        }
    }

    #[test]
    fn diagnostic_json_emits_the_contract_fields() {
        let source = "module m\nbad\n";
        let json = diagnostic_json(&diag(&[]), Some("m.bock"), Some(source));
        assert_eq!(json["severity"], "error");
        assert_eq!(json["code"], "E4002");
        assert_eq!(json["message"], "undefined variable `bad`");
        assert_eq!(json["span"]["file"], "m.bock");
        assert_eq!(json["span"]["start"], 9);
        assert_eq!(json["span"]["end"], 12);
        assert_eq!(json["span"]["line"], 2);
        assert_eq!(json["span"]["col"], 1);
        assert!(json["suggestion"].is_null(), "no notes → null suggestion");
    }

    #[test]
    fn diagnostic_json_joins_notes_into_suggestion() {
        let json = diagnostic_json(
            &diag(&["did you mean `bar`?", "declare it first"]),
            Some("m.bock"),
            Some("module m\nbad\n"),
        );
        assert_eq!(json["suggestion"], "did you mean `bar`?\ndeclare it first");
    }

    #[test]
    fn diagnostic_json_without_file_uses_null_and_dummy_position() {
        let json = diagnostic_json(&diag(&[]), None, None);
        assert!(json["span"]["file"].is_null());
        assert_eq!(json["span"]["line"], 1);
        assert_eq!(json["span"]["col"], 1);
    }
}
