//! The seven v1 MCP tools: definitions (name, description, JSON Schema) and
//! handlers.
//!
//! Every tool that wraps a CLI command spawns the running `bock` binary
//! (`std::env::current_exe()`) as a subprocess with the same argv the CLI
//! user would type, then returns the command's `--format json` document as
//! the tool's text content. That makes the tool surface *identical by
//! construction* to the CLI machine contract (`crate::output`,
//! `FORMAT_VERSION` 1) — the server never re-renders or parses human text —
//! and it keeps the wrapped command's stdout/stderr (including the stdout of
//! programs `bock_run` executes) out of the protocol channel on the server's
//! own stdout.
//!
//! Where a command has no json mode yet (`run`, `build`), the handler wraps
//! the captured process output in a minimal envelope that follows the same
//! `crate::output` conventions (`format_version` / `command` / `outcome` /
//! `summary` + payload fields); the shapes are documented in
//! `docs/src/reference/mcp.md` and change only additively.
//!
//! `isError` rule: a tool call reports `isError: true` exactly when the
//! wrapped command's outcome is not clean — a non-zero exit (diagnostics
//! found, tests failed, program failed, build failed, conformance mismatch),
//! a timeout, or a subprocess that could not be spawned. The structured
//! document rides along in the content either way.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::output::{severity_name, usage_error_document, FORMAT_VERSION};

use super::conformance;

/// Default wall-clock bound, in seconds, for tools that execute user code
/// (`bock_run`, `bock_test`, `bock_build`, and each `bock_conformance`
/// build/run step). Overridable per call via `timeout_seconds`.
pub(super) const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// The result of one tool invocation: the text content to return and
/// whether it is reported as a tool-level error (`isError`).
#[derive(Debug)]
pub struct ToolOutcome {
    /// The tool's text content — normally one pretty-printed JSON document.
    pub text: String,
    /// The MCP `isError` flag (see the module docs for the mapping rule).
    pub is_error: bool,
}

/// Shared wording for the tools that execute user code. Agent frameworks
/// surface tool descriptions for permissioning, so this states the trust
/// envelope plainly and identically on each such tool.
const EXECUTION_SAFETY: &str = "SAFETY: this tool executes user code and may \
    read and write the workspace and spawn processes — the same trust \
    envelope as running the bock CLI yourself. Do not point it at untrusted \
    code you would not run locally.";

