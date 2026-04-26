//! Core lexer implementation for Bock source files.

use std::collections::VecDeque;

use bock_errors::{DiagnosticBag, DiagnosticCode, Span};
use bock_source::SourceFile;

use crate::token::{keyword_lookup, Token, TokenKind};

/// Diagnostic code for unknown/unexpected characters.
const E_UNEXPECTED_CHAR: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 1001,
};
/// Diagnostic code for unterminated string or character literals.
const E_UNTERMINATED_STRING: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 1002,
};
/// Diagnostic code for invalid escape sequences.
const E_INVALID_ESCAPE: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 1003,
};
/// Diagnostic code for malformed character literals.
const E_INVALID_CHAR_LITERAL: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 1004,
};
/// Diagnostic code for invalid digit in a numeric literal (e.g., `0b123`).
const E_INVALID_DIGIT: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 1005,
};
/// Diagnostic code for an unterminated block comment.
const E_UNTERMINATED_BLOCK_COMMENT: DiagnosticCode = DiagnosticCode {
    prefix: 'E',
    number: 1006,
};

/// Context needed to resume lexing a string after an interpolation expression ends.
struct StringResumeCtx {
    /// Byte offset of the opening delimiter (used for span construction on error).
    string_start: usize,
    is_raw: bool,
    is_multiline: bool,
}

/// The Bock lexer: advances through a [`SourceFile`] and produces a token stream.
pub struct Lexer<'src> {
    source: &'src SourceFile,
    /// Current byte position in `source.content`.
    pos: usize,
    diagnostics: DiagnosticBag,
    /// Tokens buffered for emission before resuming normal lexing.
    /// Used when a string sub-lexer produces multiple tokens at once.
    pending: VecDeque<Token>,
    /// Per-interpolation-level inner brace counter.
    /// Non-empty ⟺ we are currently inside a `${...}` interpolation.
    /// Each entry is the number of unmatched `{` seen since entering that level.
    interp_brace_depth: Vec<u32>,
    /// String resume contexts: one entry per active interpolation level.
    string_resume: Vec<StringResumeCtx>,
}

impl<'src> Lexer<'src> {
    /// Create a new [`Lexer`] for the given source file.
    #[must_use]
    pub fn new(source: &'src SourceFile) -> Self {
        Self {
            source,
            pos: 0,
            diagnostics: DiagnosticBag::new(),
            pending: VecDeque::new(),
            interp_brace_depth: Vec::new(),
            string_resume: Vec::new(),
        }
    }

    /// Tokenize the entire source file, returning all tokens including a final [`TokenKind::Eof`].
    #[must_use]
    pub fn tokenize(&mut self) -> Vec<Token> {
        let mut tokens = Vec::new();
        loop {
            let tok = self.next_token();
            let is_eof = tok.kind == TokenKind::Eof;
            tokens.push(tok);
            if is_eof {
                break;
            }
        }
        tokens
    }

