//! Bock errors — diagnostic types, span, file-id, and error reporting infrastructure.
//!
//! `Span` and `FileId` are defined here (not in `bock-source`) to avoid circular
//! dependencies. Every crate in the pipeline uses these types transitively.

use ariadne::{Color, Label as AriadneLabel, Report, ReportKind, Source};

pub mod catalog;

// ─── Source location types ────────────────────────────────────────────────────

/// Identifies a source file within the compilation session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId(pub u32);

/// A byte-offset span within a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub file: FileId,
    /// Start byte offset (inclusive).
    pub start: usize,
    /// End byte offset (exclusive).
    pub end: usize,
}

impl Span {
    /// Returns the smallest span that contains both `a` and `b`.
    /// Uses `a`'s `FileId`; callers should ensure both spans belong to the same file.
    #[must_use]
    pub fn merge(a: Span, b: Span) -> Span {
        Span {
            file: a.file,
            start: a.start.min(b.start),
            end: a.end.max(b.end),
        }
    }

    /// A sentinel span for synthetic/compiler-generated nodes.
    #[must_use]
    pub fn dummy() -> Span {
        Span {
            file: FileId(0),
            start: 0,
            end: 0,
        }
    }
}

// ─── Diagnostics ─────────────────────────────────────────────────────────────

/// Diagnostic severity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

/// A structured diagnostic code like `E0001` or `W0042`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagnosticCode {
    /// Category prefix character (`E` for error, `W` for warning, etc.).
    pub prefix: char,
    /// Numeric code.
    pub number: u16,
}

impl std::fmt::Display for DiagnosticCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{:04}", self.prefix, self.number)
    }
}

/// A secondary label pointing at a span with an explanatory message.
#[derive(Debug, Clone)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

/// A structured compiler diagnostic.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: DiagnosticCode,
    pub message: String,
    pub span: Span,
    pub labels: Vec<Label>,
    pub notes: Vec<String>,
}

impl Diagnostic {
    /// Attach an additional label to this diagnostic (builder-style).
    pub fn label(&mut self, span: Span, message: impl Into<String>) -> &mut Self {
        self.labels.push(Label {
            span,
            message: message.into(),
        });
        self
    }

    /// Attach a trailing note to this diagnostic (builder-style).
    pub fn note(&mut self, message: impl Into<String>) -> &mut Self {
        self.notes.push(message.into());
        self
    }
}

// ─── DiagnosticBag ───────────────────────────────────────────────────────────

/// Accumulates diagnostics emitted during a compilation pass.
#[derive(Debug, Default)]
pub struct DiagnosticBag {
    items: Vec<Diagnostic>,
}

impl DiagnosticBag {
    /// Create an empty bag.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Emit an error diagnostic and return a mutable reference for further decoration.
    pub fn error(
        &mut self,
        code: DiagnosticCode,
        message: impl Into<String>,
        span: Span,
    ) -> &mut Diagnostic {
        self.push(Severity::Error, code, message, span)
    }

    /// Emit a warning diagnostic and return a mutable reference for further decoration.
    pub fn warning(
        &mut self,
        code: DiagnosticCode,
        message: impl Into<String>,
        span: Span,
    ) -> &mut Diagnostic {
        self.push(Severity::Warning, code, message, span)
    }

    /// Emit an info diagnostic and return a mutable reference for further decoration.
    pub fn info(
        &mut self,
        code: DiagnosticCode,
        message: impl Into<String>,
        span: Span,
    ) -> &mut Diagnostic {
        self.push(Severity::Info, code, message, span)
    }

    /// Emit a hint diagnostic and return a mutable reference for further decoration.
    pub fn hint(
        &mut self,
        code: DiagnosticCode,
        message: impl Into<String>,
        span: Span,
    ) -> &mut Diagnostic {
        self.push(Severity::Hint, code, message, span)
    }

    /// Returns `true` if any error-severity diagnostics have been emitted.
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.items.iter().any(|d| d.severity == Severity::Error)
    }

    /// Returns the number of error-severity diagnostics emitted so far.
    #[must_use]
    pub fn error_count(&self) -> usize {
        self.items
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count()
    }

    /// Returns the number of warning-severity diagnostics emitted so far.
    #[must_use]
    pub fn warning_count(&self) -> usize {
        self.items
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .count()
    }

    /// Iterate over all collected diagnostics.
    pub fn iter(&self) -> impl Iterator<Item = &Diagnostic> {
        self.items.iter()
    }

    /// Total number of diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Returns `true` if no diagnostics have been emitted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn push(
        &mut self,
        severity: Severity,
        code: DiagnosticCode,
        message: impl Into<String>,
        span: Span,
    ) -> &mut Diagnostic {
        self.items.push(Diagnostic {
            severity,
            code,
            message: message.into(),
            span,
            labels: Vec::new(),
            notes: Vec::new(),
        });
        self.items.last_mut().expect("just pushed")
    }
}