/// The seven tool definitions, in the order `tools/list` reports them.
///
/// Each entry is the MCP tool shape: `name`, `description`, `inputSchema`
/// (JSON Schema for the `tools/call` arguments object). These definitions
/// are the canonical, agent-facing API contract.
pub fn tool_list() -> Vec<Value> {
    vec![
        json!({
            "name": "bock_check",
            "description": "Type-check and lint Bock source files without building (wraps `bock check --format json`). Returns one JSON document: format_version, command:\"check\", outcome (\"clean\"|\"failed\"|\"usage-error\"), summary {files, errors, warnings}, and a `diagnostics` array whose entries carry severity, code (e.g. \"E4002\"; null for I/O-class failures), message, span {file, start, end, line, col}, and suggestion. isError is true when the check found errors (non-zero CLI exit). Read-only: analyses the given files, executes nothing.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "files": {
                        "type": "array",
                        "items": { "type": "string" },
                        "minItems": 1,
                        "description": "Paths to the .bock files to check (absolute paths recommended; relative paths resolve against the server process's working directory). Multi-file programs: pass every file so cross-file `use` imports resolve."
                    },
                    "strict": {
                        "type": "boolean",
                        "default": false,
                        "description": "Force production strictness: completeness gaps that are warnings at the default development strictness (e.g. a public item missing @context) become errors."
                    },
                    "only": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["types", "context"] },
                        "description": "Restrict the check to specific aspects (spec §20.1.1). Omit to run the full check."
                    }
                },
                "required": ["files"]
            }
        }),
        json!({
            "name": "bock_run",
            "description": format!("Execute a Bock program with the reference interpreter (wraps `bock run`). Returns one JSON document: format_version, command:\"run\", outcome (\"clean\"|\"failed\"|\"timeout\"), summary {{exit_code}}, exit_code (null if killed by timeout or signal), stdout, stderr. Compile errors appear as rendered text in `stderr`; for structured diagnostics run bock_check first. isError is true when the program did not exit 0. {EXECUTION_SAFETY}"),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": {
                        "type": "string",
                        "description": "Path to the entry .bock file (absolute recommended). Files in the same bock.project are compiled alongside it, so cross-file imports resolve."
                    },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Arguments passed to the Bock program (the CLI's `-- args...`)."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Working directory for the program (defaults to the server process's working directory)."
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "minimum": 1,
                        "default": 120,
                        "description": "Wall-clock bound; on expiry the process is killed and outcome is \"timeout\"."
                    }
                },
                "required": ["file"]
            }
        }),
        json!({
            "name": "bock_test",
            "description": format!("Run @test functions with the reference interpreter (wraps `bock test --format json`). Returns one JSON document: format_version, command:\"test\", outcome (\"clean\"|\"failed\"), summary {{tests, passed, failed}}, a `tests` array ({{name, file, passed, message, duration_ms}}), and a top-level `diagnostics` array carrying structured compile-error diagnostics (same entry shape as bock_check) when a test file failed to compile. isError is true when any test failed. On timeout the content is a plain-text report instead of a JSON document. {EXECUTION_SAFETY}"),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "files": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Paths to .bock files to test. If omitted, discovers all .bock files recursively under `cwd`."
                    },
                    "filter": {
                        "type": "string",
                        "description": "Only run tests whose name contains this substring."
                    },
                    "cwd": {
                        "type": "string",
                        "description": "Working directory for discovery and relative paths (defaults to the server process's working directory)."
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "minimum": 1,
                        "default": 120,
                        "description": "Wall-clock bound for the whole test run."
                    }
                }
            }
        }),
        json!({
            "name": "bock_build",
            "description": format!("Build a Bock project for one target language (wraps `bock build --target <T>` in the project directory). Returns one JSON document: format_version, command:\"build\", outcome (\"clean\"|\"failed\"|\"timeout\"), summary {{target, exit_code, files}}, exit_code, stdout, stderr, output_dir (\"build/<target>\", null on failure), and `files` — the emitted output tree as sorted paths relative to output_dir (toolchain artifact dirs like target/ and node_modules/ excluded). Build errors appear as rendered text in stdout/stderr; for structured diagnostics run bock_check first. isError is true when the build failed. Project mode also invokes the target toolchain to validate output, and writes build/ and .bock/ in the project. {EXECUTION_SAFETY}"),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "The project directory to build in (where bock.project lives; the build discovers every .bock file under it)."
                    },
                    "target": {
                        "type": "string",
                        "enum": ["js", "ts", "python", "rust", "go"],
                        "description": "Target language to build for."
                    },
                    "source_only": {
                        "type": "boolean",
                        "default": false,
                        "description": "Emit transpiled source only, without scaffolding a runnable project or invoking the target toolchain."
                    },
                    "strict": {
                        "type": "boolean",
                        "default": false,
                        "description": "Force production strictness (fails on unpinned build-scope decisions)."
                    },
                    "release": {
                        "type": "boolean",
                        "default": false,
                        "description": "Enable release optimizations (implies production strictness)."
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "minimum": 1,
                        "default": 120,
                        "description": "Wall-clock bound for the build (target-toolchain validation included)."
                    }
                },
                "required": ["project_dir", "target"]
            }
        }),
        json!({
            "name": "bock_conformance",
            "description": format!("Cross-target behavior comparison for ONE Bock file: does this program behave identically on every target? Runs the file on the reference interpreter, then for each requested target whose toolchain is installed locally, builds it (`bock build -t <T>`) in an isolated temp project and executes the output, byte-comparing stdout against the interpreter's (trailing newlines and \\r normalized). Returns one JSON document: format_version, command:\"conformance\", outcome (\"clean\"|\"failed\"), summary {{targets_exercised, targets_skipped, matched, mismatched}}, file, reference {{runner:\"interpreter\", exit_code, stdout, stderr}}, and a `targets` array with one entry per requested target: {{target, status (\"matched\"|\"mismatched\"|\"skipped\"|\"build-failed\"|\"run-failed\"), exit_code, stdout, detail}}. A missing toolchain is a reported skip (status \"skipped\" with the install hint in detail), never a silent pass — check summary.targets_exercised before concluding conformance. COST: builds and runs the program once per available target; expect seconds-to-minutes with compiled targets (rust, go). Note: timeout_seconds bounds the reference run and each per-target build, but not the target-toolchain execution step in v1. {EXECUTION_SAFETY}"),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "file": {
                        "type": "string",
                        "description": "Path to the single .bock file to compare across targets (absolute recommended). The file is copied into an isolated temp project per target, so sibling files are NOT included: the program must be self-contained (embedded core.* stdlib imports are fine)."
                    },
                    "targets": {
                        "type": "array",
                        "items": { "type": "string", "enum": ["js", "ts", "python", "rust", "go"] },
                        "description": "Targets to compare (defaults to all five). An empty array runs the reference interpreter only."
                    },
                    "timeout_seconds": {
                        "type": "integer",
                        "minimum": 1,
                        "default": 120,
                        "description": "Wall-clock bound applied to the reference run and to each per-target build step."
                    }
                },
                "required": ["file"]
            }
        }),
        json!({
            "name": "bock_inspect",
            "description": "List recorded AI decisions for a Bock project (wraps `bock inspect --format json`; read-only). Returns one JSON document: format_version, command:\"inspect\", outcome (\"clean\"), summary {decisions}, and a `decisions` array of decision-manifest entries: {scope (\"build\"|\"runtime\"), prefixed_id, decision {id, module_path, decision_type, choice, alternatives, reasoning, pinned, timestamp, ...}}. Useful for auditing what the AI decided during builds and what is pinned for deterministic replay.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "The project directory whose .bock/decisions manifests to read (defaults to the server process's working directory)."
                    },
                    "scope": {
                        "type": "string",
                        "enum": ["build", "runtime", "all"],
                        "default": "build",
                        "description": "Which decision manifest(s) to list."
                    },
                    "unpinned": {
                        "type": "boolean",
                        "default": false,
                        "description": "Only list decisions that are not yet pinned."
                    },
                    "module": {
                        "type": "string",
                        "description": "Filter by module path substring."
                    },
                    "type": {
                        "type": "string",
                        "description": "Filter by decision type (e.g. \"codegen\", \"repair\", \"adaptive_recovery\")."
                    }
                }
            }
        }),
        json!({
            "name": "bock_explain",
            "description": "Explain a Bock diagnostic code (e.g. \"E4002\", \"W1001\") from the compiled-in diagnostic catalog — the same registry that backs editor hovers. Returns one JSON document: format_version, command:\"explain\", outcome (\"clean\"|\"usage-error\"), summary {codes}, and an `explanations` array with one entry: {code, severity, summary, description, spec_refs (spec section references like \"§10\")}. An unknown code returns outcome \"usage-error\" with isError true. v1 serves catalog text only; richer fix-pattern context packs arrive with a later release (Q-mcp-pack-resources). Read-only, instant.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "code": {
                        "type": "string",
                        "pattern": "^[EWew][0-9]{4}$",
                        "description": "The diagnostic code to explain (E-codes are errors, W-codes warnings; case-insensitive)."
                    }
                },
                "required": ["code"]
            }
        }),
    ]
}

