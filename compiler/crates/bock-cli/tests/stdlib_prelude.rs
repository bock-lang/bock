//! Integration tests for the §18.2 prelude (auto-imported symbols).
//!
//! These tests invoke the real `bock` binary against user files that name the
//! prelude symbols `Ordering`/`Less`/`Equal`/`Greater`, the core traits
//! `Comparable`/`Equatable`/`From`/`Into`/`Displayable`/`Error`, and the
//! builtin generic types/constructors `Optional`/`Some`/`None`,
//! `Result`/`Ok`/`Err`, `List` — *without* any `use`. They assert that the
//! prelude makes the `core.*`-defined symbols resolve and type-check, and that
//! an explicit `use core.<module>.{...}` still works (no double-definition).
//!
//! The prelude subset that is *defined in an embedded core module* (the
//! `core.compare`/`core.convert`/`core.error` traits + `Ordering`) is seeded
//! into every user module via `bock_types::seed_prelude`, reusing the same
//! registry path as an explicit import.

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

/// Resolve the repo's conformance fixture directory for prelude fixtures.
fn prelude_conformance_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/compiler/crates/bock-cli
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/conformance/stdlib/prelude")
}

/// Every `conformance/stdlib/prelude/*.bock` fixture (each of which uses prelude
/// symbols WITHOUT any `use`) checks clean through the real binary.
#[test]
fn prelude_conformance_fixtures_check_clean() {
    let dir = prelude_conformance_dir();
    let mut count = 0;
    for entry in
        std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
    {
        let path = entry.unwrap().path();
        if path.extension().and_then(|s| s.to_str()) != Some("bock") {
            continue;
        }
        count += 1;
        let output = bock_bin().arg("check").arg(&path).output().unwrap();
        assert_exit_code(&output, 0, &format!("prelude fixture {}", path.display()));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("no errors"),
            "fixture {} stdout: {stdout}",
            path.display(),
        );
    }
    assert!(count >= 7, "expected >= 7 prelude fixtures, found {count}");
}

/// `Ordering` is nameable and `Less`/`Equal`/`Greater` are usable as bare
/// values WITHOUT any `use` — the regression that motivated the feature.
#[test]
fn bare_ordering_variant_value_resolves_without_use() {
    let f = write_temp_file(
        "module userapp\n\
         \n\
         public fn least() -> Ordering {\n\
         \x20\x20Less\n\
         }\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "bare Less as a value without use");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no errors"), "stdout: {stdout}");
}

/// A user type implements the prelude `Comparable` trait — naming `Ordering`
/// and returning its variants — and a caller matches the `.compare(...)`
/// result, all WITHOUT a `use core.compare`.
#[test]
fn comparable_impl_and_match_resolve_without_use() {
    let f = write_temp_file(
        "module userapp\n\
         \n\
         public record Money { cents: Int }\n\
         \n\
         impl Comparable for Money {\n\
         \x20\x20public fn compare(self, other: Money) -> Ordering {\n\
         \x20\x20\x20\x20if (self.cents < other.cents) {\n\
         \x20\x20\x20\x20\x20\x20Less\n\
         \x20\x20\x20\x20} else if (self.cents == other.cents) {\n\
         \x20\x20\x20\x20\x20\x20Equal\n\
         \x20\x20\x20\x20} else {\n\
         \x20\x20\x20\x20\x20\x20Greater\n\
         \x20\x20\x20\x20}\n\
         \x20\x20}\n\
         }\n\
         \n\
         public fn rank(a: Money, b: Money) -> String {\n\
         \x20\x20match a.compare(b) {\n\
         \x20\x20\x20\x20Less => \"less\"\n\
         \x20\x20\x20\x20Equal => \"equal\"\n\
         \x20\x20\x20\x20Greater => \"greater\"\n\
         \x20\x20}\n\
         }\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "Comparable impl + Ordering match without use");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no errors"), "stdout: {stdout}");
}

/// The prelude convert/error traits (`From`/`Into`/`Displayable`/`Error`) and
/// the builtin generic types/constructors (`Optional`/`Some`/`None`,
/// `Result`/`Ok`/`Err`, `List`) all resolve without any `use`.
#[test]
fn convert_error_and_builtins_resolve_without_use() {
    let f = write_temp_file(
        "module userapp\n\
         \n\
         public record Meters { value: Float }\n\
         public record Feet { value: Float }\n\
         \n\
         impl From[Meters] for Feet {\n\
         \x20\x20public fn from(value: Meters) -> Feet {\n\
         \x20\x20\x20\x20Feet { value: value.value * 3.28084 }\n\
         \x20\x20}\n\
         }\n\
         \n\
         public fn to_feet(m: Meters) -> Feet { m.into() }\n\
         \n\
         public record NotFound { what: String }\n\
         impl Error for NotFound {\n\
         \x20\x20public fn message(self) -> String { self.what }\n\
         }\n\
         \n\
         public fn pick(flag: Bool) -> Optional[Int] {\n\
         \x20\x20if (flag) { Some(1) } else { None }\n\
         }\n\
         \n\
         public fn divide(a: Int, b: Int) -> Result[Int, String] {\n\
         \x20\x20if (b == 0) { Err(\"div by zero\") } else { Ok(a / b) }\n\
         }\n\
         \n\
         public fn nums() -> List[Int] { [1, 2, 3] }\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "convert/error traits + builtins without use");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no errors"), "stdout: {stdout}");
}

/// Explicit `use core.compare.{...}` of the same symbols the prelude seeds
/// still type-checks: the prelude-seeded symbol and the explicit import bind
/// the same definition, so there is no double-definition / shadowing error.
#[test]
fn explicit_use_still_works_alongside_prelude() {
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
         }\n\
         \n\
         public fn first() -> Ordering { Less }\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert_exit_code(&output, 0, "explicit use coexists with prelude");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no errors"), "stdout: {stdout}");
}
