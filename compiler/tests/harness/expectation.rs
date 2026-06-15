//! Parsing of `// EXPECT:` directives from `.bock` test files.

use std::collections::BTreeSet;
use std::fmt;

/// A location referenced in an error expectation (`line:col`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    pub line: u32,
    pub col: u32,
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.line, self.col)
    }
}

/// A single expectation parsed from an `// EXPECT: …` directive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expectation {
    /// `// EXPECT: no_errors` — the file should produce no diagnostics.
    NoErrors,
    /// `// EXPECT: error E<code> at <line>:<col>` — a specific error at a location.
    ErrorAt { code: String, location: Location },
    /// `// EXPECT: output "<text>"` — the program should print this text.
    /// The quoted value supports `\n`/`\t`/`\"`/`\\` escapes, so a program
    /// that prints multiple lines (one `println` per line) declares its expected
    /// stdout with `\n`. See `decode_output_escapes` for the decoder.
    Output(String),
    /// `// EXPECT: targets <a>, <b>, ...` — restrict execution to the listed
    /// transpilation targets (by id: `js`, `ts`, `python`, `rust`, `go`).
    /// Absent ⇒ the fixture runs on every target. Lets a fixture exercise a
    /// backend-specific defect without failing on targets where the relevant
    /// feature is not yet supported.
    Targets(BTreeSet<String>),
}

impl fmt::Display for Expectation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expectation::NoErrors => write!(f, "no_errors"),
            Expectation::ErrorAt { code, location } => {
                write!(f, "error {} at {}", code, location)
            }
            Expectation::Output(text) => write!(f, "output {:?}", text),
            Expectation::Targets(targets) => {
                write!(
                    f,
                    "targets {}",
                    targets.iter().cloned().collect::<Vec<_>>().join(", ")
                )
            }
        }
    }
}

/// Parse error returned when a directive line is malformed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line_number: usize,
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line_number, self.message)
    }
}

/// Parse a single `EXPECT` value string (everything after `// EXPECT: `).
///
/// Unrecognised expectation values are a **hard error** (typo guard). They
/// used to be silently ignored "for forward compatibility", which let a
/// fixture declare `// EXPECT: no errors` (typo for `no_errors`) and run
/// expectation-free for weeks without anyone noticing
/// (Q-conformance-directive-wiring). A new expectation type must be added to
/// this parser (and to the assertion paths in `execution.rs`) before any
/// fixture may use it.
pub fn parse_expectation(value: &str, line_number: usize) -> Result<Expectation, ParseError> {
    let value = value.trim();

    if value == "no_errors" {
        return Ok(Expectation::NoErrors);
    }

    if let Some(rest) = value.strip_prefix("error ") {
        // `error E<code> at <line>:<col>`
        let parts: Vec<&str> = rest.splitn(3, ' ').collect();
        if parts.len() == 3 && parts[1] == "at" {
            let code = parts[0].to_string();
            let loc_str = parts[2];
            if let Some(loc) = parse_location(loc_str) {
                return Ok(Expectation::ErrorAt {
                    code,
                    location: loc,
                });
            }
        }
        return Err(ParseError {
            line_number,
            message: format!(
                "malformed error expectation {:?}; expected `error E<code> at <line>:<col>`",
                value
            ),
        });
    }

    if let Some(rest) = value.strip_prefix("output ") {
        // `output "<text>"`
        let text = rest.trim();
        if text.starts_with('"') && text.ends_with('"') && text.len() >= 2 {
            let inner = &text[1..text.len() - 1];
            return decode_output_escapes(inner, line_number).map(Expectation::Output);
        }
        return Err(ParseError {
            line_number,
            message: format!(
                "malformed output expectation {:?}; expected `output \"<text>\"`",
                value
            ),
        });
    }

    if let Some(rest) = value.strip_prefix("targets ") {
        // `targets <id>, <id>, ...`
        let mut set = BTreeSet::new();
        for tok in rest.split(',') {
            let tok = tok.trim();
            if !tok.is_empty() {
                set.insert(tok.to_string());
            }
        }
        if set.is_empty() {
            return Err(ParseError {
                line_number,
                message: format!(
                    "malformed targets expectation {value:?}; expected `targets <id>, ...`"
                ),
            });
        }
        return Ok(Expectation::Targets(set));
    }

    // Unknown expectation value — hard error (typo guard; see the fn docs).
    Err(ParseError {
        line_number,
        message: format!(
            "unknown expectation {value:?}; known forms: `no_errors`, \
             `error E<code> at <line>:<col>`, `output \"<text>\"`, `targets <id>, ...`"
        ),
    })
}