/// Dispatch one `tools/call` to the named tool.
///
/// `Err` is a protocol-level failure (unknown tool, malformed arguments)
/// that the caller maps to JSON-RPC `-32602`; execution failures come back
/// as `Ok` outcomes with `is_error: true`.
pub fn call_tool(name: &str, args: &Value) -> Result<ToolOutcome, String> {
    match name {
        "bock_check" => check(args),
        "bock_run" => run(args),
        "bock_test" => test(args),
        "bock_build" => build(args),
        "bock_conformance" => conformance::run(args),
        "bock_inspect" => inspect(args),
        "bock_explain" => explain(args),
        other => Err(format!("Unknown tool: {other}")),
    }
}

// ── Argument extraction helpers ─────────────────────────────────────────────

/// Required string argument, or a protocol-level error message.
fn req_str(args: &Value, key: &str) -> Result<String, String> {
    match args.get(key) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(format!("argument `{key}` must be a string")),
        None => Err(format!("missing required argument `{key}`")),
    }
}

/// Optional string argument.
fn opt_str(args: &Value, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("argument `{key}` must be a string")),
    }
}

/// Optional boolean argument (defaults to `false`).
fn opt_bool(args: &Value, key: &str) -> Result<bool, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(b)) => Ok(*b),
        Some(_) => Err(format!("argument `{key}` must be a boolean")),
    }
}

