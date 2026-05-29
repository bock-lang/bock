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

/// Assert that a finished process exited with exactly the given code.
///
/// The check command's exit contract is binary: 0 on a clean check, 1 on any
/// error. Asserting the exact code (not just `success()`/`!success()`) pins the
/// contract so the refactor away from scattered `process::exit(1)` to a
/// centralized `ExitCode` mapping cannot silently drift.
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

#[test]
fn check_valid_file_exits_0() {
    let f = write_temp_file("fn add(a: Int, b: Int) -> Int { a + b }\n");
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "clean check");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no errors"), "stdout: {stdout}");
}

#[test]
fn check_syntax_error_exits_1() {
    let f = write_temp_file("fn { broken\n");
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 1, "parse error");
}

#[test]
fn check_file_not_found_exits_1() {
    let output = bock_bin()
        .arg("check")
        .arg("/tmp/nonexistent_bock_file_12345.bock")
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "expected non-zero exit for missing file",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("nonexistent_bock_file_12345.bock"),
        "stderr should mention the file: {stderr}",
    );
}

// ─── Aspect surface: --only / --brief (§20.1.1) ────────────────────────────
//
// These migrate the pre-amendment `--types` / `--lint` / `--no-context` cases
// to the spec-aligned `--only=<aspect>` / `--brief` surface and add coverage
// for comma-separated lists, repeated flags, and unknown-aspect rejection.

#[test]
fn check_brief_flag_disables_source_context() {
    // Migrated from --no-context: --brief yields compact one-line diagnostics
    // with no source-context snippet (no caret/underline render).
    let f = write_temp_file("fn { broken\n");
    let output = bock_bin()
        .arg("check")
        .arg("--brief")
        .arg(f.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Brief format is the bracketed one-line form: `error[CODE]: msg (at file:..)`.
    assert!(
        stderr.contains("error["),
        "brief output should use bracket format: {stderr}",
    );
    assert!(
        stderr.contains("(at "),
        "brief output should include the compact `(at file:..)` location: {stderr}",
    );
    // Compact output omits source-context rendering: the rich renderer draws a
    // source snippet with box-drawing connectors (╭ │ ┬ ╰); brief never does.
    for snippet_char in ['\u{256d}', '\u{2502}', '\u{252c}', '\u{2570}'] {
        assert!(
            !stderr.contains(snippet_char),
            "brief output should omit source-context snippets (found {snippet_char:?}): {stderr}",
        );
    }
    // Every error line is the single-line bracketed form; the diagnostic count
    // equals the number of `error[` lines (no multi-line snippet spans).
    let diag_lines = stderr.lines().filter(|l| l.contains("error[")).count();
    assert!(
        diag_lines >= 1,
        "expected at least one diagnostic: {stderr}"
    );
}

#[test]
fn check_only_types_passes_clean_file() {
    // Migrated from --types: --only=types runs just the type-checking aspect.
    let f = write_temp_file("fn add(a: Int, b: Int) -> Int { a + b }\n");
    let output = bock_bin()
        .arg("check")
        .arg("--only=types")
        .arg(f.path())
        .output()
        .unwrap();
    assert_exit_code(&output, 0, "--only=types on a clean file");
}

#[test]
fn check_only_context_passes_clean_file() {
    // The `context` aspect maps to §11 capability verification.
    let f = write_temp_file("fn add(a: Int, b: Int) -> Int { a + b }\n");
    let output = bock_bin()
        .arg("check")
        .arg("--only=context")
        .arg(f.path())
        .output()
        .unwrap();
    assert_exit_code(&output, 0, "--only=context on a clean file");
}

#[test]
fn check_only_comma_list_passes_clean_file() {
    let f = write_temp_file("fn add(a: Int, b: Int) -> Int { a + b }\n");
    let output = bock_bin()
        .arg("check")
        .arg("--only=types,context")
        .arg(f.path())
        .output()
        .unwrap();
    assert_exit_code(&output, 0, "--only=types,context on a clean file");
}

#[test]
fn check_only_repeated_flag_passes_clean_file() {
    // Repeated --only is equivalent to a comma-separated list.
    let f = write_temp_file("fn add(a: Int, b: Int) -> Int { a + b }\n");
    let output = bock_bin()
        .arg("check")
        .arg("--only=types")
        .arg("--only=context")
        .arg(f.path())
        .output()
        .unwrap();
    assert_exit_code(&output, 0, "repeated --only on a clean file");
}

#[test]
fn check_only_unknown_aspect_is_rejected() {
    // `lint` is a v1.x aspect — rejected as unknown in v1, just like a typo.
    for bad in ["lint", "ownership", "typos"] {
        let f = write_temp_file("fn add(a: Int, b: Int) -> Int { a + b }\n");
        let output = bock_bin()
            .arg("check")
            .arg(format!("--only={bad}"))
            .arg(f.path())
            .output()
            .unwrap();
        assert_exit_code(&output, 1, &format!("--only={bad}"));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(bad),
            "error should name the offending aspect '{bad}': {stderr}",
        );
        // The error must list the valid v1 aspects.
        assert!(
            stderr.contains("types") && stderr.contains("context"),
            "error should list valid aspects (types, context): {stderr}",
        );
    }
}

