//! Cross-target compile verification for the embedded `core.convert` module.
//!
//! For each v1 target, `bock build --source-only` over a `core.convert`-using
//! project must succeed and emit `core.convert`'s declarations (four traits, an
//! error record, a sample parameterized `From[Celsius] for Fahrenheit` impl,
//! and a constructor) — proving the embedded stdlib flows through codegen on
//! every target. As of S3 **all five v1 targets** emit the per-module tree (DQ19
//! resolved): `core.convert` lives in its own module file carrying the
//! `ConvertError` record, and the entry wires to it with the target's native
//! import/module mechanism (python/js/ts/rust imports; go shares one
//! `package main`, so no import). See `stdlib_error_targets.rs` for the
//! per-target layout details.
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

/// Read the emitted entry file, if present. The entry is `main.<ext>` at the
/// build root for every target except rust, whose per-module output is a
/// `src/`-rooted Cargo crate (`src/main.rs`).
fn read_entry(build_dir: &Path, target: &str, ext: &str) -> Option<String> {
    let target_dir = build_dir.join(target);
    let entry = if target == "rust" {
        target_dir.join("src").join("main.rs")
    } else {
        target_dir.join(format!("main.{ext}"))
    };
    fs::read_to_string(entry).ok()
}

/// Path to the per-module file for `module` (dotted) under `build/<target>/`, in
/// the layout each target emits: python/js/ts mirror the tree
/// (`core/<m>.<ext>`); rust roots under `src/` (`src/core/<m>.rs`); go is one
/// flat package (`core.<m>.go`, dots kept — `_test.go` is Go's test suffix).
fn module_file_path(build_dir: &Path, target: &str, module: &str, ext: &str) -> PathBuf {
    let target_dir = build_dir.join(target);
    match target {
        "rust" => {
            let mut p = target_dir.join("src");
            for seg in module.split('.') {
                p = p.join(seg);
            }
            p.with_extension(ext)
        }
        // Go keeps the dotted module path in the flat filename (`core.test.go`)
        // — flattening to `_` would hit Go's reserved `_test.go` suffix.
        "go" => target_dir.join(format!("{module}.{ext}")),
        _ => {
            let mut p = target_dir;
            let segs: Vec<&str> = module.split('.').collect();
            for seg in &segs[..segs.len() - 1] {
                p = p.join(seg);
            }
            p.join(format!("{}.{ext}", segs[segs.len() - 1]))
        }
    }
}

/// Assert the entry file wires to the per-module `module` file (dotted path) the
/// way `target` emits cross-module references: python `from <module> import …`;
/// js/ts ESM `import … from "./<path>.js"`; rust `use crate::<m::path>::…;`; go
/// shares one `package main`, so there is no import — the separate module file
/// (asserted by the caller) is the per-module evidence.
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
                "target {target}: entry must `import … from \"{rel}\""
            );
        }
        "rust" => {
            let crate_path = format!("crate::{}::", module.replace('.', "::"));
            assert!(
                entry.contains(&format!("use {crate_path}")),
                "target {target}: entry must `use {crate_path}…;`",
            );
        }
        "go" => {}
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

        // Per-module tree (all five targets, S3): `core.convert` is its own file
        // carrying `ConvertError`, and the entry wires to it with the target's native
        // import/module mechanism rather than inlining the declaration.
        let build_dir = root.join("build");
        let module_file = module_file_path(&build_dir, target, "core.convert", ext);
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
        let entry = read_entry(&build_dir, target, ext)
            .unwrap_or_else(|| panic!("target {target}: no entry file"));
        assert_entry_imports_module(&entry, target, "core.convert");
    }
}