/// Optional array-of-strings argument.
fn opt_str_array(args: &Value, key: &str) -> Result<Option<Vec<String>>, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::String(s) => out.push(s.clone()),
                    _ => return Err(format!("argument `{key}` must be an array of strings")),
                }
            }
            Ok(Some(out))
        }
        Some(_) => Err(format!("argument `{key}` must be an array of strings")),
    }
}

/// The `timeout_seconds` argument as a [`Duration`], defaulting to
/// [`DEFAULT_TIMEOUT_SECS`].
pub(super) fn timeout_arg(args: &Value) -> Result<Duration, String> {
    match args.get("timeout_seconds") {
        None | Some(Value::Null) => Ok(Duration::from_secs(DEFAULT_TIMEOUT_SECS)),
        Some(v) => match v.as_u64() {
            Some(secs) if secs >= 1 => Ok(Duration::from_secs(secs)),
            _ => Err("argument `timeout_seconds` must be a positive integer".to_string()),
        },
    }
}

// ── Subprocess plumbing ─────────────────────────────────────────────────────

/// The captured output of one wrapped CLI invocation.
pub(super) struct Captured {
    /// The process exit code; `None` if it was killed (timeout or signal).
    pub exit_code: Option<i32>,
    /// Everything the process wrote to stdout.
    pub stdout: String,
    /// Everything the process wrote to stderr.
    pub stderr: String,
    /// Whether the wall-clock bound expired and the process was killed.
    pub timed_out: bool,
}

impl Captured {
    /// Whether the process exited 0 within the bound.
    pub fn success(&self) -> bool {
        !self.timed_out && self.exit_code == Some(0)
    }
}

/// Spawn the running `bock` binary with `argv`, capturing stdout/stderr.
///
/// `cwd` sets the child's working directory; `timeout` (when given) bounds
/// the wall clock, after which the child is killed and the capture is
/// returned with `timed_out: true`. The child's streams are drained on
/// separate threads so a chatty program cannot deadlock on a full pipe.
///
/// `Err` means the subprocess could not even be spawned (the caller reports
/// it as a tool-level failure).
pub(super) fn spawn_cli(
    argv: &[String],
    cwd: Option<&Path>,
    timeout: Option<Duration>,
) -> Result<Captured, String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("could not locate the running bock binary: {e}"))?;
    let mut command = Command::new(&exe);
    command
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("failed to spawn `bock {}`: {e}", argv.join(" ")))?;

    // Drain both pipes on their own threads (a child that fills a pipe we
    // are not reading would otherwise block forever).
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let drain = |pipe: Option<std::process::ChildStdout>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let drain_err = |pipe: Option<std::process::ChildStderr>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let stdout_thread = drain(stdout_pipe);
    let stderr_thread = drain_err(stderr_pipe);

    // Wait for exit, honoring the wall-clock bound with a poll loop (the
    // std library has no wait-with-timeout; hand-rolled per the
    // no-new-dependencies decision).
    let started = Instant::now();
    let mut timed_out = false;
    let exit_status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if let Some(bound) = timeout {
                    if started.elapsed() >= bound {
                        timed_out = true;
                        let _ = child.kill();
                        break child.wait().ok();
                    }
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                let _ = child.kill();
                return Err(format!("failed waiting on `bock {}`: {e}", argv.join(" ")));
            }
        }
    };

    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();
    Ok(Captured {
        exit_code: exit_status.and_then(|s| s.code()),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        timed_out,
    })
}

