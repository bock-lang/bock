//! Integration tests for `bock inspect air` — the machine-readable AIR dump.
//!
//! Pins the command's two output contracts end-to-end through the binary:
//!
//! * Human mode: an indented tree (kind, name, `@line:col`, byte range) on
//!   stdout, standard diagnostics on stderr for failures.
//! * `--json` mode: on success a single JSON object per node with exactly
//!   `kind` / `name` / `span {start, end, line, col}` / `children`; on
//!   failure a `{"error": {...}}` object instead of a tree.
//!
//! Exit contract mirrors `bock check`: 0 = lowered cleanly, 1 = any
//! frontend error. The VS Code extension's AIR tree viewer consumes the
//! `--json` shape — these assertions are the compatibility gate for it.

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

/// Depth-first search for a node with the given kind (and name, if given)
/// anywhere in a JSON AIR tree.
fn find_node<'a>(
    node: &'a serde_json::Value,
    kind: &str,
    name: Option<&str>,
) -> Option<&'a serde_json::Value> {
    let kind_matches = node["kind"] == kind;
    let name_matches = match name {
        Some(n) => node["name"] == n,
        None => true,
    };
    if kind_matches && name_matches {
        return Some(node);
    }
    node["children"]
        .as_array()?
        .iter()
        .find_map(|c| find_node(c, kind, name))
}

/// Assert a node satisfies the per-node JSON contract: exactly the four
/// fields, with a well-formed span.
fn assert_node_contract(node: &serde_json::Value, ctx: &str) {
    let obj = node
        .as_object()
        .unwrap_or_else(|| panic!("{ctx}: node must be an object"));
    assert_eq!(
        obj.len(),
        4,
        "{ctx}: a node has exactly kind/name/span/children, got keys {:?}",
        obj.keys().collect::<Vec<_>>()
    );
    assert!(node["kind"].is_string(), "{ctx}: kind is a string");
    assert!(
        node["name"].is_string() || node["name"].is_null(),
        "{ctx}: name is string|null"
    );
    let span = &node["span"];
    for field in ["start", "end", "line", "col"] {
        assert!(
            span[field].is_u64(),
            "{ctx}: span.{field} must be a non-negative integer, span = {span}"
        );
    }
    assert!(
        span["line"].as_u64().unwrap() >= 1 && span["col"].as_u64().unwrap() >= 1,
        "{ctx}: line/col are 1-based"
    );
    assert!(
        span["start"].as_u64().unwrap() <= span["end"].as_u64().unwrap(),
        "{ctx}: start <= end"
    );
    assert!(node["children"].is_array(), "{ctx}: children is an array");
}

const ADD_FN: &str = "fn add(a: Int, b: Int) -> Int { a + b }\n";

// ─── Happy path ─────────────────────────────────────────────────────────────

#[test]
fn air_valid_file_exits_0_and_prints_indented_tree() {
    let f = write_temp_file(ADD_FN);
    let output = bock_bin()
        .arg("inspect")
        .arg("air")
        .arg(f.path())
        .output()
        .unwrap();
    assert_exit_code(&output, 0, "human-mode air dump");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Root + a named declaration + a leaf expression, with the
    // `kind [name] @line:col (start..end)` line shape.
    assert!(stdout.starts_with("Module"), "root line: {stdout}");
    assert!(stdout.contains("FnDecl add @"), "fn line: {stdout}");
    assert!(stdout.contains("BinaryOp"), "body op: {stdout}");
    assert!(stdout.contains("@1:1 (0.."), "root span: {stdout}");
    // Children are indented under the root.
    assert!(
        stdout.lines().any(|l| l.starts_with("  FnDecl")),
        "FnDecl indented one level under Module: {stdout}"
    );
}

#[test]
fn air_json_valid_file_emits_contract_tree() {
    let f = write_temp_file(ADD_FN);
    let output = bock_bin()
        .arg("inspect")
        .arg("air")
        .arg(f.path())
        .arg("--json")
        .output()
        .unwrap();
    assert_exit_code(&output, 0, "json-mode air dump");

    let root: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("stdout must be valid JSON");
    assert!(root.get("error").is_none(), "no error key on success");
    assert_eq!(root["kind"], "Module");
    assert!(
        root["name"].is_null(),
        "file without a `module` declaration has a null module name"
    );
    assert_node_contract(&root, "root");

    // The fn declaration is a direct child of the module, named.
    let fn_decl = find_node(&root, "FnDecl", Some("add")).expect("FnDecl add in tree");
    assert_node_contract(fn_decl, "FnDecl");
    assert_eq!(fn_decl["span"]["start"], 0);
    assert_eq!(fn_decl["span"]["line"], 1);
    assert_eq!(fn_decl["span"]["col"], 1);

    // Structure reaches expression depth: params, body, and the `a + b`.
    assert!(find_node(&root, "Param", None).is_some(), "params present");
    assert!(find_node(&root, "Block", None).is_some(), "body present");
    let binop = find_node(&root, "BinaryOp", None).expect("a + b lowered");
    assert_node_contract(binop, "BinaryOp");
    assert!(
        find_node(binop, "Identifier", Some("a")).is_some(),
        "left operand"
    );
}

