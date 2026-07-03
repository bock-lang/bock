//! Integration tests for `--format json` — the machine-readable structured
//! output on `bock check`, `bock test`, and `bock inspect`.
//!
//! Pins the public machine contract end-to-end through the binary:
//!
//! * stdout carries exactly ONE JSON document — the whole stream is parsed
//!   with `serde_json::from_slice`, which rejects trailing garbage, so a
//!   passing parse also pins stdout purity (no human lines, no ANSI, no
//!   second document);
//! * the shared envelope: `format_version` / `command` / `outcome` /
//!   `summary`, plus the per-command payload array;
//! * diagnostics serialize from the structured layer (severity, code,
//!   message, span `{file, start, end, line, col}`, suggestion) — the same
//!   `Diagnostic` values the human renderer consumes;
//! * exit codes are identical to human mode;
//! * `inspect air --json` keeps its established (non-envelope) tree
//!   contract, and `--format json` there is an alias for it.

use std::io::Write;
use std::process::Command;

use tempfile::NamedTempFile;

fn bock_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_bock"))
}

fn write_temp_file(content: &str) -> NamedTempFile {
    let mut f = NamedTempFile::with_suffix(".bock").unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f.flush().unwrap();
    f
}

fn assert_exit_code(output: &std::process::Output, expected: i32, ctx: &str) {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "{ctx}: expected exit {expected}, got {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Parse stdout as exactly one JSON document, panicking with context on any
/// impurity (a human line before/after the document, a second document, or
/// ANSI escapes would all fail the parse or the escape assertion).
fn parse_stdout(output: &std::process::Output) -> serde_json::Value {
    assert!(
        !output.stdout.contains(&0x1b),
        "stdout must not contain ANSI escapes: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout must be exactly one JSON document: {e}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

/// Assert the shared machine-output envelope.
fn assert_envelope(doc: &serde_json::Value, command: &str, outcome: &str) {
    assert_eq!(doc["format_version"], 1, "format_version: {doc}");
    assert_eq!(doc["command"], command, "command: {doc}");
    assert_eq!(doc["outcome"], outcome, "outcome: {doc}");
    assert!(doc["summary"].is_object(), "summary: {doc}");
}

/// Assert one diagnostic entry satisfies the contract shape.
fn assert_diagnostic_contract(d: &serde_json::Value, ctx: &str) {
    assert!(d["severity"].is_string(), "{ctx}: severity: {d}");
    let code = d["code"]
        .as_str()
        .unwrap_or_else(|| panic!("{ctx}: code must be a string: {d}"));
    assert_eq!(
        code.len(),
        5,
        "{ctx}: code is a 5-char code like E4002: {d}"
    );
    assert!(d["message"].is_string(), "{ctx}: message: {d}");
    let span = &d["span"];
    assert!(
        span["file"].is_string() || span["file"].is_null(),
        "{ctx}: span.file is string|null: {d}"
    );
    for field in ["start", "end", "line", "col"] {
        assert!(span[field].is_u64(), "{ctx}: span.{field}: {d}");
    }
    assert!(
        span["line"].as_u64().unwrap() >= 1 && span["col"].as_u64().unwrap() >= 1,
        "{ctx}: line/col are 1-based: {d}"
    );
    assert!(
        span["start"].as_u64().unwrap() <= span["end"].as_u64().unwrap(),
        "{ctx}: start <= end: {d}"
    );
    assert!(
        d.get("suggestion").is_some(),
        "{ctx}: suggestion key must be present (string or null): {d}"
    );
}

// ─── bock check --format json ───────────────────────────────────────────────

#[test]
fn check_json_clean_file() {
    let f = write_temp_file("fn add(a: Int, b: Int) -> Int { a + b }\n");
    let output = bock_bin()
        .arg("check")
        .arg("--format=json")
        .arg(f.path())
        .output()
        .unwrap();
    assert_exit_code(&output, 0, "clean check, json");

    let doc = parse_stdout(&output);
    assert_envelope(&doc, "check", "clean");
    assert_eq!(doc["summary"]["files"], 1);
    assert_eq!(doc["summary"]["errors"], 0);
    assert_eq!(doc["summary"]["warnings"], 0);
    assert!(
        doc["diagnostics"].as_array().unwrap().is_empty(),
        "clean check → empty diagnostics: {doc}"
    );
}

#[test]
fn check_json_parse_error_carries_structured_diagnostics() {
    let f = write_temp_file("fn { broken\n");
    let output = bock_bin()
        .arg("check")
        .arg("--format=json")
        .arg(f.path())
        .output()
        .unwrap();
    assert_exit_code(&output, 1, "parse error, json");

    let doc = parse_stdout(&output);
    assert_envelope(&doc, "check", "failed");
    let errors = doc["summary"]["errors"].as_u64().unwrap();
    assert!(errors >= 1, "at least one error counted: {doc}");

    let diags = doc["diagnostics"].as_array().unwrap();
    assert!(!diags.is_empty(), "diagnostics must be present: {doc}");
    for (i, d) in diags.iter().enumerate() {
        assert_diagnostic_contract(d, &format!("diag {i}"));
    }
    let first = &diags[0];
    assert_eq!(first["severity"], "error");
    assert!(
        first["code"].as_str().unwrap().starts_with('E'),
        "error code prefix: {first}"
    );
    assert!(
        first["span"]["file"].as_str().unwrap().ends_with(".bock"),
        "span names the input file: {first}"
    );
}

#[test]
fn check_json_warnings_appear_but_outcome_stays_clean() {
    // Missing @context on a public item is a warning at the default
    // (development) strictness: it must appear in the document while the
    // outcome — and the exit code — stay clean, exactly as in human mode.
    let f = write_temp_file("module Lib\n\npublic fn add(a: Int, b: Int) -> Int { a + b }\n");
    let output = bock_bin()
        .arg("check")
        .arg("--format=json")
        .arg(f.path())
        .output()
        .unwrap();
    assert_exit_code(&output, 0, "warning-only check, json");

    let doc = parse_stdout(&output);
    assert_envelope(&doc, "check", "clean");
    assert!(
        doc["summary"]["warnings"].as_u64().unwrap() >= 1,
        "warnings counted: {doc}"
    );
    assert_eq!(doc["summary"]["errors"], 0);
    let diags = doc["diagnostics"].as_array().unwrap();
    let warning = diags
        .iter()
        .find(|d| d["severity"] == "warning")
        .unwrap_or_else(|| panic!("a warning diagnostic must surface: {doc}"));
    assert_diagnostic_contract(warning, "warning");
    assert!(
        warning["code"].as_str().unwrap().starts_with('W'),
        "warning code prefix: {warning}"
    );
}

#[test]
fn check_json_module_cycle_has_code_and_suggestion() {
    // The E1008 cycle diagnostic carries its fix hint as a note — in JSON
    // that lands in `suggestion`, non-null.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("a.bock"),
        "module a\nuse b.{ fromB }\npublic fn fromA() -> Int { 1 }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("b.bock"),
        "module b\nuse a.{ fromA }\npublic fn fromB() -> Int { 2 }\n",
    )
    .unwrap();

    let output = bock_bin()
        .arg("check")
        .arg("--format=json")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_exit_code(&output, 1, "module cycle, json");

    let doc = parse_stdout(&output);
    assert_envelope(&doc, "check", "failed");
    let diags = doc["diagnostics"].as_array().unwrap();
    let cycle = diags
        .iter()
        .find(|d| d["code"] == "E1008")
        .unwrap_or_else(|| panic!("E1008 must surface: {doc}"));
    assert!(
        cycle["message"]
            .as_str()
            .unwrap()
            .contains("circular module dependency"),
        "message: {cycle}"
    );
    assert!(
        cycle["suggestion"].as_str().unwrap().contains("Break it"),
        "the note becomes the suggestion: {cycle}"
    );
}

#[test]
fn check_json_missing_file_fails_with_document() {
    // A missing input file fails the check; stdout still carries exactly one
    // document (the unreadable-file reason goes to stderr — a known
    // unstructured path).
    let output = bock_bin()
        .arg("check")
        .arg("--format=json")
        .arg("/tmp/nonexistent_bock_format_json_12345.bock")
        .output()
        .unwrap();
    assert_exit_code(&output, 1, "missing file, json");
    let doc = parse_stdout(&output);
    assert_envelope(&doc, "check", "failed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("nonexistent_bock_format_json_12345.bock"),
        "stderr names the file: {stderr}"
    );
}

#[test]
fn check_json_rejects_unknown_format_value() {
    let f = write_temp_file("fn add(a: Int, b: Int) -> Int { a + b }\n");
    let output = bock_bin()
        .arg("check")
        .arg("--format=xml")
        .arg(f.path())
        .output()
        .unwrap();
    assert!(!output.status.success(), "unknown format must be rejected");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("possible values"),
        "clap lists the valid formats: {stderr}"
    );
}

// ─── bock test --format json ────────────────────────────────────────────────

#[test]
fn test_json_passing_and_failing_tests() {
    let f = write_temp_file(
        r#"@test
fn test_ok() {
    expect(1 + 1).to_equal(2)
}

@test
fn test_bad() {
    expect(1 + 1).to_equal(3)
}
"#,
    );
    let output = bock_bin()
        .arg("test")
        .arg("--format=json")
        .arg(f.path())
        .output()
        .unwrap();
    assert_exit_code(&output, 1, "one failing test, json");

    let doc = parse_stdout(&output);
    assert_envelope(&doc, "test", "failed");
    assert_eq!(doc["summary"]["tests"], 2);
    assert_eq!(doc["summary"]["passed"], 1);
    assert_eq!(doc["summary"]["failed"], 1);

    let tests = doc["tests"].as_array().unwrap();
    assert_eq!(tests.len(), 2);
    let ok = tests
        .iter()
        .find(|t| t["name"].as_str().unwrap().ends_with("::test_ok"))
        .unwrap_or_else(|| panic!("test_ok entry: {doc}"));
    assert_eq!(ok["passed"], true);
    assert!(ok["message"].is_null(), "passing test → null message: {ok}");
    assert!(
        ok["file"].as_str().unwrap().ends_with(".bock"),
        "file recorded: {ok}"
    );

    let bad = tests
        .iter()
        .find(|t| t["name"].as_str().unwrap().ends_with("::test_bad"))
        .unwrap_or_else(|| panic!("test_bad entry: {doc}"));
    assert_eq!(bad["passed"], false);
    assert!(
        bad["message"]
            .as_str()
            .unwrap()
            .contains("assertion failed"),
        "failure message: {bad}"
    );
}

#[test]
fn test_json_all_passing_exits_0() {
    let f = write_temp_file(
        r#"@test
fn test_ok() {
    expect(2 + 2).to_equal(4)
}
"#,
    );
    let output = bock_bin()
        .arg("test")
        .arg("--format=json")
        .arg(f.path())
        .output()
        .unwrap();
    assert_exit_code(&output, 0, "all passing, json");
    let doc = parse_stdout(&output);
    assert_envelope(&doc, "test", "clean");
    assert_eq!(doc["summary"]["failed"], 0);
}

#[test]
fn test_json_no_tests_found_is_clean_document() {
    // "No tests found." is a human message; in json mode stdout is still one
    // document with zero counts (and exit 0, matching human mode).
    let f = write_temp_file("fn not_a_test() -> Int { 1 }\n");
    let output = bock_bin()
        .arg("test")
        .arg("--format=json")
        .arg(f.path())
        .output()
        .unwrap();
    assert_exit_code(&output, 0, "no tests, json");
    let doc = parse_stdout(&output);
    assert_envelope(&doc, "test", "clean");
    assert_eq!(doc["summary"]["tests"], 0);
    assert!(doc["tests"].as_array().unwrap().is_empty());
}

#[test]
fn test_json_compile_error_becomes_failed_entry() {
    let f = write_temp_file("fn { broken\n");
    let output = bock_bin()
        .arg("test")
        .arg("--format=json")
        .arg(f.path())
        .output()
        .unwrap();
    assert_exit_code(&output, 1, "compile error, json");
    let doc = parse_stdout(&output);
    assert_envelope(&doc, "test", "failed");
    let tests = doc["tests"].as_array().unwrap();
    assert_eq!(tests.len(), 1);
    assert_eq!(tests[0]["passed"], false);
    assert!(
        tests[0]["message"]
            .as_str()
            .unwrap()
            .starts_with("compilation error:"),
        "compile failures are structured entries: {doc}"
    );
}

// ─── bock inspect --format json ─────────────────────────────────────────────

/// Seed a minimal project with one build-scope decision (mirrors the
/// `d9_commands.rs` fixture).
fn seed_project_with_decision(dir: &std::path::Path) {
    std::fs::write(dir.join("bock.project"), "[project]\nname = \"t\"\n").unwrap();
    let manifest = dir.join(".bock/decisions/build/src/api.bock.json");
    std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    let entry = serde_json::json!([{
        "id": "abc123",
        "module": "src/api.bock",
        "target": "js",
        "decision_type": "codegen",
        "choice": "// stub code",
        "alternatives": [],
        "reasoning": "stub",
        "model_id": "stub:stub",
        "confidence": 0.9,
        "pinned": false,
        "pin_reason": null,
        "pinned_at": null,
        "pinned_by": null,
        "timestamp": "2026-04-22T10:00:00Z"
    }]);
    std::fs::write(&manifest, serde_json::to_string_pretty(&entry).unwrap()).unwrap();
}

#[test]
fn inspect_json_decisions_envelope_wraps_legacy_entries() {
    let dir = tempfile::tempdir().unwrap();
    seed_project_with_decision(dir.path());

    let output = bock_bin()
        .args(["inspect", "--format=json"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_exit_code(&output, 0, "inspect decisions, json");

    let doc = parse_stdout(&output);
    assert_envelope(&doc, "inspect", "clean");
    assert_eq!(doc["summary"]["decisions"], 1);
    let decisions = doc["decisions"].as_array().unwrap();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0]["scope"], "build");
    assert_eq!(decisions[0]["prefixed_id"], "build:abc123");
    assert_eq!(decisions[0]["decision"]["id"], "abc123");

    // The envelope's entries are exactly the legacy `--json` array entries.
    let legacy = bock_bin()
        .args(["inspect", "--json"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_exit_code(&legacy, 0, "inspect decisions, legacy --json");
    let legacy_doc = parse_stdout(&legacy);
    let legacy_entries = legacy_doc
        .as_array()
        .unwrap_or_else(|| panic!("legacy --json stays a bare array: {legacy_doc}"));
    assert_eq!(*legacy_entries, *decisions, "the two forms must not drift");
}

#[test]
fn inspect_json_conflicts_with_legacy_flag() {
    let dir = tempfile::tempdir().unwrap();
    seed_project_with_decision(dir.path());
    let output = bock_bin()
        .args(["inspect", "--json", "--format=json"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "--json and --format are mutually exclusive"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("cannot be used with"),
        "clap reports the conflict: {stderr}"
    );
}

// ─── bock inspect air: --format json is an alias for --json ────────────────

#[test]
fn inspect_air_format_json_emits_the_established_tree_not_the_envelope() {
    let f = write_temp_file("fn add(a: Int, b: Int) -> Int { a + b }\n");
    let output = bock_bin()
        .args(["inspect", "air"])
        .arg(f.path())
        .arg("--format=json")
        .output()
        .unwrap();
    assert_exit_code(&output, 0, "inspect air --format json");

    let doc = parse_stdout(&output);
    // The AIR tree contract, unchanged: a root Module node, no envelope.
    assert_eq!(doc["kind"], "Module");
    assert!(
        doc.get("format_version").is_none(),
        "inspect air keeps its established tree shape: {doc}"
    );
    assert!(doc["children"].is_array());
}
