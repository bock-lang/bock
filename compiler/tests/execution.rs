//! Cross-target conformance **execution** test.
//!
//! This is PR1 of the codegen-correctness workstream: pure test
//! infrastructure. It does not change codegen or interpreter behavior. It
//! turns silent codegen defects into observable test failures by actually
//! *running* generated programs and comparing their stdout against the
//! `// EXPECT: output "..."` directive on each fixture.
//!
//! For every conformance fixture under `conformance/exec/` that declares an
//! [`Expectation::Output`], and for every target whose toolchain is installed
//! on the host, the harness:
//!
//! 1. writes the fixture source into an isolated temp project,
//! 2. runs `bock build -t <target> --source-only` to emit target source,
//! 3. executes the emitted `main.<ext>` via the target's run plan
//!    ([`ToolchainRegistry::run`]), and
//! 4. asserts the trimmed stdout equals the expected output.
//!
//! Targets whose toolchain is **absent** are *skipped* (recorded and printed),
//! not failed — so a developer without, say, Go installed still gets a green
//! run for the targets they do have. To make absence a hard error on CI lanes
//! that install toolchains, set `BOCK_CONFORMANCE_REQUIRE=rust,go,...` (or
//! `all`); any required-but-absent target then fails the test.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use bock_build::toolchain::{ToolchainError, ToolchainRegistry};
use bock_test_harness::{discover_tests, Expectation, TestCase};

/// Map a target id to the emitted entrypoint file extension.
///
/// Mirrors `bock build`, which writes the entry module as `main.<ext>` under
/// `build/<target>/`.
fn entry_extension(target: &str) -> &'static str {
    match target {
        "js" => "js",
        "ts" => "ts",
        "python" => "py",
        "rust" => "rs",
        "go" => "go",
        other => panic!("unknown target id: {other}"),
    }
}

/// Stable ordering of the v1 targets for deterministic reporting.
const TARGET_ORDER: &[&str] = &["js", "ts", "python", "rust", "go"];

/// Locate the compiled `bock` CLI binary.
///
/// `cargo test --workspace` builds every workspace binary, so the binary is
/// normally found next to the test runner (`<target>/<profile>/bock`). When the
/// test crate is run in isolation (`cargo test -p bock-test-harness`), the
/// binary may be missing; in that case we build it on demand.
fn bock_binary() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let exe_name = if cfg!(windows) { "bock.exe" } else { "bock" };

        // current_exe() -> <target>/<profile>/deps/execution-<hash>
        // The sibling binary lives at <target>/<profile>/<exe_name>.
        if let Ok(test_exe) = std::env::current_exe() {
            // deps/ -> profile dir
            if let Some(profile_dir) = test_exe.parent().and_then(|p| p.parent()) {
                let candidate = profile_dir.join(exe_name);
                if candidate.is_file() {
                    return candidate;
                }
            }
        }

        // Fallback: build it explicitly. This keeps `-p bock-test-harness`
        // working in isolation without relying on build ordering.
        let status = Command::new(env!("CARGO"))
            .args(["build", "-p", "bock", "--bin", "bock"])
            .status()
            .expect("failed to invoke cargo to build the bock binary");
        assert!(status.success(), "cargo build -p bock failed");

        // After building, re-derive the profile dir and locate the binary.
        if let Ok(test_exe) = std::env::current_exe() {
            if let Some(profile_dir) = test_exe.parent().and_then(|p| p.parent()) {
                let candidate = profile_dir.join(exe_name);
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
        panic!("could not locate the `bock` binary after building it");
    })
    .as_path()
}

/// Parse the `BOCK_CONFORMANCE_REQUIRE` env override into a set of target ids.
///
/// `all` expands to every v1 target. Empty / unset yields an empty set
/// (everything is skip-if-absent).
fn required_targets() -> BTreeSet<String> {
    let mut required = BTreeSet::new();
    let Ok(value) = std::env::var("BOCK_CONFORMANCE_REQUIRE") else {
        return required;
    };
    for token in value.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        if token.eq_ignore_ascii_case("all") {
            for t in TARGET_ORDER {
                required.insert((*t).to_string());
            }
        } else {
            required.insert(token.to_string());
        }
    }
    required
}

/// Resolve the conformance directory that holds execution fixtures.
fn conformance_exec_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("conformance/exec")
}

