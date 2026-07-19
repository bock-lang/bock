//! The `bock_conformance` tool: single-file cross-target exec-compare.
//!
//! Mirrors the conformance execution harness (`compiler/tests/execution.rs`)
//! in miniature, for one caller-supplied file: run the file on the reference
//! interpreter (`bock run`), then for each requested target whose toolchain
//! is locally available, build it (`bock build -t <target>`) in an isolated
//! temp project and execute the emitted output through the target's run plan
//! ([`ToolchainRegistry::run`]), comparing normalized stdout against the
//! interpreter's. A missing toolchain is a **reported skip** — never a
//! silent pass.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bock_build::toolchain::{ToolchainError, ToolchainRegistry};
use serde_json::{json, Value};

use crate::output::FORMAT_VERSION;

use super::tools::{pretty, spawn_cli, timeout_arg, ToolOutcome};

/// Stable ordering of the v1 targets, matching the conformance harness.
const TARGET_ORDER: &[&str] = &["js", "ts", "python", "rust", "go"];

/// Run the `bock_conformance` tool.
///
/// `Err` is a protocol-level failure (malformed arguments); everything else
/// — including an unreadable file or a failing reference run — comes back as
/// an `Ok` outcome carrying the structured conformance document.
pub fn run(args: &Value) -> Result<ToolOutcome, String> {
    let file = match args.get("file").and_then(Value::as_str) {
        Some(f) => f.to_string(),
        None => return Err("missing required argument `file` (string)".to_string()),
    };
    let timeout = timeout_arg(args)?;
    let requested: Vec<String> = match args.get("targets") {
        None | Some(Value::Null) => TARGET_ORDER.iter().map(|t| (*t).to_string()).collect(),
        Some(Value::Array(items)) => {
            let mut seen = BTreeSet::new();
            for item in items {
                let Some(s) = item.as_str() else {
                    return Err("argument `targets` must be an array of strings".to_string());
                };
                if !TARGET_ORDER.contains(&s) {
                    return Err(format!(
                        "unknown target `{s}` — valid targets: {}",
                        TARGET_ORDER.join(", ")
                    ));
                }
                seen.insert(s);
            }
            // Report in the harness's stable order regardless of input order.
            TARGET_ORDER
                .iter()
                .filter(|t| seen.contains(**t))
                .map(|t| (*t).to_string())
                .collect()
        }
        Some(_) => return Err("argument `targets` must be an array of strings".to_string()),
    };

    // Read the source up front: the file must exist and be self-contained
    // (it is copied into an isolated temp project per target).
    let source = match std::fs::read_to_string(&file) {
        Ok(s) => s,
        Err(e) => {
            let doc = failure_document(&file, &format!("could not read `{file}`: {e}"));
            return Ok(ToolOutcome {
                text: pretty(&doc),
                is_error: true,
            });
        }
    };

    // Reference run: the interpreter is the behavioral baseline.
    let abs_file = std::fs::canonicalize(&file).unwrap_or_else(|_| PathBuf::from(&file));
    let reference = spawn_cli(
        &["run".to_string(), abs_file.display().to_string()],
        None,
        Some(timeout),
    )?;
    let reference_json = json!({
        "runner": "interpreter",
        "exit_code": reference.exit_code,
        "stdout": reference.stdout,
        "stderr": reference.stderr,
    });

    if !reference.success() {
        // Without a baseline there is nothing to compare: every requested
        // target reports "skipped" with the reason, and the run is a failure.
        let targets: Vec<Value> = requested
            .iter()
            .map(|t| {
                target_entry(
                    t,
                    "skipped",
                    None,
                    None,
                    Some(if reference.timed_out {
                        "reference interpreter run timed out".to_string()
                    } else {
                        "reference interpreter run failed — no baseline to compare against"
                            .to_string()
                    }),
                )
            })
            .collect();
        let doc = json!({
            "format_version": FORMAT_VERSION,
            "command": "conformance",
            "outcome": "failed",
            "summary": {
                "targets_exercised": 0,
                "targets_skipped": requested.len(),
                "matched": 0,
                "mismatched": 0,
            },
            "file": file,
            "reference": reference_json,
            "targets": targets,
        });
        return Ok(ToolOutcome {
            text: pretty(&doc),
            is_error: true,
        });
    }

    let registry = ToolchainRegistry::with_builtins();
    let mut targets: Vec<Value> = Vec::new();
    let (mut exercised, mut skipped, mut matched, mut mismatched, mut hard_failures) =
        (0usize, 0usize, 0usize, 0usize, 0usize);

    for target in &requested {
        let entry = compare_target(&registry, target, &source, &reference.stdout, timeout)?;
        match entry["status"].as_str() {
            Some("skipped") => skipped += 1,
            Some("matched") => {
                exercised += 1;
                matched += 1;
            }
            Some("mismatched") => {
                exercised += 1;
                mismatched += 1;
            }
            _ => {
                // build-failed / run-failed: the target was exercised but
                // could not produce comparable output.
                exercised += 1;
                hard_failures += 1;
            }
        }
        targets.push(entry);
    }

    let clean = mismatched == 0 && hard_failures == 0;
    let doc = json!({
        "format_version": FORMAT_VERSION,
        "command": "conformance",
        "outcome": if clean { "clean" } else { "failed" },
        "summary": {
            "targets_exercised": exercised,
            "targets_skipped": skipped,
            "matched": matched,
            "mismatched": mismatched,
        },
        "file": file,
        "reference": reference_json,
        "targets": targets,
    });
    Ok(ToolOutcome {
        text: pretty(&doc),
        is_error: !clean,
    })
}

