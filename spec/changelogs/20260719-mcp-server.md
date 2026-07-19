# §20 tooling notes — `bock mcp` ships; §20.1 flag lists refreshed

**Date:** 2026-07-19
**Affects:** §20.1 (CLI), §20.3 (Language Server — v1.x sequencing note)
**Type:** non-normative tooling-register update (no semantic, grammar, or
type-system change)

## Change

Two touches, both inside §20's non-normative command register (§20.1 states
"the spec is normative for capabilities, not for the precise shape of the
command surface"):

1. **§20.1 — new "Servers" group + per-command flag refresh.**
   - A new **Servers** group lists `bock lsp` (already governed by §20.3 but
     previously absent from the §20.1 command register) and the new
     **`bock mcp`** subcommand: an MCP (Model Context Protocol) server over
     stdio exposing the compiler surface — check / run / test / build /
     single-file cross-target conformance / inspect / diagnostic-code
     explanations — as tools for agentic clients, with tool results reusing
     the `--format json` machine-output documents.
   - The per-command flag lists are brought current against the v1 binary
     (`bock --help`), which they had lagged since the structured-output work
     (#427/#440):
     - `bock check` — adds `--format <human|json>` (one machine-readable
       JSON document on stdout; exit codes unchanged by format).
     - `bock test` — adds `--format <human|json>` (per-test results plus
       structured compile-error diagnostics).
     - `bock inspect` — adds `--format <human|json>` (shared machine-output
       envelope), notes the legacy `--json` bare-array flag and their mutual
       exclusivity, and adds the `air <file>` subcommand (present in the
       binary; its established `--json` tree shape stands apart from the
       shared envelope).
     - `bock run` — the line previously described `--target` as a v1 flag
       and `--watch` as working hot reload. The v1 binary has no
       `run --target` (interpreter is the only execution path) and accepts
       `--watch` as not-yet-implemented. The line now states the v1 truth
       and points cross-target execution at the §20.4 v1.x plan.

2. **§20.3 — sequencing note updated.** The v1.x sequencing note (added by
   the 2026-06-09 design audit) listed an MCP server as the *lead planned*
   agent-first tooling item. It now records that the item has **shipped** as
   the `bock mcp` subcommand — inside the CLI binary (stdio transport,
   version-locked to the compiler by construction) rather than as the
   separate `bock-mcp` crate the note originally sketched, per the
   integrated bock-mcp design brief (2026-07-03) and the operator's
   hand-rolled-protocol decision of the same date.

## Rationale

The MCP server is the agent-first counterpart of the LSP: it makes any
MCP-capable agent environment a competent Bock environment with zero extra
install. Shipping it inside the CLI keeps the tool surface version-locked to
the compiler, and reusing the `--format json` contract (FORMAT_VERSION 1)
means the MCP tool documents and the CLI machine output cannot drift apart.
The §20.1 flag-list refresh restores the register's "amended to match
implementation experience" promise.

## Migration

None — tooling additions and register corrections only. Documentation:
`docs/src/reference/mcp.md` (new page) documents the server, the protocol
subset, all seven tool schemas, and the execution-safety envelope;
`docs/src/reference/cli.md` gains the `bock mcp` entry.
