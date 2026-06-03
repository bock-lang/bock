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
//! 2. runs `bock build -t <target>` in **project mode** (the default — no
//!    `--source-only`) to emit the per-module source tree *plus* the
//!    scaffolder's run-affordance manifest (`Cargo.toml` / `go.mod` /
//!    `package.json`), making the output runnable in the target toolchain,
//! 3. executes the emitted entry via the target's run plan
//!    ([`ToolchainRegistry::run`]), and
//! 4. asserts the trimmed stdout equals the expected output.
//!
//! Project mode (rather than `--source-only`) is what the harness exercises so
//! the scaffolder's manifest output — the thing that makes the per-module tree
//! actually run — is on the tested path (S6a, DV18). `bock build` in project
//! mode also validates the output via the target toolchain (`cargo check` /
//! `go build` / per-file `node --check` / `tsc --noEmit` / `py_compile`) before
//! the harness runs it; for rust/go that is a redundant compile with the
//! subsequent `cargo run` / `go run .`, but cargo/go reuse their build caches so
//! the cost is a warm rebuild, and the build→run path stays coherent. When a
//! target's toolchain is absent, `bock build` only warns (compilation skipped),
//! so the manifest is still written and the absent-toolchain target is skipped
//! at the run step below.
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

/// The emitted entry file's path **relative to `build/<target>/`**.
///
/// Most targets place the entry at `main.<ext>` at the build root. Rust's
/// per-module output is a cargo-idiomatic `src/`-rooted crate (S3), so its
/// entry is `src/main.rs` — the `Cargo.toml` `[[bin]]` points there and the run
/// plan is `cargo run` from the build root.
fn entry_relpath(target: &str) -> PathBuf {
    match target {
        "rust" => PathBuf::from("src").join("main.rs"),
        other => PathBuf::from(format!("main.{}", entry_extension(other))),
    }
}

/// Stable ordering of the v1 targets for deterministic reporting.
const TARGET_ORDER: &[&str] = &["js", "ts", "python", "rust", "go"];

// Per the per-module-output milestone (DQ19 resolved), every v1 target emits a
// **per-module native import tree** — one target file per reached module, wired
// with the target's native imports/modules — and runs through the target's
// normal runner from the build root (`ToolchainRegistry::run`'s `workdir`). In
// project mode (which this harness now builds — S6a / DV18) the run-affordance
// manifest is emitted by the per-target *scaffolder*, not codegen:
// - **python** — `python3 main.py`; sibling files (`core/option.py`,
//   `_bock_runtime.py`) resolve as package imports (`core` is a PEP 420
//   namespace package). No manifest needed.
// - **js / ts** — `node main.js` (ts first `tsc main.ts`); a minimal
//   `package.json` `{"type":"module"}` (scaffolder) makes Node treat the `.js`
//   tree as ESM, and the emitted `import … from "./core/option.js"` resolve
//   relatively.
// - **rust** — `cargo run` over the emitted Cargo crate (`Cargo.toml`
//   (scaffolder) + `src/main.rs` + the `src/<module>.rs` tree wired with
//   `mod`/`use crate::…`).
// - **go** — `go run .` over the emitted Go module (`go.mod` (scaffolder) + the
//   flat per-module `.go` files in one `package main`, plus a shared
//   `bock_runtime.go`).

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

/// Resolve the conformance directory that holds effect-system fixtures whose
/// expectation is a *diagnostic* (an `// EXPECT: error E<code> at <l>:<c>`),
/// not runnable output. These are driven through `bock check`, not the
/// per-target execution path.
fn conformance_effects_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("conformance/effects")
}

/// Resolve the conformance directory that holds type-checker *diagnostic*
/// fixtures whose expectation is an `// EXPECT: error E<code> at <l>:<c>` for a
/// type error the checker must report (e.g. a type error inside an impl/class
/// method body, per Q-impl-body-typecheck). Driven through `bock check`.
fn conformance_types_diagnostics_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("conformance/types-diagnostics")
}

