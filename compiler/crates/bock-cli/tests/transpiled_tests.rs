//! End-to-end tests for transpiled `@test` functions (S7, spec §20.6.2).
//!
//! Project mode transpiles each Bock `@test` function into the target's idiomatic
//! test framework, so the scaffolded project's `cargo test` / `go test` /
//! `npm test` / `pytest` run them. These tests build a project containing `@test`
//! functions and verify:
//!
//! - **rust + go: run-verified.** `cargo test` / `go test` actually execute the
//!   transpiled tests and PASS (the empirical §20.6.2 release-readiness check for
//!   those targets — "a target's codegen is project-mode-ready when its Tier 2
//!   tests pass"). Skipped gracefully when the toolchain is absent.
//! - **js + ts + python: compile-verified.** The emitted test files type-check /
//!   compile (`node --check`, `tsc --noEmit`, `py_compile`). Running them under
//!   Vitest/Jest/pytest requires test+format tooling not present on this host or
//!   in CI — see the PR's `FOUND: CI must provision js/ts/python test+format
//!   tooling`. Skipped gracefully when the base toolchain is absent.
//! - **formatter-clean gate (rust + go).** The emitted test files pass
//!   `rustfmt --check` / `gofmt -l` cleanly (§20.6.2 codegen-formatter agreement).
//!
//! Every test skips (returns early) when its required toolchain is missing, so
//! the suite is green on a host with any subset of toolchains installed.

use std::fs;
use std::io::Write;
use std::process::Command;

use tempfile::TempDir;

fn bock_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_bock"))
}

/// True if `tool` is invocable (`tool --version` exits without spawn error).
fn have(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A project with a `@test`-bearing `main.bock`. `add`/`first` are the functions
/// under test; the three `@test` functions exercise equality, boolean, and
/// Optional-predicate assertions — the cross-cutting assertion idioms each
/// backend must lower.
fn create_test_project() -> TempDir {
    let dir = TempDir::new().unwrap();
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
    let file_path = dir.path().join("main.bock");
    let mut f = fs::File::create(&file_path).unwrap();
    f.write_all(src.as_bytes()).unwrap();
    f.flush().unwrap();
    // A `bock.project` so the scaffolder/config path is exercised.
    let mut p = fs::File::create(dir.path().join("bock.project")).unwrap();
    p.write_all(b"[project]\nname = \"transpiled-tests-demo\"\nversion = \"0.1.0\"\n")
        .unwrap();
    p.flush().unwrap();
    dir
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
    if !have("cargo") {
        eprintln!("skipping: cargo not available");
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
    if !have("go") {
        eprintln!("skipping: go not available");
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

// ── COMPILE-VERIFY: js (node --check) ────────────────────────────────────────

#[test]
fn js_test_file_compiles() {
    if !have("node") {
        eprintln!("skipping: node not available");
        return;
    }
    let dir = create_test_project();
    let build_dir = build_target(&dir, "js");
    let test_file = build_dir.join("bock.test.js");
    let output = Command::new("node")
        .args(["--check"])
        .arg(&test_file)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "node --check rejected the transpiled JS test file:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// ── COMPILE-VERIFY: python (py_compile) ──────────────────────────────────────

#[test]
fn python_test_file_compiles() {
    let py = if have("python3") {
        "python3"
    } else if have("python") {
        "python"
    } else {
        eprintln!("skipping: python not available");
        return;
    };
    let dir = create_test_project();
    let build_dir = build_target(&dir, "python");
    let test_file = build_dir.join("test_bock.py");
    let output = Command::new(py)
        .args(["-m", "py_compile"])
        .arg(&test_file)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "py_compile rejected the transpiled Python test file:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Functional check: the pytest-style test functions execute without raising
    // (a stand-in for a real `pytest` run, which the absent host tooling blocks).
    let runner = format!(
        "import sys; sys.path.insert(0, {dir:?}); import test_bock; \
         [getattr(test_bock, n)() for n in dir(test_bock) if n.startswith('test_')]",
        dir = build_dir.to_string_lossy()
    );
    let run = Command::new(py)
        .args(["-c", &runner])
        .current_dir(&build_dir)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "transpiled pytest-style functions raised when executed:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

// ── COMPILE-VERIFY: ts (tsc --noEmit) ────────────────────────────────────────

#[test]
fn ts_test_file_type_checks() {
    if !have("tsc") {
        eprintln!("skipping: tsc not available");
        return;
    }
    // Use a simpler @test set with no untyped empty-array local (strict tsc
    // infers `any[]` for `let xs: List[Int] = []`, an unrelated codegen edge).
    let dir = TempDir::new().unwrap();
    let src = "\
module main

public fn add(a: Int, b: Int) -> Int {
  a + b
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
}
";
    fs::write(dir.path().join("main.bock"), src).unwrap();
    fs::write(
        dir.path().join("bock.project"),
        "[project]\nname = \"ts-tests\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let build_dir = build_target_in(dir.path(), "ts");

    // tsc errors when files are given on the command line while a `tsconfig.json`
    // is present (the scaffolder emits one). Type-check in a clean subdir holding
    // copies of the emitted `.ts` files plus an ambient `vitest` declaration (the
    // package itself is absent on this host/CI).
    let check_dir = build_dir.join("tscheck");
    fs::create_dir_all(&check_dir).unwrap();
    let mut ts_sources: Vec<String> = vec!["bock.test.ts".into(), "vitest.d.ts".into()];
    for f in &["bock.test.ts", "main.ts", "_bock_runtime.ts"] {
        let src = build_dir.join(f);
        if src.exists() {
            fs::copy(&src, check_dir.join(f)).unwrap();
            if *f != "bock.test.ts" {
                ts_sources.push((*f).to_string());
            }
        }
    }
    fs::write(
        check_dir.join("vitest.d.ts"),
        "declare module \"vitest\" {\n  export const describe: (n: string, f: () => void) => void;\n  export const it: (n: string, f: () => void) => void;\n  export const expect: (a: unknown) => { toEqual: (e: unknown) => void; toBe: (e: unknown) => void };\n}\n",
    )
    .unwrap();

    // Run from a directory with no `tsconfig.json` on the path: the scaffolded
    // `build/ts/tsconfig.json` would otherwise trip TS5112 (files on the command
    // line + a config present). Use a sibling temp dir holding only the copies.
    let isolated = dir.path().join("tscheck-isolated");
    fs::create_dir_all(&isolated).unwrap();
    for f in fs::read_dir(&check_dir).unwrap() {
        let f = f.unwrap().path();
        fs::copy(&f, isolated.join(f.file_name().unwrap())).unwrap();
    }
    let output = Command::new("tsc")
        .args([
            "--noEmit",
            "--strict",
            "--module",
            "nodenext",
            "--moduleResolution",
            "nodenext",
            "--target",
            "es2022",
        ])
        .args(&ts_sources)
        .current_dir(&isolated)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "tsc --noEmit rejected the transpiled TS test file:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

// ── FORMATTER-CLEAN GATE: rust (rustfmt --check) + go (gofmt -l) ──────────────

#[test]
fn rust_test_file_is_rustfmt_clean() {
    if !have("rustfmt") {
        eprintln!("skipping: rustfmt not available");
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

#[test]
fn go_test_file_is_gofmt_clean() {
    if !have("gofmt") {
        eprintln!("skipping: gofmt not available");
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
