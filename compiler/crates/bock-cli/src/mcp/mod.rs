//! The `bock mcp` subcommand: the compiler surface as MCP tools.
//!
//! A hand-rolled, deliberately thin [Model Context Protocol] server over
//! stdio. It implements exactly the JSON-RPC 2.0 subset the served surface
//! needs — `initialize`, `ping`, `tools/list`, `tools/call`,
//! `resources/list`, `resources/read` — on the existing `serde_json`, with
//! **no third-party protocol dependency** (operator decision 2026-07-03: we
//! own protocol tracking; a post-MCP protocol shift must cost a transport
//! shim, not a rewrite).
//!
//! # Framing and channel discipline
//!
//! Messages are newline-delimited JSON (the current MCP stdio framing): one
//! JSON-RPC message per line on stdin, one per line on stdout. **stdout
//! carries only protocol messages**; anything else (logging) goes to stderr.
//! This is why every tool that wraps a CLI command spawns the `bock` binary
//! as a subprocess rather than calling the command function in-process: the
//! wrapped commands (and the programs `bock run` executes) write freely to
//! their own stdout/stderr, and subprocess isolation is what keeps those
//! bytes out of the protocol channel. It is also, by construction, *exactly*
//! the code path the CLI's `--format json` mode exercises — the server layer
//! never re-renders or re-parses human output (see [`tools`]).
//!
//! # Error mapping
//!
//! Protocol-level failures are JSON-RPC errors: `-32700` parse error,
//! `-32600` invalid request, `-32601` unknown method, `-32602` invalid
//! params (including unknown tool names and malformed tool arguments), and
//! `-32002` (the MCP-assigned code) for an unknown resource URI.
//!
//! # Resources
//!
//! `resources/list` and `resources/read` serve the AI context pack, the
//! language specification, and the stdlib reference as readable documents,
//! compiled into the binary from generated in-tree assets. See [`resources`]
//! for the tiers, the URI scheme, and the `bock_explain` → spec bridge.
//! Tool-level failures — the wrapped command failing, a subprocess that
//! cannot be spawned — are **successful** `tools/call` responses with
//! `isError: true`, carrying the structured document (or a plain-text
//! explanation when no document exists). Malformed frames and unknown
//! notifications never crash the loop; EOF on stdin exits cleanly.
//!
//! [Model Context Protocol]: https://modelcontextprotocol.io/

mod conformance;
mod resources;
mod tools;

use std::io::{BufRead, Write};

use serde_json::{json, Value};

/// The MCP protocol revision this server speaks by default.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// Protocol revisions the server accepts from a client's `initialize`. The
/// subset served here is identical across these revisions, so the server
/// echoes any of them back; anything else is answered with
/// [`PROTOCOL_VERSION`] (the client then decides whether to proceed).
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];

/// JSON-RPC 2.0 error codes used by this server.
mod rpc_error {
    /// Invalid JSON was received.
    pub const PARSE_ERROR: i64 = -32700;
    /// The JSON sent is not a valid request object.
    pub const INVALID_REQUEST: i64 = -32600;
    /// The method does not exist.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Invalid method parameters (also: unknown tool, malformed tool args).
    pub const INVALID_PARAMS: i64 = -32602;
    /// MCP-assigned: the requested resource URI does not exist.
    pub const RESOURCE_NOT_FOUND: i64 = -32002;
}

/// Run the MCP server over stdio until EOF.
///
/// Blocks reading newline-delimited JSON-RPC messages from stdin and writing
/// responses to stdout. Returns `Ok(())` (a zero exit) when stdin closes.
pub fn run() -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    serve(stdin.lock(), stdout.lock())
}

/// The server loop, generic over the transport for testability.
fn serve(reader: impl BufRead, mut writer: impl Write) -> anyhow::Result<()> {
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            // A read error on stdin (e.g. the pipe vanished) is treated as
            // end-of-stream: exit the loop cleanly rather than crash.
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(&line) {
            Err(_) => {
                // Malformed frame: report it (id is unknowable → null) and
                // keep serving.
                write_message(
                    &mut writer,
                    &error_response(Value::Null, rpc_error::PARSE_ERROR, "Parse error"),
                )?;
            }
            Ok(message) => {
                if let Some(response) = handle_message(&message) {
                    write_message(&mut writer, &response)?;
                }
            }
        }
    }
    Ok(())
}

