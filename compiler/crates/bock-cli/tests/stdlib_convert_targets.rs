//! Cross-target compile verification for the embedded `core.convert` module.
//!
//! For each v1 target, `bock build --source-only` over a `core.convert`-using
//! project must succeed and emit a `core.convert` output file alongside the
//! user module — proving the embedded stdlib (four traits, an error record, a
//! sample parameterized `From[Celsius] for Fahrenheit` impl, and a constructor)
//! flows through codegen on every target.
//!
//! This is *compile* (source-emission) verification only. Full conformance
//! *execution* across targets is the separate Q-fconf task and is NOT covered
//! here.

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

/// Create an isolated temp project that uses `core.convert`, returning its
/// root. The user `main` constructs the stdlib `Celsius`/`Fahrenheit` sample
/// types, performs the cross-module associated-function conversion
/// `Fahrenheit.from(c)`, and builds a `ConvertError` via the `convert_error`
/// constructor — so the convert module's traits, records, impl, and
/// constructor all reach codegen.
fn make_project(tag: &str) -> PathBuf {
    let root =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("stdlib_convert_targets_{tag}"));
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
         use core.convert.{Celsius, Fahrenheit, ConvertError, convert_error}\n\
         \n\
         public fn to_f(c: Celsius) -> Fahrenheit {\n\
         \x20\x20Fahrenheit.from(c)\n\
         }\n\
         \n\
         public fn oops() -> ConvertError {\n\
         \x20\x20convert_error(\"out of range\")\n\
         }\n\
         \n\
         public fn main() {\n\
         \x20\x20let c = Celsius { degrees: 100.0 }\n\
         \x20\x20let f = to_f(c)\n\
         \x20\x20println(\"ok\")\n\
         }\n",
    )
    .unwrap();
    root
}

/// Recursively check whether any file under `dir` ends with
/// `core/convert/convert.<ext>`.
fn emitted_core_convert(dir: &Path, ext: &str) -> bool {
    let suffix = format!("core/convert/convert.{ext}");
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
fn core_convert_compiles_on_every_target() {
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
            emitted_core_convert(&build_dir, ext),
            "target {target}: no core.convert output (core/convert/convert.{ext}) under {}",
            build_dir.display(),
        );
    }
}
