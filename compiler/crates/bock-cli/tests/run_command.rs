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
fn run_simple_main() {
    let f = write_temp_file("fn main() { println(\"hello\") }\n");
    let output = bock_bin().arg("run").arg(f.path()).output().unwrap();
    assert!(
        output.status.success(),
        "expected exit 0, got {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("hello"), "stdout: {stdout}");
}

#[test]
fn run_no_main_exits_1() {
    let f = write_temp_file("fn add(a: Int, b: Int) -> Int { a + b }\n");
    let output = bock_bin().arg("run").arg(f.path()).output().unwrap();
    assert!(
        !output.status.success(),
        "expected non-zero exit when no main function",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no main function found"),
        "stderr: {stderr}",
    );
}

#[test]
fn run_syntax_error_exits_1() {
    let f = write_temp_file("fn { broken\n");
    let output = bock_bin().arg("run").arg(f.path()).output().unwrap();
    assert!(
        !output.status.success(),
        "expected non-zero exit for syntax error",
    );
}

#[test]
fn run_file_not_found_exits_1() {
    let output = bock_bin()
        .arg("run")
        .arg("/tmp/nonexistent_bock_file_99999.bock")
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "expected non-zero exit for missing file",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("file not found"), "stderr: {stderr}",);
}

#[test]
fn run_no_args_looks_for_main_bock() {
    // Run in an empty temp dir — should fail because no main.bock
    let dir = tempfile::tempdir().unwrap();
    let output = bock_bin()
        .arg("run")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "expected non-zero exit when no main.bock found",
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no entry file found"), "stderr: {stderr}",);
}

#[test]
fn run_no_args_finds_main_bock() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("main.bock"),
        "fn main() { println(\"from main.bock\") }\n",
    )
    .unwrap();
    let output = bock_bin()
        .arg("run")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "expected exit 0, got {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("from main.bock"), "stdout: {stdout}");
}

#[test]
fn run_no_args_finds_src_main_bock() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("src/main.bock"),
        "fn main() { println(\"from src\") }\n",
    )
    .unwrap();
    let output = bock_bin()
        .arg("run")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "expected exit 0, got {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("from src"), "stdout: {stdout}");
}

#[test]
fn run_program_args_after_double_dash() {
    let f = write_temp_file("fn main() { println(\"ok\") }\n");
    let output = bock_bin()
        .arg("run")
        .arg(f.path())
        .arg("--")
        .arg("arg1")
        .arg("arg2")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "expected exit 0 with program args, got {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn run_watch_flag_prints_not_implemented() {
    let f = write_temp_file("fn main() { println(\"ok\") }\n");
    let output = bock_bin()
        .arg("run")
        .arg("--watch")
        .arg(f.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "expected exit 0 for --watch stub, got {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("not yet implemented"), "stdout: {stdout}",);
}

#[test]
fn run_with_multiple_functions() {
    let f = write_temp_file("fn helper() -> String { \"42\" }\nfn main() { println(helper()) }\n");
    let output = bock_bin().arg("run").arg(f.path()).output().unwrap();
    assert!(
        output.status.success(),
        "expected exit 0, got {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("42"), "stdout: {stdout}");
}

#[test]
fn run_multifile_project() {
    // Multi-file project: helper module + main that calls it.
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join("helpers.bock"),
        "module Helpers\n\npublic fn greet() -> String { \"hello from helpers\" }\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("main.bock"),
        "module Main\n\nuse Helpers.{greet}\n\nfn main() {\n    println(greet())\n}\n",
    )
    .unwrap();

    let output = bock_bin()
        .arg("run")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "expected exit 0 for multi-file run, got {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("hello from helpers"),
        "stdout should contain greeting: {stdout}",
    );
}