    /// Access the accumulated diagnostics.
    #[must_use]
    pub fn diagnostics(&self) -> &DiagnosticBag {
        &self.diagnostics
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    /// Return the current character without advancing, or `None` at EOF.
    fn peek(&self) -> Option<char> {
        self.source.content[self.pos..].chars().next()
    }

    /// Return the character after the current one without advancing, or `None`.
    fn peek_next(&self) -> Option<char> {
        let mut chars = self.source.content[self.pos..].chars();
        chars.next(); // skip current
        chars.next()
    }

    /// Advance past the current character and return it, or `None` at EOF.
    fn advance(&mut self) -> Option<char> {
        let ch = self.source.content[self.pos..].chars().next()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    /// Skip whitespace characters that are NOT newlines.
    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.peek() {
            if ch == '\n' || !ch.is_whitespace() {
                break;
            }
            self.advance();
        }
    }

    /// Build a span from `start` to the current position.
    fn span_from(&self, start: usize) -> Span {
        Span {
            file: self.source.id,
            start,
            end: self.pos,
        }
    }

    /// Make a simple token with no literal.
    fn make_token(&self, kind: TokenKind, start: usize) -> Token {
        Token::new(kind, self.span_from(start), None)
    }

    // ── Main dispatch ─────────────────────────────────────────────────────────

    /// Lex the next token.
    fn next_token(&mut self) -> Token {
        // Drain any tokens buffered by the string sub-lexer.
        if let Some(tok) = self.pending.pop_front() {
            return tok;
        }

        self.skip_whitespace();

        let start = self.pos;

        let ch = match self.peek() {
            None => return self.make_token(TokenKind::Eof, start),
            Some(c) => c,
        };

        // Newline
        if ch == '\n' {
            self.advance();
            return self.make_token(TokenKind::Newline, start);
        }

        // Windows-style \r\n — treat as a single newline
        if ch == '\r' {
            self.advance();
            if self.peek() == Some('\n') {
                self.advance();
            }
            return self.make_token(TokenKind::Newline, start);
        }

        // Comments: // or /*
        if ch == '/' && (self.peek_next() == Some('/') || self.peek_next() == Some('*')) {
            self.lex_comment();
            // After a comment, recurse (skip it, get next real token)
            return self.next_token();
        }

        // String / raw-string literals
        if ch == '"' {
            return self.lex_string();
        }
        if ch == 'r' && self.peek_next() == Some('"') {
            return self.lex_string();
        }

        // Character literal
        if ch == '\'' {
            return self.lex_char();
        }

        // Numeric literal
        if ch.is_ascii_digit() {
            return self.lex_number();
        }
        // Also dispatch 0x / 0o / 0b handled inside lex_number
        // (the leading char is always a digit so this is fine)

        // Identifier or keyword
        if ch.is_alphabetic() || ch == '_' {
            return self.lex_ident_or_keyword();
        }

        // Backslash line continuation: `\` immediately followed by newline
        // consumes both and continues lexing on the next line.
        if ch == '\\' {
            if self.peek_next() == Some('\n') {
                self.advance(); // consume '\'
                self.advance(); // consume '\n'
                return self.next_token();
            }
            if self.peek_next() == Some('\r') {
                // Check for \r\n
                self.advance(); // consume '\'
                self.advance(); // consume '\r'
                if self.peek() == Some('\n') {
                    self.advance(); // consume '\n'
                }
                return self.next_token();
            }
            // `\` not followed by newline — fall through to lex_operator
            // which will emit an Error token.
        }

        // Operators and punctuation
        self.lex_operator()
    }

    // ── String lexing (P1.3) ──────────────────────────────────────────────────

    /// Lex a string literal (standard, raw, or multi-line).
    fn lex_string(&mut self) -> Token {
        let start = self.pos;

        let is_raw = self.peek() == Some('r');
        if is_raw {
            self.advance(); // consume 'r'
        }

        // Check for triple-quote multiline string.
        let is_multiline = self.source.content[self.pos..].starts_with("\"\"\"");
        if is_multiline {
            self.pos += 3; // consume """ (each " is 1 byte)
        } else {
            self.advance(); // consume single "
        }

        self.process_string_body(start, is_raw, is_multiline, false)
    }

    /// Process string content starting at the current position.
    ///
    /// `string_start` is the byte offset of the opening delimiter (for spans/errors).
    /// `is_continuation` is `true` when resuming after an interpolation — in that case
    /// the returned token is always `StringLiteralPart` even if there is no further
    /// interpolation, so the parser can see where the string ends.
    fn process_string_body(
        &mut self,
        string_start: usize,
        is_raw: bool,
        is_multiline: bool,
        is_continuation: bool,
    ) -> Token {
        let segment_start = self.pos;
        let mut content = String::new();

        loop {
            match self.peek() {
                // EOF before the closing delimiter.
                None => {
                    let span = self.span_from(string_start);
                    self.diagnostics.error(
                        E_UNTERMINATED_STRING,
                        "unterminated string literal",
                        span,
                    );
                    let kind = closing_kind(is_raw, is_multiline, is_continuation);
                    return Token::new(kind, span, Some(content));
                }

                // Closing delimiter check.
                Some('"') => {
                    if is_multiline {
                        if self.source.content[self.pos..].starts_with("\"\"\"") {
                            self.pos += 3; // consume """
                            let span = self.span_from(string_start);
                            let processed = if is_multiline && !is_raw {
                                strip_common_indent(&content)
                            } else {
                                content
                            };
                            let kind = closing_kind(is_raw, is_multiline, is_continuation);
                            return Token::new(kind, span, Some(processed));
                        } else {
                            // A lone `"` inside a multiline string is just a character.
                            content.push('"');
                            self.advance();
                        }
                    } else {
                        // Single-line string: closing `"`.
                        self.advance();
                        let span = self.span_from(string_start);
                        let kind = closing_kind(is_raw, is_multiline, is_continuation);
                        return Token::new(kind, span, Some(content));
                    }
                }

                // Newline inside a single-line string = unterminated.
                Some('\n') if !is_multiline => {
                    let span = self.span_from(string_start);
                    self.diagnostics.error(
                        E_UNTERMINATED_STRING,
                        "unterminated string literal (newline)",
                        span,
                    );
                    let kind = closing_kind(is_raw, is_multiline, is_continuation);
                    return Token::new(kind, span, Some(content));
                }

                // Backslash escape — only in non-raw strings.
                Some('\\') if !is_raw => {
                    self.advance(); // consume '\'
                    match self.advance() {
                        Some('n') => content.push('\n'),
                        Some('t') => content.push('\t'),
                        Some('r') => content.push('\r'),
                        Some('\\') => content.push('\\'),
                        Some('"') => content.push('"'),
                        Some('\'') => content.push('\''),
                        Some('0') => content.push('\0'),
                        Some('$') => content.push('$'),
                        Some('u') => {
                            self.lex_unicode_escape(&mut content, string_start);
                        }
                        Some(other) => {
                            let span = self.span_from(string_start);
                            self.diagnostics.error(
                                E_INVALID_ESCAPE,
                                format!("unknown escape sequence: \\{other}"),
                                span,
                            );
                            content.push(other);
                        }
                        None => {
                            let span = self.span_from(string_start);
                            self.diagnostics.error(
                                E_UNTERMINATED_STRING,
                                "unterminated string literal after backslash",
                                span,
                            );
                            let kind = closing_kind(is_raw, is_multiline, is_continuation);
                            return Token::new(kind, span, Some(content));
                        }
                    }
                }

                // Interpolation `${` — only in non-raw strings.
                Some('$') if !is_raw => {
                    if self.source.content[self.pos..].starts_with("${") {
                        // Emit the text before the interpolation as a StringLiteralPart.
                        let part_span = Span {
                            file: self.source.id,
                            start: segment_start,
                            end: self.pos,
                        };
                        let part_tok =
                            Token::new(TokenKind::StringLiteralPart, part_span, Some(content));

                        let interp_start = self.pos;
                        self.pos += 2; // consume '${'
                        let interp_span = Span {
                            file: self.source.id,
                            start: interp_start,
                            end: self.pos,
                        };
                        let interp_tok =
                            Token::new(TokenKind::InterpolationStart, interp_span, None);

                        // Buffer InterpolationStart; push resume context.
                        self.pending.push_back(interp_tok);
                        self.interp_brace_depth.push(0);
                        self.string_resume.push(StringResumeCtx {
                            string_start,
                            is_raw,
                            is_multiline,
                        });

                        return part_tok;
                    } else if self.source.content[self.pos..].starts_with("$$") {
                        // `$$` is an escaped dollar sign in non-raw strings.
                        content.push('$');
                        self.pos += 2; // consume both '$'
                    } else {
                        content.push('$');
                        self.advance();
                    }
                }

                // Any other character — include as-is.
                Some(ch) => {
                    content.push(ch);
                    self.advance();
                }
            }
        }
    }

    /// Called from `lex_operator` when `}` closes an interpolation.
    /// Resumes lexing the string body and pushes the resulting token(s) to `pending`.
    fn resume_string_lex(&mut self, ctx: StringResumeCtx) {
        let tok = self.process_string_body(ctx.string_start, ctx.is_raw, ctx.is_multiline, true);
        // Push to front so it comes before any InterpolationStart that may have been
        // buffered if there is an immediately following `${` in the continued string.
        self.pending.push_front(tok);
    }

    /// Process a `\u{HHHH}` Unicode escape, appending the decoded character to `out`.
    fn lex_unicode_escape(&mut self, out: &mut String, string_start: usize) {
        if self.peek() != Some('{') {
            let span = self.span_from(string_start);
            self.diagnostics.error(
                E_INVALID_ESCAPE,
                "expected '{' after \\u in Unicode escape",
                span,
            );
            return;
        }
        self.advance(); // consume '{'

        let hex_start = self.pos;
        while self.peek().map(|c| c.is_ascii_hexdigit()).unwrap_or(false) {
            self.advance();
        }
        let hex_str = &self.source.content[hex_start..self.pos];

        if self.peek() != Some('}') {
            let span = self.span_from(string_start);
            self.diagnostics.error(
                E_INVALID_ESCAPE,
                "expected '}' to close Unicode escape \\u{...}",
                span,
            );
            return;
        }
        self.advance(); // consume '}'

        match u32::from_str_radix(hex_str, 16)
            .ok()
            .and_then(char::from_u32)
        {
            Some(c) => out.push(c),
            None => {
                let span = self.span_from(string_start);
                self.diagnostics.error(
                    E_INVALID_ESCAPE,
                    format!("invalid Unicode codepoint: \\u{{{hex_str}}}"),
                    span,
                );
            }
        }
    }

    // ── Character literal (P1.3) ──────────────────────────────────────────────

    /// Lex a character literal: `'c'`, `'\n'`, `'\u{1F600}'`.
    fn lex_char(&mut self) -> Token {
        let start = self.pos;
        self.advance(); // consume opening '

        let ch = match self.peek() {
            None => {
                let span = self.span_from(start);
                self.diagnostics.error(
                    E_INVALID_CHAR_LITERAL,
                    "unterminated character literal",
                    span,
                );
                return Token::new(TokenKind::Error, span, None);
            }
            Some('\'') => {
                // Empty literal ''
                self.advance();
                let span = self.span_from(start);
                self.diagnostics
                    .error(E_INVALID_CHAR_LITERAL, "empty character literal", span);
                return Token::new(TokenKind::Error, span, None);
            }
            Some('\\') => {
                self.advance(); // consume '\'
                match self.advance() {
                    Some('n') => '\n',
                    Some('t') => '\t',
                    Some('r') => '\r',
                    Some('\\') => '\\',
                    Some('\'') => '\'',
                    Some('"') => '"',
                    Some('0') => '\0',
                    Some('u') => {
                        let mut buf = String::new();
                        self.lex_unicode_escape(&mut buf, start);
                        buf.chars().next().unwrap_or('\0')
                    }
                    Some(other) => {
                        let span = self.span_from(start);
                        self.diagnostics.error(
                            E_INVALID_ESCAPE,
                            format!("unknown escape sequence: \\{other}"),
                            span,
                        );
                        other
                    }
                    None => {
                        let span = self.span_from(start);
                        self.diagnostics.error(
                            E_INVALID_CHAR_LITERAL,
                            "unterminated character literal",
                            span,
                        );
                        return Token::new(TokenKind::Error, span, None);
                    }
                }
            }
            Some(c) => {
                self.advance();
                c
            }
        };

        // Expect closing '
        if self.peek() == Some('\'') {
            self.advance();
            let span = self.span_from(start);
            Token::new(TokenKind::CharLiteral, span, Some(ch.to_string()))
        } else {
            let span = self.span_from(start);
            self.diagnostics.error(
                E_INVALID_CHAR_LITERAL,
                "expected closing ' in character literal",
                span,
            );
            Token::new(TokenKind::Error, span, Some(ch.to_string()))
        }
    }

    // ── Sub-lexer stubs ───────────────────────────────────────────────────────

    /// Lex a numeric literal (integer or float, all bases, optional type suffix).
    ///
    /// Handles:
    /// - Decimal: `42`, `1_000_000`
    /// - Hex: `0xFF`, `0XFF`
    /// - Octal: `0o77`, `0O77`
    /// - Binary: `0b1010`, `0B1010`
    /// - Float: `3.14`, `1.0e10`, `2.5E-3`, `1e6`
    /// - Type suffix: `42_u8`, `3.14_f64` (underscore + type ident)
    /// - Disambiguation: `1..2` emits `IntLiteral(1)`, `DotDot`, `IntLiteral(2)`.
    fn lex_number(&mut self) -> Token {
        let start = self.pos;
        let mut literal = String::new();
        let mut is_float = false;

        // Consume the first digit (already peeked as ascii_digit in caller).
        let first = self.advance().expect("caller guarantees a digit");
        literal.push(first);

        // Detect base prefix: 0x / 0o / 0b (only when first digit is '0').
        if first == '0' {
            match self.peek() {
                Some('x') | Some('X') => {
                    let prefix = self.advance().expect("peek confirmed 'x'/'X'");
                    literal.push(prefix);
                    let digit_start = self.pos;
                    // Consume hex digits and underscores.
                    self.consume_digits(&mut literal, |c| c.is_ascii_hexdigit() || c == '_');
                    if self.pos == digit_start {
                        self.diagnostics.error(
                            E_INVALID_DIGIT,
                            "expected hexadecimal digit after '0x'",
                            self.span_from(start),
                        );
                    }
                    let suffix = self.try_consume_suffix();
                    let full = format!("{}{}", literal, suffix.as_deref().unwrap_or(""));
                    return Token::new(TokenKind::IntLiteral, self.span_from(start), Some(full));
                }
                Some('o') | Some('O') => {
                    let prefix = self.advance().expect("peek confirmed 'o'/'O'");
                    literal.push(prefix);
                    let digit_start = self.pos;
                    // Consume all digits/underscores, collecting the full body for validation.
                    self.consume_digits(&mut literal, |c| c.is_ascii_digit() || c == '_');
                    if self.pos == digit_start {
                        self.diagnostics.error(
                            E_INVALID_DIGIT,
                            "expected octal digit after '0o'",
                            self.span_from(start),
                        );
                    } else {
                        let body = &self.source.content[digit_start..self.pos];
                        for ch in body.chars() {
                            if ch != '_' && !matches!(ch, '0'..='7') {
                                self.diagnostics.error(
                                    E_INVALID_DIGIT,
                                    format!("invalid octal digit '{ch}'"),
                                    self.span_from(start),
                                );
                                break;
                            }
                        }
                    }
                    let suffix = self.try_consume_suffix();
                    let full = format!("{}{}", literal, suffix.as_deref().unwrap_or(""));
                    return Token::new(TokenKind::IntLiteral, self.span_from(start), Some(full));
                }
                Some('b') | Some('B') => {
                    let prefix = self.advance().expect("peek confirmed 'b'/'B'");
                    literal.push(prefix);
                    let digit_start = self.pos;
                    // Consume all digits/underscores, collecting the full body for validation.
                    self.consume_digits(&mut literal, |c| c.is_ascii_digit() || c == '_');
                    if self.pos == digit_start {
                        self.diagnostics.error(
                            E_INVALID_DIGIT,
                            "expected binary digit after '0b'",
                            self.span_from(start),
                        );
                    } else {
                        let body = &self.source.content[digit_start..self.pos];
                        for ch in body.chars() {
                            if ch != '_' && !matches!(ch, '0' | '1') {
                                self.diagnostics.error(
                                    E_INVALID_DIGIT,
                                    format!("invalid binary digit '{ch}'"),
                                    self.span_from(start),
                                );
                                break;
                            }
                        }
                    }
                    let suffix = self.try_consume_suffix();
                    let full = format!("{}{}", literal, suffix.as_deref().unwrap_or(""));
                    return Token::new(TokenKind::IntLiteral, self.span_from(start), Some(full));
                }
                _ => {}
            }
        }

        // Decimal integer body. Underscores are digit separators, but stop before
        // a `_` that is immediately followed by an alphabetic char (type suffix start).
        self.consume_decimal_digits(&mut literal);

        // Float detection: `.` followed by a digit (not `..`).
        if self.peek() == Some('.') && self.peek_next().is_some_and(|c| c.is_ascii_digit()) {
            is_float = true;
            literal.push(self.advance().expect("peek confirmed '.'")); // consume '.'
            self.consume_decimal_digits(&mut literal);
        }

        // Exponent: e/E followed by optional +/- and digits.
        if matches!(self.peek(), Some('e') | Some('E')) {
            is_float = true;
            literal.push(self.advance().expect("peek confirmed 'e'/'E'")); // consume 'e' or 'E'
            if matches!(self.peek(), Some('+') | Some('-')) {
                literal.push(self.advance().expect("peek confirmed '+'/'-'"));
            }
            self.consume_decimal_digits(&mut literal);
        }

        // Optional type suffix: `_` followed by a type identifier (e.g., `u8`, `f64`).
        let suffix = self.try_consume_suffix();
        let full = format!("{}{}", literal, suffix.as_deref().unwrap_or(""));

        let kind = if is_float {
            TokenKind::FloatLiteral
        } else {
            TokenKind::IntLiteral
        };
        Token::new(kind, self.span_from(start), Some(full))
    }

    /// Consume characters matching `predicate` into `buf`.
    fn consume_digits(&mut self, buf: &mut String, predicate: impl Fn(char) -> bool) {
        while let Some(ch) = self.peek() {
            if predicate(ch) {
                buf.push(ch);
                self.advance();
            } else {
                break;
            }
        }
    }

    /// Consume decimal digits and underscore separators, but stop before a `_`
    /// that is immediately followed by an alphabetic character (type suffix).
    fn consume_decimal_digits(&mut self, buf: &mut String) {
        loop {
            match self.peek() {
                Some(c) if c.is_ascii_digit() => {
                    buf.push(c);
                    self.advance();
                }
                Some('_') => {
                    // Peek ahead: if next char after `_` is alphabetic, this is a
                    // type suffix — stop consuming.
                    if self.peek_next().is_some_and(|c| c.is_alphabetic()) {
                        break;
                    }
                    buf.push('_');
                    self.advance();
                }
                _ => break,
            }
        }
    }

    /// Try to consume a type suffix (`_` followed by identifier chars), returning
    /// the suffix string (including the leading `_`) if present.
    fn try_consume_suffix(&mut self) -> Option<String> {
        // A suffix starts with `_` followed immediately by an alphabetic char.
        if self.peek() == Some('_') && self.peek_next().is_some_and(|c| c.is_alphabetic()) {
            let mut suffix = String::new();
            suffix.push(self.advance().expect("peek confirmed '_'")); // '_'
            while let Some(ch) = self.peek() {
                if ch.is_alphanumeric() || ch == '_' {
                    suffix.push(ch);
                    self.advance();
                } else {
                    break;
                }
            }
            Some(suffix)
        } else {
            None
        }
    }

    /// Lex a comment, advancing past it.
    ///
    /// Regular comments (`//`, `/* */`) produce no token.
    /// Doc comments (`///`, `//!`) push a token into `self.pending` so that
    /// the next call to `next_token` returns it.
    fn lex_comment(&mut self) {
        let start = self.pos;
        // Consume the opening `/`
        self.advance();

        match self.peek() {
            Some('/') => {
                // Line comment: `//`, `///`, or `//!`
                self.advance(); // consume second `/`

                if self.peek() == Some('/') {
                    // `///` — doc comment
                    self.advance(); // consume third `/`
                    let content_start = self.pos;
                    while let Some(ch) = self.peek() {
                        if ch == '\n' {
                            break;
                        }
                        self.advance();
                    }
                    let content = self.source.content[content_start..self.pos]
                        .trim()
                        .to_owned();
                    let span = self.span_from(start);
                    self.pending
                        .push_back(Token::new(TokenKind::DocComment, span, Some(content)));
                } else if self.peek() == Some('!') {
                    // `//!` — module doc comment
                    self.advance(); // consume `!`
                    let content_start = self.pos;
                    while let Some(ch) = self.peek() {
                        if ch == '\n' {
                            break;
                        }
                        self.advance();
                    }
                    let content = self.source.content[content_start..self.pos]
                        .trim()
                        .to_owned();
                    let span = self.span_from(start);
                    self.pending.push_back(Token::new(
                        TokenKind::ModuleDocComment,
                        span,
                        Some(content),
                    ));
                } else {
                    // Regular `//` line comment — consume to end of line, emit nothing
                    while let Some(ch) = self.peek() {
                        if ch == '\n' {
                            break;
                        }
                        self.advance();
                    }
                }
            }
            Some('*') => {
                // Block comment: `/* ... */` (nestable)
                self.advance(); // consume `*`
                let mut depth: u32 = 1;
                loop {
                    match self.peek() {
                        None => {
                            let span = self.span_from(start);
                            self.diagnostics.error(
                                E_UNTERMINATED_BLOCK_COMMENT,
                                "unterminated block comment",
                                span,
                            );
                            break;
                        }
                        Some('/') => {
                            self.advance();
                            if self.peek() == Some('*') {
                                self.advance();
                                depth += 1;
                            }
                        }
                        Some('*') => {
                            self.advance();
                            if self.peek() == Some('/') {
                                self.advance();
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                        }
                        Some(_) => {
                            self.advance();
                        }
                    }
                }
            }
            _ => {
                // Shouldn't happen: caller checks that next char is `/` or `*`
            }
        }
    }

    // ── Identifier / keyword ──────────────────────────────────────────────────

    /// Lex an identifier or keyword starting at the current position.
    fn lex_ident_or_keyword(&mut self) -> Token {
        let start = self.pos;

        while let Some(ch) = self.peek() {
            if ch.is_alphanumeric() || ch == '_' {
                self.advance();
            } else {
                break;
            }
        }

        let text = &self.source.content[start..self.pos];
        let span = self.span_from(start);

        if let Some(kw) = keyword_lookup(text) {
            Token::new(kw, span, None)
        } else if text.starts_with(|c: char| c.is_uppercase()) {
            Token::new(TokenKind::TypeIdent, span, Some(text.to_owned()))
        } else if text == "_" {
            Token::new(TokenKind::Underscore, span, None)
        } else {
            Token::new(TokenKind::Ident, span, Some(text.to_owned()))
        }
    }

    // ── Operators ─────────────────────────────────────────────────────────────

    /// Lex a single operator or punctuation token.
    #[allow(clippy::too_many_lines)]
    fn lex_operator(&mut self) -> Token {
        let start = self.pos;
        let ch = self.advance().expect("called with a character available");

        let kind = match ch {
            // Single-char punctuation
            '(' => TokenKind::LParen,
            ')' => TokenKind::RParen,
            '[' => TokenKind::LBracket,
            ']' => TokenKind::RBracket,

            // `{` — track inner brace depth when inside an interpolation.
            '{' => {
                if !self.interp_brace_depth.is_empty() {
                    *self.interp_brace_depth.last_mut().expect("non-empty") += 1;
                }
                TokenKind::LBrace
            }

            // `}` — either closes an interpolation or is a normal RBrace.
            '}' => {
                if !self.interp_brace_depth.is_empty() {
                    let top = *self.interp_brace_depth.last().expect("non-empty");
                    if top == 0 {
                        // This `}` closes the interpolation.
                        self.interp_brace_depth.pop();
                        let ctx = self
                            .string_resume
                            .pop()
                            .expect("resume stack mirrors brace stack");
                        self.resume_string_lex(ctx);
                        TokenKind::InterpolationEnd
                    } else {
                        *self.interp_brace_depth.last_mut().expect("non-empty") -= 1;
                        TokenKind::RBrace
                    }
                } else {
                    TokenKind::RBrace
                }
            }

            ',' => TokenKind::Comma,
            ':' => TokenKind::Colon,
            ';' => TokenKind::Semicolon,
            '@' => TokenKind::At,
            '#' => TokenKind::Hash,
            '~' => TokenKind::BitNot,
            '^' => TokenKind::BitXor,
            '?' => TokenKind::Question,

            // `+` or `+=`
            '+' => {
                if self.peek() == Some('=') {
                    self.advance();
                    TokenKind::PlusEq
                } else {
                    TokenKind::Plus
                }
            }

            // `-` or `-=` or `->`
            '-' => match self.peek() {
                Some('=') => {
                    self.advance();
                    TokenKind::MinusEq
                }
                Some('>') => {
                    self.advance();
                    TokenKind::ThinArrow
                }
                _ => TokenKind::Minus,
            },

            // `*` or `*=` or `**`
            '*' => match self.peek() {
                Some('=') => {
                    self.advance();
                    TokenKind::StarEq
                }
                Some('*') => {
                    self.advance();
                    TokenKind::Power
                }
                _ => TokenKind::Star,
            },

            // `/` or `/=`  (comments already dispatched above)
            '/' => {
                if self.peek() == Some('=') {
                    self.advance();
                    TokenKind::SlashEq
                } else {
                    TokenKind::Slash
                }
            }

            // `%` or `%=`
            '%' => {
                if self.peek() == Some('=') {
                    self.advance();
                    TokenKind::PercentEq
                } else {
                    TokenKind::Percent
                }
            }

            // `=` or `==` or `=>`
            '=' => match self.peek() {
                Some('=') => {
                    self.advance();
                    TokenKind::Eq
                }
                Some('>') => {
                    self.advance();
                    TokenKind::FatArrow
                }
                _ => TokenKind::Assign,
            },

            // `!` or `!=`
            '!' => {
                if self.peek() == Some('=') {
                    self.advance();
                    TokenKind::Neq
                } else {
                    TokenKind::Not
                }
            }

            // `<` or `<=` or `<<`
            '<' => match self.peek() {
                Some('=') => {
                    self.advance();
                    TokenKind::Lte
                }
                Some('<') => {
                    self.advance();
                    TokenKind::Shl
                }
                _ => TokenKind::Lt,
            },

            // `>` or `>=` or `>>`
            '>' => match self.peek() {
                Some('=') => {
                    self.advance();
                    TokenKind::Gte
                }
                Some('>') => {
                    self.advance();
                    TokenKind::Shr
                }
                _ => TokenKind::Gt,
            },

            // `&` or `&&`
            '&' => {
                if self.peek() == Some('&') {
                    self.advance();
                    TokenKind::And
                } else {
                    TokenKind::BitAnd
                }
            }

            // `|` or `||` or `|>`
            '|' => match self.peek() {
                Some('|') => {
                    self.advance();
                    TokenKind::Or
                }
                Some('>') => {
                    self.advance();
                    TokenKind::Pipe
                }
                _ => TokenKind::BitOr,
            },

            // `.` or `..` or `..=`
            '.' => {
                if self.peek() == Some('.') {
                    self.advance(); // consume second '.'
                    if self.peek() == Some('=') {
                        self.advance(); // consume '='
                        TokenKind::DotDotEq
                    } else {
                        TokenKind::DotDot
                    }
                } else {
                    TokenKind::Dot
                }
            }

            // Unknown character
            other => {
                let span = self.span_from(start);
                self.diagnostics.error(
                    E_UNEXPECTED_CHAR,
                    format!("unexpected character {:?}", other),
                    span,
                );
                return Token::new(TokenKind::Error, span, Some(other.to_string()));
            }
        };

        self.make_token(kind, start)
    }
}

// ── Free helpers ──────────────────────────────────────────────────────────────

/// Determine the closing token kind for a string based on its attributes.
fn closing_kind(is_raw: bool, is_multiline: bool, is_continuation: bool) -> TokenKind {
    if is_continuation {
        TokenKind::StringLiteralPart
    } else if is_raw && is_multiline {
        TokenKind::RawMultiLineStringLiteral
    } else if is_multiline {
        TokenKind::MultiLineStringLiteral
    } else if is_raw {
        TokenKind::RawStringLiteral
    } else {
        TokenKind::StringLiteral
    }
}

/// Strip the common leading indentation from a multi-line string body.
///
/// The first line (right after the opening `"""`) is stripped if it is blank.
/// The last newline before the closing `"""` is also trimmed.
fn strip_common_indent(s: &str) -> String {
    let raw_lines: Vec<&str> = s.split('\n').collect();

    // Drop the first line if it's blank (the text after the opening `"""`).
    let lines: &[&str] = if raw_lines
        .first()
        .map(|l| l.trim().is_empty())
        .unwrap_or(false)
    {
        &raw_lines[1..]
    } else {
        &raw_lines
    };

    // Find common indentation (only non-empty lines count).
    let common = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);

    // Strip common indent from each line.
    let stripped: Vec<&str> = lines
        .iter()
        .map(|l| if l.len() >= common { &l[common..] } else { *l })
        .collect();

    let joined = stripped.join("\n");
    // Remove a single trailing newline that precedes the closing `"""`.
    joined.trim_end_matches('\n').to_string()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use bock_source::SourceFile;
    use std::path::PathBuf;

    fn lex(src: &str) -> Vec<Token> {
        let file = SourceFile::new(
            bock_errors::FileId(0),
            PathBuf::from("test.bock"),
            src.to_string(),
        );
        let mut lexer = Lexer::new(&file);
        lexer.tokenize()
    }

    fn kinds(src: &str) -> Vec<TokenKind> {
        lex(src).into_iter().map(|t| t.kind).collect()
    }

    fn literals(src: &str) -> Vec<Option<String>> {
        lex(src).into_iter().map(|t| t.literal).collect()
    }

    // ── Identifiers and keywords ───────────────────────────────────────────────

    #[test]
    fn lex_simple_identifier() {
        let toks = kinds("foo");
        assert_eq!(toks, vec![TokenKind::Ident, TokenKind::Eof]);
    }

    #[test]
    fn lex_type_identifier() {
        let toks = kinds("Foo");
        assert_eq!(toks, vec![TokenKind::TypeIdent, TokenKind::Eof]);
    }

    #[test]
    fn lex_underscore() {
        let toks = kinds("_");
        assert_eq!(toks, vec![TokenKind::Underscore, TokenKind::Eof]);
    }

    #[test]
    fn lex_underscore_ident() {
        // _foo starts with _ and has more chars → Ident
        let toks = kinds("_foo");
        assert_eq!(toks, vec![TokenKind::Ident, TokenKind::Eof]);
    }

    #[test]
    fn lex_keywords() {
        let toks = kinds("fn let mut const if else match for in while loop break continue return");
        assert_eq!(
            toks,
            vec![
                TokenKind::Fn,
                TokenKind::Let,
                TokenKind::Mut,
                TokenKind::Const,
                TokenKind::If,
                TokenKind::Else,
                TokenKind::Match,
                TokenKind::For,
                TokenKind::In,
                TokenKind::While,
                TokenKind::Loop,
                TokenKind::Break,
                TokenKind::Continue,
                TokenKind::Return,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lex_true_false_as_bool_literal() {
        let toks = kinds("true false");
        assert_eq!(
            toks,
            vec![
                TokenKind::BoolLiteral,
                TokenKind::BoolLiteral,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn bool_literal_round_trip() {
        let src = "true false";
        let tokens = lex(src);
        // Both emit BoolLiteral
        assert_eq!(tokens[0].kind, TokenKind::BoolLiteral);
        assert_eq!(tokens[1].kind, TokenKind::BoolLiteral);
        // Source text is recoverable from span
        assert_eq!(&src[tokens[0].span.start..tokens[0].span.end], "true");
        assert_eq!(&src[tokens[1].span.start..tokens[1].span.end], "false");
    }

    #[test]
    fn lex_self_keywords() {
        let toks = kinds("self Self");
        assert_eq!(
            toks,
            vec![TokenKind::SelfLower, TokenKind::SelfUpper, TokenKind::Eof]
        );
    }

    #[test]
    fn lex_ok_err_some_none() {
        let toks = kinds("Ok Err Some None");
        assert_eq!(
            toks,
            vec![
                TokenKind::Ok_,
                TokenKind::Err_,
                TokenKind::Some_,
                TokenKind::None_,
                TokenKind::Eof,
            ]
        );
    }

    // ── Operators ─────────────────────────────────────────────────────────────

    #[test]
    fn lex_single_char_ops() {
        let toks = kinds("+ - * / % ! & | ^ ~ ? # @");
        assert_eq!(
            toks,
            vec![
                TokenKind::Plus,
                TokenKind::Minus,
                TokenKind::Star,
                TokenKind::Slash,
                TokenKind::Percent,
                TokenKind::Not,
                TokenKind::BitAnd,
                TokenKind::BitOr,
                TokenKind::BitXor,
                TokenKind::BitNot,
                TokenKind::Question,
                TokenKind::Hash,
                TokenKind::At,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lex_pipe_vs_bitor() {
        let toks = kinds("|> |");
        assert_eq!(
            toks,
            vec![TokenKind::Pipe, TokenKind::BitOr, TokenKind::Eof]
        );
    }

    #[test]
    fn lex_compose() {
        // `>>` is lexed as Shr; parser re-interprets in expression context
        let toks = kinds(">>");
        assert_eq!(toks, vec![TokenKind::Shr, TokenKind::Eof]);
    }

    #[test]
    fn lex_dotdot_dotdoteq_dot() {
        let toks = kinds(". .. ..=");
        assert_eq!(
            toks,
            vec![
                TokenKind::Dot,
                TokenKind::DotDot,
                TokenKind::DotDotEq,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn lex_fat_arrow_vs_eq() {
        let toks = kinds("=> = ==");
        assert_eq!(
            toks,
            vec![
                TokenKind::FatArrow,
                TokenKind::Assign,
                TokenKind::Eq,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn lex_thin_arrow_vs_minus() {
        let toks = kinds("-> - -=");
        assert_eq!(
            toks,
            vec![
                TokenKind::ThinArrow,
                TokenKind::Minus,
                TokenKind::MinusEq,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn lex_power_vs_star() {
        let toks = kinds("** * *=");
        assert_eq!(
            toks,
            vec![
                TokenKind::Power,
                TokenKind::Star,
                TokenKind::StarEq,
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn lex_shift_ops() {
        let toks = kinds("<< >>");
        assert_eq!(toks, vec![TokenKind::Shl, TokenKind::Shr, TokenKind::Eof]);
    }

    #[test]
    fn lex_assignment_ops() {
        let toks = kinds("+= -= *= /= %=");
        assert_eq!(
            toks,
            vec![
                TokenKind::PlusEq,
                TokenKind::MinusEq,
                TokenKind::StarEq,
                TokenKind::SlashEq,
                TokenKind::PercentEq,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lex_comparison_ops() {
        let toks = kinds("== != < > <= >=");
        assert_eq!(
            toks,
            vec![
                TokenKind::Eq,
                TokenKind::Neq,
                TokenKind::Lt,
                TokenKind::Gt,
                TokenKind::Lte,
                TokenKind::Gte,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lex_logical_ops() {
        let toks = kinds("&& || !");
        assert_eq!(
            toks,
            vec![
                TokenKind::And,
                TokenKind::Or,
                TokenKind::Not,
                TokenKind::Eof
            ]
        );
    }

    // ── Punctuation ───────────────────────────────────────────────────────────

    #[test]
    fn lex_delimiters() {
        let toks = kinds("( ) [ ] { }");
        assert_eq!(
            toks,
            vec![
                TokenKind::LParen,
                TokenKind::RParen,
                TokenKind::LBracket,
                TokenKind::RBracket,
                TokenKind::LBrace,
                TokenKind::RBrace,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lex_misc_punct() {
        let toks = kinds(", : ;");
        assert_eq!(
            toks,
            vec![
                TokenKind::Comma,
                TokenKind::Colon,
                TokenKind::Semicolon,
                TokenKind::Eof
            ]
        );
    }

    // ── Newlines ──────────────────────────────────────────────────────────────

    #[test]
    fn lex_newlines() {
        let toks = kinds("foo\nbar");
        assert_eq!(
            toks,
            vec![
                TokenKind::Ident,
                TokenKind::Newline,
                TokenKind::Ident,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lex_crlf_newline() {
        let toks = kinds("foo\r\nbar");
        assert_eq!(
            toks,
            vec![
                TokenKind::Ident,
                TokenKind::Newline,
                TokenKind::Ident,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lex_multiple_newlines() {
        let toks = kinds("a\n\nb");
        assert_eq!(
            toks,
            vec![
                TokenKind::Ident,
                TokenKind::Newline,
                TokenKind::Newline,
                TokenKind::Ident,
                TokenKind::Eof,
            ]
        );
    }

    // ── Error tokens ──────────────────────────────────────────────────────────

    #[test]
    fn lex_unknown_char_produces_error() {
        let file = SourceFile::new(
            bock_errors::FileId(0),
            PathBuf::from("test.bock"),
            "§".to_string(),
        );
        let mut lexer = Lexer::new(&file);
        let toks = lexer.tokenize();
        assert_eq!(toks[0].kind, TokenKind::Error);
        assert!(lexer.diagnostics().has_errors());
    }

    // ── Integration: idents + keywords + operators ────────────────────────────

    #[test]
    fn integration_basic_function_signature() {
        // fn add(x: Int) -> Int
        let toks = kinds("fn add(x: Int) -> Int");
        assert_eq!(
            toks,
            vec![
                TokenKind::Fn,
                TokenKind::Ident, // add
                TokenKind::LParen,
                TokenKind::Ident, // x
                TokenKind::Colon,
                TokenKind::TypeIdent, // Int
                TokenKind::RParen,
                TokenKind::ThinArrow,
                TokenKind::TypeIdent, // Int
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn integration_let_binding() {
        // let x = 42  — number is dispatched to lex_number (todo! for P1.4),
        // so skip the number and just test the surrounding tokens
        let toks = kinds("let mut x =");
        assert_eq!(
            toks,
            vec![
                TokenKind::Let,
                TokenKind::Mut,
                TokenKind::Ident,
                TokenKind::Assign,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn integration_match_arm() {
        // Ok(x) => x
        let toks = kinds("Ok(x) => x");
        assert_eq!(
            toks,
            vec![
                TokenKind::Ok_,
                TokenKind::LParen,
                TokenKind::Ident,
                TokenKind::RParen,
                TokenKind::FatArrow,
                TokenKind::Ident,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn integration_pipe_expression() {
        // xs |> map |> filter
        let toks = kinds("xs |> map |> filter");
        assert_eq!(
            toks,
            vec![
                TokenKind::Ident,
                TokenKind::Pipe,
                TokenKind::Ident,
                TokenKind::Pipe,
                TokenKind::Ident,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn integration_multiline() {
        let src = "fn foo()\n  let x = y\n  x";
        let toks = kinds(src);
        // fn foo ( ) <newline> let x = y <newline> x <eof>
        // Note: `=` and `y` are operators/idents (no number), numeric `lex_number` not called
        assert_eq!(
            toks,
            vec![
                TokenKind::Fn,
                TokenKind::Ident,
                TokenKind::LParen,
                TokenKind::RParen,
                TokenKind::Newline,
                TokenKind::Let,
                TokenKind::Ident,
                TokenKind::Assign,
                TokenKind::Ident,
                TokenKind::Newline,
                TokenKind::Ident,
                TokenKind::Eof,
            ]
        );
    }

    // ── String literals (P1.3) ────────────────────────────────────────────────

    #[test]
    fn lex_plain_string() {
        let toks = lex(r#""hello""#);
        assert_eq!(toks[0].kind, TokenKind::StringLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("hello"));
        assert_eq!(toks[1].kind, TokenKind::Eof);
    }

    #[test]
    fn lex_string_escape_sequences() {
        // "a\nb\tc\\"
        let toks = lex("\"a\\nb\\tc\\\\\"");
        assert_eq!(toks[0].kind, TokenKind::StringLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("a\nb\tc\\"));
    }

    #[test]
    fn lex_string_escape_dollar() {
        let toks = lex(r#""\$""#);
        assert_eq!(toks[0].kind, TokenKind::StringLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("$"));
    }

    #[test]
    fn lex_string_double_dollar_escape() {
        // $$ is an escaped dollar in non-raw strings
        let toks = lex(r#""$$""#);
        assert_eq!(toks[0].kind, TokenKind::StringLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("$"));
    }

    #[test]
    fn lex_string_unicode_escape() {
        // "\u{41}" → "A"
        let toks = lex("\"\\u{41}\"");
        assert_eq!(toks[0].kind, TokenKind::StringLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("A"));
    }

    #[test]
    fn lex_string_unicode_escape_multibyte() {
        // "\u{1F600}" → "😀"
        let toks = lex("\"\\u{1F600}\"");
        assert_eq!(toks[0].kind, TokenKind::StringLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("😀"));
    }

    #[test]
    fn lex_raw_string() {
        let toks = lex(r#"r"hello\nworld""#);
        assert_eq!(toks[0].kind, TokenKind::RawStringLiteral);
        // Raw strings don't process escapes: backslash-n is two chars
        assert_eq!(toks[0].literal.as_deref(), Some("hello\\nworld"));
    }

    #[test]
    fn lex_raw_string_dollar_literal() {
        // In raw strings, ${ is not an interpolation
        let toks = lex(r#"r"${not interp}""#);
        assert_eq!(toks[0].kind, TokenKind::RawStringLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("${not interp}"));
    }

    #[test]
    fn lex_multiline_string() {
        let src = "\"\"\"hello world\"\"\"";
        let toks = lex(src);
        assert_eq!(toks[0].kind, TokenKind::MultiLineStringLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("hello world"));
    }

    #[test]
    fn lex_multiline_string_indent_stripping() {
        // """
        //   hello
        //   world
        // """
        let src = "\"\"\"\n  hello\n  world\n\"\"\"";
        let toks = lex(src);
        assert_eq!(toks[0].kind, TokenKind::MultiLineStringLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("hello\nworld"));
    }

    #[test]
    fn lex_raw_multiline_string() {
        let src = "r\"\"\"\nhello\\nworld\n\"\"\"";
        let toks = lex(src);
        assert_eq!(toks[0].kind, TokenKind::RawMultiLineStringLiteral);
        // Raw: no escape processing; but closing """ is consumed
        assert!(toks[0]
            .literal
            .as_deref()
            .unwrap()
            .contains("hello\\nworld"));
    }

    // ── String interpolation ──────────────────────────────────────────────────

    #[test]
    fn lex_interpolated_string_simple() {
        // "hello ${name}!"
        let toks = lex("\"hello ${name}!\"");
        // Expected: StringLiteralPart("hello "), InterpolationStart, Ident("name"),
        //           InterpolationEnd, StringLiteralPart("!"), Eof
        assert_eq!(toks[0].kind, TokenKind::StringLiteralPart);
        assert_eq!(toks[0].literal.as_deref(), Some("hello "));
        assert_eq!(toks[1].kind, TokenKind::InterpolationStart);
        assert_eq!(toks[2].kind, TokenKind::Ident);
        assert_eq!(toks[3].kind, TokenKind::InterpolationEnd);
        assert_eq!(toks[4].kind, TokenKind::StringLiteralPart);
        assert_eq!(toks[4].literal.as_deref(), Some("!"));
        assert_eq!(toks[5].kind, TokenKind::Eof);
    }

    #[test]
    fn lex_interpolated_string_nested_braces() {
        // "${f({key: val})}"  — inner {} must not close the interpolation
        let toks = lex("\"${f({key: val})}\"");
        // StringLiteralPart(""), InterpolationStart, Ident(f), LParen,
        // LBrace, Ident(key), Colon, Ident(val), RBrace,
        // RParen, InterpolationEnd, StringLiteralPart(""), Eof
        let ks: Vec<_> = toks.iter().map(|t| t.kind.clone()).collect();
        assert_eq!(
            ks,
            vec![
                TokenKind::StringLiteralPart, // ""
                TokenKind::InterpolationStart,
                TokenKind::Ident, // f
                TokenKind::LParen,
                TokenKind::LBrace,
                TokenKind::Ident, // key
                TokenKind::Colon,
                TokenKind::Ident, // val
                TokenKind::RBrace,
                TokenKind::RParen,
                TokenKind::InterpolationEnd,
                TokenKind::StringLiteralPart, // ""
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lex_interpolated_string_multiple_interps() {
        // "${a} + ${b}"
        let toks = lex("\"${a} + ${b}\"");
        let ks: Vec<_> = toks.iter().map(|t| t.kind.clone()).collect();
        assert_eq!(
            ks,
            vec![
                TokenKind::StringLiteralPart, // ""
                TokenKind::InterpolationStart,
                TokenKind::Ident, // a
                TokenKind::InterpolationEnd,
                TokenKind::StringLiteralPart, // " + "
                TokenKind::InterpolationStart,
                TokenKind::Ident, // b
                TokenKind::InterpolationEnd,
                TokenKind::StringLiteralPart, // ""
                TokenKind::Eof,
            ]
        );
        assert_eq!(toks[4].literal.as_deref(), Some(" + "));
    }

    // ── Character literals ────────────────────────────────────────────────────

    #[test]
    fn lex_char_simple() {
        let toks = lex("'a'");
        assert_eq!(toks[0].kind, TokenKind::CharLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("a"));
    }

    #[test]
    fn lex_char_newline_escape() {
        let toks = lex("'\\n'");
        assert_eq!(toks[0].kind, TokenKind::CharLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("\n"));
    }

    #[test]
    fn lex_char_unicode_escape() {
        // '\u{1F600}' → 😀
        let toks = lex("'\\u{1F600}'");
        assert_eq!(toks[0].kind, TokenKind::CharLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("😀"));
    }

    #[test]
    fn lex_char_multibyte_unicode() {
        // '😀' — a directly embedded Unicode character
        let toks = lex("'😀'");
        assert_eq!(toks[0].kind, TokenKind::CharLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("😀"));
    }

    // ── Diagnostics for invalid literals ─────────────────────────────────────

    #[test]
    fn lex_unterminated_string_produces_diagnostic() {
        let file = SourceFile::new(
            bock_errors::FileId(0),
            PathBuf::from("test.bock"),
            "\"unterminated".to_string(),
        );
        let mut lexer = Lexer::new(&file);
        let _ = lexer.tokenize();
        assert!(lexer.diagnostics().has_errors());
    }

    #[test]
    fn lex_empty_char_literal_produces_diagnostic() {
        let file = SourceFile::new(
            bock_errors::FileId(0),
            PathBuf::from("test.bock"),
            "''".to_string(),
        );
        let mut lexer = Lexer::new(&file);
        let toks = lexer.tokenize();
        assert_eq!(toks[0].kind, TokenKind::Error);
        assert!(lexer.diagnostics().has_errors());
    }

    #[test]
    fn lex_literals_helper() {
        // Smoke-test the `literals` helper used in some tests above.
        let lits = literals(r#""hi""#);
        assert_eq!(lits[0].as_deref(), Some("hi"));
    }

    // ── Numeric literals (P1.4) ───────────────────────────────────────────────

    fn lex_num(src: &str) -> Vec<Token> {
        lex(src)
    }

    #[test]
    fn lex_decimal_integer() {
        let toks = lex_num("42");
        assert_eq!(toks[0].kind, TokenKind::IntLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("42"));
    }

    #[test]
    fn lex_decimal_with_underscores() {
        let toks = lex_num("1_000_000");
        assert_eq!(toks[0].kind, TokenKind::IntLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("1_000_000"));
    }

    #[test]
    fn lex_hex_literal() {
        let toks = lex_num("0xFF");
        assert_eq!(toks[0].kind, TokenKind::IntLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("0xFF"));
    }

    #[test]
    fn lex_hex_literal_uppercase_prefix() {
        let toks = lex_num("0XFF");
        assert_eq!(toks[0].kind, TokenKind::IntLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("0XFF"));
    }

    #[test]
    fn lex_octal_literal() {
        let toks = lex_num("0o77");
        assert_eq!(toks[0].kind, TokenKind::IntLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("0o77"));
    }

    #[test]
    fn lex_octal_literal_uppercase_prefix() {
        let toks = lex_num("0O77");
        assert_eq!(toks[0].kind, TokenKind::IntLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("0O77"));
    }

    #[test]
    fn lex_binary_literal() {
        let toks = lex_num("0b1010");
        assert_eq!(toks[0].kind, TokenKind::IntLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("0b1010"));
    }

    #[test]
    fn lex_binary_literal_uppercase_prefix() {
        let toks = lex_num("0B1010");
        assert_eq!(toks[0].kind, TokenKind::IntLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("0B1010"));
    }

    #[test]
    fn lex_float_simple() {
        let toks = lex_num("3.14");
        assert_eq!(toks[0].kind, TokenKind::FloatLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("3.14"));
    }

    #[test]
    fn lex_float_exponent_lower() {
        let toks = lex_num("1.0e10");
        assert_eq!(toks[0].kind, TokenKind::FloatLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("1.0e10"));
    }

    #[test]
    fn lex_float_exponent_upper() {
        let toks = lex_num("2.5E-3");
        assert_eq!(toks[0].kind, TokenKind::FloatLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("2.5E-3"));
    }

    #[test]
    fn lex_float_exponent_no_dot() {
        // `1e6` — exponent without fractional part
        let toks = lex_num("1e6");
        assert_eq!(toks[0].kind, TokenKind::FloatLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("1e6"));
    }

    #[test]
    fn lex_float_exponent_plus() {
        let toks = lex_num("1.5E+3");
        assert_eq!(toks[0].kind, TokenKind::FloatLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("1.5E+3"));
    }

    #[test]
    fn lex_int_with_type_suffix() {
        let toks = lex_num("42_u8");
        assert_eq!(toks[0].kind, TokenKind::IntLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("42_u8"));
    }

    #[test]
    fn lex_float_with_type_suffix() {
        let toks = lex_num("3.14_f64");
        assert_eq!(toks[0].kind, TokenKind::FloatLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("3.14_f64"));
    }

    #[test]
    fn lex_range_does_not_consume_dotdot() {
        // `1..2` must produce IntLiteral(1), DotDot, IntLiteral(2) — not a float.
        let toks = lex_num("1..2");
        assert_eq!(toks[0].kind, TokenKind::IntLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("1"));
        assert_eq!(toks[1].kind, TokenKind::DotDot);
        assert_eq!(toks[2].kind, TokenKind::IntLiteral);
        assert_eq!(toks[2].literal.as_deref(), Some("2"));
    }

    #[test]
    fn lex_invalid_binary_digit_produces_diagnostic() {
        let file = bock_source::SourceFile::new(
            bock_errors::FileId(0),
            std::path::PathBuf::from("test.bock"),
            "0b123".to_string(),
        );
        let mut lexer = Lexer::new(&file);
        let _ = lexer.tokenize();
        assert!(
            !lexer.diagnostics().is_empty(),
            "expected diagnostic for invalid binary digit"
        );
    }

    #[test]
    fn lex_invalid_octal_digit_produces_diagnostic() {
        let file = bock_source::SourceFile::new(
            bock_errors::FileId(0),
            std::path::PathBuf::from("test.bock"),
            "0o89".to_string(),
        );
        let mut lexer = Lexer::new(&file);
        let _ = lexer.tokenize();
        assert!(
            !lexer.diagnostics().is_empty(),
            "expected diagnostic for invalid octal digit"
        );
    }

    #[test]
    fn lex_hex_with_underscores() {
        let toks = lex_num("0xFF_FF");
        assert_eq!(toks[0].kind, TokenKind::IntLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("0xFF_FF"));
    }

    #[test]
    fn lex_zero_alone() {
        let toks = lex_num("0");
        assert_eq!(toks[0].kind, TokenKind::IntLiteral);
        assert_eq!(toks[0].literal.as_deref(), Some("0"));
    }

    // ── Comments (P1.5) ───────────────────────────────────────────────────────

    fn has_errors(src: &str) -> bool {
        let file = SourceFile::new(
            bock_errors::FileId(0),
            std::path::PathBuf::from("test.bock"),
            src.to_string(),
        );
        let mut lexer = Lexer::new(&file);
        let _ = lexer.tokenize();
        !lexer.diagnostics().is_empty()
    }

    #[test]
    fn lex_line_comment_produces_no_token() {
        // A line comment before an identifier should be invisible
        let toks = kinds("// this is a comment\nfoo");
        assert_eq!(
            toks,
            vec![TokenKind::Newline, TokenKind::Ident, TokenKind::Eof]
        );
    }

    #[test]
    fn lex_line_comment_at_eof() {
        // A line comment at end of file with no trailing newline
        let toks = kinds("// comment at eof");
        assert_eq!(toks, vec![TokenKind::Eof]);
    }

    #[test]
    fn lex_doc_comment_produces_token() {
        let toks = lex("/// doc comment");
        assert_eq!(toks[0].kind, TokenKind::DocComment);
        assert_eq!(toks[0].literal.as_deref(), Some("doc comment"));
    }

    #[test]
    fn lex_doc_comment_content_trimmed() {
        let toks = lex("///   spaces around   ");
        assert_eq!(toks[0].kind, TokenKind::DocComment);
        assert_eq!(toks[0].literal.as_deref(), Some("spaces around"));
    }

    #[test]
    fn lex_module_doc_comment_produces_token() {
        let toks = lex("//! module doc");
        assert_eq!(toks[0].kind, TokenKind::ModuleDocComment);
        assert_eq!(toks[0].literal.as_deref(), Some("module doc"));
    }

    #[test]
    fn lex_doc_comment_then_ident() {
        let toks = kinds("/// docs\nfoo");
        assert_eq!(
            toks,
            vec![
                TokenKind::DocComment,
                TokenKind::Newline,
                TokenKind::Ident,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn lex_block_comment_produces_no_token() {
        let toks = kinds("/* block comment */ foo");
        assert_eq!(toks, vec![TokenKind::Ident, TokenKind::Eof]);
    }

    #[test]
    fn lex_nested_block_comment() {
        // Nested block comments must be properly balanced
        let toks = kinds("/* outer /* inner */ still outer */ foo");
        assert_eq!(toks, vec![TokenKind::Ident, TokenKind::Eof]);
    }

    #[test]
    fn lex_deeply_nested_block_comment() {
        let toks = kinds("/* a /* b /* c */ b */ a */ x");
        assert_eq!(toks, vec![TokenKind::Ident, TokenKind::Eof]);
    }

    #[test]
    fn lex_unterminated_block_comment_produces_diagnostic() {
        assert!(
            has_errors("/* not closed"),
            "expected diagnostic for unterminated block comment"
        );
    }

    #[test]
    fn lex_block_comment_inline() {
        // Block comment between tokens
        let toks = kinds("foo /* ignore */ bar");
        assert_eq!(
            toks,
            vec![TokenKind::Ident, TokenKind::Ident, TokenKind::Eof]
        );
    }

    // ── M-010: Raw multiline string distinct token kind ──────────────────────

    #[test]
    fn raw_multiline_string_has_distinct_kind() {
        let src = "r\"\"\"\nhello\n\"\"\"";
        let toks = lex(src);
        assert_eq!(toks[0].kind, TokenKind::RawMultiLineStringLiteral);
        // Non-raw multiline should still be MultiLineStringLiteral
        let toks2 = lex("\"\"\"\nhello\n\"\"\"");
        assert_eq!(toks2[0].kind, TokenKind::MultiLineStringLiteral);
    }

    // ── M-011: Backslash line continuation ───────────────────────────────────

    #[test]
    fn backslash_newline_joins_lines() {
        // `let \\\nx = 1` should lex as `let x = 1`
        let toks = kinds("let \\\nx = 1");
        assert_eq!(
            toks,
            vec![
                TokenKind::Let,
                TokenKind::Ident,
                TokenKind::Assign,
                TokenKind::IntLiteral,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn backslash_without_newline_is_error() {
        let toks = lex("\\x");
        assert_eq!(toks[0].kind, TokenKind::Error);
    }

    #[test]
    fn backslash_continuation_multiline_expr() {
        // Multi-line expression: `1 + \\\n  2 + \\\n  3`
        let toks = kinds("1 + \\\n  2 + \\\n  3");
        assert_eq!(
            toks,
            vec![
                TokenKind::IntLiteral,
                TokenKind::Plus,
                TokenKind::IntLiteral,
                TokenKind::Plus,
                TokenKind::IntLiteral,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn backslash_at_eof_is_error() {
        let toks = lex("\\");
        assert_eq!(toks[0].kind, TokenKind::Error);
    }
}
