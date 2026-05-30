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
//! // EXPECT: targets go, rust, js
//! ```
//!
//! All directives must appear at the **top** of the file (before any
//! non-directive, non-blank lines).
//!
//! `// EXPECT: targets <ids>` restricts an execution fixture to the listed
//! transpilation targets (`js`, `ts`, `python`, `rust`, `go`); when absent the
//! fixture runs on every target. It lets a fixture exercise a backend-specific
//! defect without failing on targets where the relevant feature is not yet
//! supported.

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
    /// The Bock source text for the **entry** module (everything after the
    /// directives, up to the first `// FILE:` marker).
    pub source: String,
    /// Auxiliary source files for a **multi-file** fixture, as
    /// `(relative-path, content)` pairs. A fixture declares an extra module
    /// with a `// FILE: <relpath>` marker line; everything until the next
    /// `// FILE:` marker (or EOF) is that file's content. Empty for ordinary
    /// single-file fixtures. The harness writes each pair into the temp project
    /// alongside the entry `main.bock`, so the build's recursive `.bock`
    /// discovery picks them up — exercising the real cross-module `use` path.
    pub aux_files: Vec<(PathBuf, String)>,
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

    let body = &content[source_start.min(content.len())..];
    let (source, aux_files) = split_file_sections(body);

    Ok(TestCase {
        path: path.to_path_buf(),
        name,
        expectations,
        source,
        aux_files,
    })
}

