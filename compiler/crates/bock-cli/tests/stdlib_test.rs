//! Integration tests for the `core.test` assertion surface under `bock test`.
//!
//! `core.test` ships TWO things that these tests pin from two angles:
//!
//!   1. **A pure-Bock, cross-target assertion library** (`assert_true`,
//!      `assert_eq`, `expect(x).to_equal(y)`, …) that lowers on every codegen
//!      target. That half is proven by the conformance fixtures
//!      (`exec_core_test.bock` runs it ×5; `stdlib/test/test_no_errors.bock`
//!      type-checks it) and the cross-target source-emission test below.
//!
//!   2. **The `@test`-runner integration.** `bock test` discovers `@test`
//!      functions and runs each in a fresh interpreter, reporting pass/fail. The
//!      assertions a `@test` body uses raise the interpreter's
//!      `RuntimeError::AssertionFailed` path on failure, which the runner reports
//!      as a failing test (exit 1).
//!
//! IMPORTANT — a FOUND this suite encodes: `bock test`'s pipeline compiles ONLY
//! the single user file. Unlike `bock check`/`run`/`build`, it does NOT prepend
//! the embedded `core.*` sources, has no `ModuleRegistry`, and does not seed
//! imports — so a `@test` file CANNOT today `use core.test.{...}` (or any
//! `core.*`). The runner instead exposes the equivalent assertions as
//! interpreter built-ins: the prelude `assert(cond)` global and the built-in
//! `expect(x).to_equal(y)` matcher chain (registered via `register_test_builtins`).
//! These tests drive those built-in forms — the mechanism `@test` files actually
//! use — and the [`use_core_test_under_bock_test_is_not_yet_supported`] test
//! locks the import gap so the FOUND is visible and will flip when `bock test`
//! is wired to load the embedded core (the larger compiler change that makes the
//! stdlib `core.test` directly importable from a `@test` body).

use std::io::Write;
use std::process::{Command, Output};

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

fn run_bock_test(source: &str) -> Output {
    let f = write_temp_file(source);
    bock_bin().arg("test").arg(f.path()).output().unwrap()
}

/// A `@test` file whose assertions all pass runs clean: the runner reports the
/// expected pass count, no failures, and exits 0. Drives the prelude `assert`
/// and the built-in `expect` matcher chain — the assertion forms a `@test` body
/// uses today (the stdlib `core.test` is not yet importable here; see the module
/// docs).
#[test]
fn passing_tests_report_pass_and_exit_zero() {
    let source = "module passing\n\
        \n\
        @test\n\
        fn test_bool_assert() {\n\
        \x20\x20assert(1 + 1 == 2)\n\
        }\n\
        \n\
        @test\n\
        fn test_expect_equal() {\n\
        \x20\x20expect(1 + 1).to_equal(2)\n\
        }\n\
        \n\
        @test\n\
        fn test_expect_some() {\n\
        \x20\x20let xs = [10, 20]\n\
        \x20\x20expect(xs.get(0)).to_be_some()\n\
        }\n";
    let output = run_bock_test(source);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(
        output.status.code(),
        Some(0),
        "all-passing test file should exit 0\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        stdout.contains("3 passed, 0 failed, 3 total"),
        "expected `3 passed, 0 failed, 3 total` summary, got:\n{stdout}",
    );
}

/// A `@test` whose assertion fails is reported as FAIL, the run's summary counts
/// it, and the process exits non-zero (exit 1). This is the FAILURE-PATH proof:
/// a false assertion raises the interpreter's `AssertionFailed` path, which the
/// runner surfaces as a failing test rather than swallowing.
#[test]
fn failing_assertion_reports_fail_and_exits_nonzero() {
    let source = "module failing\n\
        \n\
        @test\n\
        fn test_passes() {\n\
        \x20\x20assert(true)\n\
        }\n\
        \n\
        @test\n\
        fn test_fails() {\n\
        \x20\x20assert(1 + 1 == 3)\n\
        }\n";
    let output = run_bock_test(source);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(
        output.status.code(),
        Some(1),
        "a file with a failing test must exit 1\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        stdout.contains("1 passed, 1 failed, 2 total"),
        "expected `1 passed, 1 failed, 2 total` summary, got:\n{stdout}",
    );
    assert!(
        stdout.contains("FAIL") && stdout.contains("test_fails"),
        "expected the failing test to be reported as FAIL, got:\n{stdout}",
    );
    assert!(
        stdout.contains("assertion failed"),
        "expected the failure detail to mention `assertion failed`, got:\n{stdout}",
    );
}

/// The built-in `expect(x).to_be_*` matchers fail the test on a mismatch, the
/// same way the free assertions do — a second failure-path proof through the
/// fluent matcher entry point.
#[test]
fn failing_expect_matcher_reports_fail() {
    let source = "module expectfail\n\
        \n\
        @test\n\
        fn test_wrong_equal() {\n\
        \x20\x20expect(2 + 2).to_equal(5)\n\
        }\n";
    let output = run_bock_test(source);
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert_eq!(
        output.status.code(),
        Some(1),
        "a failing `expect` matcher must exit 1\nstdout:\n{stdout}",
    );
    assert!(
        stdout.contains("0 passed, 1 failed, 1 total"),
        "expected `0 passed, 1 failed, 1 total`, got:\n{stdout}",
    );
}

/// FOUND lock: `bock test` does not yet load the embedded `core.*` stdlib, so a
/// `@test` file cannot `use core.test.{...}`. The import fails name resolution
/// (`undefined variable`) and the run errors out (exit 1). This test PINS that
/// current limitation: when `bock test` is wired to load embedded core (the
/// FOUND fix), this test will start failing and must be updated to assert that
/// the import now resolves — making the gap impossible to close silently.
#[test]
fn use_core_test_under_bock_test_is_not_yet_supported() {
    let source = "module imports\n\
        \n\
        use core.test.{assert_true}\n\
        \n\
        @test\n\
        fn test_uses_stdlib() {\n\
        \x20\x20assert_true(true)\n\
        }\n";
    let output = run_bock_test(source);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    assert_ne!(
        output.status.code(),
        Some(0),
        "FOUND regressed: `use core.test` now works under `bock test` — update \
         this test to assert the import resolves.\noutput:\n{combined}",
    );
    assert!(
        combined.contains("undefined variable") || combined.contains("not found"),
        "expected a name-resolution failure for the unloaded `core.test` import, got:\n{combined}",
    );
}