// ─── String distance helpers ─────────────────────────────────────────────────

/// Levenshtein edit distance between two strings.
///
/// Counts the minimum number of single-character insertions, deletions,
/// and substitutions required to transform `a` into `b`. Used by diagnostic
/// passes to produce "did you mean X?" suggestions.
#[must_use]
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr: Vec<usize> = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (curr[j] + 1)
                .min(prev[j + 1] + 1)
                .min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Find the closest candidate within `max_distance` edits of `name`.
///
/// Returns a clone of the closest matching candidate. Ties are broken by
/// insertion order (the first candidate wins).
#[must_use]
pub fn suggest_similar<S, I>(name: &str, candidates: I, max_distance: usize) -> Option<String>
where
    S: AsRef<str>,
    I: IntoIterator<Item = S>,
{
    candidates
        .into_iter()
        .map(|s| {
            let d = levenshtein(name, s.as_ref());
            (s, d)
        })
        .filter(|(_, d)| *d <= max_distance)
        .min_by_key(|(_, d)| *d)
        .map(|(s, _)| s.as_ref().to_string())
}

// ─── Rendering ───────────────────────────────────────────────────────────────

/// Render a slice of diagnostics to a string using ariadne for source context.
///
/// `filename` and `source` must correspond to the file referenced by the
/// diagnostics' spans. This function is intentionally decoupled from
/// `bock-source` types so that `bock-errors` stays dependency-free.
#[must_use]
pub fn render(diagnostics: &[Diagnostic], filename: &str, source: &str) -> String {
    let mut out = Vec::new();
    let cache = (filename, Source::from(source));

    for diag in diagnostics {
        let kind = severity_to_kind(diag.severity);
        let span_range = diag.span.start..diag.span.end;

        let mut builder = Report::build(kind, filename, diag.span.start)
            .with_message(format!("[{}] {}", diag.code, diag.message))
            .with_label(
                AriadneLabel::new((filename, span_range))
                    .with_message(&diag.message)
                    .with_color(severity_color(diag.severity)),
            );

        for label in &diag.labels {
            builder = builder.with_label(
                AriadneLabel::new((filename, label.span.start..label.span.end))
                    .with_message(&label.message)
                    .with_color(Color::Blue),
            );
        }

        for note in &diag.notes {
            builder = builder.with_note(note);
        }

        builder
            .finish()
            .write(cache.clone(), &mut out)
            .expect("write to Vec is infallible");
    }

    String::from_utf8_lossy(&out).into_owned()
}

fn severity_to_kind(severity: Severity) -> ReportKind<'static> {
    match severity {
        Severity::Error => ReportKind::Error,
        Severity::Warning => ReportKind::Warning,
        Severity::Info | Severity::Hint => ReportKind::Advice,
    }
}