/// Split a fixture body into the entry-module source and any auxiliary files.
///
/// A `// FILE: <relpath>` marker line begins an auxiliary source file; its
/// content runs until the next `// FILE:` marker or end of input. Everything
/// before the first marker is the entry module's source. This lets one fixture
/// describe a multi-file project (e.g. a `main` module that `use`s a sibling
/// user module) so the cross-module `use` path can be exercised end to end.
fn split_file_sections(body: &str) -> (String, Vec<(PathBuf, String)>) {
    let mut entry = String::new();
    let mut aux: Vec<(PathBuf, String)> = Vec::new();
    let mut current: Option<(PathBuf, String)> = None;

    for line in body.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("// FILE:") {
            if let Some(pair) = current.take() {
                aux.push(pair);
            }
            current = Some((PathBuf::from(rest.trim()), String::new()));
            continue;
        }
        match current.as_mut() {
            Some((_, buf)) => {
                buf.push_str(line);
                buf.push('\n');
            }
            None => {
                entry.push_str(line);
                entry.push('\n');
            }
        }
    }
    if let Some(pair) = current.take() {
        aux.push(pair);
    }
    (entry, aux)
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
    fn stdlib_error_fixtures_parse_directives() {
        // The `core.error` conformance fixtures must load cleanly and expose
        // their directives (the descriptive `//` comment block after the
        // directives must not swallow the TEST/EXPECT lines).
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dir = manifest.join("conformance/stdlib/error");
        let results = discover_tests(&dir);
        assert!(
            results.len() >= 3,
            "expected at least 3 stdlib/error fixtures, got {}",
            results.len()
        );
        let mut cases = Vec::new();
        for result in &results {
            match result {
                Ok(tc) => cases.push(tc),
                Err(e) => panic!("stdlib/error fixture failed to load: {e}"),
            }
        }
        let by_name = |n: &str| cases.iter().find(|c| c.name == n);

        let trait_case =
            by_name("stdlib_error_trait_resolves").expect("missing stdlib_error_trait_resolves");
        assert_eq!(trait_case.expectations, vec![Expectation::NoErrors]);

        let ctor_case = by_name("stdlib_error_construct_and_use")
            .expect("missing stdlib_error_construct_and_use");
        assert_eq!(ctor_case.expectations, vec![Expectation::NoErrors]);

        let output_case =
            by_name("stdlib_error_output_smoke").expect("missing stdlib_error_output_smoke");
        assert_eq!(
            output_case.expectations,
            vec![Expectation::Output("boom".to_string())]
        );
    }

    #[test]
    fn stdlib_convert_fixtures_parse_directives() {
        // The `core.convert` conformance fixtures must load cleanly and expose
        // their directives (the descriptive `//` block must not swallow the
        // TEST/EXPECT lines, and the narrowing fixture's `error E<code> at
        // <line>:<col>` directive must parse).
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dir = manifest.join("conformance/stdlib/convert");
        let results = discover_tests(&dir);
        assert!(
            results.len() >= 4,
            "expected at least 4 stdlib/convert fixtures, got {}",
            results.len()
        );
        let mut cases = Vec::new();
        for result in &results {
            match result {
                Ok(tc) => cases.push(tc),
                Err(e) => panic!("stdlib/convert fixture failed to load: {e}"),
            }
        }
        let by_name = |n: &str| cases.iter().find(|c| c.name == n);

        assert_eq!(
            by_name("stdlib_convert_user_from_into")
                .expect("missing stdlib_convert_user_from_into")
                .expectations,
            vec![Expectation::NoErrors]
        );
        assert_eq!(
            by_name("stdlib_convert_primitive_conversions")
                .expect("missing stdlib_convert_primitive_conversions")
                .expectations,
            vec![Expectation::NoErrors]
        );
        let narrowing = by_name("stdlib_convert_primitive_narrowing_excluded")
            .expect("missing stdlib_convert_primitive_narrowing_excluded");
        assert_eq!(
            narrowing.expectations,
            vec![Expectation::ErrorAt {
                code: "E4012".to_string(),
                location: expectation::Location { line: 11, col: 3 },
            }]
        );
    }

    #[test]
    fn stdlib_prelude_fixtures_parse_directives() {
        // The §18.2 prelude conformance fixtures must load cleanly and expose
        // their directives. Every fixture is `no_errors`: each names prelude
        // symbols (`Ordering`/`Less`/`Comparable`/`From`/`Into`/`Error`/…)
        // WITHOUT a `use`, proving the auto-import.
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dir = manifest.join("conformance/stdlib/prelude");
        let results = discover_tests(&dir);
        assert!(
            results.len() >= 7,
            "expected at least 7 stdlib/prelude fixtures, got {}",
            results.len()
        );
        let mut cases = Vec::new();
        for result in &results {
            match result {
                Ok(tc) => cases.push(tc),
                Err(e) => panic!("stdlib/prelude fixture failed to load: {e}"),
            }
        }
        let by_name = |n: &str| cases.iter().find(|c| c.name == n);

        for name in [
            "stdlib_prelude_ordering_match",
            "stdlib_prelude_ordering_value",
            "stdlib_prelude_equatable",
            "stdlib_prelude_convert_from_into",
            "stdlib_prelude_displayable",
            "stdlib_prelude_error",
            "stdlib_prelude_builtins_no_use",
        ] {
            let case = by_name(name).unwrap_or_else(|| panic!("missing {name}"));
            assert_eq!(
                case.expectations,
                vec![Expectation::NoErrors],
                "{name} should expect no_errors",
            );
        }
    }

    #[test]
    fn blank_lines_between_directives() {
        let src = "// TEST: spaced\n\n// EXPECT: no_errors\n\nfn foo() {}\n";
        let tc = parse_test_file(&fake_path(), src).unwrap();
        assert_eq!(tc.name, "spaced");
        assert_eq!(tc.expectations, vec![Expectation::NoErrors]);
        assert!(tc.aux_files.is_empty());
    }

    #[test]
    fn single_file_fixture_has_no_aux_files() {
        let src = "// TEST: solo\nmodule main\nfn main() {}\n";
        let tc = parse_test_file(&fake_path(), src).unwrap();
        assert!(tc.source.contains("module main"));
        assert!(tc.aux_files.is_empty());
    }

    #[test]
    fn file_marker_splits_multi_file_fixture() {
        let src = "// TEST: multi_file\n// EXPECT: output \"ok\"\n\
            module main\nuse util.{f}\n\
            // FILE: util.bock\nmodule util\npublic fn f() {}\n";
        let tc = parse_test_file(&fake_path(), src).unwrap();
        // Entry source is everything before the first `// FILE:` marker.
        assert!(tc.source.contains("module main"));
        assert!(tc.source.contains("use util.{f}"));
        assert!(!tc.source.contains("module util"));
        // The aux file carries the marker's path and the section's content.
        assert_eq!(tc.aux_files.len(), 1);
        assert_eq!(tc.aux_files[0].0, PathBuf::from("util.bock"));
        assert!(tc.aux_files[0].1.contains("module util"));
        assert!(tc.aux_files[0].1.contains("public fn f()"));
    }
}
