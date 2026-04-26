//! Parsing of `// EXPECT:` directives from `.bock` test files.

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
    /// `// EXPECT: output "<text>"` — the interpreter should print this text.
    Output(String),
}

impl fmt::Display for Expectation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expectation::NoErrors => write!(f, "no_errors"),
            Expectation::ErrorAt { code, location } => {
                write!(f, "error {} at {}", code, location)
            }
            Expectation::Output(text) => write!(f, "output {:?}", text),
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
/// Returns `Ok(None)` for unrecognised (future) expectation types so the
/// harness can be extended without breaking existing tests.
pub fn parse_expectation(
    value: &str,
    line_number: usize,
) -> Result<Option<Expectation>, ParseError> {
    let value = value.trim();

    if value == "no_errors" {
        return Ok(Some(Expectation::NoErrors));
    }

    if let Some(rest) = value.strip_prefix("error ") {
        // `error E<code> at <line>:<col>`
        let parts: Vec<&str> = rest.splitn(3, ' ').collect();
        if parts.len() == 3 && parts[1] == "at" {
            let code = parts[0].to_string();
            let loc_str = parts[2];
            if let Some(loc) = parse_location(loc_str) {
                return Ok(Some(Expectation::ErrorAt {
                    code,
                    location: loc,
                }));
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
            return Ok(Some(Expectation::Output(inner.to_string())));
        }
        return Err(ParseError {
            line_number,
            message: format!(
                "malformed output expectation {:?}; expected `output \"<text>\"`",
                value
            ),
        });
    }

    // Unknown expectation type — forward-compatible: ignore silently.
    Ok(None)
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
        let exp = parse_expectation("no_errors", 1).unwrap().unwrap();
        assert_eq!(exp, Expectation::NoErrors);
    }

    #[test]
    fn test_error_at() {
        let exp = parse_expectation("error E0205 at 3:10", 1)
            .unwrap()
            .unwrap();
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
        let exp = parse_expectation("output \"hello world\"", 1)
            .unwrap()
            .unwrap();
        assert_eq!(exp, Expectation::Output("hello world".to_string()));
    }

    #[test]
    fn test_unknown_is_none() {
        let result = parse_expectation("run_in_future_phase", 1).unwrap();
        assert!(result.is_none());
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
}
