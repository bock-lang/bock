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

#[test]
fn check_valid_file_exits_0() {
    let f = write_temp_file("fn add(a: Int, b: Int) -> Int { a + b }\n");
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert!(
        output.status.success(),
        "expected exit 0, got {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("no errors"), "stdout: {stdout}");
}

#[test]
fn check_syntax_error_exits_1() {
    let f = write_temp_file("fn { broken\n");
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert!(
        !output.status.success(),
        "expected non-zero exit, got {}",
        output.status,
    );
}

#[test]
fn check_file_not_found_exits_1() {
    let output = bock_bin()
        .arg("check")
        .arg("/tmp/nonexistent_bock_file_12345.bock")
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "expected non-zero exit for missing file",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("nonexistent_bock_file_12345.bock"),
        "stderr should mention the file: {stderr}",
    );
}

#[test]
fn check_no_context_flag_disables_source_context() {
    let f = write_temp_file("fn { broken\n");
    let output = bock_bin()
        .arg("check")
        .arg("--no-context")
        .arg(f.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    // With --no-context, output should be simple one-line format
    assert!(
        stderr.contains("error["),
        "should show error in bracket format: {stderr}",
    );
}

#[test]
fn check_types_flag() {
    let f = write_temp_file("fn add(a: Int, b: Int) -> Int { a + b }\n");
    let output = bock_bin()
        .arg("check")
        .arg("--types")
        .arg(f.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "expected exit 0 with --types, got {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn check_lint_flag() {
    let f = write_temp_file("fn add(a: Int, b: Int) -> Int { a + b }\n");
    let output = bock_bin()
        .arg("check")
        .arg("--lint")
        .arg(f.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "expected exit 0 with --lint, got {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn check_no_files_in_empty_dir() {
    let dir = tempfile::tempdir().unwrap();
    let output = bock_bin()
        .arg("check")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "expected non-zero exit when no .bock files found",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("No .bock files found"), "stderr: {stderr}",);
}

#[test]
fn check_reports_ownership_error() {
    // A function that moves a record and then uses it should trigger an ownership error.
    // (Primitives like Int have copy semantics and won't trigger this.)
    let f = write_temp_file(
        "record Thing { id: Int }\nfn process() {\n    let data = Thing { id: 1 }\n    let archive = data\n    let x = data\n}\n",
    );
    let output = bock_bin().arg("check").arg(f.path()).output().unwrap();
    assert!(
        !output.status.success(),
        "expected non-zero exit for ownership error, got {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("moved") || stderr.contains("E5001"),
        "stderr should mention moved variable or E5001: {stderr}",
    );
}

#[test]
fn check_multiple_files() {
    let f1 = write_temp_file("fn foo() -> Int { 1 }\n");
    let f2 = write_temp_file("fn bar() -> Int { 2 }\n");
    let output = bock_bin()
        .arg("check")
        .arg(f1.path())
        .arg(f2.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "expected exit 0 for two valid files, got {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("2 files checked"), "stdout: {stdout}");
}

#[test]
fn check_multifile_dependency_ordering() {
    // Two-module project: main imports from models.
    // Tests that the dependency graph correctly orders compilation
    // (Models compiled before Main since Main depends on Models).
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join("models.bock"),
        "module Models\n\npublic record User {\n    id: Int\n    name: String\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("main.bock"),
        "module Main\n\nuse Models.{User}\n\nfn make_user() -> Int {\n    42\n}\n",
    )
    .unwrap();

    let output = bock_bin()
        .arg("check")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "expected exit 0 for multi-file project, got {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("2 files checked"), "stdout: {stdout}");
}

#[test]
fn check_circular_dependency_detected() {
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join("a.bock"),
        "module A\n\nuse B.{foo}\n\nfn bar() -> Int { 1 }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("b.bock"),
        "module B\n\nuse A.{bar}\n\nfn foo() -> Int { 2 }\n",
    )
    .unwrap();

    let output = bock_bin()
        .arg("check")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "expected non-zero exit for circular dependency",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("circular"),
        "stderr should mention circular dependency: {stderr}",
    );
}

#[test]
fn check_multifile_error_shows_correct_path() {
    // A valid file and a file with a type error — the error message
    // should reference the correct file path.
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join("good.bock"),
        "fn helper() -> Int { 42 }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("bad.bock"),
        "fn broken( { }\n",
    )
    .unwrap();

    let output = bock_bin()
        .arg("check")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "expected non-zero exit due to parse error",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("bad.bock"),
        "error should reference bad.bock: {stderr}",
    );
}

