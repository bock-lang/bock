//! Conversion from Bock [`Diagnostic`] to LSP [`lsp_types::Diagnostic`].
//!
//! Bock uses byte offsets inside a [`Span`]; LSP uses 0-indexed
//! [`lsp_types::Position`] (line, character). We go through
//! [`bock_source::SourceFile::line_col`] which returns 1-indexed
//! (line, column), then subtract 1 on both axes.

use bock_errors::{Diagnostic as BockDiagnostic, Severity, Span};
use bock_source::SourceFile;
use tower_lsp::lsp_types::{
    Diagnostic as LspDiagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, Location,
    NumberOrString, Position, Range, Url,
};

/// Convert a Bock [`Span`] to an LSP [`Range`] using the given source file
/// for line/column lookup.
#[must_use]
pub fn span_to_range(span: Span, source: &SourceFile) -> Range {
    let start = offset_to_position(span.start, source);
    let end_offset = span.end.max(span.start);
    let end = offset_to_position(end_offset, source);
    Range { start, end }
}

fn offset_to_position(offset: usize, source: &SourceFile) -> Position {
    let clamped = offset.min(source.content.len());
    let (line, col) = source.line_col(clamped);
    Position {
        line: (line.saturating_sub(1)) as u32,
        character: (col.saturating_sub(1)) as u32,
    }
}

/// Convert a Bock [`Severity`] to an LSP [`DiagnosticSeverity`].
#[must_use]
pub fn severity_to_lsp(severity: Severity) -> DiagnosticSeverity {
    match severity {
        Severity::Error => DiagnosticSeverity::ERROR,
        Severity::Warning => DiagnosticSeverity::WARNING,
        Severity::Info => DiagnosticSeverity::INFORMATION,
        Severity::Hint => DiagnosticSeverity::HINT,
    }
}

/// Convert a single Bock [`Diagnostic`] to its LSP representation.
///
/// `uri` is the document URI associated with `source`; secondary labels are
/// attached as [`DiagnosticRelatedInformation`] pointing back at the same URI.
#[must_use]
pub fn to_lsp_diagnostic(
    diag: &BockDiagnostic,
    uri: &Url,
    source: &SourceFile,
) -> LspDiagnostic {
    let range = span_to_range(diag.span, source);

    let related = if diag.labels.is_empty() && diag.notes.is_empty() {
        None
    } else {
        let mut items: Vec<DiagnosticRelatedInformation> = diag
            .labels
            .iter()
            .map(|label| DiagnosticRelatedInformation {
                location: Location {
                    uri: uri.clone(),
                    range: span_to_range(label.span, source),
                },
                message: label.message.clone(),
            })
            .collect();

        for note in &diag.notes {
            items.push(DiagnosticRelatedInformation {
                location: Location {
                    uri: uri.clone(),
                    range,
                },
                message: format!("note: {note}"),
            });
        }

        Some(items)
    };

    LspDiagnostic {
        range,
        severity: Some(severity_to_lsp(diag.severity)),
        code: Some(NumberOrString::String(diag.code.to_string())),
        code_description: None,
        source: Some("bock".to_string()),
        message: diag.message.clone(),
        related_information: related,
        tags: None,
        data: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bock_errors::{DiagnosticCode, FileId, Label};
    use bock_source::SourceMap;
    use std::path::PathBuf;

    fn make_source(content: &str) -> (SourceMap, FileId) {
        let mut map = SourceMap::new();
        let id = map.add_file(PathBuf::from("test.bock"), content.to_string());
        (map, id)
    }

    #[test]
    fn span_to_range_single_line() {
        let (map, id) = make_source("let x = 1;\n");
        let file = map.get_file(id);
        let span = Span {
            file: id,
            start: 4,
            end: 5,
        };
        let range = span_to_range(span, file);
        assert_eq!(range.start, Position { line: 0, character: 4 });
        assert_eq!(range.end, Position { line: 0, character: 5 });
    }

    #[test]
    fn span_to_range_crosses_lines() {
        let source = "fn f() {\n  1\n}\n";
        let (map, id) = make_source(source);
        let file = map.get_file(id);
        // Span covers from '{' on line 1 (byte 7) through '}' on line 3 (byte 13).
        let span = Span {
            file: id,
            start: 7,
            end: 14,
        };
        let range = span_to_range(span, file);
        assert_eq!(range.start.line, 0);
        assert_eq!(range.start.character, 7);
        assert_eq!(range.end.line, 2);
        assert_eq!(range.end.character, 1);
    }

    #[test]
    fn span_at_end_of_file_is_clamped() {
        let source = "abc";
        let (map, id) = make_source(source);
        let file = map.get_file(id);
        let span = Span {
            file: id,
            start: 3,
            end: 3,
        };
        let range = span_to_range(span, file);
        assert_eq!(range.start, range.end);
        assert_eq!(range.start.line, 0);
        assert_eq!(range.start.character, 3);
    }

    #[test]
    fn severity_mapping_is_total() {
        assert_eq!(severity_to_lsp(Severity::Error), DiagnosticSeverity::ERROR);
        assert_eq!(
            severity_to_lsp(Severity::Warning),
            DiagnosticSeverity::WARNING,
        );
        assert_eq!(
            severity_to_lsp(Severity::Info),
            DiagnosticSeverity::INFORMATION,
        );
        assert_eq!(severity_to_lsp(Severity::Hint), DiagnosticSeverity::HINT);
    }

    #[test]
    fn to_lsp_diagnostic_preserves_code_and_message() {
        let source = "let x = 1;";
        let (map, id) = make_source(source);
        let file = map.get_file(id);
        let uri = Url::parse("file:///tmp/test.bock").unwrap();

        let diag = BockDiagnostic {
            severity: Severity::Error,
            code: DiagnosticCode {
                prefix: 'E',
                number: 2001,
            },
            message: "type mismatch".into(),
            span: Span {
                file: id,
                start: 4,
                end: 5,
            },
            labels: vec![],
            notes: vec![],
        };

        let lsp = to_lsp_diagnostic(&diag, &uri, file);
        assert_eq!(lsp.message, "type mismatch");
        assert_eq!(lsp.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(lsp.source.as_deref(), Some("bock"));
        assert_eq!(lsp.code, Some(NumberOrString::String("E2001".to_string())));
        assert!(lsp.related_information.is_none());
    }

    #[test]
    fn to_lsp_diagnostic_attaches_labels_and_notes() {
        let source = "let x = 1;\nlet y = 2;\n";
        let (map, id) = make_source(source);
        let file = map.get_file(id);
        let uri = Url::parse("file:///tmp/test.bock").unwrap();

        let diag = BockDiagnostic {
            severity: Severity::Warning,
            code: DiagnosticCode {
                prefix: 'W',
                number: 1,
            },
            message: "shadowed binding".into(),
            span: Span {
                file: id,
                start: 4,
                end: 5,
            },
            labels: vec![Label {
                span: Span {
                    file: id,
                    start: 15,
                    end: 16,
                },
                message: "previous definition here".into(),
            }],
            notes: vec!["consider renaming".into()],
        };

        let lsp = to_lsp_diagnostic(&diag, &uri, file);
        let related = lsp.related_information.expect("related info");
        assert_eq!(related.len(), 2);
        assert_eq!(related[0].message, "previous definition here");
        assert_eq!(related[0].location.range.start.line, 1);
        assert!(related[1].message.contains("consider renaming"));
    }
}
