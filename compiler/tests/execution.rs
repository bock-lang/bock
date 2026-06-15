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

/// A process-private cargo target directory for the rust execution fixtures.
///
/// # The race this isolates
///
/// Every rust fixture this harness builds is a Cargo crate whose `[package]`
/// and `[[bin]]` are *both* named `bock_app` (a fixed, project-independent name;
/// see the rust scaffolder). The build→run path for a rust fixture runs `cargo`
/// twice over that crate: `bock build -t rust` validates it with `cargo check`,
/// then [`ToolchainRegistry::run`] executes it with `cargo run --quiet`. Both
/// `cargo` invocations resolve their output directory from the **process
/// environment's `CARGO_TARGET_DIR`**, which the test process inherits (CI and
/// the worktree session both export it so workspace crates share build
/// artifacts).
///
/// Under `cargo test --workspace`, the per-binary test processes run in
/// parallel, and several of them (`bock-test-harness`'s `execution`,
/// `bock-cli`'s `build_command`) shell out to `cargo` against the **same**
/// shared `CARGO_TARGET_DIR`. Because the crate/bin name is the constant
/// `bock_app`, those concurrent builds all write the **same**
/// `<CARGO_TARGET_DIR>/debug/bock_app` artifact. One process's compile can
/// clobber the `bock_app` binary in between this harness's `cargo check` and the
/// `cargo run` that executes it — so a fixture runs *another fixture's* program
/// and we see cross-fixture stdout contamination (e.g. `exec_map_literal`
/// printing `exec_list_first_last_concat`'s output). A per-`config.toml`
/// `build.target-dir` cannot fix this: the `CARGO_TARGET_DIR` **env var takes
/// precedence** over `.cargo/config.toml`, so as long as it is set in the
/// environment it wins.
///
/// # The fix
///
/// Point every `cargo` invocation on the rust execution path at a target
/// directory **private to this test process**, so its `bock_app` artifact can
/// never be observed or overwritten by another process's build. The directory
/// is created once and reused across fixtures: the execution test runs its rust
/// fixtures sequentially (a single `#[test]` looping the fixtures), so they
/// never collide *with each other* in this one dir, and warm cargo caches keep
/// the repeated builds fast. We set it both on the process environment (so the
/// `cargo run` step, which [`ToolchainRegistry::run`] spawns with the inherited
/// environment, picks it up) and explicitly on the `bock build` command (so the
/// in-build `cargo check` validation uses it too). The env var is assigned
/// exactly once via [`OnceLock`] and never toggled, so it does not race the
/// other tests in this binary (the diagnostic tests drive `bock check`, which
/// never shells out to `cargo`).
fn rust_target_dir() -> &'static Path {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    let dir = DIR.get_or_init(|| {
        let td = tempfile::Builder::new()
            .prefix("bock-exec-rust-target-")
            .tempdir()
            .expect("create private cargo target dir for rust execution fixtures");
        // Make the inherited-environment `cargo run` step (spawned by
        // `ToolchainRegistry::run`) use this private dir as well. Edition 2021,
        // so `set_var` is safe; assigned once here and never changed, with no
        // concurrent reader of `CARGO_TARGET_DIR` elsewhere in this binary.
        std::env::set_var("CARGO_TARGET_DIR", td.path());
        td
    });
    dir.path()
}

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

/// Resolve the **root** of the conformance fixture tree — every category.
///
/// The check-driven tests below walk this whole tree, so a diagnostic or
/// `no_errors` expectation declared in ANY category (present or future) is
/// asserted against the live compiler. Until 2026-06-10 only `effects/` and
/// `types-diagnostics/` were driven through `bock check`; the `error E<code>
/// at <l>:<c>` directives everywhere else were parsed but never executed,
/// which let `types/type_mismatch.bock` declare a code the compiler does not
/// emit (E0205) for weeks (Q-conformance-directive-wiring).
fn conformance_root_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("conformance")
}