/// Wrap a wrapped-command run whose stdout is one `--format json` document:
/// pass the document through as the tool text, with `isError` mirroring the
/// exit code. Falls back to a plain-text report when no document came back
/// (timeout, or a subprocess crash before the single stdout write).
fn document_outcome(command_name: &str, captured: &Captured) -> ToolOutcome {
    if captured.timed_out {
        return ToolOutcome {
            text: format!(
                "`bock {command_name}` timed out and was killed before emitting its JSON \
                 document.\n--- captured stdout ---\n{}\n--- captured stderr ---\n{}",
                captured.stdout, captured.stderr
            ),
            is_error: true,
        };
    }
    match serde_json::from_str::<Value>(&captured.stdout) {
        Ok(doc) => ToolOutcome {
            text: pretty(&doc),
            is_error: !captured.success(),
        },
        Err(_) => ToolOutcome {
            text: format!(
                "`bock {command_name}` exited {:?} without emitting its JSON document.\n\
                 --- stdout ---\n{}\n--- stderr ---\n{}",
                captured.exit_code, captured.stdout, captured.stderr
            ),
            is_error: true,
        },
    }
}

/// Pretty-print a document for the tool text content.
pub(super) fn pretty(doc: &Value) -> String {
    serde_json::to_string_pretty(doc).unwrap_or_else(|_| doc.to_string())
}

// ── Tool handlers ───────────────────────────────────────────────────────────

/// `bock_check`: wraps `bock check --format json`.
fn check(args: &Value) -> Result<ToolOutcome, String> {
    let files = opt_str_array(args, "files")?
        .filter(|f| !f.is_empty())
        .ok_or_else(|| "missing required argument `files` (non-empty array)".to_string())?;
    let mut argv = vec!["check".to_string(), "--format".into(), "json".into()];
    if opt_bool(args, "strict")? {
        argv.push("--strict".into());
    }
    if let Some(only) = opt_str_array(args, "only")? {
        if !only.is_empty() {
            argv.push(format!("--only={}", only.join(",")));
        }
    }
    argv.extend(files);
    let captured = spawn_cli(&argv, None, None)?;
    Ok(document_outcome("check", &captured))
}

/// `bock_run`: wraps `bock run`, packaging stdout/stderr/exit in the
/// documented run envelope (the CLI has no json mode for `run` yet).
fn run(args: &Value) -> Result<ToolOutcome, String> {
    let file = req_str(args, "file")?;
    let cwd = opt_str(args, "cwd")?.map(PathBuf::from);
    let timeout = timeout_arg(args)?;
    let mut argv = vec!["run".to_string(), file];
    if let Some(program_args) = opt_str_array(args, "args")? {
        if !program_args.is_empty() {
            argv.push("--".into());
            argv.extend(program_args);
        }
    }
    let captured = spawn_cli(&argv, cwd.as_deref(), Some(timeout))?;
    let outcome = if captured.timed_out {
        "timeout"
    } else if captured.success() {
        "clean"
    } else {
        "failed"
    };
    let doc = json!({
        "format_version": FORMAT_VERSION,
        "command": "run",
        "outcome": outcome,
        "summary": { "exit_code": captured.exit_code },
        "exit_code": captured.exit_code,
        "stdout": captured.stdout,
        "stderr": captured.stderr,
    });
    Ok(ToolOutcome {
        text: pretty(&doc),
        is_error: outcome != "clean",
    })
}

/// `bock_test`: wraps `bock test --format json`.
fn test(args: &Value) -> Result<ToolOutcome, String> {
    let cwd = opt_str(args, "cwd")?.map(PathBuf::from);
    let timeout = timeout_arg(args)?;
    let mut argv = vec!["test".to_string(), "--format".into(), "json".into()];
    if let Some(filter) = opt_str(args, "filter")? {
        argv.push("--filter".into());
        argv.push(filter);
    }
    if let Some(files) = opt_str_array(args, "files")? {
        argv.extend(files);
    }
    let captured = spawn_cli(&argv, cwd.as_deref(), Some(timeout))?;
    Ok(document_outcome("test", &captured))
}

