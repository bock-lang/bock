//! End-to-end tests for `bock override` (pin + --promote workflow).

use std::fs;
use std::process::Command;

use tempfile::TempDir;

fn bock_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_bock"))
}

fn touch_project(dir: &std::path::Path) {
    fs::write(dir.join("bock.project"), "[project]\nname = \"t\"\n").unwrap();
    // `bock override` walks up to find bock.project; writing a source file
    // isn't needed but keeps the project shape realistic.
    fs::write(dir.join("main.bock"), "fn id(x: Int) -> Int { x }\n").unwrap();
}

fn seed_runtime(dir: &std::path::Path, id: &str, pinned: bool) {
    let path = dir.join(".bock/decisions/runtime/src/api.bock.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let pin_reason = if pinned { "\"manual\"" } else { "null" };
    let pinned_at = if pinned {
        "\"2026-04-22T10:00:00Z\""
    } else {
        "null"
    };
    let pinned_by = if pinned { "\"alice\"" } else { "null" };
    let json = format!(
        r#"[{{
            "id": "{id}",
            "module": "src/api.bock",
            "target": null,
            "decision_type": "adaptive_recovery",
            "choice": "retry(max=3)",
            "alternatives": ["use_cached"],
            "reasoning": "PCI-DSS prohibits cache for payment data",
            "model_id": "stub:stub",
            "confidence": 0.92,
            "pinned": {pinned},
            "pin_reason": {pin_reason},
            "pinned_at": {pinned_at},
            "pinned_by": {pinned_by},
            "timestamp": "2026-04-22T10:00:00Z"
        }}]"#
    );
    fs::write(path, json).unwrap();
}

#[test]
fn promote_pinned_runtime_decision_writes_build_entry() {
    let dir = TempDir::new().unwrap();
    touch_project(dir.path());
    seed_runtime(dir.path(), "rt-ok", true);

    let output = bock_bin()
        .args(["override", "--promote", "rt-ok"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "promote should succeed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let build_path = dir.path().join(".bock/decisions/build/src/api.bock.json");
    assert!(build_path.exists(), "build manifest should be created");
    let build_content = fs::read_to_string(&build_path).unwrap();
    assert!(build_content.contains("rt-ok+promoted"));
    assert!(build_content.contains("handler_choice"));
    assert!(build_content.contains("\"pinned\": true"));

    let runtime_path = dir.path().join(".bock/decisions/runtime/src/api.bock.json");
    let runtime_content = fs::read_to_string(&runtime_path).unwrap();
    assert!(
        runtime_content.contains("\"superseded_by\": \"rt-ok+promoted\""),
        "runtime entry should be marked superseded: {runtime_content}"
    );
}

#[test]
fn promote_unpinned_runtime_decision_fails() {
    let dir = TempDir::new().unwrap();
    touch_project(dir.path());
    seed_runtime(dir.path(), "rt-unpinned", false);

    let output = bock_bin()
        .args(["override", "--promote", "rt-unpinned"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "promote on unpinned must fail; stdout was: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("not pinned"), "stderr: {stderr}");
}

#[test]
fn promote_missing_decision_fails() {
    let dir = TempDir::new().unwrap();
    touch_project(dir.path());

    let output = bock_bin()
        .args(["override", "--promote", "does-not-exist"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no decision found"), "stderr: {stderr}");
}

#[test]
fn full_pipeline_runtime_pin_promote_then_strict_build() {
    // 1. Seed an *unpinned* runtime decision and pin it via `bock override`.
    let dir = TempDir::new().unwrap();
    touch_project(dir.path());
    seed_runtime(dir.path(), "rt-full", false);

    let pin = bock_bin()
        .args(["override", "rt-full", "--runtime", "--reason", "reviewed"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        pin.status.success(),
        "pin should succeed: {}",
        String::from_utf8_lossy(&pin.stderr)
    );

    // 2. Promote to build.
    let promote = bock_bin()
        .args(["override", "--promote", "rt-full"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        promote.status.success(),
        "promote should succeed: {}",
        String::from_utf8_lossy(&promote.stderr)
    );

    // 3. The build manifest now has a pinned HandlerChoice — a strict
    //    build should pass the production gate.
    let build_path = dir.path().join(".bock/decisions/build/src/api.bock.json");
    let content = fs::read_to_string(&build_path).unwrap();
    assert!(content.contains("\"pinned\": true"));
    assert!(content.contains("handler_choice"));
}