/// The conformance category (top-level directory under `conformance/`) a
/// fixture lives in, e.g. `types-diagnostics` or `stdlib` (subdirectories like
/// `stdlib/convert` all report as `stdlib`).
fn fixture_category(root: &Path, fixture: &Path) -> String {
    fixture
        .strip_prefix(root)
        .ok()
        .and_then(|rel| rel.components().next())
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .unwrap_or_else(|| {
            panic!(
                "fixture {} is not under {}",
                fixture.display(),
                root.display()
            )
        })
}

/// Categories that must each contribute at least one check-driven *diagnostic*
/// fixture (an `// EXPECT: error E<code> at <l>:<c>`).
///
/// This is a **tripwire, not a filter**: the diagnostic test walks the entire
/// conformance tree, so every category — including ones not listed here — is
/// wired automatically. The list pins the categories that are known to carry
/// diagnostic fixtures today, so a discovery regression (a directory rename, a
/// walk bug, fixtures silently dropping out) fails loudly instead of shrinking
/// coverage. When a fixture set legitimately moves, update this list in the
/// same PR. Keep it in lockstep with `HARNESS_WIRED_DIAGNOSTIC_CATEGORIES` in
/// `tools/corpus/generate.py` (which mirrors the harness's enforcement scope).
const DIAGNOSTIC_FIXTURE_CATEGORIES: &[&str] = &[
    "context",
    "effects",
    "parse",
    "stdlib",
    "types",
    "types-diagnostics",
];

/// Floor on the number of fixtures the diagnostic test must drive (the count
/// on 2026-06-10). Falling below it means discovery silently lost fixtures.
const MIN_DIAGNOSTIC_FIXTURES: usize = 14;

/// Floor on the number of fixtures the `no_errors` test must drive (the count
/// on 2026-06-10). Falling below it means discovery silently lost fixtures.
const MIN_NO_ERRORS_FIXTURES: usize = 40;

/// Categories that must each contribute at least one **output** fixture (an
/// `// EXPECT: output "..."`) to the cross-target execution lane.
///
/// Like [`DIAGNOSTIC_FIXTURE_CATEGORIES`] this is a **tripwire, not a filter**:
/// the execution test walks the entire conformance tree (every category — even
/// ones not listed here — is wired automatically), and this list pins the
/// categories known to carry output fixtures today. A discovery regression (a
/// directory rename, a walk bug, fixtures silently dropping out of the run)
/// then fails loudly instead of shrinking coverage. Before 2026-06-15 the
/// execution lane discovered fixtures from `conformance/exec/` ONLY, so the
/// output fixtures under `interp/`, `stdlib/`, and `time/` were parsed but
/// never executed (Q-exec-output-directive-wiring) — exactly the silent
/// exclusion this tripwire now guards against. When a fixture set legitimately
/// moves, update this list in the same PR.
const OUTPUT_FIXTURE_CATEGORIES: &[&str] = &["exec", "interp", "stdlib", "time"];

/// Floor on the number of `// EXPECT: output` fixtures the execution lane must
/// discover and schedule (the count on 2026-06-15 was 248; the floor sits below
/// it with headroom for routine churn). Falling below it means discovery
/// silently lost output fixtures — the precise failure mode
/// Q-exec-output-directive-wiring fixed (5+ fixtures outside `exec/` were never
/// executed). Mirrors [`MIN_DIAGNOSTIC_FIXTURES`] / [`MIN_NO_ERRORS_FIXTURES`].
const MIN_OUTPUT_FIXTURES: usize = 200;

