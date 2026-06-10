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
/// stdout/stderr. Returns `Some(output)` if the process finished in time, or
/// `None` (after killing the child) if it exceeded `timeout`.
///
/// The standard library has no built-in process timeout, so we poll
/// `try_wait` on a short interval. A `None` result is a *test failure signal*:
/// it means the program hung (e.g. a `mut self` iterator drive that never
/// advances its cursor). This guard ensures a regression surfaces as a clean
/// assertion rather than wedging the whole test run.
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

/// Regression (Q-iter-interp-mutself): a `loop { match it.next() { ... } }`
/// drive over a record with a `next(mut self)` cursor must TERMINATE under the
/// interpreter and yield the correct total. Before the fix, `mut self` field
/// mutations did not persist across method-call frames, so the cursor never
/// advanced, `None` was never reached, and the loop spun forever — the
/// interpreter-only hang the compiled targets never had. The `run_with_timeout`
/// guard turns any regression back into a clean assertion failure instead of a
/// wedged CI run.
#[test]
fn run_mut_self_iterator_drive_terminates() {
    let f = write_temp_file(
        "module main\n\
         \n\
         record ListIter {\n\
         \x20\x20xs: List[Int]\n\
         \x20\x20cursor: Int\n\
         }\n\
         \n\
         impl ListIter {\n\
         \x20\x20fn next(mut self) -> Optional[Int] {\n\
         \x20\x20\x20\x20match self.xs.get(self.cursor) {\n\
         \x20\x20\x20\x20\x20\x20Some(v) => {\n\
         \x20\x20\x20\x20\x20\x20\x20\x20self.cursor = self.cursor + 1\n\
         \x20\x20\x20\x20\x20\x20\x20\x20return Some(v)\n\
         \x20\x20\x20\x20\x20\x20}\n\
         \x20\x20\x20\x20\x20\x20None => return None\n\
         \x20\x20\x20\x20}\n\
         \x20\x20}\n\
         }\n\
         \n\
         fn main() -> Void {\n\
         \x20\x20let mut c: ListIter = ListIter { xs: [1, 2, 3], cursor: 0 }\n\
         \x20\x20let mut total: Int = 0\n\
         \x20\x20loop {\n\
         \x20\x20\x20\x20match c.next() {\n\
         \x20\x20\x20\x20\x20\x20Some(x) => {\n\
         \x20\x20\x20\x20\x20\x20\x20\x20total = total + x\n\
         \x20\x20\x20\x20\x20\x20}\n\
         \x20\x20\x20\x20\x20\x20None => break\n\
         \x20\x20\x20\x20}\n\
         \x20\x20}\n\
         \x20\x20println(\"sum=${total}\")\n\
         }\n",
    );
    let mut cmd = bock_bin();
    cmd.arg("run").arg(f.path());
    let output = run_with_timeout(cmd, Duration::from_secs(30)).expect(
        "`bock run` hung: mut self iterator cursor did not advance (Q-iter-interp-mutself)",
    );
    assert!(
        output.status.success(),
        "expected exit 0, got {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("sum=6"), "stdout: {stdout}");
}

/// `mut self` cursor advancement persists across *successive* method calls
/// (not just within a loop): each `bump()` call mutates and returns the
/// running counter, and a later non-`mut`-self `peek()` observes the persisted
/// state. Guards that the write-back targets a plain-variable receiver lvalue.
#[test]
fn run_mut_self_persists_across_calls() {
    let f = write_temp_file(
        "module main\n\
         \n\
         record Counter {\n\
         \x20\x20n: Int\n\
         }\n\
         \n\
         impl Counter {\n\
         \x20\x20fn bump(mut self) -> Int {\n\
         \x20\x20\x20\x20self.n = self.n + 1\n\
         \x20\x20\x20\x20return self.n\n\
         \x20\x20}\n\
         \x20\x20fn peek(self) -> Int {\n\
         \x20\x20\x20\x20return self.n\n\
         \x20\x20}\n\
         }\n\
         \n\
         fn main() -> Void {\n\
         \x20\x20let mut c: Counter = Counter { n: 0 }\n\
         \x20\x20let a: Int = c.bump()\n\
         \x20\x20let b: Int = c.bump()\n\
         \x20\x20let d: Int = c.bump()\n\
         \x20\x20println(\"a=${a} b=${b} d=${d} final=${c.peek()}\")\n\
         }\n",
    );
    let mut cmd = bock_bin();
    cmd.arg("run").arg(f.path());
    let output = run_with_timeout(cmd, Duration::from_secs(30))
        .expect("`bock run` hung on successive mut self calls");
    assert!(
        output.status.success(),
        "expected exit 0, got {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("a=1 b=2 d=3 final=3"), "stdout: {stdout}",);
}

/// Regression (Q-interp-question-propagation): `expr?` on an `Err`/`None` must
/// early-return the `Err`/`None` from the *enclosing function* (§7.10) — the
/// caller observes it as a normal `Result`/`Optional` value and execution
/// continues — instead of aborting the whole program. Before the fix this
/// program died after the first line with `runtime error: propagated error:
/// too big`, while every compiled target (js verified as the parity reference)
/// printed all four lines. Mirrors the conformance pair
/// `conformance/interp/question_propagation.bock` (interpreter) and
/// `conformance/exec/exec_question_propagation.bock` (compiled targets ×5).
#[test]
fn run_question_propagation_returns_err_to_caller() {
    let f = write_temp_file(
        "module main\n\
         \n\
         fn parse_small(s: String) -> Result[Int, String] {\n\
         \x20\x20if (s.len() > 3) {\n\
         \x20\x20\x20\x20return Err(\"too big\")\n\
         \x20\x20}\n\
         \x20\x20Ok(s.len())\n\
         }\n\
         \n\
         fn double_len(s: String) -> Result[Int, String] {\n\
         \x20\x20let n = parse_small(s)?\n\
         \x20\x20Ok(n * 2)\n\
         }\n\
         \n\
         fn lookup(s: String) -> Optional[Int] {\n\
         \x20\x20if (s.len() == 0) {\n\
         \x20\x20\x20\x20return None\n\
         \x20\x20}\n\
         \x20\x20Some(s.len())\n\
         }\n\
         \n\
         fn first_or_none(s: String) -> Optional[Int] {\n\
         \x20\x20let n = lookup(s)?\n\
         \x20\x20Some(n + 1)\n\
         }\n\
         \n\
         fn main() -> Void {\n\
         \x20\x20match double_len(\"ab\") {\n\
         \x20\x20\x20\x20Ok(n) => println(\"ok: ${n}\")\n\
         \x20\x20\x20\x20Err(e) => println(\"err: ${e}\")\n\
         \x20\x20}\n\
         \x20\x20match double_len(\"toolong\") {\n\
         \x20\x20\x20\x20Ok(n) => println(\"ok: ${n}\")\n\
         \x20\x20\x20\x20Err(e) => println(\"err: ${e}\")\n\
         \x20\x20}\n\
         \x20\x20match first_or_none(\"\") {\n\
         \x20\x20\x20\x20Some(n) => println(\"some: ${n}\")\n\
         \x20\x20\x20\x20None => println(\"none\")\n\
         \x20\x20}\n\
         \x20\x20println(\"done\")\n\
         }\n",
    );
    let output = bock_bin().arg("run").arg(f.path()).output().unwrap();
    assert!(
        output.status.success(),
        "expected exit 0 (propagated Err must be observed by the caller, not \
         abort the program), got {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Pin the full stdout: the js/compiled-target reference output for this
    // program, including the lines *after* the first propagated Err.
    assert_eq!(
        stdout, "ok: 4\nerr: too big\nnone\ndone\n",
        "interpreter output diverged from the compiled-target reference"
    );
}

/// The Equatable primitive bridge under `bock run`
/// (Q-interp-assert-primitives root cause): `a.eq(b)` on concrete primitive
/// receivers must dispatch to native equality, as the compiled targets lower
/// it (js `===`, rust `==`, …). Before the fix the interpreter registered the
/// bridge under the never-referenced name `equals`, so `.eq` died with
/// `method 'eq' not found on Int`.
#[test]
fn run_primitive_eq_bridge_dispatches() {
    let f = write_temp_file(
        "module main\n\
         \n\
         fn main() -> Void {\n\
         \x20\x20let i = (3).eq(3)\n\
         \x20\x20let f = (2.5).eq(2.5)\n\
         \x20\x20let b = (true).eq(true)\n\
         \x20\x20let s = \"ab\".eq(\"ab\")\n\
         \x20\x20let c = ('x').eq('x')\n\
         \x20\x20let n = (3).eq(4)\n\
         \x20\x20println(\"int=${i};float=${f};bool=${b};string=${s};char=${c};neq=${n}\")\n\
         }\n",
    );
    let output = bock_bin().arg("run").arg(f.path()).output().unwrap();
    assert!(
        output.status.success(),
        "expected exit 0, got {}\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("int=true;float=true;bool=true;string=true;char=true;neq=false"),
        "stdout: {stdout}"
    );
}
