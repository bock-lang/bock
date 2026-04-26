//! Test harness for Bock spec conformance tests.
//!
//! `.bock` files use comment directives to declare their name and expected
//! outcomes.  The harness parses these directives; actual execution is wired
//! in as compiler phases are implemented.
//!
//! # Directive format
//!
//! ```bock
//! // TEST: <name>
//! // EXPECT: no_errors
//! // EXPECT: error E0205 at 3:10
//! // EXPECT: output "hello world"
//! ```
//!
//! All directives must appear at the **top** of the file (before any
//! non-directive, non-blank lines).

pub mod expectation;

use std::{
    fs,
    path::{Path, PathBuf},
};

pub use expectation::{Expectation, ParseError};

/// A parsed test case loaded from a single `.bock` file.
#[derive(Debug, Clone)]
pub struct TestCase {
    /// Path to the source file.
    pub path: PathBuf,
    /// Value of the `// TEST: <name>` directive.
    pub name: String,
    /// All `// EXPECT: …` directives, in order.
    pub expectations: Vec<Expectation>,
    /// The Bock source text (everything after the directives).
    pub source: String,
}

/// Error produced when a `.bock` file cannot be loaded or its directives are invalid.
#[derive(Debug)]
pub enum LoadError {
    Io(std::io::Error),
    MissingTestDirective(PathBuf),
    BadExpectation(PathBuf, ParseError),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "I/O error: {}", e),
            LoadError::MissingTestDirective(p) => {
                write!(f, "{}: missing `// TEST: <name>` directive", p.display())
            }
            LoadError::BadExpectation(p, e) => {
                write!(f, "{}: {}", p.display(), e)
            }
        }
    }
}

impl From<std::io::Error> for LoadError {
    fn from(e: std::io::Error) -> Self {
        LoadError::Io(e)
    }
}

/// Load and parse a single `.bock` test file.
pub fn load_test_file(path: &Path) -> Result<TestCase, LoadError> {
    let content = fs::read_to_string(path)?;
    parse_test_file(path, &content)
}

/// Parse directives from `content` as if it came from `path`.
pub fn parse_test_file(path: &Path, content: &str) -> Result<TestCase, LoadError> {
    let mut test_name: Option<String> = None;
    let mut expectations: Vec<Expectation> = Vec::new();
    let mut source_start = 0usize;

    for (idx, line) in content.lines().enumerate() {
        let line_number = idx + 1;
        let trimmed = line.trim();

        if trimmed.is_empty() {
            // blank lines between directives are fine
            source_start += line.len() + 1;
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("// TEST: ") {
            test_name = Some(rest.trim().to_string());
            source_start += line.len() + 1;
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("// EXPECT: ") {
            match expectation::parse_expectation(rest, line_number) {
                Ok(Some(exp)) => expectations.push(exp),
                Ok(None) => {} // unknown — ignore for forward compat
                Err(e) => return Err(LoadError::BadExpectation(path.to_path_buf(), e)),
            }
            source_start += line.len() + 1;
            continue;
        }

        // First non-directive line: the rest is source.
        break;
    }

    let name = test_name.ok_or_else(|| LoadError::MissingTestDirective(path.to_path_buf()))?;

    let source = content[source_start.min(content.len())..].to_string();

    Ok(TestCase {
        path: path.to_path_buf(),
        name,
        expectations,
        source,
    })
}

/// Discover all `.bock` test files under `dir` (recursively).
///
/// Files that fail to parse produce a `LoadError` entry; successfully parsed
/// files produce a `TestCase`.  Both are returned so callers can decide how
/// to handle errors.
pub fn discover_tests(dir: &Path) -> Vec<Result<TestCase, LoadError>> {
    let mut results = Vec::new();
    collect_bock_files(dir, &mut results);
    results
}

fn collect_bock_files(dir: &Path, out: &mut Vec<Result<TestCase, LoadError>>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            out.push(Err(LoadError::Io(e)));
            return;
        }
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort(); // deterministic order
    for path in paths {
        if path.is_dir() {
            collect_bock_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("bock") {
            out.push(load_test_file(&path));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fake_path() -> PathBuf {
        PathBuf::from("test.bock")
    }

    #[test]
    fn parse_no_errors_case() {
        let src = "// TEST: parse_success\n// EXPECT: no_errors\nfn add(a: Int, b: Int) -> Int { a + b }\n";
        let tc = parse_test_file(&fake_path(), src).unwrap();
        assert_eq!(tc.name, "parse_success");
        assert_eq!(tc.expectations, vec![Expectation::NoErrors]);
        assert!(tc.source.contains("fn add"));
    }

    #[test]
    fn parse_error_at_case() {
        let src = "// TEST: type_error\n// EXPECT: error E0205 at 3:10\nfn broken() -> Int { \"not an int\" }\n";
        let tc = parse_test_file(&fake_path(), src).unwrap();
        assert_eq!(tc.name, "type_error");
        assert_eq!(tc.expectations.len(), 1);
        matches!(tc.expectations[0], Expectation::ErrorAt { .. });
    }

    #[test]
    fn parse_output_case() {
        let src = "// TEST: interpreter_output\n// EXPECT: output \"hello world\"\nfn main() { println(\"hello world\") }\n";
        let tc = parse_test_file(&fake_path(), src).unwrap();
        assert_eq!(tc.name, "interpreter_output");
        assert_eq!(
            tc.expectations,
            vec![Expectation::Output("hello world".to_string())]
        );
    }

    #[test]
    fn missing_test_directive_is_error() {
        let src = "// EXPECT: no_errors\nfn foo() {}\n";
        let result = parse_test_file(&fake_path(), src);
        assert!(matches!(result, Err(LoadError::MissingTestDirective(_))));
    }

    #[test]
    fn multiple_expectations() {
        let src = "// TEST: multi\n// EXPECT: no_errors\n// EXPECT: output \"ok\"\nfn main() {}\n";
        let tc = parse_test_file(&fake_path(), src).unwrap();
        assert_eq!(tc.expectations.len(), 2);
    }

    #[test]
    fn discover_spec_fixtures() {
        // Resolve path relative to the workspace root via CARGO_MANIFEST_DIR.
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let spec_dir = manifest.join("conformance");
        let results = discover_tests(&spec_dir);
        // We should find at least our 5 example files, all parseable.
        assert!(
            results.len() >= 5,
            "expected at least 5 conformance fixtures, got {}",
            results.len()
        );
        for result in &results {
            if let Err(e) = result {
                panic!("fixture failed to load: {}", e);
            }
        }
        let names: Vec<&str> = results
            .iter()
            .map(|r| r.as_ref().unwrap().name.as_str())
            .collect();
        assert!(
            names.contains(&"parse_success_add"),
            "missing parse_success_add"
        );
        assert!(
            names.contains(&"interpreter_output_hello"),
            "missing interpreter_output_hello"
        );
    }

    #[test]
    fn blank_lines_between_directives() {
        let src = "// TEST: spaced\n\n// EXPECT: no_errors\n\nfn foo() {}\n";
        let tc = parse_test_file(&fake_path(), src).unwrap();
        assert_eq!(tc.name, "spaced");
        assert_eq!(tc.expectations, vec![Expectation::NoErrors]);
    }
}
