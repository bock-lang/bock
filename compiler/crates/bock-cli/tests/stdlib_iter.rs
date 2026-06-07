//! Integration tests for the embedded `core.iter` module.
//!
//! These tests invoke the real `bock` binary against temp user files that
//! `use core.iter.{...}`, asserting the embedded stdlib is discoverable and
//! resolves through the existing `ModuleRegistry` with no special-casing — the
//! iteration counterpart to the `stdlib_compare` / `stdlib_convert` suites.
//!
//! Type-checking (`bock check`) is exercised broadly: the traits, the concrete
//! `ListIterator` record, the `list_iter` constructor, and every combinator
//! type-check. The runtime smoke (`bock run`) now drives the full `loop { match
//! it.next() }` shape: the tree-walking interpreter persists `mut self`
//! mutations across method-call frames (Q-iter-interp-mutself), so the cursor
//! advances and the loop terminates — matching the compiled js/ts/python
//! targets that the `exec_iter_*` conformance fixtures prove. The drive test is
//! wrapped in a wall-clock guard so a regression of the cursor write-back
//! surfaces as a clean assertion rather than a wedged CI run.

use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

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

/// Run a prepared `bock` command with a hard wall-clock timeout, capturing
/// output. Returns `None` (after killing the child) if the process hangs past
/// `timeout` — a test-failure signal for a non-terminating iterator drive.
fn run_with_timeout(mut cmd: Command, timeout: Duration) -> Option<Output> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn bock");
    let start = Instant::now();
    loop {
        match child.try_wait().expect("try_wait failed") {
            Some(_status) => {
                return Some(
                    child
                        .wait_with_output()
                        .expect("wait_with_output after exit failed"),
                );
            }
            None => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
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

/// Runtime drive: constructing a `ListIterator` via `list_iter(...)` and
/// driving it with the proven `loop { match it.next() { Some(x) => ...; None =>
/// break } }` shape dispatches into the embedded `core.iter` impl at run time,
/// advances the `mut self` cursor across calls, terminates, and accumulates the
/// correct total. The wall-clock guard catches a regression of the cursor
/// write-back as an assertion rather than a hang.
#[test]
fn run_drive_loop_accumulates_total() {
    let f = write_temp_file(
        "module main\n\
         \n\
         use core.iter.{ListIterator, list_iter}\n\
         \n\
         public fn main() {\n\
         \x20\x20let mut it: ListIterator[Int] = list_iter([7, 8, 9])\n\
         \x20\x20let mut total: Int = 0\n\
         \x20\x20loop {\n\
         \x20\x20\x20\x20match it.next() {\n\
         \x20\x20\x20\x20\x20\x20Some(x) => {\n\
         \x20\x20\x20\x20\x20\x20\x20\x20total = total + x\n\
         \x20\x20\x20\x20\x20\x20}\n\
         \x20\x20\x20\x20\x20\x20None => break\n\
         \x20\x20\x20\x20}\n\
         \x20\x20}\n\
         \x20\x20println(\"total=${total}\")\n\
         }\n",
    );
    let mut cmd = bock_bin();
    cmd.arg("run").arg(f.path());
    let output = run_with_timeout(cmd, Duration::from_secs(30))
        .expect("`bock run` hung: core.iter ListIterator cursor did not advance");
    assert_exit_code(&output, 0, "run core.iter drive loop");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("total=24"),
        "expected `total=24` in stdout, got: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Runtime smoke: constructing a `ListIterator` via `list_iter(...)` and calling
/// `it.next()` once dispatches into the embedded `core.iter` impl at run time and
/// yields `Some(first)`.
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

/// The §18.5 `for`-over-`Iterable` desugar: a user `record Bag` implementing
/// `Iterable[Int]` can be iterated with `for x in bag { ... }`. The checker
/// rewrites the loop into the manual `loop { match it.next() }` drive, and the
/// `Some(x)` arm types `x` to the concrete element `Int`, so the body's
/// `total + x` (an Int op) type-checks. This is the type-checking half of the
/// `exec_for_user_iterable*` conformance fixtures.
#[test]
fn check_for_over_user_iterable_desugars() {
    let f = write_temp_file(
        "module userapp\n\
         \n\
         use core.iter.{list_iter}\n\
         \n\
         public record Bag {\n\
         \x20\x20items: List[Int]\n\
         }\n\
         \n\
         impl Iterable[Int] for Bag {\n\
         \x20\x20public fn iter(self) -> ListIterator[Int] {\n\
         \x20\x20\x20\x20list_iter(self.items)\n\
         \x20\x20}\n\
         }\n\
         \n\
         public fn sum(bag: Bag) -> Int {\n\
         \x20\x20let mut total: Int = 0\n\
         \x20\x20for x in bag {\n\
         \x20\x20\x20\x20total = total + x\n\
         \x20\x20}\n\
         \x20\x20total\n\
         }\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "for-over-Iterable desugar type-checks");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no errors"), "stdout: {stdout}");
}