/// Build `case`'s source for `target` into `project_dir`, returning the
/// directory containing the emitted `main.<ext>` (i.e.
/// `project_dir/build/<target>`).
///
/// A **fixture-attributable** failure — the target toolchain rejecting the
/// emitted code, a missing entrypoint, or a multi-file fixture that emitted no
/// sibling modules — is returned as `Err(message)` so the caller records it as
/// one `Outcome::Failed` and the run continues to the next (fixture, target),
/// surfacing EVERY failing pair in a single run instead of aborting at the
/// first. Only genuine harness faults (cannot spawn `bock build`, cannot write
/// the temp project) still panic, since they invalidate the whole run.
fn build_fixture(case: &TestCase, target: &str, project_dir: &Path) -> Result<PathBuf, String> {
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
    let mut cmd = Command::new(bock_binary());
    cmd.current_dir(project_dir).args(["build", "-t", target]);
    // Rust's in-build `cargo check` validation shares `CARGO_TARGET_DIR` with
    // every other workspace `cargo` invocation; point it at this process's
    // private rust target dir so the constant-named `bock_app` artifact is never
    // clobbered by a concurrent build (see `rust_target_dir`). Other targets do
    // not use cargo, so they are left untouched.
    if target == "rust" {
        cmd.env("CARGO_TARGET_DIR", rust_target_dir());
    }
    let output = cmd.output().expect("failed to spawn bock build");

    if !output.status.success() {
        return Err(format!(
            "`bock build -t {target}` failed for fixture `{}` \
             (declare the exclusion with `// EXPECT: targets ...` if this is a \
             known per-target codegen gap):\nstdout:\n{}\nstderr:\n{}",
            case.name,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }

    let build_dir = project_dir.join("build").join(target);
    let entry = build_dir.join(entry_relpath(target));
    if !entry.is_file() {
        return Err(format!(
            "expected emitted entrypoint {} for fixture `{}`, but it was not written",
            entry.display(),
            case.name,
        ));
    }

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
        if sibling_count == 0 {
            return Err(format!(
                "fixture `{}` is multi-file but target `{target}` (per-module tree) \
                 emitted only `main.{ext}` — expected sibling module files",
                case.name,
            ));
        }
    }

    Ok(build_dir)
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
    let build_dir = match build_fixture(case, target, tmp.path()) {
        Ok(dir) => dir,
        Err(msg) => return Outcome::Failed(msg),
    };

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

/// Every conformance fixture — in EVERY category — that declares
/// `// EXPECT: output "..."` is compiled in project mode, run on each present
/// target, and stdout-diffed against the directive.
///
/// Until 2026-06-15 this lane discovered fixtures from `conformance/exec/`
/// **only**, while the three check-driven tests below
/// already walked the whole tree. The output fixtures under `interp/`,
/// `stdlib/`, and `time/` were therefore parsed but never executed — a silent
/// exclusion that let a fixture's `// EXPECT: output` drift from its program's
/// actual stdout without any cross-target test noticing
/// (Q-exec-output-directive-wiring). This walk mirrors the diagnostic lane's
/// whole-tree approach (#341): every present-and-future output fixture
/// auto-wires, with no per-directory allow-list to forget to update.
///
/// A fixture may declare `// EXPECT: targets <ids>` to restrict which backends
/// it runs on (absent ⇒ every target). That directive is the **loud, declared**
/// way to exclude a target a fixture genuinely cannot build on today — e.g.
/// `stdlib/compare/compare_output_smoke.bock` excludes `rust`, where the user
/// `Equatable::eq` impl collides with Rust's `PartialEq::eq` at `a.eq(&b)`
/// (E0034, a separate codegen defect) — rather than a silent skip.
#[test]
fn conformance_fixtures_execute_on_every_present_target() {
    let registry = ToolchainRegistry::with_builtins();
    let required = required_targets();
    let root = conformance_root_dir();

    let discovered = discover_tests(&root);
    assert!(
        !discovered.is_empty(),
        "no fixtures discovered under {}; expected at least one execution fixture",
        root.display()
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
        root.display()
    );

    // Tally discovered output fixtures per category for the coverage tripwire.
    let mut by_category: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for (case, _, _) in &output_cases {
        *by_category
            .entry(fixture_category(&root, &case.path))
            .or_default() += 1;
    }

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
    eprintln!("  output fixtures discovered: {}", output_cases.len());
    for (category, count) in &by_category {
        eprintln!("    {category}: {count} fixture(s)");
    }
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

    // Tripwires against silent coverage shrink (see the constants' docs). These
    // run AFTER the failure assertion so a real mismatch is reported first.
    for category in OUTPUT_FIXTURE_CATEGORIES {
        assert!(
            by_category.contains_key(*category),
            "category `{category}` contributed no `// EXPECT: output` fixtures to \
             the execution lane — coverage shrank (or fixtures moved; update \
             OUTPUT_FIXTURE_CATEGORIES)",
        );
    }
    assert!(
        output_cases.len() >= MIN_OUTPUT_FIXTURES,
        "expected >= {MIN_OUTPUT_FIXTURES} output fixtures, discovered {}",
        output_cases.len(),
    );
}

/// Run `bock check` against a fixture's ORIGINAL on-disk path (not the
/// directive-stripped `case.source`): a diagnostic directive's `<line>:<col>`
/// refers to the file as written on disk, including the leading `// TEST:` /
/// `// EXPECT:` comment lines. Returns `(success, combined stdout+stderr)`.
///
/// The check-driven paths only support **single-file** fixtures: `bock check
/// <path>` cannot resolve a `// FILE:` auxiliary module, and materializing the
/// sections elsewhere would break the on-disk `<line>:<col>` contract. This is
/// the one explicit limitation of the wiring — `kind` names the calling test
/// so the panic points at the right place to extend if a multi-file
/// diagnostic fixture is ever needed (none exists as of 2026-06-10).
fn bock_check_fixture(case: &TestCase, kind: &str) -> (bool, String) {
    assert!(
        case.aux_files.is_empty(),
        "{kind} fixture `{}` is multi-file (`// FILE:` sections); the \
         check-driven conformance path only supports single-file fixtures — \
         see `bock_check_fixture` in execution.rs",
        case.name,
    );
    let output = Command::new(bock_binary())
        .arg("check")
        .arg(&case.path)
        .output()
        .expect("failed to spawn bock check");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    (output.status.success(), combined)
}

/// Every conformance fixture — in EVERY category — that declares one or more
/// `// EXPECT: error E<code> at <line>:<col>` directives is driven through
/// `bock check` and must fail, surfacing **each** declared code and location.
///
/// Before 2026-06-10 only `conformance/effects/` and
/// `conformance/types-diagnostics/` were wired (two near-identical tests);
/// the same directives under `context/`, `parse/`, `stdlib/`, and `types/`
/// were parsed but never asserted, which let `types/type_mismatch.bock`
/// declare a code the compiler does not emit — E0205 — without any test
/// noticing (Q-conformance-directive-wiring). This test replaces the
/// per-category pair with one uniform walk of the whole conformance tree, so
/// no category can be silently unwired again.
///
/// What the wired fixtures pin (provenance of the former per-category tests):
/// - `effects/` — §10 effect-system error paths: a genuinely-unhandled bare
///   op (E1001) and the v1.x-reserved lambda handler surface (E4002). The
///   *positive* effect forms are covered on every target by the
///   `exec_effect_*` execution fixtures above.
/// - `types-diagnostics/` — type errors the checker must report but cannot
///   run, e.g. method-body type errors inside `impl`/`class` blocks
///   (Q-impl-body-typecheck: `check_item` used to silently skip them).
/// - `context/`, `parse/`, `stdlib/`, `types/` — newly live: `@performance`
///   unit enforcement (E8003), the deferred tuple-index diagnostic (E2092),
///   primitive core-trait sealing (E4011), narrowing-conversion exclusion
///   (E4012), and the body/return type mismatch (E4001).
#[test]
fn conformance_diagnostic_fixtures_check_as_declared() {
    let root = conformance_root_dir();
    let discovered = discover_tests(&root);
    assert!(
        !discovered.is_empty(),
        "no conformance fixtures discovered under {}",
        root.display()
    );

    let mut checked = 0usize;
    let mut by_category: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();

    for result in discovered {
        let case = result.expect("conformance fixture failed to load");

        // Collect EVERY declared `error E<code> at <l>:<c>` — a fixture may
        // declare several (e.g. context/performance_bare_int_rejected.bock
        // declares two E8003 sites on one line).
        let errors: Vec<_> = case
            .expectations
            .iter()
            .filter_map(|e| match e {
                Expectation::ErrorAt { code, location } => Some((code, location)),
                _ => None,
            })
            .collect();
        if errors.is_empty() {
            continue;
        }

        // A fixture cannot promise an error and a clean check at once.
        assert!(
            !case.expectations.contains(&Expectation::NoErrors),
            "diagnostic fixture `{}` declares BOTH `no_errors` and `error ...` \
             directives — contradictory; fix the fixture",
            case.name,
        );

        let (success, combined) = bock_check_fixture(&case, "diagnostic");

        // A diagnostic fixture must FAIL to check.
        assert!(
            !success,
            "diagnostic fixture `{}` ({}) checked clean but expected {}:\n{combined}",
            case.name,
            case.path.display(),
            errors
                .iter()
                .map(|(code, loc)| format!("`{code}` at {loc}"))
                .collect::<Vec<_>>()
                .join(", "),
        );

        // Every declared code must appear in the combined output, and every
        // declared `<line>:<col>` must be reported.
        for (code, location) in &errors {
            assert!(
                combined.contains(code.as_str()),
                "diagnostic fixture `{}` ({}) did not surface `{code}`:\n{combined}",
                case.name,
                case.path.display(),
            );
            let loc_str = format!("{}:{}", location.line, location.col);
            assert!(
                combined.contains(&loc_str),
                "diagnostic fixture `{}` ({}) did not report location `{loc_str}`:\n{combined}",
                case.name,
                case.path.display(),
            );
        }

        checked += 1;
        *by_category
            .entry(fixture_category(&root, &case.path))
            .or_default() += 1;
    }

    eprintln!("\n=== conformance diagnostic (bock check) summary ===");
    for (category, count) in &by_category {
        eprintln!("  {category}: {count} fixture(s)");
    }

    // Tripwires against silent coverage shrink (see the constants' docs).
    for category in DIAGNOSTIC_FIXTURE_CATEGORIES {
        assert!(
            by_category.contains_key(*category),
            "category `{category}` contributed no diagnostic fixtures — \
             coverage shrank (or fixtures moved; update \
             DIAGNOSTIC_FIXTURE_CATEGORIES and the corpus generator's \
             HARNESS_WIRED_DIAGNOSTIC_CATEGORIES in lockstep)",
        );
    }
    assert!(
        checked >= MIN_DIAGNOSTIC_FIXTURES,
        "expected >= {MIN_DIAGNOSTIC_FIXTURES} diagnostic fixtures, checked {checked}"
    );
}

/// Every conformance fixture — in EVERY category — that declares
/// `// EXPECT: no_errors` is driven through `bock check` and must check
/// clean (exit 0).
///
/// Until 2026-06-10 NOTHING asserted `no_errors` against the live compiler:
/// the harness lib tests only checked that the directive *parses*. Fixing the
/// `// EXPECT: no errors` typo in types/fn_type_param.bock (silently ignored
/// since the file was written) is only meaningful if the repaired directive
/// is actually enforced — this test is that enforcement.
#[test]
fn conformance_no_errors_fixtures_check_clean() {
    let root = conformance_root_dir();
    let discovered = discover_tests(&root);
    assert!(
        !discovered.is_empty(),
        "no conformance fixtures discovered under {}",
        root.display()
    );

    let mut checked = 0usize;
    for result in discovered {
        let case = result.expect("conformance fixture failed to load");
        if !case.expectations.contains(&Expectation::NoErrors) {
            continue;
        }
        // Contradictory combinations are rejected by the diagnostic test
        // above; here just skip them so each defect is reported once.
        if case
            .expectations
            .iter()
            .any(|e| matches!(e, Expectation::ErrorAt { .. }))
        {
            continue;
        }

        let (success, combined) = bock_check_fixture(&case, "no_errors");
        assert!(
            success,
            "fixture `{}` ({}) declares `no_errors` but `bock check` failed:\n{combined}",
            case.name,
            case.path.display(),
        );
        checked += 1;
    }

    eprintln!("\n=== conformance no_errors (bock check) summary ===");
    eprintln!("  checked clean: {checked} fixture(s)");

    assert!(
        checked >= MIN_NO_ERRORS_FIXTURES,
        "expected >= {MIN_NO_ERRORS_FIXTURES} no_errors fixtures, checked {checked}"
    );
}