/// Decode the backslash escapes permitted inside an `// EXPECT: output "..."`
/// directive's quoted text.
///
/// A directive line is a single physical line, so a fixture whose program emits
/// **multiple lines** of stdout (one `println` per line) expresses its expected
/// output with `\n`. The recognised escapes are `\n` (newline), `\t` (tab),
/// `\"` (a literal double-quote inside the value) and `\\` (a literal
/// backslash). Any other escape — e.g. a stray `\x` — is a hard error (the
/// typo-guard discipline: a malformed escape must never be silently passed
/// through and then mismatch at run time). A value with no backslash decodes to
/// itself unchanged, so every pre-escape directive keeps its exact meaning.
fn decode_output_escapes(inner: &str, line_number: usize) -> Result<String, ParseError> {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            other => {
                return Err(ParseError {
                    line_number,
                    message: format!(
                        "invalid escape `\\{}` in output expectation; \
                         supported escapes are `\\n`, `\\t`, `\\\"`, `\\\\`",
                        other.map(|c| c.to_string()).unwrap_or_default()
                    ),
                });
            }
        }
    }
    Ok(out)
}

fn parse_location(s: &str) -> Option<Location> {
    let (line_str, col_str) = s.split_once(':')?;
    let line = line_str.trim().parse().ok()?;
    let col = col_str.trim().parse().ok()?;
    Some(Location { line, col })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_errors() {
        let exp = parse_expectation("no_errors", 1).unwrap();
        assert_eq!(exp, Expectation::NoErrors);
    }

    #[test]
    fn test_error_at() {
        let exp = parse_expectation("error E0205 at 3:10", 1).unwrap();
        assert_eq!(
            exp,
            Expectation::ErrorAt {
                code: "E0205".to_string(),
                location: Location { line: 3, col: 10 },
            }
        );
    }

    #[test]
    fn test_output() {
        let exp = parse_expectation("output \"hello world\"", 1).unwrap();
        assert_eq!(exp, Expectation::Output("hello world".to_string()));
    }

    #[test]
    fn test_output_newline_escape() {
        // A multi-line program (one `println` per line) expresses its expected
        // stdout with `\n`; the decoder turns it into a real newline.
        let exp = parse_expectation("output \"a\\nb\\nc\"", 1).unwrap();
        assert_eq!(exp, Expectation::Output("a\nb\nc".to_string()));
    }

    #[test]
    fn test_output_escapes_tab_quote_backslash() {
        let exp = parse_expectation("output \"x\\ty\\\"z\\\\w\"", 1).unwrap();
        assert_eq!(exp, Expectation::Output("x\ty\"z\\w".to_string()));
    }

    #[test]
    fn test_output_no_backslash_is_verbatim() {
        // A value with no backslash decodes to itself: pre-escape directives
        // keep their exact meaning.
        let exp = parse_expectation("output \"3;1,2,3;1|2,3\"", 1).unwrap();
        assert_eq!(exp, Expectation::Output("3;1,2,3;1|2,3".to_string()));
    }

    #[test]
    fn test_output_invalid_escape_is_error() {
        // A stray `\x` is a hard error (typo guard for escapes) — never silently
        // passed through to mismatch at run time.
        let result = parse_expectation("output \"bad\\xescape\"", 9);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().line_number, 9);
    }

    #[test]
    fn test_unknown_is_error() {
        // Typo guard: an unrecognised expectation value must be a hard error,
        // not silently ignored — `// EXPECT: run_in_future_phase` (or a typo
        // like `no errors`) must never run expectation-free.
        let result = parse_expectation("run_in_future_phase", 1);
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("unknown expectation"));
    }

    #[test]
    fn test_no_errors_typo_is_error() {
        // The exact typo class that motivated the guard: `no errors` (space
        // instead of underscore) ran unasserted in types/fn_type_param.bock.
        let result = parse_expectation("no errors", 2);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().line_number, 2);
    }

    #[test]
    fn test_malformed_error() {
        let result = parse_expectation("error missing_location", 5);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().line_number, 5);
    }

    #[test]
    fn test_malformed_output() {
        let result = parse_expectation("output not_quoted", 7);
        assert!(result.is_err());
    }

    #[test]
    fn test_targets() {
        let exp = parse_expectation("targets go, rust , js", 1).unwrap();
        let mut want = BTreeSet::new();
        want.insert("go".to_string());
        want.insert("rust".to_string());
        want.insert("js".to_string());
        assert_eq!(exp, Expectation::Targets(want));
    }

    #[test]
    fn test_targets_empty_is_error() {
        // The prefix is present but yields no target tokens (only separators).
        let result = parse_expectation("targets ,,", 3);
        assert!(result.is_err());
    }

    #[test]
    fn test_targets_bare_word_is_error() {
        // `targets` with no trailing space/args is malformed; under the typo
        // guard it is a hard error (it used to be silently ignored).
        let result = parse_expectation("targets", 3);
        assert!(result.is_err());
    }
}
