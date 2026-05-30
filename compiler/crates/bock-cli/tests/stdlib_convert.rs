//! Integration tests for the embedded `core.convert` module.
//!
//! These tests invoke the real `bock` binary against temp user files that
//! `use core.convert.{...}`, asserting the embedded stdlib is discoverable and
//! that parameterized-trait conversion resolves end to end: the explicit
//! associated-function call `Target.from(source)` (cross-module), the blanket
//! `Into[Target] for Source` derived from a user `From[Source] for Target`
//! (within the defining module, return-type-driven via `.into()`), and the
//! `E4012` diagnostic when no conversion relates the source and target types.

use std::io::Write;
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

/// Importing the parameterized conversion traits by name resolves cleanly
/// through the embedded stdlib: the four public traits (`From`, `Into`,
/// `TryFrom`, `Displayable`), the `ConvertError` record, and the sample
/// `Celsius`/`Fahrenheit` types are all discoverable.
#[test]
fn check_imports_resolve() {
    let f = write_temp_file(
        "module userapp\n\
         \n\
         use core.convert.{From, Into, TryFrom, Displayable, ConvertError, Celsius, Fahrenheit}\n\
         \n\
         public fn convert(c: Celsius) -> Fahrenheit {\n\
         \x20\x20Fahrenheit.from(c)\n\
         }\n\
         \n\
         public fn wrap(msg: String) -> ConvertError {\n\
         \x20\x20ConvertError { message: msg }\n\
         }\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "core.convert imports resolve");
}

/// The stdlib sample conversion `From[Celsius] for Fahrenheit` resolves across
/// the module boundary via the associated-function call `Fahrenheit.from(c)`,
/// and type-checks against the declared `from(value: Celsius) -> Fahrenheit`
/// signature (a wrong argument type is rejected — see `from_rejects_wrong_arg`).
#[test]
fn check_stdlib_from_resolves() {
    let f = write_temp_file(
        "module userapp\n\
         \n\
         use core.convert.{Celsius, Fahrenheit}\n\
         \n\
         public fn to_f(c: Celsius) -> Fahrenheit {\n\
         \x20\x20Fahrenheit.from(c)\n\
         }\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "stdlib From[Celsius] for Fahrenheit resolves");
}

/// A cross-module `Target.from(arg)` call type-checks against the declared
/// conversion signature: passing a `String` where `Celsius` is required is a
/// type mismatch (proving `from` is not a loose, unchecked fallthrough).
#[test]
fn from_rejects_wrong_arg() {
    let f = write_temp_file(
        "module userapp\n\
         \n\
         use core.convert.{Fahrenheit}\n\
         \n\
         public fn bad(s: String) -> Fahrenheit {\n\
         \x20\x20Fahrenheit.from(s)\n\
         }\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 1, "from rejects wrong argument type");
}

/// A user-defined `From[Source] for Target` makes both `Target.from(source)`
/// and the blanket `source.into()` resolve within the defining module. The
/// `.into()` target is taken from the return-type position (`-> Target`).
#[test]
fn user_from_enables_into_and_from() {
    let f = write_temp_file(
        "module userapp\n\
         \n\
         use core.convert.{From, Into}\n\
         \n\
         public record Meter {\n\
         \x20\x20value: Float\n\
         }\n\
         \n\
         public record Foot {\n\
         \x20\x20value: Float\n\
         }\n\
         \n\
         impl From[Meter] for Foot {\n\
         \x20\x20public fn from(value: Meter) -> Foot {\n\
         \x20\x20\x20\x20Foot { value: value.value * 3.28 }\n\
         \x20\x20}\n\
         }\n\
         \n\
         public fn via_from(m: Meter) -> Foot {\n\
         \x20\x20Foot.from(m)\n\
         }\n\
         \n\
         public fn via_into(m: Meter) -> Foot {\n\
         \x20\x20m.into()\n\
         }\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "user From enables .into() and .from()");
}

/// Return-type-driven `.into()` is sound: when the expected target type has no
/// `From`/`Into` impl from the receiver, the checker emits `E4012` rather than
/// silently accepting the call (the pre-fix behavior was an unsound fresh-var
/// fallthrough that accepted any target).
#[test]
fn into_to_unrelated_target_errors() {
    let f = write_temp_file(
        "module userapp\n\
         \n\
         use core.convert.{From}\n\
         \n\
         public record Meter {\n\
         \x20\x20value: Float\n\
         }\n\
         \n\
         public record Foot {\n\
         \x20\x20value: Float\n\
         }\n\
         \n\
         impl From[Meter] for Foot {\n\
         \x20\x20public fn from(value: Meter) -> Foot {\n\
         \x20\x20\x20\x20Foot { value: value.value * 3.28 }\n\
         \x20\x20}\n\
         }\n\
         \n\
         public fn bad(m: Meter) -> String {\n\
         \x20\x20m.into()\n\
         }\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 1, ".into() to unrelated target errors");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stderr.contains("E4012") || stdout.contains("E4012"),
        "expected E4012, stdout: {stdout}\nstderr: {stderr}"
    );
}

/// End-to-end runtime smoke: a `main` that builds a `ConvertError` via the
/// `core.convert.convert_error` constructor and prints its `message` runs
/// through the interpreter (with the embedded `core.convert` module compiled
/// and registered) and prints `out of range`. This backs the
/// `convert_output_smoke` conformance fixture, whose `// EXPECT: output`
/// directive is parsed-but-not-executed by the harness today (staged for
/// Q-fconf).
///
/// The smoke uses the free `convert_error(...)` constructor + field access
/// rather than a conversion trait method: at run time the interpreter cannot
/// dispatch `Type.from(x)` (user associated functions), the derived blanket
/// `.into()` (no method body), or `Displayable.to_string` (shadowed by the
/// universal record `to_string` builtin). Type-checking and codegen handle all
/// three; this is an interpreter-scoping gap. See the session PR notes.
#[test]
fn run_prints_convert_error_message() {
    let f = write_temp_file(
        "module main\n\
         \n\
         use core.convert.{convert_error}\n\
         \n\
         public fn main() {\n\
         \x20\x20println(convert_error(\"out of range\").message)\n\
         }\n",
    );
    let output = bock_bin().arg("run").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "run core.convert main");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("out of range"),
        "expected `out of range` in stdout, got: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
}