/// Build `case`'s source for `target` into `project_dir`, returning the
/// directory containing the emitted `main.<ext>` (i.e. `project_dir/build/<target>`).
fn build_fixture(case: &TestCase, target: &str, project_dir: &Path) -> PathBuf {
    let main_path = project_dir.join("main.bock");
    std::fs::write(&main_path, &case.source).expect("write fixture source");

    // Multi-file fixtures: write each auxiliary module so `bock build`'s
    // recursive `.bock` discovery compiles it alongside the entry module,
    // exercising the real cross-module `use` path (DV13).
    for (rel, content) in &case.aux_files {
        let dest = project_dir.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).expect("create aux module dir");
        }
        std::fs::write(&dest, content).expect("write aux fixture source");
    }

    // Project mode (no `--source-only`): emit the per-module source tree plus
    // the scaffolder's run-affordance manifest, so the output is runnable in the
    // target toolchain (S6a, DV18). `run_one` has already confirmed the target's
    // toolchain is present before calling here (skip-if-absent), so the
    // in-build toolchain validation (`cargo check` / `go build` / `node --check`
    // / `tsc --noEmit` / `py_compile`) runs and must succeed.
    let output = Command::new(bock_binary())
        .current_dir(project_dir)
        .args(["build", "-t", target])
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
    let entry = build_dir.join(entry_relpath(target));
    assert!(
        entry.is_file(),
        "expected emitted entrypoint {} for fixture `{}`, but it was not written",
        entry.display(),
        case.name,
    );

    // Every target emits a per-module tree: a multi-file fixture — one that
    // ships an auxiliary `.bock` module via a `// FILE:` marker — must emit a
    // real import tree, i.e. at least one sibling module file *in addition to*
    // the entry `main.<ext>`. (A program that only uses the embedded `core.*`
    // stdlib also emits sibling files, but those live under `core/`; an aux
    // module guarantees a deterministic sibling regardless of stdlib layout.)
    if !case.aux_files.is_empty() {
        let ext = entry_extension(target);
        let mut sibling_count = 0usize;
        let mut walk = vec![build_dir.clone()];
        while let Some(dir) = walk.pop() {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for e in entries.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        // Skip target-toolchain build artifacts that project-mode
                        // `bock build` leaves behind (cargo's `target/`, node's
                        // `node_modules/`): they are not part of the emitted
                        // per-module source tree and would otherwise inflate the
                        // sibling count with dependency sources.
                        let name = p.file_name().and_then(|s| s.to_str());
                        if matches!(name, Some("target" | "node_modules")) {
                            continue;
                        }
                        walk.push(p);
                    } else if p.extension().and_then(|s| s.to_str()) == Some(ext) && p != entry {
                        sibling_count += 1;
                    }
                }
            }
        }
        assert!(
            sibling_count > 0,
            "fixture `{}` is multi-file but target `{target}` (per-module tree) \
             emitted only `main.{ext}` — expected sibling module files",
            case.name,
        );
    }

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

