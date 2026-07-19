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
//!   contract, and `--format json` there is an alias for it;
//! * the usage-error boundary: post-clap usage errors (e.g. an unknown
//!   `--only` aspect) emit an `outcome: "usage-error"` document, while
//!   errors clap itself raises stay clap-native on stderr with an empty
//!   stdout;
//! * I/O-class failures (unreadable input, no files found) surface in the
//!   `check` document as `code: null` entries, and `test` compile errors
//!   surface as structured diagnostics in a top-level `diagnostics` array.

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
    // document and the reason also goes to stderr. (The document additionally
    // carries an I/O-class entry — pinned separately by
    // check_json_missing_file_carries_io_diagnostic_entry.)
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

#[test]
fn test_json_compile_error_carries_structured_diagnostics() {
    // A file that fails to compile keeps its pinned failed entry (message
    // starts with `compilation error:`) AND now carries the structured
    // diagnostics in the document's top-level `diagnostics` array — the
    // same entry shape `bock check` emits, serialized from the diagnostic
    // layer rather than re-rendered text.
    let f = write_temp_file("fn { broken\n");
    let output = bock_bin()
        .arg("test")
        .arg("--format=json")
        .arg(f.path())
        .output()
        .unwrap();
    assert_exit_code(&output, 1, "compile error, json, structured");

    let doc = parse_stdout(&output);
    assert_envelope(&doc, "test", "failed");

    // The pinned per-file failed entry stays, with a concise message.
    let tests = doc["tests"].as_array().unwrap();
    assert_eq!(tests.len(), 1);
    assert_eq!(tests[0]["passed"], false);
    let message = tests[0]["message"].as_str().unwrap();
    assert!(
        message.starts_with("compilation error:"),
        "entry keeps its prefix: {message}"
    );
    assert!(
        !message.contains('\n'),
        "the entry message is a one-line summary, not rendered text: {message:?}"
    );

    // The structured diagnostics ride alongside.
    let diags = doc["diagnostics"].as_array().unwrap();
    assert!(!diags.is_empty(), "diagnostics must be present: {doc}");
    for (i, d) in diags.iter().enumerate() {
        assert_diagnostic_contract(d, &format!("test compile diag {i}"));
    }
    assert_eq!(diags[0]["severity"], "error");
    assert!(
        diags[0]["code"].as_str().unwrap().starts_with('E'),
        "error code prefix: {}",
        diags[0]
    );
    assert!(
        diags[0]["span"]["file"]
            .as_str()
            .unwrap()
            .ends_with(".bock"),
        "span names the input file: {}",
        diags[0]
    );
}

#[test]
fn test_json_clean_run_has_empty_diagnostics_array() {
    // The additive top-level `diagnostics` array is present on every test
    // document — empty when every file compiled.
    let f = write_temp_file(
        r#"@test
fn test_ok() {
    expect(1 + 1).to_equal(2)
}
"#,
    );
    let output = bock_bin()
        .arg("test")
        .arg("--format=json")
        .arg(f.path())
        .output()
        .unwrap();
    assert_exit_code(&output, 0, "clean test run, json");
    let doc = parse_stdout(&output);
    assert!(
        doc["diagnostics"].as_array().unwrap().is_empty(),
        "clean run → empty diagnostics array: {doc}"
    );
}

// ─── Usage-error boundary (post-clap vs clap-native) ────────────────────────

#[test]
fn check_json_unknown_only_aspect_emits_usage_error_document() {
    // A usage-class error detected by our own command code after clap parsed
    // argv (json mode is known): stdout carries exactly one document with
    // `outcome: "usage-error"`, the problem in `error.message`, and an empty
    // payload array. Exit code unchanged from human mode.
    let f = write_temp_file("fn add(a: Int, b: Int) -> Int { a + b }\n");
    let output = bock_bin()
        .arg("check")
        .arg("--format=json")
        .arg("--only=bogus")
        .arg(f.path())
        .output()
        .unwrap();
    assert_exit_code(&output, 1, "unknown --only aspect, json");

    let doc = parse_stdout(&output);
    assert_eq!(doc["format_version"], 1, "format_version: {doc}");
    assert_eq!(doc["command"], "check", "command: {doc}");
    assert_eq!(doc["outcome"], "usage-error", "outcome: {doc}");
    assert!(doc["summary"].is_object(), "summary: {doc}");
    let message = doc["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("bogus"),
        "message names the offending value: {message}"
    );
    assert!(
        message.contains("types") && message.contains("context"),
        "message lists the valid aspects: {message}"
    );
    assert!(
        doc["diagnostics"].as_array().unwrap().is_empty(),
        "usage-error keeps an empty payload array: {doc}"
    );
}