/// Serialize one protocol message as a single line on the transport.
fn write_message(writer: &mut impl Write, message: &Value) -> anyhow::Result<()> {
    // Compact serialization: the framing is one message per line, so the
    // message itself must not contain raw newlines (serde_json escapes any
    // in string values).
    let serialized = serde_json::to_string(message)?;
    writer.write_all(serialized.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

/// Handle one decoded JSON-RPC message. Returns the response to send, or
/// `None` for notifications (and other messages that must not be answered).
fn handle_message(message: &Value) -> Option<Value> {
    let Some(obj) = message.as_object() else {
        // A frame that is valid JSON but not an object (e.g. `[]`, `42`)
        // is an invalid request; its id is unknowable.
        return Some(error_response(
            Value::Null,
            rpc_error::INVALID_REQUEST,
            "Invalid Request: expected a JSON-RPC message object",
        ));
    };

    let id = obj.get("id").cloned();
    let method = obj.get("method").and_then(Value::as_str);

    let Some(method) = method else {
        // No method: either a response to a request we never sent (ignore —
        // answering it could loop) or a malformed request.
        if obj.contains_key("result") || obj.contains_key("error") {
            return None;
        }
        return id.map(|id| {
            error_response(
                id,
                rpc_error::INVALID_REQUEST,
                "Invalid Request: missing method",
            )
        });
    };

    let Some(id) = id else {
        // A notification. `notifications/initialized` requires no action in
        // this server; unknown notifications are ignored without responding
        // (JSON-RPC 2.0 forbids replying to notifications).
        return None;
    };

    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    Some(handle_request(id, method, &params))
}

/// Dispatch one request to its method handler and wrap the outcome in a
/// JSON-RPC response envelope.
fn handle_request(id: Value, method: &str, params: &Value) -> Value {
    match method {
        "initialize" => success_response(id, initialize_result(params)),
        "ping" => success_response(id, json!({})),
        "tools/list" => success_response(id, json!({ "tools": tools::tool_list() })),
        "tools/call" => match handle_tools_call(params) {
            Ok(result) => success_response(id, result),
            Err(message) => error_response(id, rpc_error::INVALID_PARAMS, &message),
        },
        "resources/list" => {
            success_response(id, json!({ "resources": resources::resource_list() }))
        }
        "resources/read" => {
            let uri = params
                .get("uri")
                .and_then(Value::as_str)
                .unwrap_or("<missing uri>");
            match resources::read_resource(uri) {
                Some(result) => success_response(id, result),
                None => error_response(
                    id,
                    rpc_error::RESOURCE_NOT_FOUND,
                    &format!(
                        "Resource not found: {uri} — call resources/list, or start from \
                         bock://index for an orientation across the pack, spec, and stdlib tiers"
                    ),
                ),
            }
        }
        other => error_response(
            id,
            rpc_error::METHOD_NOT_FOUND,
            &format!("Method not found: {other}"),
        ),
    }
}

/// Build the `initialize` result: protocol version, capabilities, serverInfo.
fn initialize_result(params: &Value) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    let protocol_version = match requested {
        Some(v) if SUPPORTED_PROTOCOL_VERSIONS.contains(&v) => v,
        _ => PROTOCOL_VERSION,
    };
    json!({
        "protocolVersion": protocol_version,
        "capabilities": {
            "tools": {},
            "resources": {},
        },
        "serverInfo": {
            "name": "bock",
            "version": env!("CARGO_PKG_VERSION"),
        },
    })
}

/// Handle `tools/call`: validate the params envelope, dispatch to the named
/// tool, and wrap its outcome in the MCP tool-result shape.
///
/// The returned `Err` is a *protocol* failure (unknown tool, malformed
/// arguments) mapped to `-32602` by the caller. Tool-*execution* failures
/// come back as `Ok` results with `isError: true`.
fn handle_tools_call(params: &Value) -> Result<Value, String> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "tools/call params must carry a string `name`".to_string())?;
    let empty = json!({});
    let arguments = match params.get("arguments") {
        None | Some(Value::Null) => &empty,
        Some(v @ Value::Object(_)) => v,
        Some(_) => return Err("tools/call `arguments` must be an object".to_string()),
    };
    let outcome = tools::call_tool(name, arguments)?;
    Ok(json!({
        "content": [ { "type": "text", "text": outcome.text } ],
        "isError": outcome.is_error,
    }))
}

