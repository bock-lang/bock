//! Integration tests for the embedded `core.compare` module.
//!
//! These tests invoke the real `bock` binary against temp user files that
//! `use core.compare.{...}`, asserting the embedded stdlib is discoverable and
//! resolves through the existing `ModuleRegistry` with no special-casing —
//! including the things the `core.error` pilot did not exercise: a generic
//! trait whose method takes a second operand of the implementing type, and a
//! generic function bounded by that trait (`max[T: Comparable]`).

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

/// A user file importing the `Comparable` and `Equatable` traits by name and
/// referencing them via method calls (`.compare` returning `Ordering`, `.eq`
/// returning `Bool`) resolves cleanly through the embedded stdlib. This is the
/// generic-trait validation: the trait methods declare a second operand of the
/// implementing type, and the call sites type-check.
#[test]
fn check_resolves_compare_traits() {
    let f = write_temp_file(
        "module userapp\n\
         \n\
         use core.compare.{Ordering, Comparable, Equatable, Key}\n\
         \n\
         public fn order(a: Key, b: Key) -> Ordering {\n\
         \x20\x20a.compare(b)\n\
         }\n\
         \n\
         public fn same(a: Key, b: Key) -> Bool {\n\
         \x20\x20a.eq(b)\n\
         }\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "core.compare traits resolve");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no errors"), "stdout: {stdout}");
}

/// Constructing a `Key` via the `key(...)` constructor and matching on the
/// `Ordering` returned by `.compare(...)` resolves the imported enum variants
/// (`Less`/`Equal`/`Greater`) and type-checks against the embedded module.
#[test]
fn check_constructs_and_matches_ordering() {
    let f = write_temp_file(
        "module userapp\n\
         \n\
         use core.compare.{Ordering, Key, key}\n\
         \n\
         public fn describe(n: Int, m: Int) -> String {\n\
         \x20\x20let a: Key = key(n)\n\
         \x20\x20let b: Key = key(m)\n\
         \x20\x20match a.compare(b) {\n\
         \x20\x20\x20\x20Less => \"less\"\n\
         \x20\x20\x20\x20Equal => \"equal\"\n\
         \x20\x20\x20\x20Greater => \"greater\"\n\
         \x20\x20}\n\
         }\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "Ordering construct + match");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no errors"), "stdout: {stdout}");
}

/// The generic, trait-bounded helpers `max[T: Comparable]` and
/// `min[T: Comparable]` accept any `Comparable` value (here `Key`), dispatching
/// through its `compare` impl. This is the generic-bounded-function validation
/// that `core.error` did not cover.
#[test]
fn check_generic_bounded_helpers() {
    let f = write_temp_file(
        "module userapp\n\
         \n\
         use core.compare.{Key, key, max, min}\n\
         \n\
         public fn larger(a: Key, b: Key) -> Key {\n\
         \x20\x20max(a, b)\n\
         }\n\
         \n\
         public fn smaller(a: Key, b: Key) -> Key {\n\
         \x20\x20min(a, b)\n\
         }\n\
         \n\
         public fn pick() -> Key {\n\
         \x20\x20max(key(3), key(9))\n\
         }\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "max/min generic helpers");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no errors"), "stdout: {stdout}");
}

/// End-to-end runtime smoke test: a `main` that builds two equal `Key`s and
/// prints whether they are `eq` runs through the interpreter (with the embedded
/// `core.compare` module compiled and registered) and prints `equal`. This
/// backs the `compare_output_smoke` conformance fixture, whose `// EXPECT:
/// output` directive is parsed-but-not-executed by the harness today (staged
/// for Q-fconf).
///
/// The smoke uses `.eq(...)` (whose impl body returns a `Bool`) rather than
/// `.compare(...)`: constructing an enum variant inside a *cross-module* stdlib
/// impl body is currently undefined at run time in the interpreter (an
/// interpreter-scoping gap distinct from type-checking and codegen, both of
/// which handle it). `.eq` exercises the same cross-module impl dispatch
/// without that gap.
#[test]
fn run_prints_equality() {
    let f = write_temp_file(
        "module main\n\
         \n\
         use core.compare.{Key, key}\n\
         \n\
         public fn main() {\n\
         \x20\x20let a = key(2)\n\
         \x20\x20let b = key(2)\n\
         \x20\x20if (a.eq(b)) {\n\
         \x20\x20\x20\x20println(\"equal\")\n\
         \x20\x20} else {\n\
         \x20\x20\x20\x20println(\"differ\")\n\
         \x20\x20}\n\
         }\n",
    );
    let output = bock_bin().arg("run").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "run core.compare main");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("equal"),
        "expected `equal` in stdout, got: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
}
