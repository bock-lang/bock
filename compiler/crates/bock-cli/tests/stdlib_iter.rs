//! Integration tests for the embedded `core.iter` module.
//!
//! These tests invoke the real `bock` binary against temp user files that
//! `use core.iter.{...}`, asserting the embedded stdlib is discoverable and
//! resolves through the existing `ModuleRegistry` with no special-casing — the
//! iteration counterpart to the `stdlib_compare` / `stdlib_convert` suites.
//!
//! Type-checking (`bock check`) is exercised broadly: the traits, the concrete
//! `ListIterator` record, the `list_iter` constructor, and every combinator
//! type-check. The runtime smoke (`bock run`) is deliberately a SINGLE
//! `it.next()` rather than a drive loop: the tree-walking interpreter does not
//! persist `mut self` mutations across method calls, so a `loop { match
//! it.next() }` over a cursor never advances and spins forever (a pre-existing
//! interpreter gap that also affects the hand-written
//! `generic_iter_concrete_match.bock` fixture; the *compiled* js/ts/python
//! targets drive the loop correctly, as the `exec_iter_*` conformance fixtures
//! prove). A single `next()` returns `Some(first)` and exits cleanly, which is
//! enough to prove cross-module method dispatch into the embedded module at run
//! time.

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

/// Importing the `core.iter` traits, the concrete `ListIterator` record, and the
/// `list_iter` constructor resolves cleanly, and driving the iterator with the
/// proven `loop { match it.next() { Some(x) => ...; None => break } }` shape
/// type-checks — the `Some` payload `x` resolves to the concrete element type
/// `Int`.
#[test]
fn check_resolves_iter_traits_and_drive() {
    let f = write_temp_file(
        "module userapp\n\
         \n\
         use core.iter.{Iterator, Iterable, ListIterator, list_iter}\n\
         \n\
         public fn sum_via_drive() -> Int {\n\
         \x20\x20let mut it: ListIterator[Int] = list_iter([1, 2, 3])\n\
         \x20\x20let mut total: Int = 0\n\
         \x20\x20loop {\n\
         \x20\x20\x20\x20match it.next() {\n\
         \x20\x20\x20\x20\x20\x20Some(x) => {\n\
         \x20\x20\x20\x20\x20\x20\x20\x20total = total + x\n\
         \x20\x20\x20\x20\x20\x20}\n\
         \x20\x20\x20\x20\x20\x20None => break\n\
         \x20\x20\x20\x20}\n\
         \x20\x20}\n\
         \x20\x20total\n\
         }\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "core.iter traits + drive resolve");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no errors"), "stdout: {stdout}");
}

/// Every `core.iter` combinator type-checks against a `ListIterator[Int]`:
/// `to_list`/`count`/`fold`/`map`/`filter`/`take`. The higher-order combinators
/// receive Bock lambdas, exercising the function-typed parameters
/// (`Fn(A, T) -> A`, `Fn(T) -> U`, `Fn(T) -> Bool`).
#[test]
fn check_combinators_typecheck() {
    let f = write_temp_file(
        "module userapp\n\
         \n\
         use core.iter.{ListIterator, list_iter, to_list, count, fold, map, filter, take}\n\
         \n\
         public fn all() -> Int {\n\
         \x20\x20let listed: List[Int] = to_list(list_iter([1, 2, 3]))\n\
         \x20\x20let n: Int = count(list_iter([1, 2, 3]))\n\
         \x20\x20let summed: Int = fold(list_iter([1, 2, 3]), 0, (acc, x) => acc + x)\n\
         \x20\x20let doubled: List[Int] = map(list_iter([1, 2, 3]), (x) => x * 2)\n\
         \x20\x20let kept: List[Int] = filter(list_iter([1, 2, 3, 4]), (x) => x > 2)\n\
         \x20\x20let taken: List[Int] = take(list_iter([1, 2, 3, 4]), 2)\n\
         \x20\x20n + summed\n\
         }\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "core.iter combinators type-check");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no errors"), "stdout: {stdout}");
}

/// Runtime smoke: constructing a `ListIterator` via `list_iter(...)` and calling
/// `it.next()` once dispatches into the embedded `core.iter` impl at run time and
/// yields `Some(first)`. A SINGLE `next()` (not a loop) is used deliberately —
/// see the module docs for the interpreter `mut self` cursor limitation.
#[test]
fn run_single_next_yields_first() {
    let f = write_temp_file(
        "module main\n\
         \n\
         use core.iter.{ListIterator, list_iter}\n\
         \n\
         public fn main() {\n\
         \x20\x20let mut it: ListIterator[Int] = list_iter([7, 8, 9])\n\
         \x20\x20match it.next() {\n\
         \x20\x20\x20\x20Some(x) => {\n\
         \x20\x20\x20\x20\x20\x20println(\"first=${x}\")\n\
         \x20\x20\x20\x20}\n\
         \x20\x20\x20\x20None => {\n\
         \x20\x20\x20\x20\x20\x20println(\"empty\")\n\
         \x20\x20\x20\x20}\n\
         \x20\x20}\n\
         }\n",
    );
    let output = bock_bin().arg("run").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "run core.iter single next");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("first=7"),
        "expected `first=7` in stdout, got: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
}
