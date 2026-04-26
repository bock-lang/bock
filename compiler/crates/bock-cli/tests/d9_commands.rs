//! End-to-end tests for the D.9 CLI surface:
//! `bock inspect`, `bock pin`, `bock unpin`, extended `bock override`,
//! and `bock cache`.

use std::fs;
use std::path::Path;
use std::process::Command;

use tempfile::TempDir;

fn bock_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_bock"))
}

fn touch_project(dir: &Path) {
    fs::write(dir.join("bock.project"), "[project]\nname = \"t\"\n").unwrap();
    fs::write(dir.join("main.bock"), "fn id(x: Int) -> Int { x }\n").unwrap();
}

fn seed_build_decision(dir: &Path, id: &str, pinned: bool) {
    let path = dir.join(".bock/decisions/build/src/api.bock.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let pin_reason = if pinned { "\"manual\"" } else { "null" };
    let pinned_at = if pinned {
        "\"2026-04-22T10:00:00Z\""
    } else {
        "null"
    };
    let pinned_by = if pinned { "\"alice\"" } else { "null" };

    let existing = fs::read_to_string(&path).unwrap_or_else(|_| "[]".into());
    let mut entries: serde_json::Value =
        serde_json::from_str(&existing).unwrap_or_else(|_| serde_json::json!([]));
    let new_entry = serde_json::json!({
        "id": id,
        "module": "src/api.bock",
        "target": "js",
        "decision_type": "codegen",
        "choice": "// stub code",
        "alternatives": [],
        "reasoning": "stub",
        "model_id": "stub:stub",
        "confidence": 0.9,
        "pinned": pinned,
        "pin_reason": serde_json::from_str::<serde_json::Value>(pin_reason).unwrap(),
        "pinned_at": serde_json::from_str::<serde_json::Value>(pinned_at).unwrap(),
        "pinned_by": serde_json::from_str::<serde_json::Value>(pinned_by).unwrap(),
        "timestamp": "2026-04-22T10:00:00Z"
    });
    entries.as_array_mut().unwrap().push(new_entry);
    fs::write(&path, serde_json::to_string_pretty(&entries).unwrap()).unwrap();
}

fn seed_runtime_decision(dir: &Path, id: &str, pinned: bool) {
    let path = dir.join(".bock/decisions/runtime/src/api.bock.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let pin_reason = if pinned { "\"manual\"" } else { "null" };
    let pinned_at = if pinned {
        "\"2026-04-22T10:00:00Z\""
    } else {
        "null"
    };
    let pinned_by = if pinned { "\"alice\"" } else { "null" };
    let existing = fs::read_to_string(&path).unwrap_or_else(|_| "[]".into());
    let mut entries: serde_json::Value =
        serde_json::from_str(&existing).unwrap_or_else(|_| serde_json::json!([]));
    let new_entry = serde_json::json!({
        "id": id,
        "module": "src/api.bock",
        "target": null,
        "decision_type": "adaptive_recovery",
        "choice": "retry(max=3)",
        "alternatives": ["use_cached"],
        "reasoning": "timeout retry",
        "model_id": "stub:stub",
        "confidence": 0.92,
        "pinned": pinned,
        "pin_reason": serde_json::from_str::<serde_json::Value>(pin_reason).unwrap(),
        "pinned_at": serde_json::from_str::<serde_json::Value>(pinned_at).unwrap(),
        "pinned_by": serde_json::from_str::<serde_json::Value>(pinned_by).unwrap(),
        "timestamp": "2026-04-22T10:00:00Z"
    });
    entries.as_array_mut().unwrap().push(new_entry);
    fs::write(&path, serde_json::to_string_pretty(&entries).unwrap()).unwrap();
}

// ── inspect ──────────────────────────────────────────────────────────────────

#[test]
fn inspect_defaults_to_build_scope() {
    let dir = TempDir::new().unwrap();
    touch_project(dir.path());
    seed_build_decision(dir.path(), "b1", false);
    seed_runtime_decision(dir.path(), "r1", false);

    let out = bock_bin()
        .args(["inspect", "--json"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"id\": \"b1\""), "stdout: {stdout}");
    assert!(!stdout.contains("\"id\": \"r1\""));
}

#[test]
fn inspect_runtime_shows_runtime_only() {
    let dir = TempDir::new().unwrap();
    touch_project(dir.path());
    seed_build_decision(dir.path(), "b1", false);
    seed_runtime_decision(dir.path(), "r1", false);

    let out = bock_bin()
        .args(["inspect", "--runtime", "--json"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"id\": \"r1\""));
    assert!(!stdout.contains("\"id\": \"b1\""));
}

