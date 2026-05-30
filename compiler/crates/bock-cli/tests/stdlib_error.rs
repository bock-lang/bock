//! Integration tests for the embedded core-stdlib loading mechanism, proven
//! with the `core.error` pilot module.
//!
//! These tests invoke the real `bock` binary against temp user files that
//! `use core.error.{...}`, asserting the embedded stdlib is discoverable and
//! resolves through the existing `ModuleRegistry` with no special-casing.

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

/// The pilot: a user file importing the `Error` trait by name and referencing
/// it in a signature + method call resolves cleanly through the embedded
/// stdlib (no `core.error` source on disk in the user's project).
#[test]
fn check_resolves_core_error_trait() {
    let f = write_temp_file(
        "module userapp\n\
         \n\
         use core.error.{Error}\n\
         \n\
         public fn describe(e: Error) -> String {\n\
         \x20\x20e.message()\n\
         }\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "core.error.Error resolves");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no errors"), "stdout: {stdout}");
}

/// Constructing `SimpleError` via the `error(...)` constructor and calling its
/// `message()` accessor resolves and type-checks against the embedded module.
#[test]
fn check_constructs_and_uses_simple_error() {
    let f = write_temp_file(
        "module userapp\n\
         \n\
         use core.error.{SimpleError, error}\n\
         \n\
         public fn boom() -> String {\n\
         \x20\x20let e: SimpleError = error(\"boom\")\n\
         \x20\x20e.message()\n\
         }\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "SimpleError construct + use");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no errors"), "stdout: {stdout}");
}

/// End-to-end runtime smoke test: a `main` that prints
/// `error("boom").message()` runs through the interpreter (with the embedded
/// `core.error` module compiled and registered) and prints `boom`. This backs
/// the `error_output_smoke` conformance fixture, whose `// EXPECT: output`
/// directive is parsed-but-not-executed by the harness today (staged for
/// Q-fconf).
#[test]
fn run_prints_error_message() {
    let f = write_temp_file(
        "module main\n\
         \n\
         use core.error.{error}\n\
         \n\
         public fn main() {\n\
         \x20\x20println(error(\"boom\").message())\n\
         }\n",
    );
    let output = bock_bin().arg("run").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "run core.error main");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("boom"),
        "expected `boom` in stdout, got: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
}
