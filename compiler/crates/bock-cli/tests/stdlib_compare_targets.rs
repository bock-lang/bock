//! Cross-target compile verification for the embedded `core.compare` module.
//!
//! For each v1 target, `bock build --source-only` over a `core.compare`-importing
//! project must succeed and emit a `core.compare` output file alongside the user
//! module — proving the embedded stdlib (an enum, two generic traits, a sample
//! impl, and generic trait-bounded helpers) flows through codegen on every
//! target.
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

/// Recursively check whether any file under `dir` ends with
/// `core/compare/compare.<ext>`.
fn emitted_core_compare(dir: &Path, ext: &str) -> bool {
    let suffix = format!("core/compare/compare.{ext}");
    fn walk(dir: &Path, suffix: &str) -> bool {
        let Ok(entries) = fs::read_dir(dir) else {
            return false;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if walk(&path, suffix) {
                    return true;
                }
            } else if path.to_string_lossy().replace('\\', "/").ends_with(suffix) {
                return true;
            }
        }
        false
    }
    walk(dir, &suffix)
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

        let build_dir = root.join("build");
        assert!(
            emitted_core_compare(&build_dir, ext),
            "target {target}: no core.compare output (core/compare/compare.{ext}) under {}",
            build_dir.display(),
        );
    }
}