/// The effect-system *diagnostic* fixtures under `conformance/effects/` are
/// driven through `bock check` (they have no runnable output): each declares
/// an `// EXPECT: error E<code> at <line>:<col>` directive for an effect-system
/// error path the spec (§10) defines — a genuinely-unhandled bare op (E1001),
/// and the v1.x-reserved lambda handler surface (E4002). This test wires the
/// previously-inert suite into the harness: every such fixture must `bock
/// check`-fail and surface its declared error code.
///
/// (The *positive* effect forms — §10.4 bare-op-in-`handling`, §10.3 Layer-1/2
/// resolution, innermost-shadow, `with`-clause propagation, cross-module — are
/// covered end to end on every target by the `exec_effect_*` execution fixtures
/// above; those assert real runtime output, which a diagnostic fixture cannot.)
#[test]
fn conformance_effect_diagnostic_fixtures_check_as_declared() {
    let dir = conformance_effects_dir();
    let discovered = discover_tests(&dir);
    assert!(
        !discovered.is_empty(),
        "no effect diagnostic fixtures discovered under {}",
        dir.display()
    );

    let mut checked = 0usize;
    for result in discovered {
        let case = result.expect("effect diagnostic fixture failed to load");

        // Pull the single `error E<code> at <l>:<c>` expectation.
        let Some((code, location)) = case.expectations.iter().find_map(|e| match e {
            Expectation::ErrorAt { code, location } => Some((code.clone(), location.clone())),
            _ => None,
        }) else {
            panic!(
                "effect diagnostic fixture `{}` declares no `// EXPECT: error ...` directive",
                case.name
            );
        };

        // Run `bock check` on the ORIGINAL fixture path (not the
        // directive-stripped `case.source`): the directive's `<line>:<col>`
        // refers to the file as written on disk, including the leading
        // `// TEST:` / `// EXPECT:` comment lines.
        let output = Command::new(bock_binary())
            .arg("check")
            .arg(&case.path)
            .output()
            .expect("failed to spawn bock check");

        // A diagnostic fixture must FAIL to check.
        assert!(
            !output.status.success(),
            "effect diagnostic fixture `{}` checked clean but expected `{code}`:\nstdout:\n{}\nstderr:\n{}",
            case.name,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );

        // The expected error code must appear in the combined output, and the
        // reported location must match the `<line>:<col>` the directive names.
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(
            combined.contains(&code),
            "effect diagnostic fixture `{}` did not surface `{code}`:\n{combined}",
            case.name,
        );
        let loc_str = format!("{}:{}", location.line, location.col);
        assert!(
            combined.contains(&loc_str),
            "effect diagnostic fixture `{}` did not report location `{loc_str}`:\n{combined}",
            case.name,
        );
        checked += 1;
    }

    assert!(
        checked >= 2,
        "expected >= 2 effect diagnostic fixtures, checked {checked}"
    );
}

/// The type-checker *diagnostic* fixtures under `conformance/types-diagnostics/`
/// pin type errors the checker must report but cannot run (they fail to check):
/// each declares an `// EXPECT: error E<code> at <line>:<col>`. These are driven
/// through `bock check`, mirroring [`conformance_effect_diagnostic_fixtures_check_as_declared`].
///
/// Q-impl-body-typecheck: before the fix, `check_item` dispatched only `FnDecl`
/// and `ConstDecl`; `ImplBlock`/`ClassDecl` fell through to a `record Void` arm,
/// so type errors inside impl/class method bodies were silently missed. These
/// fixtures assert a method-body type error now surfaces its declared `E4001`
/// at the body expression's location — for both an `impl` and a `class`.
#[test]
fn conformance_method_body_diagnostic_fixtures_check_as_declared() {
    let dir = conformance_types_diagnostics_dir();
    let discovered = discover_tests(&dir);
    assert!(
        !discovered.is_empty(),
        "no type diagnostic fixtures discovered under {}",
        dir.display()
    );

    let mut checked = 0usize;
    for result in discovered {
        let case = result.expect("type diagnostic fixture failed to load");

        let Some((code, location)) = case.expectations.iter().find_map(|e| match e {
            Expectation::ErrorAt { code, location } => Some((code.clone(), location.clone())),
            _ => None,
        }) else {
            panic!(
                "type diagnostic fixture `{}` declares no `// EXPECT: error ...` directive",
                case.name
            );
        };

        // Run `bock check` on the ORIGINAL fixture path so the directive's
        // `<line>:<col>` lines up with the file as written on disk (including
        // the leading `// TEST:` / `// EXPECT:` comment lines).
        let output = Command::new(bock_binary())
            .arg("check")
            .arg(&case.path)
            .output()
            .expect("failed to spawn bock check");

        assert!(
            !output.status.success(),
            "type diagnostic fixture `{}` checked clean but expected `{code}`:\nstdout:\n{}\nstderr:\n{}",
            case.name,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );

        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(
            combined.contains(&code),
            "type diagnostic fixture `{}` did not surface `{code}`:\n{combined}",
            case.name,
        );
        let loc_str = format!("{}:{}", location.line, location.col);
        assert!(
            combined.contains(&loc_str),
            "type diagnostic fixture `{}` did not report location `{loc_str}`:\n{combined}",
            case.name,
        );
        checked += 1;
    }

    assert!(
        checked >= 2,
        "expected >= 2 type diagnostic fixtures, checked {checked}"
    );
}
