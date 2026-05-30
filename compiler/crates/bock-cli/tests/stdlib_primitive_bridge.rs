//! Integration tests for the Q-bridge: compiler-provided canonical trait
//! conformances for primitive types.
//!
//! These tests invoke the real `bock` binary and verify, end to end, that:
//!
//! 1. **Bound satisfaction** — a generic function bounded by a core trait
//!    (`where (T: Comparable)`) accepts primitive arguments (`Int`, `String`)
//!    because the compiler registers canonical conformances into the same
//!    trait-impl table user `impl` blocks populate; and *rejects* a type with
//!    no such conformance (proving the previously-dead bound check is wired).
//! 2. **Method resolution (#104)** — a primitive's trait method resolves to the
//!    trait's declared return type: `(1).compare(2)` yields `Ordering`
//!    (matchable on `Less`/`Equal`/`Greater`) and `s.eq(t)` yields `Bool`.
//! 3. **Sealing (Q1b)** — a user `impl <CoreTrait> for <Primitive>` is rejected
//!    with `E4011`, while the newtype escape hatch (`impl Comparable for
//!    MyNewtype`) still compiles.
//! 4. **Codegen invariance** — primitive comparison lowers to the static
//!    intrinsic operator (`(a < b)`) on every target, with no dynamic dispatch;
//!    the bridge changes type *resolution* only, never codegen.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;

use tempfile::NamedTempFile;

fn bock_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_bock"))
}

fn write_temp_file(content: &str) -> NamedTempFile {
    let mut f = NamedTempFile::with_suffix(".bock").unwrap();
    f.write_all(content.as_bytes()).unwrap();
    f.flush().unwrap();
    f
}

fn assert_exit_code(output: &std::process::Output, expected: i32, ctx: &str) {
    assert_eq!(
        output.status.code(),
        Some(expected),
        "{ctx}: expected exit {expected}, got {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

// --- Bound satisfaction via the wired table ---

/// A locally-defined generic function bounded by `core.compare.Comparable`
/// accepts primitive arguments -- `Int` and `String` both satisfy the bound via
/// the compiler-registered canonical conformances. This exercises the wired
/// `impl_table` (without it, the bound check is a no-op).
#[test]
fn primitive_satisfies_comparable_bound() {
    let f = write_temp_file(
        "module userapp\n\
         \n\
         use core.compare.{Comparable}\n\
         \n\
         fn needs_cmp[T](x: T) -> T\n\
         \x20\x20where (T: Comparable)\n\
         {\n\
         \x20\x20x\n\
         }\n\
         \n\
         public fn use_int() -> Int {\n\
         \x20\x20needs_cmp(5)\n\
         }\n\
         \n\
         public fn use_str() -> String {\n\
         \x20\x20needs_cmp(\"hi\")\n\
         }\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "Int/String satisfy Comparable bound");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no errors"), "stdout: {stdout}");
}

/// A type with no `Comparable` conformance is *rejected* at a `Comparable`
/// bound (`E4005`). This proves the wired table actually enforces bounds -- the
/// check was previously dead (the table was never built in the real pipeline).
#[test]
fn non_conforming_type_fails_comparable_bound() {
    let f = write_temp_file(
        "module userapp\n\
         \n\
         use core.compare.{Comparable}\n\
         \n\
         public record Widget {\n\
         \x20\x20id: Int\n\
         }\n\
         \n\
         fn needs_cmp[T](x: T) -> T\n\
         \x20\x20where (T: Comparable)\n\
         {\n\
         \x20\x20x\n\
         }\n\
         \n\
         public fn use_widget(w: Widget) -> Widget {\n\
         \x20\x20needs_cmp(w)\n\
         }\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 1, "Widget fails Comparable bound");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("E4005") && combined.contains("Comparable"),
        "expected E4005 Comparable bound failure, got:\n{combined}"
    );
}

// --- Primitive method resolution (#104) ---

/// `(a).compare(b)` on `Int` resolves to `Ordering` (not the intrinsic `Int`
/// fallback): the result is matchable on `Less`/`Equal`/`Greater` and assignable
/// to an `Ordering` return; `s.eq(t)` on `String` resolves to `Bool`. This is
/// the #104 fix, dispatching through the canonical conformance.
#[test]
fn primitive_compare_resolves_to_ordering_and_eq_to_bool() {
    let f = write_temp_file(
        "module userapp\n\
         \n\
         use core.compare.{Ordering, Comparable, Equatable}\n\
         \n\
         public fn describe(a: Int, b: Int) -> String {\n\
         \x20\x20match a.compare(b) {\n\
         \x20\x20\x20\x20Less => \"less\"\n\
         \x20\x20\x20\x20Equal => \"equal\"\n\
         \x20\x20\x20\x20Greater => \"greater\"\n\
         \x20\x20}\n\
         }\n\
         \n\
         public fn order_two(a: Int, b: Int) -> Ordering {\n\
         \x20\x20a.compare(b)\n\
         }\n\
         \n\
         public fn same(a: String, b: String) -> Bool {\n\
         \x20\x20a.eq(b)\n\
         }\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "(1).compare(2) -> Ordering, s.eq(t) -> Bool");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no errors"), "stdout: {stdout}");
}

