//! Bock errors — diagnostic types, span, file-id, and error reporting infrastructure.
//!
//! `Span` and `FileId` are defined here (not in `bock-source`) to avoid circular
//! dependencies. Every crate in the pipeline uses these types transitively.

use std::io::IsTerminal;

use ariadne::{Color, Config, Label as AriadneLabel, Report, ReportKind, Source};

pub mod catalog;

// ─── Source location types ────────────────────────────────────────────────────

/// Identifies a source file within the compilation session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId(pub u32);

/// A byte-offset span within a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

    /// Absorb all diagnostics from `other` into this bag, preserving their
    /// labels and notes.
    ///
    /// Used to fold diagnostics produced by a sub-pass (such as
    /// `ImplTable::build_from`) into the main diagnostic bag.
    pub fn absorb(&mut self, other: &DiagnosticBag) {
        self.items.extend(other.items.iter().cloned());
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
            curr[j + 1] = (curr[j] + 1).min(prev[j + 1] + 1).min(prev[j] + cost);
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

/// Decide whether diagnostic rendering should include ANSI color escapes.
///
/// This is the pure decision core (injectable for tests):
/// - **`NO_COLOR`** (<https://no-color.org>): the *presence* of the
///   environment variable — any value, including the empty string —
///   disables color.
/// - **TTY**: color is only used when the stream that carries diagnostics
///   is an interactive terminal. Piped/redirected output (CI logs, agents
///   parsing `bock check` output) must never contain escape sequences.
#[must_use]
pub fn should_colorize(no_color_present: bool, stream_is_terminal: bool) -> bool {
    !no_color_present && stream_is_terminal
}

/// [`should_colorize`] wired to the real environment: reads `NO_COLOR` and
/// probes **stderr** (the stream the CLI prints rendered diagnostics to).
#[must_use]
pub fn color_enabled_for_diagnostics() -> bool {
    should_colorize(
        std::env::var_os("NO_COLOR").is_some(),
        std::io::stderr().is_terminal(),
    )
}

/// Render a slice of diagnostics to a string using ariadne for source context.
///
/// `filename` and `source` must correspond to the file referenced by the
/// diagnostics' spans. This function is intentionally decoupled from
/// `bock-source` types so that `bock-errors` stays dependency-free.
///
/// Color is decided automatically via [`color_enabled_for_diagnostics`]:
/// ANSI escapes are emitted only when stderr is an interactive terminal and
/// `NO_COLOR` is unset. Use [`render_with_color`] to force the decision.
#[must_use]
pub fn render(diagnostics: &[Diagnostic], filename: &str, source: &str) -> String {
    render_with_color(
        diagnostics,
        filename,
        source,
        color_enabled_for_diagnostics(),
    )
}

/// Convert a byte offset into `source` to a character (Unicode scalar) offset.
///
/// `Span` stores byte offsets, but `ariadne::Source` indexes by character
/// offset — so spans must be remapped at the render boundary. Feeding byte
/// offsets straight through shifts the rendered `line:col` and the underline
/// once any multibyte character precedes the span (e.g. a span after `é`
/// rendered one column too far right, and could drop its underline entirely).
///
/// Offsets at or past the end of `source` clamp to the total character count,
/// which keeps end-of-file spans renderable rather than panicking.
fn byte_to_char_offset(source: &str, byte_offset: usize) -> usize {
    if byte_offset >= source.len() {
        return source.chars().count();
    }
    // Count characters whose byte position is strictly before `byte_offset`.
    // A byte offset that lands inside a multibyte character (it should not, for
    // well-formed spans) rounds down to that character's char index.
    source
        .char_indices()
        .take_while(|(i, _)| *i < byte_offset)
        .count()
}

/// Render diagnostics with an explicit color decision.
///
/// `color: false` guarantees the output contains no ANSI escape sequences.
#[must_use]
pub fn render_with_color(
    diagnostics: &[Diagnostic],
    filename: &str,
    source: &str,
    color: bool,
) -> String {
    let mut out = Vec::new();
    let cache = (filename, Source::from(source));

    for diag in diagnostics {
        let kind = severity_to_kind(diag.severity);
        // `ariadne::Source` indexes by character offset, not byte offset, so
        // every span fed to it must be remapped from the byte offsets `Span`
        // carries (see `byte_to_char_offset`).
        let start_char = byte_to_char_offset(source, diag.span.start);
        let end_char = byte_to_char_offset(source, diag.span.end);
        let span_range = start_char..end_char;

        let mut builder = Report::build(kind, filename, start_char)
            .with_config(Config::default().with_color(color))
            .with_message(format!("[{}] {}", diag.code, diag.message))
            .with_label(
                AriadneLabel::new((filename, span_range))
                    .with_message(&diag.message)
                    .with_color(severity_color(diag.severity)),
            );

        for label in &diag.labels {
            let l_start = byte_to_char_offset(source, label.span.start);
            let l_end = byte_to_char_offset(source, label.span.end);
            builder = builder.with_label(
                AriadneLabel::new((filename, l_start..l_end))
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

    // ── color decision ────────────────────────────────────────────────────────

    #[test]
    fn should_colorize_only_on_tty_without_no_color() {
        // Interactive TTY, NO_COLOR unset → color on.
        assert!(should_colorize(false, true));
        // NO_COLOR present (any value, per https://no-color.org) → off,
        // even on a TTY.
        assert!(!should_colorize(true, true));
        // Piped / redirected output (not a terminal) → off.
        assert!(!should_colorize(false, false));
        // Both: off.
        assert!(!should_colorize(true, false));
    }

    fn ansi_test_diag() -> Diagnostic {
        Diagnostic {
            severity: Severity::Error,
            code: DiagnosticCode {
                prefix: 'E',
                number: 1,
            },
            message: "unexpected token".into(),
            span: Span {
                file: FileId(1),
                start: 8,
                end: 9,
            },
            labels: vec![],
            notes: vec![],
        }
    }

    #[test]
    fn render_without_color_has_no_ansi_escapes() {
        let out = render_with_color(&[ansi_test_diag()], "test.bock", "let x = ;", false);
        assert!(
            !out.contains('\u{1b}'),
            "expected no ANSI escapes, got: {out:?}"
        );
        assert!(out.contains("unexpected token"), "output: {out}");
        assert!(out.contains("E0001"), "output: {out}");
    }

    #[test]
    fn render_with_color_emits_ansi_escapes() {
        let out = render_with_color(&[ansi_test_diag()], "test.bock", "let x = ;", true);
        assert!(
            out.contains('\u{1b}'),
            "expected ANSI escapes when color is forced on, got: {out:?}"
        );
    }

    #[test]
    fn render_with_note_without_color_keeps_note_text() {
        let mut diag = ansi_test_diag();
        diag.note("declare the variable first");
        let out = render_with_color(&[diag], "test.bock", "let x = ;", false);
        assert!(out.contains("declare the variable first"), "output: {out}");
        assert!(!out.contains('\u{1b}'), "output: {out:?}");
    }

    // ── byte→char offset remapping for the render boundary ─────────────────────

    #[test]
    fn byte_to_char_offset_ascii_is_identity() {
        let s = "abcdef";
        assert_eq!(byte_to_char_offset(s, 0), 0);
        assert_eq!(byte_to_char_offset(s, 3), 3);
        assert_eq!(byte_to_char_offset(s, 6), 6);
    }

    #[test]
    fn byte_to_char_offset_multibyte() {
        // "fée x": bytes f(0) é(1-2) e(3) space(4) x(5); chars f=0 é=1 e=2 ' '=3 x=4.
        let s = "fée x";
        assert_eq!(byte_to_char_offset(s, 0), 0); // before 'f'
        assert_eq!(byte_to_char_offset(s, 1), 1); // before 'é'
        assert_eq!(byte_to_char_offset(s, 3), 2); // before 'e' (past 2-byte 'é')
        assert_eq!(byte_to_char_offset(s, 5), 4); // before 'x'
        assert_eq!(byte_to_char_offset(s, 6), 5); // end of file (clamped)
                                                  // Past-end clamps to the char count.
        assert_eq!(byte_to_char_offset(s, 999), 5);
    }

    #[test]
    fn render_multibyte_span_underlines_correct_char() {
        // Regression for Q-errors-render-byte-col-drift: a span after a
        // multibyte character must render at the correct char column and keep
        // its underline. "fée x" — the `x` token is byte 5..6, char 4..5, and
        // must render at column 5 (1-indexed), not column 6.
        let source = "fée x";
        let span = Span {
            file: FileId(1),
            start: 5,
            end: 6,
        };
        let diag = Diagnostic {
            severity: Severity::Error,
            code: DiagnosticCode {
                prefix: 'E',
                number: 1,
            },
            message: "bad token".into(),
            span,
            labels: vec![],
            notes: vec![],
        };
        let out = render_with_color(&[diag], "m.bock", source, false);
        // Column 5 is correct (f=1, é=2, e=3, ' '=4, x=5); the pre-fix code
        // reported 1:6 and dropped the underline.
        assert!(out.contains("m.bock:1:5"), "expected column 5, got:\n{out}");
        assert!(
            !out.contains("m.bock:1:6"),
            "should not drift to col 6:\n{out}"
        );
        // An underline must render under the offending char — ariadne uses a
        // box-drawing underline (`┬`/`─`); pre-fix the span fell off the char
        // range and no underline was drawn at all.
        assert!(
            out.contains('┬') || out.contains('─'),
            "expected an underline under the span:\n{out}"
        );
        // The flagged source line is present and intact.
        assert!(out.contains("fée x"), "source line should render:\n{out}");
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