#[test]
fn inspect_all_includes_prefixed_ids() {
    let dir = TempDir::new().unwrap();
    touch_project(dir.path());
    seed_build_decision(dir.path(), "b1", false);
    seed_runtime_decision(dir.path(), "r1", false);

    let out = bock_bin()
        .args(["inspect", "--all", "--json"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("build:b1"), "stdout: {stdout}");
    assert!(stdout.contains("runtime:r1"), "stdout: {stdout}");
}

#[test]
fn inspect_decisions_unpinned_filters_correctly() {
    let dir = TempDir::new().unwrap();
    touch_project(dir.path());
    seed_build_decision(dir.path(), "pinned-one", true);
    seed_build_decision(dir.path(), "unpinned-one", false);

    let out = bock_bin()
        .args(["inspect", "decisions", "--unpinned", "--json"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"id\": \"unpinned-one\""));
    assert!(!stdout.contains("\"id\": \"pinned-one\""));
}

#[test]
fn inspect_decision_by_bare_id() {
    let dir = TempDir::new().unwrap();
    touch_project(dir.path());
    seed_build_decision(dir.path(), "specific", false);

    let out = bock_bin()
        .args(["inspect", "decision", "specific", "--json"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"scope\": \"build\""));
    assert!(stdout.contains("\"id\": \"specific\""));
}

#[test]
fn inspect_decision_by_prefixed_id_disambiguates_same_bare_id() {
    let dir = TempDir::new().unwrap();
    touch_project(dir.path());
    seed_build_decision(dir.path(), "shared", false);
    seed_runtime_decision(dir.path(), "shared", false);

    let ambiguous = bock_bin()
        .args(["inspect", "decision", "shared"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(!ambiguous.status.success());
    let err = String::from_utf8_lossy(&ambiguous.stderr);
    assert!(err.contains("ambiguous"), "stderr: {err}");

    let ok = bock_bin()
        .args(["inspect", "decision", "runtime:shared", "--json"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(ok.status.success());
    let stdout = String::from_utf8_lossy(&ok.stdout);
    assert!(stdout.contains("\"scope\": \"runtime\""));
}

#[test]
fn inspect_cache_prints_entry_count() {
    let dir = TempDir::new().unwrap();
    touch_project(dir.path());

    let out = bock_bin()
        .args(["inspect", "cache"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("entries: 0"));
}

// ── pin / unpin ──────────────────────────────────────────────────────────────

#[test]
fn pin_bare_id_sets_pinned_true_when_unambiguous() {
    let dir = TempDir::new().unwrap();
    touch_project(dir.path());
    seed_build_decision(dir.path(), "abc123", false);

    let out = bock_bin()
        .args(["pin", "abc123", "--reason", "reviewed"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let content = fs::read_to_string(
        dir.path().join(".bock/decisions/build/src/api.bock.json"),
    )
    .unwrap();
    assert!(content.contains("\"pinned\": true"));
    assert!(content.contains("\"reviewed\""));
}

#[test]
fn pin_prefixed_id_is_accepted() {
    let dir = TempDir::new().unwrap();
    touch_project(dir.path());
    seed_build_decision(dir.path(), "b1", false);

    let out = bock_bin()
        .args(["pin", "build:b1"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
}

#[test]
fn pin_all_runtime_pins_only_runtime_decisions() {
    let dir = TempDir::new().unwrap();
    touch_project(dir.path());
    seed_build_decision(dir.path(), "b1", false);
    seed_runtime_decision(dir.path(), "r1", false);

    let out = bock_bin()
        .args(["pin", "--all-runtime"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());

    let build =
        fs::read_to_string(dir.path().join(".bock/decisions/build/src/api.bock.json")).unwrap();
    assert!(
        build.contains("\"pinned\": false"),
        "build must stay unpinned: {build}"
    );
    let runtime = fs::read_to_string(
        dir.path().join(".bock/decisions/runtime/src/api.bock.json"),
    )
    .unwrap();
    assert!(runtime.contains("\"pinned\": true"));
}

#[test]
fn pin_all_in_module_only_matches_substring() {
    let dir = TempDir::new().unwrap();
    touch_project(dir.path());
    seed_build_decision(dir.path(), "api1", false);
    // A second build decision in a different module.
    let other = dir
        .path()
        .join(".bock/decisions/build/src/other.bock.json");
    fs::create_dir_all(other.parent().unwrap()).unwrap();
    fs::write(
        &other,
        serde_json::to_string_pretty(&serde_json::json!([{
            "id": "other1",
            "module": "src/other.bock",
            "target": "js",
            "decision_type": "codegen",
            "choice": "x",
            "alternatives": [],
            "reasoning": null,
            "model_id": "stub:stub",
            "confidence": 0.9,
            "pinned": false,
            "pin_reason": null,
            "pinned_at": null,
            "pinned_by": null,
            "timestamp": "2026-04-22T10:00:00Z"
        }]))
        .unwrap(),
    )
    .unwrap();

    let out = bock_bin()
        .args(["pin", "--all-in", "src/api"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let api_content =
        fs::read_to_string(dir.path().join(".bock/decisions/build/src/api.bock.json")).unwrap();
    assert!(api_content.contains("\"pinned\": true"));
    let other_content = fs::read_to_string(&other).unwrap();
    assert!(other_content.contains("\"pinned\": false"));
}

#[test]
fn unpin_clears_pin_metadata() {
    let dir = TempDir::new().unwrap();
    touch_project(dir.path());
    seed_build_decision(dir.path(), "px", true);

    let out = bock_bin()
        .args(["unpin", "px"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let content = fs::read_to_string(
        dir.path().join(".bock/decisions/build/src/api.bock.json"),
    )
    .unwrap();
    assert!(content.contains("\"pinned\": false"));
}

// ── override: choice replacement ─────────────────────────────────────────────

#[test]
fn override_inline_choice_replaces_and_auto_pins() {
    let dir = TempDir::new().unwrap();
    touch_project(dir.path());
    seed_build_decision(dir.path(), "to-override", false);

    let out = bock_bin()
        .args(["override", "to-override", "new-code-here"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let content = fs::read_to_string(
        dir.path().join(".bock/decisions/build/src/api.bock.json"),
    )
    .unwrap();
    assert!(content.contains("\"choice\": \"new-code-here\""));
    assert!(content.contains("\"pinned\": true"));
}

#[test]
fn override_from_file_reads_file_contents() {
    let dir = TempDir::new().unwrap();
    touch_project(dir.path());
    seed_build_decision(dir.path(), "file-override", false);
    let body = "lines\nof\ncode\n";
    let choice_file = dir.path().join("choice.txt");
    fs::write(&choice_file, body).unwrap();

    let out = bock_bin()
        .args([
            "override",
            "file-override",
            "--from-file",
            choice_file.to_str().unwrap(),
        ])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let content = fs::read_to_string(
        dir.path().join(".bock/decisions/build/src/api.bock.json"),
    )
    .unwrap();
    // The file body is escaped with \n in JSON form.
    assert!(content.contains("lines\\nof\\ncode"));
    assert!(content.contains("\"pinned\": true"));
}

// ── cache ────────────────────────────────────────────────────────────────────

#[test]
fn cache_clear_wipes_ai_cache_only_by_default() {
    let dir = TempDir::new().unwrap();
    touch_project(dir.path());
    // Seed ai-cache file and decisions file.
    let ai_cache = dir.path().join(".bock/ai-cache/ab/abcd.json");
    fs::create_dir_all(ai_cache.parent().unwrap()).unwrap();
    fs::write(&ai_cache, "{\"body\":\"x\"}").unwrap();
    seed_build_decision(dir.path(), "keep", false);

    let out = bock_bin()
        .args(["cache", "clear"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(!dir.path().join(".bock/ai-cache").exists());
    assert!(dir
        .path()
        .join(".bock/decisions/build/src/api.bock.json")
        .exists());
}

#[test]
fn cache_clear_decisions_runtime_only_preserves_build() {
    let dir = TempDir::new().unwrap();
    touch_project(dir.path());
    seed_build_decision(dir.path(), "keep-build", false);
    seed_runtime_decision(dir.path(), "drop-runtime", false);

    let out = bock_bin()
        .args(["cache", "clear", "--decisions", "--runtime"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(dir
        .path()
        .join(".bock/decisions/build/src/api.bock.json")
        .exists());
    assert!(!dir.path().join(".bock/decisions/runtime").exists());
}

#[test]
fn cache_stats_prints_entry_counts() {
    let dir = TempDir::new().unwrap();
    touch_project(dir.path());
    seed_build_decision(dir.path(), "b1", false);
    seed_runtime_decision(dir.path(), "r1", false);

    let out = bock_bin()
        .args(["cache", "stats"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("build:"), "stdout: {stdout}");
    assert!(stdout.contains("runtime:"), "stdout: {stdout}");
}