#[test]
fn check_human_unknown_only_aspect_keeps_stderr_and_empty_stdout() {
    // The human side of the same usage error is unchanged: the message on
    // stderr, nothing on stdout.
    let f = write_temp_file("fn add(a: Int, b: Int) -> Int { a + b }\n");
    let output = bock_bin()
        .arg("check")
        .arg("--only=bogus")
        .arg(f.path())
        .output()
        .unwrap();
    assert_exit_code(&output, 1, "unknown --only aspect, human");
    assert!(
        output.stdout.is_empty(),
        "human usage errors write nothing to stdout: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown check aspect 'bogus'"),
        "stderr carries the message: {stderr}"
    );
}

#[test]
fn check_json_clap_level_errors_stay_native_with_empty_stdout() {
    // The other side of the boundary: errors clap itself raises before our
    // command code runs stay clap-native on stderr — no JSON document, even
    // when `--format=json` appears in argv.
    let f = write_temp_file("fn add(a: Int, b: Int) -> Int { a + b }\n");

    // An unknown flag.
    let output = bock_bin()
        .arg("check")
        .arg("--format=json")
        .arg("--definitely-not-a-flag")
        .arg(f.path())
        .output()
        .unwrap();
    assert!(!output.status.success(), "unknown flag must fail");
    assert!(
        output.stdout.is_empty(),
        "clap-level errors emit no stdout document: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--definitely-not-a-flag"),
        "clap names the flag on stderr: {stderr}"
    );

    // An invalid --format value (clap rejects the value itself).
    let output = bock_bin()
        .arg("check")
        .arg("--format=xml")
        .arg(f.path())
        .output()
        .unwrap();
    assert!(!output.status.success(), "unknown format must fail");
    assert!(
        output.stdout.is_empty(),
        "bad --format value emits no stdout document: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
}

// ─── I/O-class failures reach the check document ────────────────────────────

#[test]
fn check_json_missing_file_carries_io_diagnostic_entry() {
    // An unreadable input file: the document itself now says why the check
    // failed — an I/O-class entry with `code: null` (no catalog code is
    // minted for I/O failures) and the path in `span.file`. The stderr line
    // stays (pinned by check_json_missing_file_fails_with_document).
    let missing = "/tmp/nonexistent_bock_io_entry_67890.bock";
    let output = bock_bin()
        .arg("check")
        .arg("--format=json")
        .arg(missing)
        .output()
        .unwrap();
    assert_exit_code(&output, 1, "missing file, json, io entry");

    let doc = parse_stdout(&output);
    assert_envelope(&doc, "check", "failed");
    assert!(
        doc["summary"]["errors"].as_u64().unwrap() >= 1,
        "the I/O entry counts as an error: {doc}"
    );
    let diags = doc["diagnostics"].as_array().unwrap();
    let io = diags
        .iter()
        .find(|d| d["code"].is_null())
        .unwrap_or_else(|| panic!("an I/O-class entry must surface: {doc}"));
    assert_eq!(io["severity"], "error");
    assert_eq!(io["span"]["file"], missing, "span names the path: {io}");
    assert!(
        !io["message"].as_str().unwrap().is_empty(),
        "message carries the I/O reason: {io}"
    );
    assert!(io["suggestion"].is_null());
}

#[test]
fn check_json_no_files_found_document_names_the_reason() {
    // Zero `.bock` files discovered: the check fails (exit 1, unchanged) and
    // the document now carries the reason as an I/O-class entry with
    // `span.file: null`, instead of being silent about why nothing ran.
    let dir = tempfile::tempdir().unwrap();
    let output = bock_bin()
        .arg("check")
        .arg("--format=json")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_exit_code(&output, 1, "no files found, json");

    let doc = parse_stdout(&output);
    assert_envelope(&doc, "check", "failed");
    assert_eq!(doc["summary"]["files"], 0);
    let diags = doc["diagnostics"].as_array().unwrap();
    assert_eq!(diags.len(), 1, "exactly the I/O entry: {doc}");
    assert_eq!(diags[0]["severity"], "error");
    assert!(
        diags[0]["code"].is_null(),
        "I/O entries have no code: {doc}"
    );
    assert_eq!(diags[0]["message"], "No .bock files found.");
    assert!(diags[0]["span"]["file"].is_null());
    // The stderr line stays in json mode too.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No .bock files found."),
        "stderr keeps the human line: {stderr}"
    );
}

#[test]
fn check_human_no_files_found_stays_stderr_only() {
    // Human mode for the same failure is unchanged: the line on stderr,
    // nothing on stdout, exit 1.
    let dir = tempfile::tempdir().unwrap();
    let output = bock_bin()
        .arg("check")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_exit_code(&output, 1, "no files found, human");
    assert!(
        output.stdout.is_empty(),
        "human mode writes nothing to stdout on this failure: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No .bock files found."),
        "stderr carries the reason: {stderr}"
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