/// Without importing `core.compare`, a primitive's intrinsic methods (`abs`,
/// `to_string`) still resolve through the intrinsic fast path -- the bridge's
/// trait lookup only fires when the core trait is actually in scope.
#[test]
fn primitive_intrinsics_unaffected_without_import() {
    let f = write_temp_file(
        "module userapp\n\
         \n\
         public fn use_abs(x: Int) -> Int {\n\
         \x20\x20x.abs()\n\
         }\n\
         \n\
         public fn to_s(x: Int) -> String {\n\
         \x20\x20x.to_string()\n\
         }\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "intrinsics resolve without core.compare");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no errors"), "stdout: {stdout}");
}

// --- Sealing (Q1b / E4011) ---

/// A user `impl Comparable for Int` is rejected: core-trait conformances for
/// primitives are sealed (compiler-provided). The diagnostic is `E4011`.
#[test]
fn sealing_rejects_user_core_trait_impl_for_primitive() {
    let f = write_temp_file(
        "module userapp\n\
         \n\
         use core.compare.{Comparable, Ordering}\n\
         \n\
         impl Comparable for Int {\n\
         \x20\x20public fn compare(self, other: Int) -> Ordering {\n\
         \x20\x20\x20\x20Equal\n\
         \x20\x20}\n\
         }\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 1, "impl Comparable for Int is sealed");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined = format!("{stdout}{stderr}");
    assert!(
        combined.contains("E4011"),
        "expected E4011 sealing diagnostic, got:\n{combined}"
    );
    assert!(
        combined.contains("newtype"),
        "expected newtype hint in diagnostic, got:\n{combined}"
    );
}

/// Positive control for sealing: the newtype escape hatch -- `impl Comparable
/// for MyNewtype` -- compiles cleanly, since the seal is scoped strictly to the
/// (core trait, primitive) quadrant.
#[test]
fn sealing_allows_core_trait_impl_for_newtype() {
    let f = write_temp_file(
        "module userapp\n\
         \n\
         use core.compare.{Comparable, Ordering}\n\
         \n\
         public record MyKey {\n\
         \x20\x20value: Int\n\
         }\n\
         \n\
         impl Comparable for MyKey {\n\
         \x20\x20public fn compare(self, other: MyKey) -> Ordering {\n\
         \x20\x20\x20\x20Equal\n\
         \x20\x20}\n\
         }\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "impl Comparable for newtype compiles");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no errors"), "stdout: {stdout}");
}

// --- Runtime smoke ---

/// End-to-end runtime smoke: a `main` comparing two `Int`s via the `<` operator
/// (whose conformance the bridge now registers, though `<` lowers intrinsically)
/// runs through the interpreter and prints the expected branch.
#[test]
fn run_primitive_comparison_prints_expected() {
    let f = write_temp_file(
        "module main\n\
         \n\
         public fn main() {\n\
         \x20\x20if (1 < 2) {\n\
         \x20\x20\x20\x20println(\"less\")\n\
         \x20\x20} else {\n\
         \x20\x20\x20\x20println(\"ge\")\n\
         \x20\x20}\n\
         }\n",
    );
    let output = bock_bin().arg("run").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "run primitive comparison");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("less"),
        "expected `less` in stdout, got: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
}

// --- Codegen invariance ---

/// Build a temp project that compares two `Int`s and emit source for `rust` and
/// `python`. The primitive comparison must lower to the *static intrinsic
/// operator* (`(a < b)`) -- never a trait-method dispatch. This locks in codegen
/// invariance: the bridge changes type resolution only; codegen lowers
/// `BinaryOp` structurally from AIR and never consults the trait-impl table.
#[test]
fn codegen_lowers_primitive_comparison_statically() {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("primitive_bridge_codegen");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("bock.project"),
        "name = \"cgdemo\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(
        root.join("src/main.bock"),
        "module main\n\
         \n\
         public fn lt(a: Int, b: Int) -> Bool {\n\
         \x20\x20a < b\n\
         }\n\
         \n\
         public fn main() {\n\
         \x20\x20if (lt(1, 2)) {\n\
         \x20\x20\x20\x20println(\"less\")\n\
         \x20\x20} else {\n\
         \x20\x20\x20\x20println(\"ge\")\n\
         \x20\x20}\n\
         }\n",
    )
    .unwrap();

    // Rust: the comparison lowers to the intrinsic `(a < b)` operator.
    let rs = build_source(&root, "rust", "rs");
    assert!(
        rs.contains("(a < b)"),
        "expected static `(a < b)` in emitted Rust, got:\n{rs}"
    );
    assert!(
        !rs.contains(".compare("),
        "primitive `<` must not lower to a `.compare(...)` dispatch:\n{rs}"
    );

    // Python: same -- `(a < b)`.
    let py = build_source(&root, "python", "py");
    assert!(
        py.contains("(a < b)"),
        "expected static `(a < b)` in emitted Python, got:\n{py}"
    );
    assert!(
        !py.contains(".compare("),
        "primitive `<` must not lower to a `.compare(...)` dispatch:\n{py}"
    );
}

/// Run `bock build -t <target> --source-only` in `root` and return the emitted
/// `main.<ext>` source.
fn build_source(root: &std::path::Path, target: &str, ext: &str) -> String {
    let output = bock_bin()
        .current_dir(root)
        .arg("build")
        .arg("-t")
        .arg(target)
        .arg("--source-only")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "build --source-only failed for {target}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let main_path = root.join("build").join(target).join(format!("main.{ext}"));
    fs::read_to_string(&main_path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", main_path.display()))
}
