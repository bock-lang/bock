//! Cross-target compile verification for the embedded `core.convert` module.
//!
//! For each v1 target, `bock build --source-only` over a `core.convert`-using
//! project must succeed and **bundle** `core.convert`'s declarations (four
//! traits, an error record, a sample parameterized `From[Celsius] for
//! Fahrenheit` impl, and a constructor) into the one entry file — proving the
//! embedded stdlib flows through codegen on every target. Under single-file
//! bundling (DV13; see spec §20.6.1 divergence), the imported module is
//! concatenated into `main.<ext>` rather than emitted as a separate
//! `core/convert/convert.<ext>` file, so this asserts the bundled entry file
//! carries the module's emitted symbol (the `ConvertError` record).
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
/// dotted declared path) spelled the way `target` emits it: Python
/// `from <module> import …`; js/ts ESM `import … from "./<path>.js"`.
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

        // The `ConvertError` record must be emitted: bundled into the entry
        // file on bundling targets, or in the separate `core/convert.<ext>`
        // module file (with a real import in the entry) on per-module targets.
        let build_dir = root.join("build");
        if emits_per_module_tree(target) {
            let module_file = build_dir
                .join(target)
                .join("core")
                .join(format!("convert.{ext}"));
            let module_src = fs::read_to_string(&module_file).unwrap_or_else(|_| {
                panic!(
                    "target {target}: no per-module file {}",
                    module_file.display()
                )
            });
            assert!(
                module_src.contains("ConvertError"),
                "target {target}: core.convert module file lacks `ConvertError`",
            );
            let entry = read_entry_bundle(&build_dir, target, ext).unwrap_or_else(|| {
                panic!("target {target}: no entry file build/{target}/main.{ext}")
            });
            assert_entry_imports_module(&entry, target, "core.convert");
        } else {
            let bundle = read_entry_bundle(&build_dir, target, ext).unwrap_or_else(|| {
                panic!("target {target}: no entry bundle build/{target}/main.{ext}")
            });
            assert!(
                bundle.contains("ConvertError"),
                "target {target}: core.convert not bundled into main.{ext} (no `ConvertError`)",
            );
        }
    }
}