/// Build `case`'s source for `target` into `project_dir`, returning the
/// directory containing the emitted `main.<ext>` (i.e. `project_dir/build/<target>`).
fn build_fixture(case: &TestCase, target: &str, project_dir: &Path) -> PathBuf {
    let main_path = project_dir.join("main.bock");
    std::fs::write(&main_path, &case.source).expect("write fixture source");

    let output = Command::new(bock_binary())
        .current_dir(project_dir)
        .args(["build", "-t", target, "--source-only"])
        .output()
        .expect("failed to spawn bock build");

    assert!(
        output.status.success(),
        "`bock build -t {target}` failed for fixture `{}`:\nstdout:\n{}\nstderr:\n{}",
        case.name,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let build_dir = project_dir.join("build").join(target);
    let entry = build_dir.join(format!("main.{}", entry_extension(target)));
    assert!(
        entry.is_file(),
        "expected emitted entrypoint {} for fixture `{}`, but it was not written",
        entry.display(),
        case.name,
    );
    build_dir
}

/// Outcome of attempting one (fixture, target) pair.
enum Outcome {
    /// Ran and matched the expected output.
    Passed,
    /// Toolchain absent and not required — recorded for reporting.
    Skipped,
    /// Ran (or attempted) and failed; carries a human-readable explanation.
    Failed(String),
}

/// Run one fixture against one target, honoring skip-if-absent semantics.
fn run_one(
    registry: &ToolchainRegistry,
    case: &TestCase,
    target: &str,
    expected: &str,
    required: &BTreeSet<String>,
) -> Outcome {
    // skip-if-absent (unless the target is explicitly required).
    if let Err(ToolchainError::NotFound { install_hint, .. }) = registry.detect(target) {
        if required.contains(target) {
            return Outcome::Failed(format!(
                "target `{target}` is required (BOCK_CONFORMANCE_REQUIRE) but its \
                 toolchain is absent.\n  hint: {install_hint}"
            ));
        }
        return Outcome::Skipped;
    }

    let tmp = tempfile::tempdir().expect("create temp project dir");
    let build_dir = build_fixture(case, target, tmp.path());

    match registry.run(target, &build_dir) {
        Ok(output) => {
            let actual = output.stdout.trim_end_matches(['\n', '\r']);
            let expected_trimmed = expected.trim_end_matches(['\n', '\r']);
            if actual == expected_trimmed {
                Outcome::Passed
            } else {
                Outcome::Failed(format!(
                    "output mismatch for fixture `{}` on target `{target}`:\n  \
                     command: {}\n  expected: {expected_trimmed:?}\n  actual:   {actual:?}\n  \
                     exit: {:?}\n  stderr:\n{}",
                    case.name, output.command, output.exit, output.stderr,
                ))
            }
        }
        Err(err) => Outcome::Failed(format!(
            "failed to run fixture `{}` on target `{target}`: {err}",
            case.name
        )),
    }
}

#[test]
fn conformance_fixtures_execute_on_every_present_target() {
    let registry = ToolchainRegistry::with_builtins();
    let required = required_targets();
    let dir = conformance_exec_dir();

    let discovered = discover_tests(&dir);
    assert!(
        !discovered.is_empty(),
        "no fixtures discovered under {}; expected at least one execution fixture",
        dir.display()
    );

    let mut cases: Vec<TestCase> = Vec::new();
    for result in discovered {
        match result {
            Ok(tc) => cases.push(tc),
            Err(e) => panic!("execution fixture failed to load: {e}"),
        }
    }

    // Only fixtures that declare an output expectation are executable here.
    // A fixture may also declare `// EXPECT: targets <ids>` to restrict which
    // backends it runs on (absent ⇒ all targets).
    let output_cases: Vec<(&TestCase, &str, Option<BTreeSet<String>>)> = cases
        .iter()
        .filter_map(|tc| {
            let text = tc.expectations.iter().find_map(|e| match e {
                Expectation::Output(text) => Some(text.as_str()),
                _ => None,
            })?;
            let targets = tc.expectations.iter().find_map(|e| match e {
                Expectation::Targets(set) => Some(set.clone()),
                _ => None,
            });
            Some((tc, text, targets))
        })
        .collect();
    assert!(
        !output_cases.is_empty(),
        "no `// EXPECT: output \"...\"` fixtures under {}",
        dir.display()
    );

    let mut passed: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();

    for (case, expected, targets) in &output_cases {
        for target in TARGET_ORDER {
            // Honor a per-fixture `targets` restriction.
            if let Some(allowed) = targets {
                if !allowed.contains(*target) {
                    continue;
                }
            }
            match run_one(&registry, case, target, expected, &required) {
                Outcome::Passed => passed.push(format!("{}::{target}", case.name)),
                Outcome::Skipped => skipped.push(format!("{}::{target}", case.name)),
                Outcome::Failed(msg) => failures.push(msg),
            }
        }
    }

    // Always print a coverage summary so an all-skipped green run is not
    // mistaken for real coverage.
    eprintln!("\n=== conformance execution summary ===");
    eprintln!("  passed:  {} ({})", passed.len(), passed.join(", "));
    eprintln!(
        "  skipped: {} (toolchain absent: {})",
        skipped.len(),
        if skipped.is_empty() {
            "none".to_string()
        } else {
            skipped.join(", ")
        }
    );
    eprintln!("  failed:  {}", failures.len());
    if !required.is_empty() {
        eprintln!(
            "  required targets (BOCK_CONFORMANCE_REQUIRE): {}",
            required.iter().cloned().collect::<Vec<_>>().join(", ")
        );
    }

    assert!(
        failures.is_empty(),
        "conformance execution failures:\n\n{}",
        failures.join("\n\n")
    );
}
