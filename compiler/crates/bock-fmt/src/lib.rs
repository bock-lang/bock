//! Bock fmt — canonical source formatter for Bock programs.
//!
//! Zero-configuration, opinionated formatter that parses Bock source code
//! and re-emits it in canonical style.
//!
//! # Formatting rules
//! - 2-space indent, no tabs
//! - 80 char soft limit, 100 hard limit
//! - Opening brace same line
//! - Trailing commas in multi-line constructs
//! - Import sorting: core → std → external → local
//! - Short signatures on one line, long signatures wrap per-param

mod comments;
mod emit;

#[cfg(test)]
mod tests;

use bock_lexer::Lexer;
use bock_parser::Parser;
use bock_source::SourceFile;

pub use emit::Formatter;

/// Result of formatting a source file.
#[derive(Debug)]
pub struct FormatResult {
    /// The formatted source text.
    pub output: String,
    /// Whether the output differs from the input.
    pub changed: bool,
}

/// Format a Bock source string, returning the formatted output.
///
/// Parses the source, reformats the AST, and emits canonical source text.
/// If parsing produces errors, the original source is returned unchanged.
#[must_use]
pub fn format_source(source: &str, filename: &str) -> FormatResult {
    let file = SourceFile::new(
        bock_errors::FileId(0),
        std::path::PathBuf::from(filename),
        source.to_string(),
    );
    let mut lexer = Lexer::new(&file);
    let tokens = lexer.tokenize();

    // If lexer had errors, return unchanged
    if lexer.diagnostics().has_errors() {
        return FormatResult {
            output: source.to_string(),
            changed: false,
        };
    }

    let mut parser = Parser::new(tokens, &file);
    let module = parser.parse_module();

    // If parser had errors, return unchanged
    if parser.diagnostics().has_errors() {
        return FormatResult {
            output: source.to_string(),
            changed: false,
        };
    }

    let comments = comments::extract_comments(source);
    let mut formatter = Formatter::new(&comments, source);
    formatter.format_module(&module);
    let output = formatter.finish();

    let changed = output != source;
    FormatResult { output, changed }
}
