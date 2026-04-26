use std::fs;
use std::io::Write;
use std::process::Command;

use tempfile::TempDir;

fn bock_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_bock"))
}

fn create_project(source: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    let file_path = dir.path().join("main.bock");
    let mut f = fs::File::create(&file_path).unwrap();
    f.write_all(source.as_bytes()).unwrap();
    f.flush().unwrap();
    dir
}

const SIMPLE_SOURCE: &str = "fn add(a: Int, b: Int) -> Int { a + b }\n";

#[test]
fn build_js_produces_output_files() {
    let dir = create_project(SIMPLE_SOURCE);
    let output = bock_bin()
        .args(["build", "--target", "js", "--source-only"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected exit 0, got {}\nstdout: {stdout}\nstderr: {stderr}",
        output.status,
    );

    // Check that build/js directory was created with files
    let build_dir = dir.path().join("build/js");
    assert!(build_dir.exists(), "build/js directory should exist");

    // Should have at least one .js file
    let js_files: Vec<_> = fs::read_dir(&build_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|ext| ext.to_str())
                .map_or(false, |ext| ext == "js")
        })
        .collect();
    assert!(!js_files.is_empty(), "should have generated .js files");
}

#[test]
fn build_ts_produces_output_files() {
    let dir = create_project(SIMPLE_SOURCE);
    let output = bock_bin()
        .args(["build", "--target", "ts", "--source-only"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected exit 0, got {}\nstdout: {stdout}\nstderr: {stderr}",
        output.status,
    );

    let build_dir = dir.path().join("build/ts");
    assert!(build_dir.exists(), "build/ts directory should exist");
}

#[test]
fn build_python_produces_output_files() {
    let dir = create_project(SIMPLE_SOURCE);
    let output = bock_bin()
        .args(["build", "--target", "python", "--source-only"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected exit 0, got {}\nstdout: {stdout}\nstderr: {stderr}",
        output.status,
    );

    let build_dir = dir.path().join("build/python");
    assert!(build_dir.exists(), "build/python directory should exist");
}

#[test]
fn build_rust_target() {
    let dir = create_project(SIMPLE_SOURCE);
    let output = bock_bin()
        .args(["build", "--target", "rust", "--source-only"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected exit 0, got {}\nstdout: {stdout}\nstderr: {stderr}",
        output.status,
    );

    let build_dir = dir.path().join("build/rust");
    assert!(build_dir.exists(), "build/rust directory should exist");
}

#[test]
fn build_go_target() {
    let dir = create_project(SIMPLE_SOURCE);
    let output = bock_bin()
        .args(["build", "--target", "go", "--source-only"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected exit 0, got {}\nstdout: {stdout}\nstderr: {stderr}",
        output.status,
    );

    let build_dir = dir.path().join("build/go");
    assert!(build_dir.exists(), "build/go directory should exist");
}

#[test]
fn build_all_targets() {
    let dir = create_project(SIMPLE_SOURCE);
    let output = bock_bin()
        .args(["build", "--all-targets", "--source-only"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected exit 0, got {}\nstdout: {stdout}\nstderr: {stderr}",
        output.status,
    );

    // Should have directories for all targets
    for target in &["js", "ts", "python", "rust", "go"] {
        let build_dir = dir.path().join(format!("build/{target}"));
        assert!(build_dir.exists(), "build/{target} directory should exist");
    }
}

#[test]
fn build_release_uses_release_dir() {
    let dir = create_project(SIMPLE_SOURCE);
    let output = bock_bin()
        .args(["build", "--target", "js", "--release", "--source-only"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected exit 0, got {}\nstdout: {stdout}\nstderr: {stderr}",
        output.status,
    );

    let build_dir = dir.path().join("build/release/js");
    assert!(
        build_dir.exists(),
        "build/release/js directory should exist for release builds"
    );
}

#[test]
fn build_unknown_target_fails() {
    let dir = create_project(SIMPLE_SOURCE);
    let output = bock_bin()
        .args(["build", "--target", "java"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "expected non-zero exit for unknown target"
    );
}

#[test]
fn build_default_target_is_js() {
    let dir = create_project(SIMPLE_SOURCE);
    let output = bock_bin()
        .args(["build", "--source-only"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected exit 0, got {}\nstdout: {stdout}\nstderr: {stderr}",
        output.status,
    );

    let build_dir = dir.path().join("build/js");
    assert!(build_dir.exists(), "default target should be js");
}

#[test]
fn build_syntax_error_fails() {
    let dir = create_project("fn { broken\n");
    let output = bock_bin()
        .args(["build", "--target", "js", "--source-only"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "expected non-zero exit for syntax errors"
    );
}

#[test]
fn build_no_bock_files_fails() {
    let dir = TempDir::new().unwrap();
    let output = bock_bin()
        .args(["build", "--target", "js"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "expected non-zero exit when no .bock files found"
    );
}

#[test]
fn build_shows_timing_output() {
    let dir = create_project(SIMPLE_SOURCE);
    let output = bock_bin()
        .args(["build", "--target", "js", "--source-only"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("ms"),
        "build output should include timing info, got: {stdout}"
    );
}

#[test]
fn build_deterministic_flag_accepted() {
    let dir = create_project(SIMPLE_SOURCE);
    let output = bock_bin()
        .args([
            "build",
            "--target",
            "js",
            "--deterministic",
            "--source-only",
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected exit 0 with --deterministic, got {}\nstdout: {stdout}\nstderr: {stderr}",
        output.status,
    );
}

#[test]
fn build_multifile_dependency_ordering() {
    let dir = TempDir::new().unwrap();

    fs::write(
        dir.path().join("models.bock"),
        "module Models\n\npublic record User {\n    id: Int\n    name: String\n}\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("main.bock"),
        "module Main\n\nuse Models.{User}\n\nfn greet() -> Int { 42 }\n",
    )
    .unwrap();

    let output = bock_bin()
        .args(["build", "--target", "js", "--source-only"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "expected exit 0 for multi-file build, got {}\nstdout: {stdout}\nstderr: {stderr}",
        output.status,
    );
    assert!(
        stdout.contains("2 source file"),
        "should report 2 source files, got: {stdout}",
    );
}

#[test]
fn build_circular_dependency_fails() {
    let dir = TempDir::new().unwrap();

    fs::write(
        dir.path().join("a.bock"),
        "module A\n\nuse B.{foo}\n\nfn bar() -> Int { 1 }\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("b.bock"),
        "module B\n\nuse A.{bar}\n\nfn foo() -> Int { 2 }\n",
    )
    .unwrap();

    let output = bock_bin()
        .args(["build", "--target", "js", "--source-only"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "expected non-zero exit for circular dependency",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("circular"),
        "stderr should mention circular dependency: {stderr}",
    );
}

// ── Strictness governance (§17.6) ───────────────────────────────────────────

fn seed_unpinned_build_manifest(dir: &std::path::Path) {
    let path = dir
        .join(".bock")
        .join("decisions")
        .join("build")
        .join("main.bock.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let entry = r#"[{
        "id": "abc12345deadbeef",
        "module": "main.bock",
        "target": "js",
        "decision_type": "codegen",
        "choice": "generated JS",
        "alternatives": [],
        "reasoning": "JS async pattern",
        "model_id": "stub:stub",
        "confidence": 0.9,
        "pinned": false,
        "pin_reason": null,
        "timestamp": "2026-04-22T10:00:00Z"
    }]"#;
    fs::write(path, entry).unwrap();
}

#[test]
fn build_strict_with_unpinned_decisions_errors_out() {
    let dir = create_project(SIMPLE_SOURCE);
    seed_unpinned_build_manifest(dir.path());

    let output = bock_bin()
        .args(["build", "--target", "js", "--source-only", "--strict"])
        .current_dir(dir.path())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "expected non-zero exit when production build has unpinned decisions",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unpinned decision")
            && stderr.contains("production mode")
            && stderr.contains("bock override"),
        "stderr should name the governance failure: {stderr}",
    );
}

#[test]
fn build_pin_all_pins_every_build_decision() {
    let dir = create_project(SIMPLE_SOURCE);
    seed_unpinned_build_manifest(dir.path());

    let output = bock_bin()
        .args(["build", "--target", "js", "--source-only", "--pin-all"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "pin-all build should succeed in development mode: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("pin-all pinned 1 decision"),
        "stdout should report pinned count: {stdout}"
    );

    // Manifest entry is now pinned.
    let manifest_path = dir
        .path()
        .join(".bock/decisions/build/main.bock.json");
    let content = fs::read_to_string(&manifest_path).unwrap();
    assert!(content.contains("\"pinned\": true"));
    assert!(content.contains("pin_reason"));
    assert!(content.contains("bulk-pinned"));
}

#[test]
fn development_then_strict_workflow_succeeds_after_pin_all() {
    // 1. Seed unpinned state, then pin-all in development.
    let dir = create_project(SIMPLE_SOURCE);
    seed_unpinned_build_manifest(dir.path());

    let pin = bock_bin()
        .args(["build", "--target", "js", "--source-only", "--pin-all"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(pin.status.success());

    // 2. Now a strict (production) build passes the governance gate.
    let strict = bock_bin()
        .args(["build", "--target", "js", "--source-only", "--strict"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        strict.status.success(),
        "strict build should succeed after pin-all: {}",
        String::from_utf8_lossy(&strict.stderr)
    );
}

#[test]
fn build_no_ai_flag_is_alias_for_deterministic() {
    let dir = create_project(SIMPLE_SOURCE);
    let output = bock_bin()
        .args(["build", "--target", "js", "--source-only", "--no-ai"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "--no-ai should be accepted as alias: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