/// Build a JSON-RPC success response.
fn success_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Build a JSON-RPC error response.
fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_echoes_supported_version_and_defaults_otherwise() {
        let echoed = initialize_result(&json!({ "protocolVersion": "2024-11-05" }));
        assert_eq!(echoed["protocolVersion"], "2024-11-05");

        let defaulted = initialize_result(&json!({ "protocolVersion": "1830-01-01" }));
        assert_eq!(defaulted["protocolVersion"], PROTOCOL_VERSION);

        let missing = initialize_result(&Value::Null);
        assert_eq!(missing["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(missing["serverInfo"]["name"], "bock");
        assert!(missing["capabilities"]["tools"].is_object());
        assert!(missing["capabilities"]["resources"].is_object());
    }

    #[test]
    fn notifications_are_never_answered() {
        let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(handle_message(&note).is_none());
        let unknown = json!({ "jsonrpc": "2.0", "method": "notifications/whatever" });
        assert!(handle_message(&unknown).is_none());
    }

    #[test]
    fn responses_to_us_are_ignored() {
        let response = json!({ "jsonrpc": "2.0", "id": 7, "result": {} });
        assert!(handle_message(&response).is_none());
    }

    #[test]
    fn unknown_method_maps_to_method_not_found() {
        let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "bogus/method" });
        let resp = handle_message(&req).expect("requests are answered");
        assert_eq!(resp["error"]["code"], rpc_error::METHOD_NOT_FOUND);
        assert_eq!(resp["id"], 1);
    }

    #[test]
    fn non_object_frame_is_invalid_request() {
        let resp = handle_message(&json!([1, 2, 3])).expect("answered");
        assert_eq!(resp["error"]["code"], rpc_error::INVALID_REQUEST);
        assert!(resp["id"].is_null());
    }

    #[test]
    fn serve_reports_parse_error_and_keeps_going_until_eof() {
        let input = b"this is not json\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n";
        let mut output: Vec<u8> = Vec::new();
        serve(&input[..], &mut output).expect("serve returns cleanly at EOF");
        let lines: Vec<Value> = String::from_utf8(output)
            .expect("utf8")
            .lines()
            .map(|l| serde_json::from_str(l).expect("each output line is JSON"))
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["error"]["code"], rpc_error::PARSE_ERROR);
        assert!(lines[0]["id"].is_null());
        assert_eq!(lines[1]["id"], 2);
        assert!(lines[1]["result"].is_object());
    }

    #[test]
    fn resources_surface_lists_and_reads() {
        let list = json!({ "jsonrpc": "2.0", "id": 3, "method": "resources/list" });
        let resp = handle_message(&list).expect("answered");
        let listed = resp["result"]["resources"]
            .as_array()
            .expect("resources array");
        assert!(listed.len() >= 43, "listed only {}", listed.len());
        assert_eq!(listed[0]["uri"], "bock://index");

        let read = json!({
            "jsonrpc": "2.0", "id": 5, "method": "resources/read",
            "params": { "uri": "bock://index" },
        });
        let resp = handle_message(&read).expect("answered");
        assert_eq!(resp["result"]["contents"][0]["uri"], "bock://index");

        let read = json!({
            "jsonrpc": "2.0", "id": 4, "method": "resources/read",
            "params": { "uri": "bock://nope" },
        });
        let resp = handle_message(&read).expect("answered");
        assert_eq!(resp["error"]["code"], rpc_error::RESOURCE_NOT_FOUND);
    }
}
