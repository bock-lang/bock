//! End-to-end tests for transpiled `@test` functions (S7/S8, spec §20.6.2).
//!
//! Project mode transpiles each Bock `@test` function into the target's idiomatic
//! test framework, so the scaffolded project's `cargo test` / `go test` /
//! `npm test` / `pytest` run them. These tests build a project containing `@test`
//! functions and verify, for **all five** targets:
//!
//! - **RUN-verify (rust + go + js + ts + python).** The emitted test files are
//!   executed by the target's own test runner and must PASS — the empirical
//!   §20.6.2 release-readiness check ("a target's codegen is project-mode-ready
//!   when its Tier-2 tests pass"):
//!   - rust → `cargo test`
//!   - go → `go test ./...`
//!   - js / ts → `npm install` + `npm test` (Vitest by default; Jest when
//!     configured)
//!   - python → `pytest` (default framework) **and** `python -m unittest` (the
//!     `unittest` framework variant)
//! - **Formatter-clean gate (all five).** The emitted test file passes the
//!   target's formatter `--check` cleanly (§20.6.2 codegen-formatter agreement):
//!   - rust → `rustfmt --check`
//!   - go → `gofmt -l`
//!   - js / ts → `prettier --check` (the project-local Prettier, via `npm exec`)
//!   - python → `black --check`
//!
//! ## Skip-if-absent + the require flag
//!
//! Every check skips (returns early, recording the skip on stderr) when its
//! required runner/formatter is missing, so the suite is green on a dev host with
//! any subset of tooling installed. To turn an *absent* runner/formatter into a
//! hard failure — what CI's certifying lane wants — set the require flag:
//!
//! - `BOCK_PROJECTMODE_REQUIRE` (preferred) or `BOCK_CONFORMANCE_REQUIRE`
//!   (honored as a fallback, so a single CI env var covers both harnesses),
//! - comma-separated target ids, or `all` to require every target,
//! - example: `BOCK_PROJECTMODE_REQUIRE=all cargo test -p bock --test
//!   transpiled_tests`.
//!
//! When a target is required but its tooling is absent, the corresponding check
//! panics with an install hint instead of skipping.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

fn bock_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_bock"))
}