#[test]
fn air_json_named_module_and_core_import_lower() {
    // A declared module with a `use core.*` import: the embedded stdlib must
    // be in the registry (as in `bock check`) for this to resolve.
    let f = write_temp_file(
        "module Demo\n\nuse core.compare.{Ordering}\n\nfn pick(x: Int) -> Int { x }\n",
    );
    let output = bock_bin()
        .arg("inspect")
        .arg("air")
        .arg(f.path())
        .arg("--json")
        .output()
        .unwrap();
    assert_exit_code(&output, 0, "core import lowers");
    let root: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(root["kind"], "Module");
    assert_eq!(root["name"], "Demo", "module path is the root's name");
    assert!(
        find_node(&root, "ImportDecl", Some("core.compare")).is_some(),
        "import decl appears with its dotted path"
    );
    assert!(find_node(&root, "FnDecl", Some("pick")).is_some());
}

// ─── Failure modes ──────────────────────────────────────────────────────────

#[test]
fn air_parse_error_exits_1_and_renders_diagnostics() {
    let f = write_temp_file("fn { broken\n");
    let output = bock_bin()
        .arg("inspect")
        .arg("air")
        .arg(f.path())
        .output()
        .unwrap();
    assert_exit_code(&output, 1, "human-mode parse error");
    assert!(
        output.stdout.is_empty(),
        "no tree on stdout for a failed parse: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.to_lowercase().contains("error"),
        "diagnostics on stderr: {stderr}"
    );
}

#[test]
fn air_json_parse_error_emits_error_object_not_half_a_tree() {
    let f = write_temp_file("fn { broken\n");
    let output = bock_bin()
        .arg("inspect")
        .arg("air")
        .arg(f.path())
        .arg("--json")
        .output()
        .unwrap();
    assert_exit_code(&output, 1, "json-mode parse error");

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("error output must still be valid JSON");
    assert!(json.get("kind").is_none(), "no tree fields on failure");
    let error = &json["error"];
    assert!(error.is_object(), "top-level error object: {json}");
    assert!(error["message"].is_string());

    let diags = error["diagnostics"].as_array().expect("diagnostics array");
    assert!(!diags.is_empty(), "at least one diagnostic");
    for (i, d) in diags.iter().enumerate() {
        assert_eq!(d["severity"], "error", "diag {i}: severity");
        assert!(d["code"].is_string(), "diag {i}: code");
        assert!(d["message"].is_string(), "diag {i}: message");
        for field in ["start", "end", "line", "col"] {
            assert!(d["span"][field].is_u64(), "diag {i}: span.{field}");
        }
    }
}

#[test]
fn air_missing_file_exits_1() {
    let output = bock_bin()
        .arg("inspect")
        .arg("air")
        .arg("/tmp/nonexistent_bock_inspect_air_12345.bock")
        .output()
        .unwrap();
    assert_exit_code(&output, 1, "missing input file");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("nonexistent_bock_inspect_air_12345.bock"),
        "stderr names the file: {stderr}"
    );
}

#[test]
fn air_json_missing_file_emits_error_object() {
    let output = bock_bin()
        .arg("inspect")
        .arg("air")
        .arg("/tmp/nonexistent_bock_inspect_air_12345.bock")
        .arg("--json")
        .output()
        .unwrap();
    assert_exit_code(&output, 1, "missing input file (json)");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(json["error"]["message"]
        .as_str()
        .unwrap()
        .contains("could not read"));
    assert!(
        json["error"]["diagnostics"].as_array().unwrap().is_empty(),
        "I/O failures carry no compiler diagnostics"
    );
}

#[test]
fn air_unresolved_name_exits_1_with_resolution_diagnostics() {
    // Lowering requires clean name resolution; an undefined identifier is a
    // frontend error and must not produce half a tree.
    let f = write_temp_file("fn f() -> Int { totally_undefined_name }\n");
    let output = bock_bin()
        .arg("inspect")
        .arg("air")
        .arg(f.path())
        .arg("--json")
        .output()
        .unwrap();
    assert_exit_code(&output, 1, "unresolved name (json)");
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("name resolution"),
        "stage message names resolution: {json}"
    );
}
