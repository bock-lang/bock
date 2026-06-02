//! Cross-target compile verification for the embedded `core.test` module.
//!
//! For each v1 target, `bock build --source-only` over a `core.test`-importing
//! project must succeed and **bundle** `core.test`'s declarations (the free
//! assertion functions and the fluent `Expectation`/`BoolExpectation` records +
//! impls) into the one entry file — proving the embedded stdlib flows through
//! codegen on every target. Under single-file bundling (DV13; see spec §20.6.1
//! divergence), the imported module is concatenated into `main.<ext>` rather
//! than emitted as a separate file, so this asserts the bundled entry file
//! carries the module's emitted symbols.
//!
//! This is *compile* (source-emission) verification only; full conformance
//! *execution* across targets (running the emitted code through each toolchain
//! and comparing output) is `exec_core_test.bock` in the conformance suite.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The v1 targets and the source-file extension each emits.
const TARGETS: &[(&str, &str)] = &[
    ("js", "js"),
    ("ts", "ts"),
    ("python", "py"),
    ("rust", "rs"),
    ("go", "go"),
];

fn bock_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_bock"))
}

/// Create an isolated temp project that imports `core.test`, returning its root.
///
/// The user `main` exercises a representative slice of the surface — a free
/// assertion (`assert_true`), the user-`Equatable` equality path (`assert_eq`
/// over `Key`), an Optional matcher (`assert_some`), and both fluent entry
/// points (`expect`/`expect_bool`, let-bound for Go addressability) — so every
/// public *kind* of item reaches codegen.
fn make_project(tag: &str) -> PathBuf {
    let root =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("stdlib_test_targets_{tag}"));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("bock.project"),
        "name = \"tgtdemo\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        root.join("src/main.bock"),
        "module main\n\
         \n\
         use core.test.{assert_true, assert_eq, assert_some, expect, expect_bool}\n\
         use core.compare.{Key, key}\n\
         \n\
         public fn main() {\n\
         \x20\x20assert_true(true)\n\
         \x20\x20assert_eq(key(3), key(3))\n\
         \x20\x20let present: Optional[Int] = Some(5)\n\
         \x20\x20assert_some(present)\n\
         \x20\x20let e: Expectation[Key] = expect(key(3))\n\
         \x20\x20e.to_equal(key(3))\n\
         \x20\x20let b: BoolExpectation = expect_bool(true)\n\
         \x20\x20b.to_be_true()\n\
         }\n",
    )
    .unwrap();
    root
}

/// Read the bundled entry file `build/<target>/main.<ext>`, if present.
fn read_entry_bundle(build_dir: &Path, target: &str, ext: &str) -> Option<String> {
    fs::read_to_string(build_dir.join(target).join(format!("main.{ext}"))).ok()
}

/// Whether `target` emits a per-module native import tree (vs. bundling). Kept
/// in sync with the harness's `emits_per_module_tree`: S1 migrates `python`.
fn emits_per_module_tree(target: &str) -> bool {
    matches!(target, "python")
}

#[test]
fn core_test_compiles_on_every_target() {
    for (target, ext) in TARGETS {
        let root = make_project(target);
        let output = bock_bin()
            .current_dir(&root)
            .arg("build")
            .arg("-t")
            .arg(target)
            .arg("--source-only")
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "build --source-only failed for target {target}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );

        // `core.test`'s fluent record (`Expectation`) must be emitted: bundled
        // into the entry file on bundling targets, or in the separate
        // `core/test.<ext>` module file on per-module targets. Casing differs
        // per target (Go PascalCases), so match case-insensitively.
        let build_dir = root.join("build");
        if emits_per_module_tree(target) {
            let module_file = build_dir
                .join(target)
                .join("core")
                .join(format!("test.{ext}"));
            let module_src = fs::read_to_string(&module_file).unwrap_or_else(|_| {
                panic!(
                    "target {target}: no per-module file {}",
                    module_file.display()
                )
            });
            assert!(
                module_src.to_lowercase().contains("expectation"),
                "target {target}: core.test module file lacks `Expectation`",
            );
            let entry = read_entry_bundle(&build_dir, target, ext).unwrap_or_else(|| {
                panic!("target {target}: no entry file build/{target}/main.{ext}")
            });
            assert!(
                entry.contains("from core.test import"),
                "target {target}: entry must import from the core.test module file",
            );
        } else {
            let bundle = read_entry_bundle(&build_dir, target, ext).unwrap_or_else(|| {
                panic!("target {target}: no entry bundle build/{target}/main.{ext}")
            });
            assert!(
                bundle.to_lowercase().contains("expectation"),
                "target {target}: core.test not bundled into main.{ext} (no `Expectation`)",
            );
        }
    }
}
