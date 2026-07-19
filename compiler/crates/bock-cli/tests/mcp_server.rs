//! Integration tests for `bock mcp` — the MCP server over stdio.
//!
//! Spawns the built binary and speaks real newline-delimited JSON-RPC 2.0
//! over its stdin/stdout, pinning the protocol surface end-to-end:
//!
//! * the `initialize` handshake (protocol version, capabilities,
//!   serverInfo) and the `notifications/initialized` notification;
//! * `tools/list` — the seven v1 tools, each with a JSON Schema;
//! * `tools/call` for `bock_check` (clean and failing — diagnostics come
//!   back structured through the CLI's `--format json` contract),
//!   `bock_run`, `bock_explain`, and `bock_conformance` (the
//!   interpreter-only degenerate case plus the skip-reporting shape — no
//!   target toolchain is assumed present);
//! * protocol resilience: unknown methods, unknown tools, malformed
//!   arguments, malformed frames — none crash the loop;
//! * `resources/list` (empty in v1) and `resources/read` (proper error);
//! * EOF on stdin exits 0 cleanly.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// A test client speaking newline-delimited JSON-RPC to a `bock mcp` child.
struct McpClient {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
}

impl McpClient {
    /// Spawn `bock mcp` with piped stdio.
    fn spawn() -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_bock"))
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn bock mcp");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = BufReader::new(child.stdout.take().expect("child stdout"));
        Self {
            child,
            stdin: Some(stdin),
            stdout,
            next_id: 0,
        }
    }

    /// Send one raw line (no trailing newline needed).
    fn send_raw(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("stdin still open");
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.write_all(b"\n"))
            .and_then(|()| stdin.flush())
            .expect("write to server stdin");
    }

    /// Read one response line and parse it.
    fn read_message(&mut self) -> Value {
        let mut line = String::new();
        let n = self
            .stdout
            .read_line(&mut line)
            .expect("read from server stdout");
        assert!(n > 0, "server closed stdout unexpectedly");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("bad frame `{line}`: {e}"))
    }

    /// Send a request and read its response, asserting id round-trip.
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send_raw(
            &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string(),
        );
        let response = self.read_message();
        assert_eq!(response["jsonrpc"], "2.0", "{response}");
        assert_eq!(response["id"], id, "id must round-trip: {response}");
        response
    }

    /// Send a notification (no response expected).
    fn notify(&mut self, method: &str) {
        self.send_raw(&json!({ "jsonrpc": "2.0", "method": method }).to_string());
    }

    /// Run the standard `initialize` handshake, returning its result.
    fn initialize(&mut self) -> Value {
        let response = self.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "mcp-server-test", "version": "0" },
            }),
        );
        self.notify("notifications/initialized");
        response["result"].clone()
    }

    /// Call one tool, returning the `tools/call` result object.
    fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        let response = self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        assert!(
            response.get("error").is_none(),
            "tools/call must not be a protocol error: {response}"
        );
        response["result"].clone()
    }

    /// Close stdin (EOF) and wait for the child to exit, with a bound.
    fn shutdown(mut self) -> std::process::ExitStatus {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            match self.child.try_wait().expect("try_wait") {
                Some(status) => return status,
                None => {
                    assert!(
                        Instant::now() < deadline,
                        "server did not exit within 30s of stdin EOF"
                    );
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The text content of a tool result, parsed as a JSON document.
fn tool_document(result: &Value) -> Value {
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("tool result must carry text content: {result}"));
    serde_json::from_str(text)
        .unwrap_or_else(|e| panic!("tool text content must be a JSON document: {e}\n{text}"))
}

/// Write a temp `.bock` file, namespaced per the session conventions.
fn write_temp_bock(name: &str, content: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "{}-mcp-server-tests-{}",
        std::env::var("BOCK_TEST_NAMESPACE").unwrap_or_else(|_| "bock".into()),
        std::process::id(),
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join(name);
    std::fs::write(&path, content).expect("write temp bock file");
    path
}

// ── Handshake + surface ─────────────────────────────────────────────────────

#[test]
fn initialize_handshake_reports_server_and_capabilities() {
    let mut client = McpClient::spawn();
    let result = client.initialize();
    assert_eq!(result["protocolVersion"], "2025-06-18");
    assert_eq!(result["serverInfo"]["name"], "bock");
    assert!(
        result["serverInfo"]["version"].as_str().is_some(),
        "{result}"
    );
    assert!(result["capabilities"]["tools"].is_object());
    assert!(result["capabilities"]["resources"].is_object());

    // The notification produced no response: the next request round-trips.
    let ping = client.request("ping", json!({}));
    assert!(ping["result"].is_object(), "{ping}");
    assert_eq!(client.shutdown().code(), Some(0), "EOF exits 0");
}

#[test]
fn tools_list_serves_the_seven_tools_with_schemas() {
    let mut client = McpClient::spawn();
    client.initialize();
    let response = client.request("tools/list", json!({}));
    let tools = response["result"]["tools"].as_array().expect("tools array");
    let names: Vec<&str> = tools
        .iter()
        .map(|t| t["name"].as_str().expect("name"))
        .collect();
    assert_eq!(
        names,
        [
            "bock_check",
            "bock_run",
            "bock_test",
            "bock_build",
            "bock_conformance",
            "bock_inspect",
            "bock_explain",
        ]
    );
    for tool in tools {
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
        assert!(tool["description"].as_str().expect("description").len() > 40);
    }
}

// ── bock_check ──────────────────────────────────────────────────────────────

#[test]
fn bock_check_clean_file_returns_the_check_document() {
    let path = write_temp_bock(
        "check_clean.bock",
        "fn add(a: Int, b: Int) -> Int { a + b }\n",
    );
    let mut client = McpClient::spawn();
    client.initialize();
    let result = client.call_tool(
        "bock_check",
        json!({ "files": [path.display().to_string()] }),
    );
    assert_eq!(result["isError"], false, "{result}");
    let doc = tool_document(&result);
    assert_eq!(doc["format_version"], 1);
    assert_eq!(doc["command"], "check");
    assert_eq!(doc["outcome"], "clean");
    assert_eq!(doc["summary"]["errors"], 0);
    assert!(doc["diagnostics"].as_array().expect("array").is_empty());
}

#[test]
fn bock_check_failing_file_returns_structured_diagnostics_with_is_error() {
    let path = write_temp_bock("check_bad.bock", "fn main() -> Void {\n  bad\n}\n");
    let mut client = McpClient::spawn();
    client.initialize();
    let result = client.call_tool(
        "bock_check",
        json!({ "files": [path.display().to_string()] }),
    );
    assert_eq!(result["isError"], true, "{result}");
    let doc = tool_document(&result);
    assert_eq!(doc["command"], "check");
    assert_eq!(doc["outcome"], "failed");
    let diagnostics = doc["diagnostics"].as_array().expect("diagnostics");
    assert!(!diagnostics.is_empty());
    let first = &diagnostics[0];
    assert_eq!(first["severity"], "error");
    assert!(first["code"].as_str().expect("code").starts_with('E'));
    assert!(first["message"].as_str().is_some());
    assert!(first["span"]["line"].as_u64().is_some());
    assert!(
        first["span"]["file"]
            .as_str()
            .expect("file")
            .ends_with("check_bad.bock"),
        "{first}"
    );
}

// ── bock_run ────────────────────────────────────────────────────────────────

#[test]
fn bock_run_returns_the_run_envelope_with_program_stdout() {
    let path = write_temp_bock(
        "run_hello.bock",
        "fn main() -> Void {\n  println(\"hello from mcp\")\n}\n",
    );
    let mut client = McpClient::spawn();
    client.initialize();
    let result = client.call_tool("bock_run", json!({ "file": path.display().to_string() }));
    assert_eq!(result["isError"], false, "{result}");
    let doc = tool_document(&result);
    assert_eq!(doc["format_version"], 1);
    assert_eq!(doc["command"], "run");
    assert_eq!(doc["outcome"], "clean");
    assert_eq!(doc["exit_code"], 0);
    assert_eq!(doc["summary"]["exit_code"], 0);
    assert!(
        doc["stdout"]
            .as_str()
            .expect("stdout")
            .contains("hello from mcp"),
        "{doc}"
    );
}

// ── bock_explain ────────────────────────────────────────────────────────────

#[test]
fn bock_explain_serves_the_catalog_and_flags_unknown_codes() {
    let mut client = McpClient::spawn();
    client.initialize();

    let result = client.call_tool("bock_explain", json!({ "code": "E1002" }));
    assert_eq!(result["isError"], false, "{result}");
    let doc = tool_document(&result);
    assert_eq!(doc["command"], "explain");
    assert_eq!(doc["outcome"], "clean");
    let entry = &doc["explanations"][0];
    assert_eq!(entry["code"], "E1002");
    assert_eq!(entry["severity"], "error");
    assert!(entry["summary"].as_str().expect("summary").len() > 5);
    assert!(entry["description"].as_str().is_some());
    assert!(entry["spec_refs"].is_array());

    let unknown = client.call_tool("bock_explain", json!({ "code": "E9999" }));
    assert_eq!(unknown["isError"], true, "{unknown}");
    let doc = tool_document(&unknown);
    assert_eq!(doc["outcome"], "usage-error");
    assert!(doc["error"]["message"]
        .as_str()
        .expect("message")
        .contains("E9999"));
}

// ── bock_conformance ────────────────────────────────────────────────────────

#[test]
fn bock_conformance_interpreter_only_degenerate_case_is_clean() {
    // targets: [] runs the reference interpreter alone — the degenerate case
    // that must work with NO target toolchain present (CI-safe).
    let path = write_temp_bock(
        "conf_hello.bock",
        "fn main() -> Void {\n  println(\"conformance baseline\")\n}\n",
    );
    let mut client = McpClient::spawn();
    client.initialize();
    let result = client.call_tool(
        "bock_conformance",
        json!({ "file": path.display().to_string(), "targets": [] }),
    );
    assert_eq!(result["isError"], false, "{result}");
    let doc = tool_document(&result);
    assert_eq!(doc["format_version"], 1);
    assert_eq!(doc["command"], "conformance");
    assert_eq!(doc["outcome"], "clean");
    assert_eq!(doc["summary"]["targets_exercised"], 0);
    assert_eq!(doc["summary"]["targets_skipped"], 0);
    assert_eq!(doc["summary"]["matched"], 0);
    assert_eq!(doc["summary"]["mismatched"], 0);
    assert_eq!(doc["reference"]["runner"], "interpreter");
    assert_eq!(doc["reference"]["exit_code"], 0);
    assert!(doc["reference"]["stdout"]
        .as_str()
        .expect("stdout")
        .contains("conformance baseline"));
    assert!(doc["targets"].as_array().expect("targets").is_empty());
}

#[test]
fn bock_conformance_reports_each_requested_target_never_silently() {
    // Whatever toolchains this host has, every requested target must appear
    // exactly once with a valid status — and an absent toolchain must be a
    // reported skip carrying a detail, never a silent pass. No toolchain is
    // assumed present: every status is legal, only silence is not.
    let path = write_temp_bock(
        "conf_report.bock",
        "fn main() -> Void {\n  println(\"target report\")\n}\n",
    );
    let mut client = McpClient::spawn();
    client.initialize();
    let result = client.call_tool(
        "bock_conformance",
        json!({
            "file": path.display().to_string(),
            "targets": ["python", "go"],
            "timeout_seconds": 300,
        }),
    );
    let doc = tool_document(&result);
    assert_eq!(doc["command"], "conformance");
    let targets = doc["targets"].as_array().expect("targets");
    let reported: Vec<&str> = targets
        .iter()
        .map(|t| t["target"].as_str().expect("target"))
        .collect();
    assert_eq!(reported, ["python", "go"], "stable order, one entry each");

    let mut skipped = 0u64;
    let mut exercised = 0u64;
    for entry in targets {
        let status = entry["status"].as_str().expect("status");
        assert!(
            [
                "matched",
                "mismatched",
                "skipped",
                "build-failed",
                "run-failed"
            ]
            .contains(&status),
            "unexpected status: {entry}"
        );
        if status == "skipped" {
            skipped += 1;
            assert!(
                entry["detail"]
                    .as_str()
                    .expect("skips carry a detail")
                    .len()
                    > 5,
                "a skip must say why: {entry}"
            );
        } else {
            exercised += 1;
        }
    }
    assert_eq!(doc["summary"]["targets_skipped"], skipped, "{doc}");
    assert_eq!(doc["summary"]["targets_exercised"], exercised, "{doc}");
}

// ── Protocol errors + resilience ────────────────────────────────────────────

#[test]
fn unknown_method_unknown_tool_and_bad_args_are_protocol_errors() {
    let mut client = McpClient::spawn();
    client.initialize();

    let response = client.request("definitely/not-a-method", json!({}));
    assert_eq!(response["error"]["code"], -32601, "{response}");

    let response = client.request(
        "tools/call",
        json!({ "name": "bock_bogus", "arguments": {} }),
    );
    assert_eq!(response["error"]["code"], -32602, "{response}");

    // Missing required argument (`files`) is invalid params, not a tool error.
    let response = client.request(
        "tools/call",
        json!({ "name": "bock_check", "arguments": {} }),
    );
    assert_eq!(response["error"]["code"], -32602, "{response}");

    // Wrongly-typed argument likewise.
    let response = client.request(
        "tools/call",
        json!({ "name": "bock_explain", "arguments": { "code": 42 } }),
    );
    assert_eq!(response["error"]["code"], -32602, "{response}");
}

#[test]
fn malformed_frames_do_not_crash_the_loop() {
    let mut client = McpClient::spawn();
    client.initialize();

    client.send_raw("this is not json at all {{{");
    let response = client.read_message();
    assert_eq!(response["error"]["code"], -32700, "{response}");
    assert!(response["id"].is_null(), "{response}");

    // Valid JSON, but not an object.
    client.send_raw("[1,2,3]");
    let response = client.read_message();
    assert_eq!(response["error"]["code"], -32600, "{response}");

    // The loop is still alive and correct afterwards.
    let ping = client.request("ping", json!({}));
    assert!(ping["result"].is_object(), "{ping}");
    assert_eq!(client.shutdown().code(), Some(0));
}

#[test]
fn resources_list_is_empty_and_read_errors_properly() {
    let mut client = McpClient::spawn();
    client.initialize();

    let response = client.request("resources/list", json!({}));
    assert_eq!(response["result"]["resources"], json!([]), "{response}");

    let response = client.request("resources/read", json!({ "uri": "bock://pack/nothing" }));
    assert_eq!(response["error"]["code"], -32002, "{response}");
    assert!(
        response["error"]["message"]
            .as_str()
            .expect("message")
            .contains("bock://pack/nothing"),
        "{response}"
    );
}

#[test]
fn eof_on_stdin_exits_zero() {
    let mut client = McpClient::spawn();
    client.initialize();
    assert_eq!(client.shutdown().code(), Some(0));
}
