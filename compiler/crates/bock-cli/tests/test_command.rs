//! Integration tests for the `bock test` command (the end-to-end CLI binary).
//!
//! These drive the built `bock` binary against on-disk `.bock` files, the same
//! way a user runs `bock test`. The key behaviour exercised here is that the
//! test runner loads the embedded core stdlib through the full multi-file
//! pipeline, so a test file's `use core.<name>.{...}` resolves and the imported
//! functions run — alongside the interpreter-only `expect`/`assert` builtins.

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

/// A passing `@test` using the bare interpreter assertion builtins must still
/// run and exit 0 — the core-loading change must not regress the existing
/// builtin-only path.
#[test]
fn test_bare_builtin_assertions_pass() {
    let f = write_temp_file(
        r#"@test
fn test_addition() {
    expect(1 + 1).to_equal(2)
}
"#,
    );
    let output = bock_bin().arg("test").arg(f.path()).output().unwrap();
    assert!(
        output.status.success(),
        "expected exit 0, got {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("PASS"), "stdout: {stdout}");
    assert!(stdout.contains("1 passed"), "stdout: {stdout}");
}

/// A failing `@test` must report FAIL and exit non-zero.
#[test]
fn test_failing_assertion_exits_nonzero() {
    let f = write_temp_file(
        r#"@test
fn test_bad_math() {
    expect(1 + 1).to_equal(3)
}
"#,
    );
    let output = bock_bin().arg("test").arg(f.path()).output().unwrap();
    assert!(
        !output.status.success(),
        "expected non-zero exit for a failing test",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("FAIL"), "stdout: {stdout}");
    assert!(stdout.contains("1 failed"), "stdout: {stdout}");
}

/// The core fix: a test file that `use`s a `core.*` module must compile (the
/// import resolves through the multi-file pipeline with the embedded core
/// prepended) and run the imported function. `core.option.count(Some(5))` is
/// `1` and `count(None)` is `0`. Before the fix, `bock test` compiled only the
/// single user file and `use core.option` failed name resolution.
#[test]
fn test_use_core_option_resolves_and_passes() {
    let f = write_temp_file(
        r#"module mytest

use core.option.{count}

@test
fn test_core_option_count() {
    expect(count(Some(5))).to_equal(1)
    expect(count(None)).to_equal(0)
}
"#,
    );
    let output = bock_bin().arg("test").arg(f.path()).output().unwrap();
    assert!(
        output.status.success(),
        "expected exit 0 for a test using core.option, got {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("PASS"), "stdout: {stdout}");
    assert!(stdout.contains("1 passed"), "stdout: {stdout}");
}

/// A test file may mix `use core.*` imports with the bare assertion builtins in
/// the same test — both must work together.
#[test]
fn test_core_import_and_bare_builtins_coexist() {
    let f = write_temp_file(
        r#"module mixed

use core.option.{get_or}

@test
fn test_get_or_present() {
    expect(get_or(Some(7), 0)).to_equal(7)
}

@test
fn test_get_or_absent() {
    expect(get_or(None, 42)).to_equal(42)
}

@test
fn test_bare_builtin_still_works() {
    expect(true).to_be_true()
}
"#,
    );
    let output = bock_bin().arg("test").arg(f.path()).output().unwrap();
    assert!(
        output.status.success(),
        "expected exit 0, got {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("3 passed"), "stdout: {stdout}");
}
