//! Cross-target compile verification for the embedded `core.error` module.
//!
//! For each v1 target, `bock build --source-only` over a `core.error`-importing
//! project must succeed and **bundle** `core.error`'s declarations into the one
//! entry file — proving the embedded stdlib flows through codegen on every
//! target. Under single-file bundling (DV13; see spec §20.6.1 divergence), the
//! imported module is concatenated into `main.<ext>` rather than emitted as a
//! separate `core/error/error.<ext>` file, so this asserts the bundled entry
//! file carries the module's emitted symbol (the `SimpleError` record).
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

        // Under bundling the imported `core.error` is concatenated into the
        // entry file rather than emitted separately, so the `SimpleError`
        // record (core.error's sole record) must appear in `main.<ext>`.
        let build_dir = root.join("build");
        let bundle = read_entry_bundle(&build_dir, target, ext).unwrap_or_else(|| {
            panic!("target {target}: no entry bundle build/{target}/main.{ext}")
        });
        assert!(
            bundle.contains("SimpleError"),
            "target {target}: core.error not bundled into main.{ext} (no `SimpleError`)",
        );
    }
}