/// `bock_build`: wraps `bock build --target <T>` in the project directory,
/// packaging the captured output plus the emitted output-tree listing in the
/// documented build envelope (the CLI has no json mode for `build` yet).
fn build(args: &Value) -> Result<ToolOutcome, String> {
    let project_dir = PathBuf::from(req_str(args, "project_dir")?);
    let target = req_str(args, "target")?;
    let timeout = timeout_arg(args)?;
    let mut argv = vec!["build".to_string(), "-t".into(), target.clone()];
    if opt_bool(args, "source_only")? {
        argv.push("--source-only".into());
    }
    if opt_bool(args, "strict")? {
        argv.push("--strict".into());
    }
    if opt_bool(args, "release")? {
        argv.push("--release".into());
    }
    let captured = spawn_cli(&argv, Some(&project_dir), Some(timeout))?;

    let outcome = if captured.timed_out {
        "timeout"
    } else if captured.success() {
        "clean"
    } else {
        "failed"
    };
    let (output_dir, files) = if captured.success() {
        let dir = project_dir.join("build").join(&target);
        (
            Value::String(format!("build/{target}")),
            list_output_tree(&dir),
        )
    } else {
        (Value::Null, Vec::new())
    };
    let doc = json!({
        "format_version": FORMAT_VERSION,
        "command": "build",
        "outcome": outcome,
        "summary": {
            "target": target,
            "exit_code": captured.exit_code,
            "files": files.len(),
        },
        "exit_code": captured.exit_code,
        "stdout": captured.stdout,
        "stderr": captured.stderr,
        "output_dir": output_dir,
        "files": files,
    });
    Ok(ToolOutcome {
        text: pretty(&doc),
        is_error: outcome != "clean",
    })
}

/// List the emitted output tree under `build/<target>/` as sorted paths
/// relative to it, excluding target-toolchain artifact directories
/// (`target/`, `node_modules/`) that project-mode validation leaves behind.
fn list_output_tree(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str());
                if matches!(name, Some("target" | "node_modules")) {
                    continue;
                }
                stack.push(path);
            } else if let Ok(rel) = path.strip_prefix(root) {
                files.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    files.sort();
    files
}

/// `bock_inspect`: wraps `bock inspect --format json`.
fn inspect(args: &Value) -> Result<ToolOutcome, String> {
    let cwd = opt_str(args, "project_dir")?.map(PathBuf::from);
    let mut argv = vec!["inspect".to_string(), "--format".into(), "json".into()];
    match opt_str(args, "scope")?.as_deref() {
        None | Some("build") => {}
        Some("runtime") => argv.push("--runtime".into()),
        Some("all") => argv.push("--all".into()),
        Some(other) => {
            return Err(format!(
                "argument `scope` must be one of build, runtime, all (got `{other}`)"
            ))
        }
    }
    if opt_bool(args, "unpinned")? {
        argv.push("--unpinned".into());
    }
    if let Some(module) = opt_str(args, "module")? {
        argv.push("--module".into());
        argv.push(module);
    }
    if let Some(type_filter) = opt_str(args, "type")? {
        argv.push("--type".into());
        argv.push(type_filter);
    }
    let captured = spawn_cli(&argv, cwd.as_deref(), None)?;
    Ok(document_outcome("inspect", &captured))
}

