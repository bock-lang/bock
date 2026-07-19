# MCP Server

`bock mcp` starts a [Model Context Protocol](https://modelcontextprotocol.io/)
server over stdio, exposing the compiler surface as tools any MCP-capable
agent client (Claude Code, editors, agent frameworks) can call. It ships
inside the `bock` binary — zero extra install, version-locked to the
compiler by construction — and its tool results are the **same JSON
documents** the CLI's `--format json` mode emits (see
[CLI — Machine-Readable Output](./cli.md#machine-readable-output)):
each tool spawns the same `bock` binary with the same argv a CLI user
would type, so the tool surface and the CLI machine contract cannot
drift apart.

```bash
bock mcp
bock mcp --stdio        # same; --stdio accepted for client-config convention
```

## Client configuration

Any MCP client that launches stdio servers can use the binary directly.
For example, for Claude Code:

```bash
claude mcp add bock -- bock mcp
```

or in a JSON client configuration:

```json
{
  "mcpServers": {
    "bock": {
      "command": "bock",
      "args": ["mcp"]
    }
  }
}
```

The server reads newline-delimited JSON-RPC 2.0 messages on stdin and
writes protocol messages — and nothing else — on stdout. Logging goes to
stderr. EOF on stdin shuts the server down cleanly (exit 0).

## Protocol surface

The server is a deliberately thin, hand-rolled implementation of exactly
the MCP subset it serves (no third-party protocol dependency):

| Method                      | Behavior                                                             |
| --------------------------- | -------------------------------------------------------------------- |
| `initialize`                | Protocol version negotiation; capabilities `{tools, resources}`; serverInfo `{name: "bock", version}`. |
| `notifications/initialized` | Accepted (no response, per JSON-RPC). Unknown notifications are ignored. |
| `ping`                      | Responds with an empty result.                                        |
| `tools/list`                | The seven tools below, each with a JSON Schema `inputSchema`.         |
| `tools/call`                | Dispatches to a tool; returns one text content block.                 |
| `resources/list`            | **Empty in v1.** The context-pack-as-resources wiring is a planned follow-up; the surface exists so clients can negotiate it now. |
| `resources/read`            | JSON-RPC error `-32002` (resource not found) for any URI in v1.       |

Protocol failures are JSON-RPC errors: `-32700` (parse error — malformed
frames do not crash the loop), `-32600` (invalid request), `-32601`
(unknown method), `-32602` (invalid params — including unknown tool names
and malformed tool arguments), `-32002` (unknown resource).

**Tool-level failures** are *successful* `tools/call` responses with
`isError: true`: a tool call reports `isError: true` exactly when the
wrapped command's outcome is not clean — a non-zero exit (diagnostics
found, tests failed, program failed, build failed, conformance mismatch),
a timeout, or a subprocess that could not be spawned. The structured
document rides along in the content either way, so an agent always gets
the diagnostics, not just a failure flag.

## Execution safety

`bock_run`, `bock_test`, `bock_build`, and `bock_conformance` **execute
user code** and may read and write the workspace and spawn processes —
the same trust envelope as running the `bock` CLI yourself. Each of
these tools states this in its description field (which agent frameworks
surface for permissioning). Do not point them at untrusted code you
would not run locally. `bock_check`, `bock_inspect`, and `bock_explain`
are read-only analyses.

The execution tools accept `timeout_seconds` (default 120): on expiry
the spawned process is killed and the tool reports `isError: true`.
For `bock_conformance`, the bound applies to the reference run and each
per-target build step, but **not** to the target-toolchain execution
step in v1.

## The seven tools

Paths in tool arguments resolve against the server process's working
directory; absolute paths are recommended. Every tool result is one
pretty-printed JSON document with the shared machine-output envelope
(`format_version` — currently `1` — plus `command`, `outcome`,
`summary`, and a per-command payload).

### `bock_check`

Wraps `bock check --format json`. Returns the check document exactly as
documented in [CLI — `bock check --format json`](./cli.md#bock-check---format-json):
`outcome` (`"clean"` / `"failed"` / `"usage-error"`), `summary
{files, errors, warnings}`, and `diagnostics` entries carrying
`severity`, `code` (`null` for I/O-class failures), `message`,
`span {file, start, end, line, col}`, and `suggestion`.

```json
{
  "type": "object",
  "properties": {
    "files": {
      "type": "array", "items": { "type": "string" }, "minItems": 1,
      "description": "Paths to the .bock files to check. Multi-file programs: pass every file so cross-file `use` imports resolve."
    },
    "strict": { "type": "boolean", "default": false },
    "only": {
      "type": "array",
      "items": { "type": "string", "enum": ["types", "context"] }
    }
  },
  "required": ["files"]
}
```

### `bock_run`

Wraps `bock run` (the reference interpreter). `bock run` has no CLI json
mode yet, so the tool wraps the captured process output in a minimal
envelope following the same conventions (additive; documented here):

```json
{
  "format_version": 1,
  "command": "run",
  "outcome": "clean",
  "summary": { "exit_code": 0 },
  "exit_code": 0,
  "stdout": "hello\n",
  "stderr": ""
}
```

`outcome` is `"clean"` (exit 0), `"failed"` (non-zero exit), or
`"timeout"` (killed at the wall-clock bound; `exit_code` is then
`null`). Compile errors appear as rendered text in `stderr` — run
`bock_check` for structured diagnostics.

Arguments: `file` (required), `args` (array of program arguments),
`cwd`, `timeout_seconds`.

### `bock_test`

Wraps `bock test --format json`. Returns the test document exactly as
documented in [CLI — `bock test --format json`](./cli.md#bock-test---format-json):
`summary {tests, passed, failed}`, a `tests` array
(`{name, file, passed, message, duration_ms}`), and a top-level
`diagnostics` array carrying structured compile-error diagnostics when a
test file failed to compile.

Arguments (all optional): `files` (omit to discover recursively under
`cwd`), `filter`, `cwd`, `timeout_seconds`.

### `bock_build`

Wraps `bock build --target <T>` run in `project_dir`. Like `run`,
`build` has no CLI json mode yet; the envelope packages the captured
output plus a listing of the emitted output tree:

```json
{
  "format_version": 1,
  "command": "build",
  "outcome": "clean",
  "summary": { "target": "js", "exit_code": 0, "files": 5 },
  "exit_code": 0,
  "stdout": "build: compiling 1 source file…",
  "stderr": "",
  "output_dir": "build/js",
  "files": ["main.js", "main.js.map", "package.json"]
}
```

`files` lists the emitted tree as sorted paths relative to
`output_dir`, excluding target-toolchain artifact directories
(`target/`, `node_modules/`). On failure `output_dir` is `null` and
`files` is empty; build errors appear as rendered text in
`stdout`/`stderr`.

Arguments: `project_dir` and `target` (required; `target` one of `js`,
`ts`, `python`, `rust`, `go`), plus `source_only`, `strict`, `release`,
`timeout_seconds`.

### `bock_conformance`

The differentiator: *does this program behave identically on every
target?* as one call. v1 scope is deliberately modest — a **single,
self-contained file** (embedded `core.*` stdlib imports are fine; sibling
files are not included). The tool runs the file on the reference
interpreter, then for each requested target whose toolchain is installed
locally, builds it in an isolated temp project and executes the output,
comparing normalized stdout (`\r` stripped, trailing newlines trimmed)
against the interpreter's:

```json
{
  "format_version": 1,
  "command": "conformance",
  "outcome": "clean",
  "summary": { "targets_exercised": 2, "targets_skipped": 3, "matched": 2, "mismatched": 0 },
  "file": "hello.bock",
  "reference": { "runner": "interpreter", "exit_code": 0, "stdout": "hi\n", "stderr": "" },
  "targets": [
    { "target": "js", "status": "matched", "exit_code": 0, "stdout": "hi\n", "detail": null },
    { "target": "go", "status": "skipped", "exit_code": null, "stdout": null,
      "detail": "toolchain not found — install Go from https://go.dev/dl/" }
  ]
}
```

Per-target `status` is `"matched"`, `"mismatched"`, `"skipped"`,
`"build-failed"`, or `"run-failed"`. **A missing toolchain is a reported
skip, never a silent pass** — check `summary.targets_exercised` before
concluding a program is conformant. `outcome` is `"clean"` when nothing
mismatched or failed (skips alone do not fail it); the reference run
failing fails the whole call. Expect seconds-to-minutes of cost with
compiled targets (rust, go) present.

Arguments: `file` (required), `targets` (subset of the five; empty array
= reference interpreter only), `timeout_seconds`.

### `bock_inspect`

Wraps `bock inspect --format json` (read-only). Returns the inspect
document as documented in
[CLI — `bock inspect --format json`](./cli.md#bock-inspect---format-json):
`summary {decisions}` and a `decisions` array of
`{scope, prefixed_id, decision}` manifest entries.

Arguments (all optional): `project_dir`, `scope` (`build` / `runtime` /
`all`, default `build`), `unpinned`, `module`, `type`.

### `bock_explain`

Explains a diagnostic code from the compiled-in `bock-errors` catalog —
the same registry that backs editor hovers. In-process, instant,
read-only:

```json
{
  "format_version": 1,
  "command": "explain",
  "outcome": "clean",
  "summary": { "codes": 1 },
  "explanations": [
    {
      "code": "E1008",
      "severity": "error",
      "summary": "Circular module dependency.",
      "description": "The `use` import graph contains a cycle…",
      "spec_refs": ["§10"]
    }
  ]
}
```

An unknown code returns the shared `outcome: "usage-error"` envelope
with `isError: true`. v1 serves catalog text only; richer fix-pattern
context packs are a planned follow-up (they will arrive alongside the
resources surface).

Arguments: `code` (required; e.g. `"E4002"`, `"W1001"`;
case-insensitive).

## Resources (v1: empty)

`resources/list` returns an empty list and `resources/read` errors for
every URI. The surface exists so clients can negotiate the capability
now; serving the AI context packs (fix patterns, decision context) as
MCP resources is a planned follow-up and will be purely additive.