/// Build + run `source` on one target and compare stdout to the reference.
///
/// Returns the per-target report entry. `Err` is reserved for the
/// cannot-spawn-the-CLI failure propagated from [`spawn_cli`].
fn compare_target(
    registry: &ToolchainRegistry,
    target: &str,
    source: &str,
    reference_stdout: &str,
    timeout: Duration,
) -> Result<Value, String> {
    // skip-if-absent, with the install hint reported — never a silent pass.
    if let Err(ToolchainError::NotFound { install_hint, .. }) = registry.detect(target) {
        return Ok(target_entry(
            target,
            "skipped",
            None,
            None,
            Some(format!("toolchain not found — {install_hint}")),
        ));
    }

    // Isolated temp project: the file is compiled alone (plus the embedded
    // core stdlib), exactly like a conformance fixture.
    let project = TempProjectDir::create(target)?;
    if let Err(e) = std::fs::write(project.path().join("main.bock"), source) {
        return Ok(target_entry(
            target,
            "build-failed",
            None,
            None,
            Some(format!("could not write temp project: {e}")),
        ));
    }

    let build = spawn_cli(
        &["build".to_string(), "-t".into(), target.to_string()],
        Some(project.path()),
        Some(timeout),
    )?;
    if !build.success() {
        let detail = if build.timed_out {
            format!("`bock build -t {target}` timed out")
        } else {
            format!(
                "`bock build -t {target}` failed (exit {:?}):\n{}\n{}",
                build.exit_code, build.stdout, build.stderr
            )
        };
        return Ok(target_entry(
            target,
            "build-failed",
            None,
            None,
            Some(detail),
        ));
    }

    let build_dir = project.path().join("build").join(target);
    match registry.run(target, &build_dir) {
        Ok(output) => {
            let actual = normalize(&output.stdout);
            let expected = normalize(reference_stdout);
            if actual == expected {
                Ok(target_entry(
                    target,
                    "matched",
                    output.exit,
                    Some(output.stdout),
                    None,
                ))
            } else {
                Ok(target_entry(
                    target,
                    "mismatched",
                    output.exit,
                    Some(output.stdout),
                    Some(format!(
                        "stdout differs from the reference interpreter\nexpected: {expected:?}\nactual:   {actual:?}\nstderr:\n{}",
                        output.stderr
                    )),
                ))
            }
        }
        Err(err) => Ok(target_entry(
            target,
            "run-failed",
            None,
            None,
            Some(format!("failed to run the emitted program: {err}")),
        )),
    }
}

/// One entry of the document's `targets` array.
fn target_entry(
    target: &str,
    status: &str,
    exit_code: Option<i32>,
    stdout: Option<String>,
    detail: Option<String>,
) -> Value {
    json!({
        "target": target,
        "status": status,
        "exit_code": exit_code,
        "stdout": stdout,
        "detail": detail,
    })
}

/// The failure document for a run that could not start (unreadable input).
fn failure_document(file: &str, message: &str) -> Value {
    json!({
        "format_version": FORMAT_VERSION,
        "command": "conformance",
        "outcome": "failed",
        "summary": {
            "targets_exercised": 0,
            "targets_skipped": 0,
            "matched": 0,
            "mismatched": 0,
        },
        "file": file,
        "error": { "message": message },
        "targets": [],
    })
}

/// Normalize program stdout for comparison: strip `\r` (Windows toolchains
/// can inject it) and trailing newlines — the same rule the conformance
/// execution harness applies.
fn normalize(stdout: &str) -> String {
    stdout.replace('\r', "").trim_end_matches('\n').to_string()
}

/// A process-unique temp project directory, removed (best effort) on drop.
///
/// Hand-rolled on `std` (the `tempfile` crate is a dev-dependency only, and
/// the server adds no runtime dependencies): uniqueness comes from the
/// process id plus a process-wide counter.
struct TempProjectDir {
    path: PathBuf,
}

impl TempProjectDir {
    /// Create a fresh directory under the system temp dir.
    fn create(label: &str) -> Result<Self, String> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "bock-mcp-conformance-{}-{n}-{label}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path)
            .map_err(|e| format!("could not create temp project dir {}: {e}", path.display()))?;
        Ok(Self { path })
    }

    /// The directory path.
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempProjectDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_cr_and_trailing_newlines() {
        assert_eq!(normalize("a\r\nb\n\n"), "a\nb");
        assert_eq!(normalize("x"), "x");
        assert_eq!(normalize(""), "");
    }

    #[test]
    fn unknown_target_is_a_protocol_error() {
        let err = run(&json!({ "file": "x.bock", "targets": ["cobol"] }))
            .expect_err("unknown target must be rejected");
        assert!(err.contains("unknown target"), "{err}");
    }

    #[test]
    fn unreadable_file_reports_a_failure_document() {
        let outcome = run(&json!({
            "file": "/nonexistent/definitely-not-here.bock",
            "targets": [],
        }))
        .expect("valid call");
        assert!(outcome.is_error);
        let doc: Value = serde_json::from_str(&outcome.text).expect("json doc");
        assert_eq!(doc["command"], "conformance");
        assert_eq!(doc["outcome"], "failed");
        assert!(doc["error"]["message"]
            .as_str()
            .expect("message")
            .contains("could not read"));
    }

    #[test]
    fn temp_project_dir_is_created_and_removed() {
        let path = {
            let dir = TempProjectDir::create("test").expect("create");
            assert!(dir.path().is_dir());
            dir.path().to_path_buf()
        };
        assert!(!path.exists(), "dropped dir must be removed");
    }
}