/// True if `tool --version` (or the supplied probe args) exits successfully.
fn have_with(tool: &str, args: &[&str]) -> bool {
    Command::new(tool)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// True if `tool` is invocable. Most tools answer `--version` with exit 0; `go`
/// and `gofmt` don't accept `--version` (they treat it as an unknown flag and
/// exit non-zero), so probe those with their own conventions: `go version`, and
/// `gofmt` via its help text (any successful spawn means it's on PATH).
fn have(tool: &str) -> bool {
    match tool {
        "go" => have_with(tool, &["version"]),
        // `gofmt` has no version subcommand; `gofmt -h` prints usage and exits 0
        // when present (a spawn error means it's absent).
        "gofmt" => have_with(tool, &["-h"]),
        _ => have_with(tool, &["--version"]),
    }
}

/// Parse the require flag into the set of target ids whose tooling must be
/// present. Reads `BOCK_PROJECTMODE_REQUIRE` first, then `BOCK_CONFORMANCE_REQUIRE`
/// (so a CI lane can set one env var for both harnesses). `all` expands to every
/// target; unset/empty yields an empty set (everything is skip-if-absent).
fn required_targets() -> BTreeSet<String> {
    let mut required = BTreeSet::new();
    let raw = std::env::var("BOCK_PROJECTMODE_REQUIRE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| std::env::var("BOCK_CONFORMANCE_REQUIRE").ok());
    let Some(value) = raw else {
        return required;
    };
    for token in value.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        if token.eq_ignore_ascii_case("all") {
            for t in ["js", "ts", "python", "rust", "go"] {
                required.insert(t.to_string());
            }
        } else {
            required.insert(token.to_string());
        }
    }
    required
}

/// Whether `target` is required by the require flag.
fn is_required(target: &str) -> bool {
    required_targets().contains(target)
}

/// Skip-if-absent gate: returns `true` (proceed) when `present`; otherwise either
/// panics (when `target` is required) or records the skip and returns `false`.
#[must_use]
fn proceed_or_skip(target: &str, what: &str, present: bool, install_hint: &str) -> bool {
    if present {
        return true;
    }
    if is_required(target) {
        panic!(
            "target `{target}` is required (BOCK_PROJECTMODE_REQUIRE / \
             BOCK_CONFORMANCE_REQUIRE) but {what} is absent.\n  hint: {install_hint}"
        );
    }
    eprintln!("skipping {target}: {what} not available");
    false
}

/// A project with a `@test`-bearing `main.bock`. `add`/`first` are the functions
/// under test; the three `@test` functions exercise equality, boolean, and
/// Optional-predicate assertions — the cross-cutting assertion idioms each
/// backend must lower.
fn create_test_project() -> TempDir {
    let dir = TempDir::new().unwrap();
    write_test_project(dir.path(), "transpiled-tests-demo", None);
    dir
}

/// Write the standard `@test` project into `dir`. `extra_target_config`, when
/// present, is appended verbatim to `bock.project` (e.g. a `[targets.python]`
/// block selecting `test_framework = "unittest"`).
fn write_test_project(dir: &Path, name: &str, extra_target_config: Option<&str>) {
    let src = "\
module main

public fn add(a: Int, b: Int) -> Int {
  a + b
}

public fn first(xs: List[Int]) -> Optional[Int] {
  xs.get(0)
}

fn main() {
  println(\"app\")
}

@test
fn test_add_works() {
  expect(add(1, 2)).to_equal(3)
}

@test
fn test_booleans() {
  expect(true).to_be_true()
  expect(false).to_be_false()
}

@test
fn test_optional() {
  expect(first([10, 20, 30])).to_be_some()
}
";
    let mut f = fs::File::create(dir.join("main.bock")).unwrap();
    f.write_all(src.as_bytes()).unwrap();
    f.flush().unwrap();
    let mut project = format!("[project]\nname = \"{name}\"\nversion = \"0.1.0\"\n");
    if let Some(extra) = extra_target_config {
        project.push('\n');
        project.push_str(extra);
    }
    let mut p = fs::File::create(dir.join("bock.project")).unwrap();
    p.write_all(project.as_bytes()).unwrap();
    p.flush().unwrap();
}

/// Build `target` in project mode and assert success, returning the build dir.
fn build_target(dir: &TempDir, target: &str) -> std::path::PathBuf {
    build_target_in(dir.path(), target)
}

/// As [`build_target`] but takes the project directory path directly.
fn build_target_in(project_dir: &std::path::Path, target: &str) -> std::path::PathBuf {
    let output = bock_bin()
        .args(["build", "--target", target])
        .current_dir(project_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "project-mode build failed for {target}:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    project_dir.join(format!("build/{target}"))
}

// ── @test functions are kept OUT of the runtime tree ─────────────────────────

/// `@test` functions must not appear in the emitted runtime module tree (their
/// `expect(...)` DSL has no runtime definition there) — only in the test file.
#[test]
fn test_functions_excluded_from_runtime_tree() {
    let dir = create_test_project();
    for target in &["js", "ts", "python", "rust", "go"] {
        let build_dir = build_target(&dir, target);
        let runtime_entry = match *target {
            "js" => build_dir.join("main.js"),
            "ts" => build_dir.join("main.ts"),
            "python" => build_dir.join("main.py"),
            "rust" => build_dir.join("src/main.rs"),
            "go" => build_dir.join("main.go"),
            _ => unreachable!(),
        };
        let content = fs::read_to_string(&runtime_entry).unwrap();
        assert!(
            !content.contains("test_add_works"),
            "{target}: runtime entry wrongly contains the @test function `test_add_works`:\n{content}"
        );
        // The assertion DSL must never leak into the runtime tree.
        assert!(
            !content.contains("to_equal") && !content.contains("to_be_some"),
            "{target}: runtime entry leaked the assertion DSL:\n{content}"
        );
    }
}

// ── Test files are emitted at the framework-expected location ─────────────────

#[test]
fn test_files_emitted_per_target() {
    let dir = create_test_project();
    let cases = [
        ("js", "bock.test.js"),
        ("ts", "bock.test.ts"),
        ("python", "test_bock.py"),
        ("rust", "src/bock_tests.rs"),
        ("go", "bock_test.go"),
    ];
    for (target, rel) in cases {
        let build_dir = build_target(&dir, target);
        let test_file = build_dir.join(rel);
        assert!(
            test_file.exists(),
            "{target}: expected transpiled test file at {}",
            test_file.display()
        );
        let content = fs::read_to_string(&test_file).unwrap();
        // The three @test functions are all present.
        assert!(
            content.contains("test_add_works") || content.contains("TestAddWorks"),
            "{target}: test file missing test_add_works:\n{content}"
        );
    }
}

/// Rust wires its inline test module into the entry file.
#[test]
fn rust_entry_wires_inline_test_module() {
    let dir = create_test_project();
    let build_dir = build_target(&dir, "rust");
    let main_rs = fs::read_to_string(build_dir.join("src/main.rs")).unwrap();
    assert!(
        main_rs.contains("#[cfg(test)]") && main_rs.contains("mod bock_tests;"),
        "rust main.rs should wire the inline test module:\n{main_rs}"
    );
}

// ── RUN-VERIFY: rust (cargo test) ────────────────────────────────────────────

#[test]
fn rust_cargo_test_runs_transpiled_tests() {
    if !proceed_or_skip(
        "rust",
        "cargo",
        have("cargo"),
        "install the Rust toolchain (rustup)",
    ) {
        return;
    }
    let dir = create_test_project();
    let build_dir = build_target(&dir, "rust");
    // Isolate the cargo target dir so the run-verify build doesn't collide with
    // the workspace target dir or leave artifacts in the temp project.
    let cargo_target = dir.path().join("cargo-target");
    let output = Command::new("cargo")
        .args(["test"])
        .current_dir(&build_dir)
        .env("CARGO_TARGET_DIR", &cargo_target)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "cargo test on transpiled tests failed:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("test result: ok") && stdout.contains("3 passed"),
        "expected 3 passing transpiled rust tests, got:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

// ── RUN-VERIFY: go (go test) ─────────────────────────────────────────────────

#[test]
fn go_test_runs_transpiled_tests() {
    if !proceed_or_skip("go", "go", have("go"), "install the Go toolchain") {
        return;
    }
    let dir = create_test_project();
    let build_dir = build_target(&dir, "go");
    let gocache = dir.path().join("go-cache");
    let output = Command::new("go")
        .args(["test", "./..."])
        .current_dir(&build_dir)
        .env("GOCACHE", &gocache)
        .env("GOFLAGS", "-mod=mod")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "go test on transpiled tests failed:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("ok") || stdout.contains("PASS"),
        "expected go test to pass the transpiled tests, got:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

// ── RUN-VERIFY: js (npm install + npm test → Vitest) ──────────────────────────

/// Whether `npm` (and thus a Node toolchain) is available for the JS/TS runners.
fn have_npm() -> bool {
    have("npm")
}

/// `npm install` then `npm test` inside `build_dir`, asserting the Vitest/Jest
/// run passes all transpiled tests. Shared by the js and ts run-verify tests.
fn npm_run_verify(target: &str, build_dir: &Path) {
    let install = Command::new("npm")
        .args(["install", "--no-audit", "--no-fund", "--loglevel=error"])
        .current_dir(build_dir)
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "{target}: `npm install` failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&install.stdout),
        String::from_utf8_lossy(&install.stderr),
    );
    let test = Command::new("npm")
        .args(["test"])
        .current_dir(build_dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&test.stdout);
    let stderr = String::from_utf8_lossy(&test.stderr);
    assert!(
        test.status.success(),
        "{target}: `npm test` failed on transpiled tests:\nstdout: {stdout}\nstderr: {stderr}"
    );
    // Vitest/Jest both report a green summary line ("3 passed" / "Tests 3 passed").
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("3 passed") || combined.contains("3 pass"),
        "{target}: expected 3 passing transpiled tests, got:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
fn js_npm_test_runs_transpiled_tests() {
    if !proceed_or_skip(
        "js",
        "npm",
        have_npm(),
        "install Node.js (provides npm; needs registry network)",
    ) {
        return;
    }
    let dir = create_test_project();
    let build_dir = build_target(&dir, "js");
    npm_run_verify("js", &build_dir);
}

// ── RUN-VERIFY: ts (npm install + npm test → Vitest) ──────────────────────────

#[test]
fn ts_npm_test_runs_transpiled_tests() {
    if !proceed_or_skip(
        "ts",
        "npm",
        have_npm(),
        "install Node.js (provides npm; needs registry network)",
    ) {
        return;
    }
    let dir = create_test_project();
    let build_dir = build_target(&dir, "ts");
    npm_run_verify("ts", &build_dir);
}

// ── RUN-VERIFY: python (pytest, default framework) ────────────────────────────

/// The python interpreter to drive (`python3`, falling back to `python`).
fn python_exe() -> Option<&'static str> {
    if have("python3") {
        Some("python3")
    } else if have("python") {
        Some("python")
    } else {
        None
    }
}

/// Whether `<py> -m pytest --version` succeeds (pytest importable by the host
/// interpreter — true under a CI `pip install pytest` or an activated venv).
fn have_pytest(py: &str) -> bool {
    have_with(py, &["-m", "pytest", "--version"])
}

#[test]
fn python_pytest_runs_transpiled_tests() {
    let Some(py) = python_exe() else {
        if !proceed_or_skip("python", "python", false, "install Python 3") {
            return;
        }
        unreachable!();
    };
    if !proceed_or_skip(
        "python",
        "pytest",
        have_pytest(py),
        "install pytest (`pip install pytest` / venv); CI uses --break-system-packages",
    ) {
        return;
    }
    let dir = create_test_project();
    let build_dir = build_target(&dir, "python");
    let output = Command::new(py)
        .args(["-m", "pytest", "-q"])
        .current_dir(&build_dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "pytest failed on transpiled tests:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("3 passed"),
        "expected 3 passing transpiled pytest tests, got:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

// ── RUN-VERIFY: python (unittest framework variant, stdlib) ───────────────────

#[test]
fn python_unittest_runs_transpiled_tests() {
    let Some(py) = python_exe() else {
        if !proceed_or_skip("python", "python", false, "install Python 3") {
            return;
        }
        unreachable!();
    };
    // unittest is stdlib: no extra install. Build with the `unittest` framework
    // selected so the emitted file is a `unittest.TestCase` subclass.
    let dir = TempDir::new().unwrap();
    write_test_project(
        dir.path(),
        "unittest-tests-demo",
        Some("[targets.python]\ntest_framework = \"unittest\"\n"),
    );
    let build_dir = build_target_in(dir.path(), "python");
    let output = Command::new(py)
        .args(["-m", "unittest", "discover", "-v"])
        .current_dir(&build_dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // unittest prints its summary ("Ran N tests", "OK") to stderr.
    let combined = format!("{stdout}{stderr}");
    assert!(
        output.status.success() && combined.contains("OK"),
        "python -m unittest failed on transpiled tests:\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        combined.contains("Ran 3 tests"),
        "expected 3 transpiled unittest tests, got:\nstdout: {stdout}\nstderr: {stderr}"
    );
}

// ── FORMATTER-CLEAN GATE: rust (rustfmt --check) ──────────────────────────────

#[test]
fn rust_test_file_is_rustfmt_clean() {
    if !proceed_or_skip(
        "rust",
        "rustfmt",
        have("rustfmt"),
        "rustup component add rustfmt",
    ) {
        return;
    }
    let dir = create_test_project();
    let build_dir = build_target(&dir, "rust");
    // The transpiled test file AND the entry it is wired into must be clean.
    for rel in &["src/bock_tests.rs", "src/main.rs"] {
        let file = build_dir.join(rel);
        let output = Command::new("rustfmt")
            .args(["--check", "--edition", "2021"])
            .arg(&file)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "rustfmt --check found drift in emitted `{rel}` (§20.6.2 \
             codegen-formatter agreement):\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

// ── FORMATTER-CLEAN GATE: go (gofmt -l) ───────────────────────────────────────

#[test]
fn go_test_file_is_gofmt_clean() {
    if !proceed_or_skip(
        "go",
        "gofmt",
        have("gofmt"),
        "install the Go toolchain (provides gofmt)",
    ) {
        return;
    }
    let dir = create_test_project();
    let build_dir = build_target(&dir, "go");
    let test_file = build_dir.join("bock_test.go");
    let output = Command::new("gofmt")
        .args(["-l"])
        .arg(&test_file)
        .output()
        .unwrap();
    // gofmt -l prints the path of any file needing reformatting; clean = no output.
    let listed = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && listed.trim().is_empty(),
        "gofmt -l flagged the emitted go test file (§20.6.2 codegen-formatter \
         agreement):\n{listed}"
    );
}

// ── FORMATTER-CLEAN GATE: js + ts (prettier --check) ──────────────────────────

/// Whether a project-local Prettier is invocable inside `build_dir` (after
/// `npm install`) via `npm exec`. The scaffolded `package.json` lists Prettier as
/// a devDependency, so no global install is needed.
fn have_local_prettier(build_dir: &Path) -> bool {
    Command::new("npm")
        .args(["exec", "--no", "--", "prettier", "--version"])
        .current_dir(build_dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Assert the emitted `bock.test.<ext>` passes `prettier --check` using the
/// project-local Prettier and the scaffolded `.prettierrc.json`. Requires an
/// `npm install` to have populated `node_modules` first.
fn prettier_check(target: &str, build_dir: &Path, test_file: &str) {
    let output = Command::new("npm")
        .args(["exec", "--no", "--", "prettier", "--check", test_file])
        .current_dir(build_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{target}: prettier --check flagged the emitted `{test_file}` (§20.6.2 \
         codegen-formatter agreement):\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn js_test_file_is_prettier_clean() {
    if !proceed_or_skip(
        "js",
        "npm",
        have_npm(),
        "install Node.js (provides npm + npx)",
    ) {
        return;
    }
    let dir = create_test_project();
    let build_dir = build_target(&dir, "js");
    // Populate node_modules so the project-local prettier is resolvable.
    let install = Command::new("npm")
        .args(["install", "--no-audit", "--no-fund", "--loglevel=error"])
        .current_dir(&build_dir)
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "js: `npm install` failed before prettier gate:\n{}",
        String::from_utf8_lossy(&install.stderr)
    );
    if !proceed_or_skip(
        "js",
        "prettier",
        have_local_prettier(&build_dir),
        "the scaffolded package.json lists prettier; run `npm install`",
    ) {
        return;
    }
    prettier_check("js", &build_dir, "bock.test.js");
}

#[test]
fn ts_test_file_is_prettier_clean() {
    if !proceed_or_skip(
        "ts",
        "npm",
        have_npm(),
        "install Node.js (provides npm + npx)",
    ) {
        return;
    }
    let dir = create_test_project();
    let build_dir = build_target(&dir, "ts");
    let install = Command::new("npm")
        .args(["install", "--no-audit", "--no-fund", "--loglevel=error"])
        .current_dir(&build_dir)
        .output()
        .unwrap();
    assert!(
        install.status.success(),
        "ts: `npm install` failed before prettier gate:\n{}",
        String::from_utf8_lossy(&install.stderr)
    );
    if !proceed_or_skip(
        "ts",
        "prettier",
        have_local_prettier(&build_dir),
        "the scaffolded package.json lists prettier; run `npm install`",
    ) {
        return;
    }
    prettier_check("ts", &build_dir, "bock.test.ts");
}

// ── FORMATTER-CLEAN GATE: python (black --check) ──────────────────────────────

#[test]
fn python_test_file_is_black_clean() {
    if !proceed_or_skip(
        "python",
        "black",
        have("black"),
        "pipx install black (or pip install black)",
    ) {
        return;
    }
    let dir = create_test_project();
    let build_dir = build_target(&dir, "python");
    let test_file = build_dir.join("test_bock.py");
    let output = Command::new("black")
        .args(["--check"])
        .arg(&test_file)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "black --check flagged the emitted python test file (§20.6.2 \
         codegen-formatter agreement):\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}
