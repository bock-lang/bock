//! Cross-target compile verification for the embedded `core.error` module.
//!
//! For each v1 target, `bock build --source-only` over a `core.error`-importing
//! project must succeed and emit `core.error`'s declarations — proving the
//! embedded stdlib flows through codegen on every target. The emission *shape*
//! depends on the target's migration state (per-module-output milestone, DQ19):
//!
//! - **Bundling targets (`rust`/`go`):** the imported module is concatenated
//!   into the single entry file `main.<ext>`, so that file carries the
//!   `SimpleError` record (core.error's sole record).
//! - **Per-module targets (`python` in S1, `js`/`ts` in S2):** each module is
//!   emitted to its own file, so `SimpleError` lives in `core/error.{py,js,ts}`
//!   and the entry carries a real cross-module import (Python `from core.error
//!   import …`; js/ts `import … from "./core/error.js"`) rather than the bundled
//!   record.
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

/// Create an isolated temp project that imports `core.error`, returning its
/// root. `$CARGO_TARGET_TMPDIR` gives each test binary its own scratch space.
fn make_project(tag: &str) -> PathBuf {
    let root =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("stdlib_error_targets_{tag}"));
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
         use core.error.{error}\n\
         \n\
         public fn main() {\n\
         \x20\x20println(error(\"boom\").message())\n\
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
/// in sync with the harness's `emits_per_module_tree`: S1 migrated `python`, S2
/// adds `js`/`ts`.
fn emits_per_module_tree(target: &str) -> bool {
    matches!(target, "python" | "js" | "ts")
}

/// Assert the entry file carries a real cross-module import of `module` (the
/// dotted declared path, e.g. `core.error`) in the per-module path, spelled the
/// way the `target` emits it: Python `from core.error import …`; js/ts ESM
/// `import … from "./core/error.js"` (the relative specifier always references
/// the emitted `.js`, even for ts).
fn assert_entry_imports_module(entry: &str, target: &str, module: &str) {
    match target {
        "python" => assert!(
            entry.contains(&format!("from {module} import")),
            "target {target}: entry must import from the {module} module file",
        ),
        "js" | "ts" => {
            let rel = format!("./{}.js", module.replace('.', "/"));
            assert!(
                entry.contains("import ") && entry.contains(&rel),
                "target {target}: entry must `import … from \"{rel}\"`",
            );
        }
        other => panic!("assert_entry_imports_module: unexpected per-module target {other}"),
    }
}

#[test]
fn core_error_compiles_on_every_target() {
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

        let build_dir = root.join("build");
        if emits_per_module_tree(target) {
            // Per-module tree: `core.error` is its own file (`core/error.py`)
            // carrying `SimpleError`, and the entry imports from it rather than
            // inlining the record.
            let module_file = build_dir
                .join(target)
                .join("core")
                .join(format!("error.{ext}"));
            let module_src = fs::read_to_string(&module_file).unwrap_or_else(|_| {
                panic!(
                    "target {target}: no per-module file {}",
                    module_file.display()
                )
            });
            assert!(
                module_src.contains("SimpleError"),
                "target {target}: core.error module file lacks `SimpleError`",
            );
            let entry = read_entry_bundle(&build_dir, target, ext).unwrap_or_else(|| {
                panic!("target {target}: no entry file build/{target}/main.{ext}")
            });
            assert_entry_imports_module(&entry, target, "core.error");
        } else {
            // Bundling: the imported `core.error` is concatenated into the entry
            // file, so the `SimpleError` record must appear in `main.<ext>`.
            let bundle = read_entry_bundle(&build_dir, target, ext).unwrap_or_else(|| {
                panic!("target {target}: no entry bundle build/{target}/main.{ext}")
            });
            assert!(
                bundle.contains("SimpleError"),
                "target {target}: core.error not bundled into main.{ext} (no `SimpleError`)",
            );
        }
    }
}