#[test]
fn check_only_unknown_aspect_in_list_is_rejected() {
    // A bad value mixed with a good one still rejects the whole invocation.
    let f = write_temp_file("fn add(a: Int, b: Int) -> Int { a + b }\n");
    let output = bock_bin()
        .arg("check")
        .arg("--only=types,bogus")
        .arg(f.path())
        .output()
        .unwrap();
    assert_exit_code(&output, 1, "--only=types,bogus");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("bogus"),
        "error should name the offending aspect: {stderr}",
    );
}

#[test]
fn check_no_flag_runs_full_check() {
    // Omitting --only runs the full check (all passes), unchanged from before.
    let f = write_temp_file("fn add(a: Int, b: Int) -> Int { a + b }\n");
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "full check, no --only");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no errors"), "stdout: {stdout}");
}

#[test]
fn check_no_files_in_empty_dir() {
    let dir = tempfile::tempdir().unwrap();
    let output = bock_bin()
        .arg("check")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_exit_code(&output, 1, "no .bock files found");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("No .bock files found"), "stderr: {stderr}",);
}

#[test]
fn check_reports_ownership_error() {
    // A function that moves a record and then uses it should trigger an ownership error.
    // (Primitives like Int have copy semantics and won't trigger this.)
    let f = write_temp_file(
        "record Thing { id: Int }\nfn process() {\n    let data = Thing { id: 1 }\n    let archive = data\n    let x = data\n}\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert!(
        !output.status.success(),
        "expected non-zero exit for ownership error, got {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("moved") || stderr.contains("E5001"),
        "stderr should mention moved variable or E5001: {stderr}",
    );
}

#[test]
fn check_multiple_files() {
    let f1 = write_temp_file("fn foo() -> Int { 1 }\n");
    let f2 = write_temp_file("fn bar() -> Int { 2 }\n");
    let output = bock_bin()
        .arg("check")
        .arg(f1.path())
        .arg(f2.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "expected exit 0 for two valid files, got {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("2 files checked"), "stdout: {stdout}");
}

#[test]
fn check_multifile_dependency_ordering() {
    // Two-module project: main imports from models.
    // Tests that the dependency graph correctly orders compilation
    // (Models compiled before Main since Main depends on Models).
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join("models.bock"),
        "module Models\n\npublic record User {\n    id: Int\n    name: String\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("main.bock"),
        "module Main\n\nuse Models.{User}\n\nfn make_user() -> Int {\n    42\n}\n",
    )
    .unwrap();

    let output = bock_bin()
        .arg("check")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "expected exit 0 for multi-file project, got {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("2 files checked"), "stdout: {stdout}");
}

#[test]
fn check_circular_dependency_detected() {
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join("a.bock"),
        "module A\n\nuse B.{foo}\n\nfn bar() -> Int { 1 }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("b.bock"),
        "module B\n\nuse A.{bar}\n\nfn foo() -> Int { 2 }\n",
    )
    .unwrap();

    let output = bock_bin()
        .arg("check")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_exit_code(&output, 1, "circular dependency");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("circular"),
        "stderr should mention circular dependency: {stderr}",
    );
}

#[test]
fn check_multifile_error_shows_correct_path() {
    // A valid file and a file with a type error — the error message
    // should reference the correct file path.
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(dir.path().join("good.bock"), "fn helper() -> Int { 42 }\n").unwrap();
    std::fs::write(dir.path().join("bad.bock"), "fn broken( { }\n").unwrap();

    let output = bock_bin()
        .arg("check")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "expected non-zero exit due to parse error",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("bad.bock"),
        "error should reference bad.bock: {stderr}",
    );
}

// ─── Cross-file type checking (B.6) ────────────────────────────────────────

#[test]
fn check_crossfile_record_construct_and_field_access() {
    // models.bock exports User record; service.bock imports and uses it.
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join("models.bock"),
        "\
module Models

public record User {
    name: String
    age: Int
}
",
    )
    .unwrap();

    std::fs::write(
        dir.path().join("service.bock"),
        "\
module Service

use Models.{User}

fn create_user(name: String, age: Int) -> User {
    User { name: name, age: age }
}

fn get_name(u: User) -> String {
    u.name
}

fn get_age(u: User) -> Int {
    u.age
}
",
    )
    .unwrap();

    let output = bock_bin()
        .arg("check")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "cross-file record construct/access should pass type checking\nstderr: {stderr}",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("2 files checked"), "stdout: {stdout}");
}

