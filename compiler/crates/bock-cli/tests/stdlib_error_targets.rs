//! Cross-target compile verification for the embedded `core.error` module.
//!
//! For each v1 target, `bock build --source-only` over a `core.error`-importing
//! project must succeed and emit `core.error`'s declarations — proving the
//! embedded stdlib flows through codegen on every target. As of S3 **all five
//! v1 targets** emit the per-module tree (DQ19 resolved): each module is emitted
//! to its own file, so `SimpleError` (core.error's sole record) lives in the
//! `core.error` module file and the entry wires to it with the target's native
//! import/module mechanism:
//!
//! - **python** — `core/error.py`; entry `from core.error import …`.
//! - **js / ts** — `core/error.{js,ts}`; entry `import … from "./core/error.js"`.
//! - **rust** — `src/core/error.rs`; entry `use crate::core::error::{…}`.
//! - **go** — flat `core.error.go` in one `package main`; same-package symbols
//!   are visible without an import, so the entry has no import statement — the
//!   per-module *shape* is the separate module file (not an inlined record).
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

/// Path to the per-module file for `module` (dotted, e.g. `core.error`) under
/// `build/<target>/`, in the layout each target emits:
/// - python/js/ts — mirrored tree `core/error.<ext>`.
/// - rust — `src/`-rooted crate `src/core/error.rs`.
/// - go — flat single package: `core.error.go` (dots kept).
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

/// Assert the entry file wires to the per-module `module` file (dotted path,
/// e.g. `core.error`) the way `target` emits cross-module references:
/// - python — `from core.error import …`.
/// - js / ts — ESM `import … from "./core/error.js"` (always the `.js` ext).
/// - rust — `use crate::core::error::{…};`.
/// - go — same package, no import statement; the per-module *shape* (a separate
///   module file carrying the symbol, asserted by the caller) is the evidence.
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
        "rust" => {
            let crate_path = format!("crate::{}::", module.replace('.', "::"));
            assert!(
                entry.contains(&format!("use {crate_path}")),
                "target {target}: entry must `use {crate_path}…;`",
            );
        }
        // Go's per-module files share one `package main`, so a cross-module
        // reference needs no import — nothing to assert on the entry here.
        "go" => {}
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

        // Per-module tree (all five targets, S3): `core.error` is its own file
        // carrying `SimpleError`, and the entry wires to it with the target's
        // native import/module mechanism rather than inlining the record.
        let build_dir = root.join("build");
        let module_file = module_file_path(&build_dir, target, "core.error", ext);
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
        let entry = read_entry(&build_dir, target, ext)
            .unwrap_or_else(|| panic!("target {target}: no entry file"));
        assert_entry_imports_module(&entry, target, "core.error");
    }
}
