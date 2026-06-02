//! Cross-target compile verification for the embedded `core.compare` module.
//!
//! For each v1 target, `bock build --source-only` over a `core.compare`-importing
//! project must succeed and **bundle** `core.compare`'s declarations (an enum,
//! two generic traits, a sample impl, and generic trait-bounded helpers) into
//! the one entry file — proving the embedded stdlib flows through codegen on
//! every target. Under single-file bundling (DV13; see spec §20.6.1
//! divergence), the imported module is concatenated into `main.<ext>` rather
//! than emitted as a separate `core/compare/compare.<ext>` file, so this asserts
//! the bundled entry file carries the module's emitted symbol (the `Key` record).
//!
//! This is *compile* (source-emission) verification only. Full conformance
//! *execution* across targets (running the emitted code through each target's
//! toolchain and comparing output) is the separate Q-fconf task and is NOT
//! covered here.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The v1 targets and the source-file extension each emits.
///
/// Note the target *names* are `python`/`rust` (not `py`/`rs`); the extensions
/// are `.py`/`.rs`.
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

/// Create an isolated temp project that imports `core.compare`, returning its
/// root. `$CARGO_TARGET_TMPDIR` gives each test binary its own scratch space.
///
/// The user `main` exercises the full surface — the `Ordering` enum, the
/// `Comparable` trait (via `.compare`), the `key(...)` constructor, and the
/// generic trait-bounded `max[T: Comparable]` helper — so every public item of
/// `core.compare` reaches codegen.
fn make_project(tag: &str) -> PathBuf {
    let root =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("stdlib_compare_targets_{tag}"));
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
         use core.compare.{Ordering, Comparable, Key, key, max}\n\
         \n\
         public fn main() {\n\
         \x20\x20let a = key(3)\n\
         \x20\x20let b = key(9)\n\
         \x20\x20let bigger = max(a, b)\n\
         \x20\x20match bigger.compare(a) {\n\
         \x20\x20\x20\x20Less => println(\"less\")\n\
         \x20\x20\x20\x20Equal => println(\"equal\")\n\
         \x20\x20\x20\x20Greater => println(\"greater\")\n\
         \x20\x20}\n\
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
fn core_compare_compiles_on_every_target() {
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

        // The `Key` record must be emitted: bundled into the entry file on
        // bundling targets, or in the separate `core/compare.<ext>` module file
        // (with a real import in the entry) on per-module targets.
        let build_dir = root.join("build");
        if emits_per_module_tree(target) {
            let module_file = build_dir
                .join(target)
                .join("core")
                .join(format!("compare.{ext}"));
            let module_src = fs::read_to_string(&module_file).unwrap_or_else(|_| {
                panic!(
                    "target {target}: no per-module file {}",
                    module_file.display()
                )
            });
            assert!(
                module_src.contains("Key"),
                "target {target}: core.compare module file lacks `Key`",
            );
            let entry = read_entry_bundle(&build_dir, target, ext).unwrap_or_else(|| {
                panic!("target {target}: no entry file build/{target}/main.{ext}")
            });
            assert!(
                entry.contains("from core.compare import"),
                "target {target}: entry must import from the core.compare module file",
            );
        } else {
            let bundle = read_entry_bundle(&build_dir, target, ext).unwrap_or_else(|| {
                panic!("target {target}: no entry bundle build/{target}/main.{ext}")
            });
            assert!(
                bundle.contains("Key"),
                "target {target}: core.compare not bundled into main.{ext} (no `Key`)",
            );
        }
    }
}