#[test]
fn check_crossfile_enum_match() {
    // models.bock exports Role enum; service.bock imports and matches on it.
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join("models.bock"),
        "\
module Models

public enum Role {
    Admin
    Member
    Guest
}
",
    )
    .unwrap();

    std::fs::write(
        dir.path().join("service.bock"),
        "\
module Service

use Models.{Role, Admin, Member, Guest}

fn role_level(r: Role) -> Int {
    match (r) {
        Admin => 3
        Member => 2
        Guest => 1
    }
}
",
    )
    .unwrap();

    let output = bock_bin()
        .arg("check")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "cross-file enum match should pass type checking\nstderr: {stderr}",
    );
}

#[test]
fn check_crossfile_function_call() {
    // lib.bock exports a function; main.bock imports and calls it.
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join("lib.bock"),
        "\
module Lib

public fn add(a: Int, b: Int) -> Int {
    a + b
}
",
    )
    .unwrap();

    std::fs::write(
        dir.path().join("main.bock"),
        "\
module Main

use Lib.{add}

fn main() -> Int {
    add(1, 2)
}
",
    )
    .unwrap();

    let output = bock_bin()
        .arg("check")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "cross-file function call should pass type checking\nstderr: {stderr}",
    );
}

#[test]
fn check_crossfile_imported_type_in_signature() {
    // User is imported and used as parameter and return type.
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join("types.bock"),
        "\
module Types

public record Point {
    x: Int
    y: Int
}
",
    )
    .unwrap();

    std::fs::write(
        dir.path().join("geometry.bock"),
        "\
module Geometry

use Types.{Point}

fn origin() -> Point {
    Point { x: 0, y: 0 }
}

fn translate(p: Point, dx: Int, dy: Int) -> Point {
    Point { x: p.x + dx, y: p.y + dy }
}
",
    )
    .unwrap();

    let output = bock_bin()
        .arg("check")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "imported type in function signatures should pass\nstderr: {stderr}",
    );
}

#[test]
fn check_crossfile_three_modules() {
    // Three-module chain: Types → Models → Service
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join("types.bock"),
        "\
module Types

public record User {
    name: String
    age: Int
}

public enum Role {
    Admin
    Member
    Guest
}
",
    )
    .unwrap();

    std::fs::write(
        dir.path().join("models.bock"),
        "\
module Models

use Types.{User, Role, Admin}

public fn is_admin(r: Role) -> Bool {
    match (r) {
        Admin => true
        _ => false
    }
}

public fn make_user(name: String) -> User {
    User { name: name, age: 0 }
}
",
    )
    .unwrap();

    std::fs::write(
        dir.path().join("service.bock"),
        "\
module Service

use Types.{User}
use Models.{make_user, is_admin}

fn create_default_user() -> User {
    make_user(\"anonymous\")
}
",
    )
    .unwrap();

    let output = bock_bin()
        .arg("check")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "three-module chain should pass type checking\nstderr: {stderr}",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("3 files checked"), "stdout: {stdout}");
}

#[test]
fn check_crossfile_effect_operations() {
    // Effect defined in one file, used in another.
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join("effects.bock"),
        "\
module Effects

public effect Logger {
    fn log(msg: String) -> Void
}
",
    )
    .unwrap();

    std::fs::write(
        dir.path().join("service.bock"),
        "\
module Service

use Effects.{Logger}

fn greet(name: String) -> Void with Logger {
    log(\"Hello, \" + name)
}
",
    )
    .unwrap();

    let output = bock_bin()
        .arg("check")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "cross-file effect operations should pass\nstderr: {stderr}",
    );
}

// ─── Exit-code contract (subprocess) ───────────────────────────────────────
//
// `bock check` now decides its exit code in one place (the `ExitCode` binding
// in `main`, fed by `check::run`'s `CheckOutcome`). These tests pin the binary
// contract end-to-end: clean => 0, any error => 1.

#[test]
fn check_file_not_found_exits_exactly_1() {
    let output = bock_bin()
        .arg("check")
        .arg("/tmp/nonexistent_bock_file_for_exit_code_test.bock")
        .output()
        .unwrap();
    assert_exit_code(&output, 1, "missing input file");
}

#[test]
fn check_ownership_error_exits_exactly_1() {
    let f = write_temp_file(
        "record Thing { id: Int }\nfn process() {\n    let data = Thing { id: 1 }\n    let archive = data\n    let x = data\n}\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 1, "ownership (analysis) error");
}