// ─── Cross-file type checking (B.6) ────────────────────────────────────────

#[test]
fn check_crossfile_record_construct_and_field_access() {
    // models.bock exports User record; service.bock imports and uses it.
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join("models.bock"),
        "\
module Models

public record User {
    name: String
    age: Int
}
",
    )
    .unwrap();

    std::fs::write(
        dir.path().join("service.bock"),
        "\
module Service

use Models.{User}

fn create_user(name: String, age: Int) -> User {
    User { name: name, age: age }
}

fn get_name(u: User) -> String {
    u.name
}

fn get_age(u: User) -> Int {
    u.age
}
",
    )
    .unwrap();

    let output = bock_bin()
        .arg("check")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "cross-file record construct/access should pass type checking\nstderr: {stderr}",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("2 files checked"), "stdout: {stdout}");
}

#[test]
fn check_crossfile_enum_match() {
    // models.bock exports Role enum; service.bock imports and matches on it.
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join("models.bock"),
        "\
module Models

public enum Role {
    Admin
    Member
    Guest
}
",
    )
    .unwrap();

    std::fs::write(
        dir.path().join("service.bock"),
        "\
module Service

use Models.{Role, Admin, Member, Guest}

fn role_level(r: Role) -> Int {
    match (r) {
        Admin => 3
        Member => 2
        Guest => 1
    }
}
",
    )
    .unwrap();

    let output = bock_bin()
        .arg("check")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "cross-file enum match should pass type checking\nstderr: {stderr}",
    );
}

#[test]
fn check_crossfile_function_call() {
    // lib.bock exports a function; main.bock imports and calls it.
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join("lib.bock"),
        "\
module Lib

public fn add(a: Int, b: Int) -> Int {
    a + b
}
",
    )
    .unwrap();

    std::fs::write(
        dir.path().join("main.bock"),
        "\
module Main

use Lib.{add}

fn main() -> Int {
    add(1, 2)
}
",
    )
    .unwrap();

    let output = bock_bin()
        .arg("check")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "cross-file function call should pass type checking\nstderr: {stderr}",
    );
}

#[test]
fn check_crossfile_imported_type_in_signature() {
    // User is imported and used as parameter and return type.
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join("types.bock"),
        "\
module Types

public record Point {
    x: Int
    y: Int
}
",
    )
    .unwrap();

    std::fs::write(
        dir.path().join("geometry.bock"),
        "\
module Geometry

use Types.{Point}

fn origin() -> Point {
    Point { x: 0, y: 0 }
}

fn translate(p: Point, dx: Int, dy: Int) -> Point {
    Point { x: p.x + dx, y: p.y + dy }
}
",
    )
    .unwrap();

    let output = bock_bin()
        .arg("check")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "imported type in function signatures should pass\nstderr: {stderr}",
    );
}

#[test]
fn check_crossfile_three_modules() {
    // Three-module chain: Types → Models → Service
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join("types.bock"),
        "\
module Types

public record User {
    name: String
    age: Int
}

public enum Role {
    Admin
    Member
    Guest
}
",
    )
    .unwrap();

    std::fs::write(
        dir.path().join("models.bock"),
        "\
module Models

use Types.{User, Role, Admin}

public fn is_admin(r: Role) -> Bool {
    match (r) {
        Admin => true
        _ => false
    }
}

public fn make_user(name: String) -> User {
    User { name: name, age: 0 }
}
",
    )
    .unwrap();

    std::fs::write(
        dir.path().join("service.bock"),
        "\
module Service

use Types.{User}
use Models.{make_user, is_admin}

fn create_default_user() -> User {
    make_user(\"anonymous\")
}
",
    )
    .unwrap();

    let output = bock_bin()
        .arg("check")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "three-module chain should pass type checking\nstderr: {stderr}",
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("3 files checked"), "stdout: {stdout}");
}

#[test]
fn check_crossfile_effect_operations() {
    // Effect defined in one file, used in another.
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join("effects.bock"),
        "\
module Effects

public effect Logger {
    fn log(msg: String) -> Void
}
",
    )
    .unwrap();

    std::fs::write(
        dir.path().join("service.bock"),
        "\
module Service

use Effects.{Logger}

fn greet(name: String) -> Void with Logger {
    log(\"Hello, \" + name)
}
",
    )
    .unwrap();

    let output = bock_bin()
        .arg("check")
        .current_dir(dir.path())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "cross-file effect operations should pass\nstderr: {stderr}",
    );
}