/// Regression: `bock run path/to/proj/src/main.bock` invoked from a parent
/// directory that contains unrelated `.bock` files must only compile files
/// belonging to the entry file's project (delimited by its `bock.project`
/// marker). Previously the command recursively scanned the CWD, so a poison
/// sibling at the workspace level — e.g. a spec fixture using syntax the
/// parser doesn't accept yet — would abort the run before the entry file
/// was even processed.
#[test]
fn run_with_explicit_entry_ignores_files_outside_project_root() {
    let workspace = tempfile::tempdir().unwrap();

    // Poison file in the workspace root with parser-rejected syntax.
    std::fs::write(
        workspace.path().join("poison.bock"),
        "pure fn square(n: Int) -> Int { n * n }\n",
    )
    .unwrap();

    // A sibling subproject with its own poison file — also outside the
    // entry's project, so it must not be pulled in either.
    std::fs::create_dir(workspace.path().join("other")).unwrap();
    std::fs::write(
        workspace.path().join("other/broken.bock"),
        "pure fn noop() {}\n",
    )
    .unwrap();

    // The real project we want to run.
    let proj = workspace.path().join("proj");
    std::fs::create_dir(&proj).unwrap();
    std::fs::write(
        proj.join("bock.project"),
        "[project]\nname = \"proj\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::create_dir(proj.join("src")).unwrap();
    let entry = proj.join("src/main.bock");
    std::fs::write(&entry, "fn main() { println(\"proj-ok\") }\n").unwrap();

    let output = bock_bin()
        .arg("run")
        .arg(&entry)
        .current_dir(workspace.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "expected exit 0, got {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("proj-ok"), "stdout: {stdout}");
}

/// An explicit entry file that is not inside any `bock.project` tree should
/// compile on its own — the CWD must not be scanned. Otherwise running a
/// one-off script from a directory that happens to hold other `.bock` files
/// would drag them into the compile set.
#[test]
fn run_with_explicit_entry_and_no_project_ignores_cwd() {
    let entry_dir = tempfile::tempdir().unwrap();
    let entry = entry_dir.path().join("standalone.bock");
    std::fs::write(&entry, "fn main() { println(\"standalone-ok\") }\n").unwrap();

    // CWD has a poison file with parser-rejected syntax. If the implementation
    // scans CWD when no project root is found, this test fails.
    let cwd = tempfile::tempdir().unwrap();
    std::fs::write(
        cwd.path().join("poison.bock"),
        "pure fn square(n: Int) -> Int { n * n }\n",
    )
    .unwrap();

    let output = bock_bin()
        .arg("run")
        .arg(&entry)
        .current_dir(cwd.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "expected exit 0, got {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("standalone-ok"), "stdout: {stdout}");
}

/// Within an `bock.project`, sibling modules in peer subdirectories should
/// still be discovered so cross-module imports resolve — this exercises the
/// recursive walk from the project root rather than just the entry's parent.
#[test]
fn run_project_discovers_modules_across_subdirs() {
    let workspace = tempfile::tempdir().unwrap();
    let proj = workspace.path().join("proj");
    std::fs::create_dir(&proj).unwrap();
    std::fs::write(
        proj.join("bock.project"),
        "[project]\nname = \"proj\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    // Helper module in its own subdirectory.
    std::fs::create_dir(proj.join("lib")).unwrap();
    std::fs::write(
        proj.join("lib/helpers.bock"),
        "module helpers\n\npublic fn greet() -> String { \"cross-dir-ok\" }\n",
    )
    .unwrap();

    // Entry in a peer subdirectory imports from the helper.
    std::fs::create_dir(proj.join("src")).unwrap();
    let entry = proj.join("src/main.bock");
    std::fs::write(
        &entry,
        "module main\n\nuse helpers.{greet}\n\nfn main() { println(greet()) }\n",
    )
    .unwrap();

    // Run from the workspace root (outside the project) with an explicit entry.
    let output = bock_bin()
        .arg("run")
        .arg(&entry)
        .current_dir(workspace.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "expected exit 0, got {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("cross-dir-ok"), "stdout: {stdout}");
}
