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

/// Regression (Q-interp-assert-primitives): the `core.test` free assertions
/// `assert_eq`/`assert_ne` over **primitives** must pass under the `bock test`
/// interpreter. Their bodies dispatch `actual.eq(expected)` through the
/// `T: Equatable` bound; for a primitive instantiation that lands on the
/// interpreter's Equatable primitive bridge. Before the fix the bridge was
/// registered under the never-referenced name `equals`, so every primitive
/// `assert_eq` failed with `method 'eq' not found on Int`.
#[test]
fn test_assert_eq_assert_ne_on_primitives_pass() {
    let f = write_temp_file(
        r#"module prim_assert

use core.test.{ assert_eq, assert_ne }

@test
fn int_assert_eq() {
    assert_eq(2 + 2, 4)
}

@test
fn int_assert_ne() {
    assert_ne(2 + 2, 5)
}

@test
fn string_assert_eq() {
    assert_eq("ab" + "c", "abc")
}
"#,
    );
    let output = bock_bin().arg("test").arg(f.path()).output().unwrap();
    assert!(
        output.status.success(),
        "expected exit 0 for primitive assert_eq/assert_ne, got {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("3 passed"), "stdout: {stdout}");
}

/// The negative half of the primitive `assert_eq` path: a genuinely unequal
/// pair must FAIL the test through the assertion path (`assertion failed`),
/// not pass vacuously and not error with a method-dispatch failure.
#[test]
fn test_assert_eq_on_unequal_primitives_fails_cleanly() {
    let f = write_temp_file(
        r#"module prim_assert_neg

use core.test.{ assert_eq }

@test
fn int_assert_eq_unequal() {
    assert_eq(2 + 2, 5)
}
"#,
    );
    let output = bock_bin().arg("test").arg(f.path()).output().unwrap();
    assert!(
        !output.status.success(),
        "expected non-zero exit for a failing primitive assert_eq",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1 failed"), "stdout: {stdout}");
    assert!(
        stdout.contains("assertion failed"),
        "the failure must come from the assertion, not method dispatch: {stdout}"
    );
    assert!(
        !stdout.contains("not found"),
        "primitive `.eq` dispatch must not fail: {stdout}"
    );
}

/// Build the standard project layout (`bock.project` + `src/main.bock` +
/// `test/<name>_test.bock`) under a fresh temp dir, returning its root.
fn write_project(name: &str, main_src: &str, test_rel: &str, test_src: &str) -> std::path::PathBuf {
    let root = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("test")).unwrap();
    std::fs::write(
        root.join("bock.project"),
        format!("[project]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
    )
    .unwrap();
    std::fs::write(root.join("src/main.bock"), main_src).unwrap();
    std::fs::write(root.join(test_rel), test_src).unwrap();
    root
}

/// Regression (Q-test-interp-crossfile-use): a test file in the standard
/// `examples/*/test/` project layout must resolve `use main.{...}` against the
/// sibling `src/main.bock`. Before the fix, the `bock test` interpreter path
/// compiled only the embedded core stdlib plus the single test file — no
/// sibling-module discovery — so the import died with `[E1005] module `main`
/// not found` while the compiled-target (project build) path resolved it fine.
#[test]
fn test_crossfile_use_main_named_import_resolves() {
    let root = write_project(
        "crossfile_named",
        "module main\n\npublic fn add(a: Int, b: Int) -> Int {\n  a + b\n}\n",
        "test/add_test.bock",
        "module add_test\n\nuse main.{add}\n\n@test\nfn test_add() {\n  expect(add(2, 3)).to_equal(5)\n}\n",
    );
    let output = bock_bin()
        .arg("test")
        .arg("test/add_test.bock")
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "expected exit 0 for cross-file `use main.{{add}}`, got {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1 passed"), "stdout: {stdout}");
}

/// The glob-import form of the same layout (`use main.*` — what the in-repo
/// `examples/real-world/*/test/` files use) must resolve too, and the test
/// must exercise an imported function end to end.
#[test]
fn test_crossfile_use_main_glob_import_resolves() {
    let root = write_project(
        "crossfile_glob",
        "module main\n\npublic fn double(n: Int) -> Int {\n  n * 2\n}\n\npublic fn label() -> String {\n  \"x2\"\n}\n",
        "test/double_test.bock",
        "module double_test\n\nuse main.*\n\n@test\nfn test_double() {\n  expect(double(21)).to_equal(42)\n}\n\n@test\nfn test_label() {\n  expect(label()).to_equal(\"x2\")\n}\n",
    );
    let output = bock_bin()
        .arg("test")
        .arg("test/double_test.bock")
        .current_dir(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "expected exit 0 for cross-file `use main.*`, got {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("2 passed"), "stdout: {stdout}");
}

/// Bare `bock test` (no file argument) from the project root must discover the
/// test file AND resolve its cross-file `use main.{...}` import — the everyday
/// invocation for the standard project layout.
#[test]
fn test_crossfile_bare_invocation_from_project_root() {
    let root = write_project(
        "crossfile_bare",
        "module main\n\npublic fn triple(n: Int) -> Int {\n  n * 3\n}\n",
        "test/triple_test.bock",
        "module triple_test\n\nuse main.{triple}\n\n@test\nfn test_triple() {\n  expect(triple(3)).to_equal(9)\n}\n",
    );
    let output = bock_bin().arg("test").current_dir(&root).output().unwrap();
    assert!(
        output.status.success(),
        "expected exit 0 for bare `bock test` in a project, got {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("1 passed"), "stdout: {stdout}");
    assert!(!stdout.contains("FAIL"), "stdout: {stdout}");
}