fn severity_color(severity: Severity) -> Color {
    match severity {
        Severity::Error => Color::Red,
        Severity::Warning => Color::Yellow,
        Severity::Info => Color::Cyan,
        Severity::Hint => Color::Green,
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_span(start: usize, end: usize) -> Span {
        Span {
            file: FileId(1),
            start,
            end,
        }
    }

    // ── Span ──────────────────────────────────────────────────────────────────

    #[test]
    fn span_merge_basic() {
        let a = make_span(2, 5);
        let b = make_span(3, 8);
        let m = Span::merge(a, b);
        assert_eq!(m.start, 2);
        assert_eq!(m.end, 8);
        assert_eq!(m.file, FileId(1));
    }

    #[test]
    fn span_merge_disjoint() {
        let a = make_span(0, 3);
        let b = make_span(10, 15);
        let m = Span::merge(a, b);
        assert_eq!(m.start, 0);
        assert_eq!(m.end, 15);
    }

    #[test]
    fn span_merge_identical() {
        let s = make_span(4, 9);
        let m = Span::merge(s, s);
        assert_eq!(m, s);
    }

    #[test]
    fn span_dummy_is_zero() {
        let d = Span::dummy();
        assert_eq!(d.file, FileId(0));
        assert_eq!(d.start, 0);
        assert_eq!(d.end, 0);
    }

    // ── DiagnosticCode display ────────────────────────────────────────────────

    #[test]
    fn diagnostic_code_display() {
        let c = DiagnosticCode {
            prefix: 'E',
            number: 42,
        };
        assert_eq!(c.to_string(), "E0042");
    }

    // ── DiagnosticBag ─────────────────────────────────────────────────────────

    #[test]
    fn bag_has_errors_false_when_empty() {
        let bag = DiagnosticBag::new();
        assert!(!bag.has_errors());
    }

    #[test]
    fn bag_has_errors_false_for_warnings() {
        let mut bag = DiagnosticBag::new();
        let code = DiagnosticCode {
            prefix: 'W',
            number: 1,
        };
        bag.warning(code, "watch out", make_span(0, 1));
        assert!(!bag.has_errors());
    }

    #[test]
    fn bag_has_errors_true_for_error() {
        let mut bag = DiagnosticBag::new();
        let code = DiagnosticCode {
            prefix: 'E',
            number: 1,
        };
        bag.error(code, "oops", make_span(0, 1));
        assert!(bag.has_errors());
    }

    #[test]
    fn bag_iter_yields_all() {
        let mut bag = DiagnosticBag::new();
        let ec = DiagnosticCode {
            prefix: 'E',
            number: 1,
        };
        let wc = DiagnosticCode {
            prefix: 'W',
            number: 2,
        };
        bag.error(ec, "err", make_span(0, 1));
        bag.warning(wc, "warn", make_span(1, 2));
        let items: Vec<_> = bag.iter().collect();
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn bag_labels_and_notes() {
        let mut bag = DiagnosticBag::new();
        let code = DiagnosticCode {
            prefix: 'E',
            number: 5,
        };
        bag.error(code, "main", make_span(0, 3))
            .label(make_span(1, 2), "secondary")
            .note("fix it");
        let d = bag.iter().next().unwrap();
        assert_eq!(d.labels.len(), 1);
        assert_eq!(d.notes.len(), 1);
    }

    // ── render ────────────────────────────────────────────────────────────────

    #[test]
    fn render_error_contains_message() {
        let source = "let x = ;";
        let span = Span {
            file: FileId(1),
            start: 8,
            end: 9,
        };
        let diag = Diagnostic {
            severity: Severity::Error,
            code: DiagnosticCode {
                prefix: 'E',
                number: 1,
            },
            message: "unexpected token".into(),
            span,
            labels: vec![],
            notes: vec![],
        };
        let out = render(&[diag], "test.bock", source);
        assert!(out.contains("unexpected token"), "output: {out}");
    }

    #[test]
    fn render_empty_produces_empty_string() {
        let out = render(&[], "test.bock", "let x = 1;");
        assert!(out.is_empty());
    }

    // ── levenshtein ───────────────────────────────────────────────────────────

    #[test]
    fn levenshtein_equal_strings_is_zero() {
        assert_eq!(levenshtein("foo", "foo"), 0);
    }

    #[test]
    fn levenshtein_empty_strings() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("", "abc"), 3);
    }

    #[test]
    fn levenshtein_single_substitution() {
        assert_eq!(levenshtein("cat", "bat"), 1);
    }

    #[test]
    fn levenshtein_insert_and_delete() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }

    #[test]
    fn suggest_similar_finds_close_match() {
        let names = vec!["println", "print", "printf"];
        assert_eq!(suggest_similar("printn", names, 2), Some("println".into()));
    }

    #[test]
    fn suggest_similar_rejects_far_matches() {
        let names = vec!["elephant", "giraffe"];
        assert_eq!(suggest_similar("cat", names, 2), None);
    }

    #[test]
    fn render_with_note() {
        let source = "foo bar";
        let span = Span {
            file: FileId(1),
            start: 0,
            end: 3,
        };
        let mut diag = Diagnostic {
            severity: Severity::Warning,
            code: DiagnosticCode {
                prefix: 'W',
                number: 99,
            },
            message: "test warning".into(),
            span,
            labels: vec![],
            notes: vec![],
        };
        diag.note("consider renaming");
        let out = render(&[diag], "src.bock", source);
        assert!(out.contains("consider renaming"), "output: {out}");
    }
}