/// `bock_explain`: in-process lookup against the compiled-in
/// `bock-errors` diagnostic catalog — no subprocess needed.
fn explain(args: &Value) -> Result<ToolOutcome, String> {
    let code = req_str(args, "code")?.to_ascii_uppercase();
    let catalog = bock_errors::catalog::diagnostic_catalog();
    match catalog.iter().find(|info| info.code == code) {
        Some(info) => {
            let doc = json!({
                "format_version": FORMAT_VERSION,
                "command": "explain",
                "outcome": "clean",
                "summary": { "codes": 1 },
                "explanations": [{
                    "code": info.code,
                    "severity": severity_name(info.severity),
                    "summary": info.summary,
                    "description": info.description,
                    "spec_refs": info.spec_refs,
                }],
            });
            Ok(ToolOutcome {
                text: pretty(&doc),
                is_error: false,
            })
        }
        None => {
            // Unknown code: a usage-class error, reported through the same
            // envelope shape the CLI uses for post-parse usage errors.
            let doc = usage_error_document(
                "explain",
                "explanations",
                &format!(
                    "unknown diagnostic code `{code}` — codes look like E4002 or W1001; \
                     see the diagnostic catalog in the Bock documentation"
                ),
            );
            Ok(ToolOutcome {
                text: pretty(&doc),
                is_error: true,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_list_has_seven_tools_with_schemas() {
        let tools = tool_list();
        assert_eq!(tools.len(), 7);
        let names: Vec<&str> = tools
            .iter()
            .map(|t| t["name"].as_str().expect("tool name"))
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
        for tool in &tools {
            assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
            assert!(
                tool["description"].as_str().expect("description").len() > 40,
                "descriptions are the agent-facing contract"
            );
        }
    }

    #[test]
    fn execution_tools_state_the_safety_envelope() {
        // The four tools that execute user code must say so in their
        // descriptions (agent frameworks surface these for permissioning).
        let tools = tool_list();
        for name in ["bock_run", "bock_test", "bock_build", "bock_conformance"] {
            let tool = tools
                .iter()
                .find(|t| t["name"] == name)
                .unwrap_or_else(|| panic!("{name} missing"));
            let desc = tool["description"].as_str().expect("description");
            assert!(
                desc.contains("SAFETY: this tool executes user code"),
                "{name} must carry the execution-safety wording"
            );
        }
        // Sanity: the shared constant matches the inlined wording.
        assert!(EXECUTION_SAFETY.starts_with("SAFETY: this tool executes user code"));
    }

    #[test]
    fn unknown_tool_is_a_protocol_error() {
        let err = call_tool("bock_bogus", &json!({})).expect_err("unknown tool");
        assert!(err.contains("Unknown tool"), "{err}");
    }

    #[test]
    fn missing_required_args_are_protocol_errors() {
        assert!(call_tool("bock_check", &json!({})).is_err());
        assert!(call_tool("bock_run", &json!({})).is_err());
        assert!(call_tool("bock_build", &json!({ "target": "js" })).is_err());
        assert!(call_tool("bock_explain", &json!({})).is_err());
        assert!(call_tool("bock_conformance", &json!({})).is_err());
    }

    #[test]
    fn arg_helpers_validate_types() {
        let args = json!({ "s": "x", "b": true, "a": ["one"], "bad": 3 });
        assert_eq!(req_str(&args, "s").unwrap(), "x");
        assert!(req_str(&args, "bad").is_err());
        assert!(req_str(&args, "missing").is_err());
        assert_eq!(opt_str(&args, "missing").unwrap(), None);
        assert!(opt_bool(&args, "b").unwrap());
        assert!(!opt_bool(&args, "missing").unwrap());
        assert!(opt_bool(&args, "s").is_err());
        assert_eq!(opt_str_array(&args, "a").unwrap().unwrap(), vec!["one"]);
        assert!(opt_str_array(&args, "bad").is_err());
        assert!(opt_str_array(&args, "missing").unwrap().is_none());
    }

    #[test]
    fn timeout_arg_defaults_and_validates() {
        assert_eq!(
            timeout_arg(&json!({})).unwrap(),
            Duration::from_secs(DEFAULT_TIMEOUT_SECS)
        );
        assert_eq!(
            timeout_arg(&json!({ "timeout_seconds": 5 })).unwrap(),
            Duration::from_secs(5)
        );
        assert!(timeout_arg(&json!({ "timeout_seconds": 0 })).is_err());
        assert!(timeout_arg(&json!({ "timeout_seconds": "5" })).is_err());
    }

    #[test]
    fn explain_finds_catalog_codes_and_rejects_unknown() {
        let found = call_tool("bock_explain", &json!({ "code": "e1002" })).expect("valid call");
        assert!(!found.is_error);
        let doc: Value = serde_json::from_str(&found.text).expect("json doc");
        assert_eq!(doc["command"], "explain");
        assert_eq!(doc["outcome"], "clean");
        assert_eq!(doc["explanations"][0]["code"], "E1002");
        assert_eq!(doc["explanations"][0]["severity"], "error");

        let unknown = call_tool("bock_explain", &json!({ "code": "E9999" })).expect("valid call");
        assert!(unknown.is_error);
        let doc: Value = serde_json::from_str(&unknown.text).expect("json doc");
        assert_eq!(doc["outcome"], "usage-error");
        assert!(doc["explanations"].as_array().expect("payload").is_empty());
    }
}
