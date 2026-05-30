# Plan: Codegen-Correctness + Execution-Conformance Workstream

**Status:** approved (operator decided "fix codegen first", 2026-05-30). v1-blocking.
The `core.iter` spike exposed pre-existing codegen defects that break general
Bock→Rust/Go/Python output and were never caught because the conformance suite
doesn't execute. `core.iter` (R1) is BLOCKED on this workstream.

## Why this is v1-blocking
These defects break **general** Bock code on Rust/Go/Python (not just iterators),
so the v1 "one language, five targets, codegen parity" property is currently
**false and untested**. They went unnoticed because the conformance suite
parses/discovers but never EXECUTES (Q-fconf) — codegen output was never run
per-target. Fixing them is required for v1 regardless of stdlib.

## The defects (empirically reproduced; with one correction to the original brief)
1. **Statement-bodied `match` arms** (`break`/`continue`/`return`/assign) →
   `/* unsupported */` on **all 5 backends** (`rs.rs:1906`, `go.rs:2000`,
   `ts.rs:1991`, `js.rs:1677`, `py.rs:1712`). Root: these parse as expressions →
   block tails → `emit_match_arm` inlines via `emit_expr`, which doesn't handle
   them. Position (expr vs stmt) is NOT modeled in AIR — decided by which emit fn
   reaches the node.
2. **Go emits `match` as an expression IIFE** (`func() interface{}{ switch }()`)
   → any statement-bodied arm broken. Go has a real statement `switch`
   (`go.rs:2008`) reached only in statement position.
3. **`self`-method codegen broken on Rust, Go, AND Python** (the brief's "Py OK"
   was WRONG — Python emits `def swap(self, self)` → SyntaxError). Rust emits
   `pub fn get(&self, self: _)` (receiver double-counted) + `mut self` ignored;
   Go adds `self interface{}`. JS/TS genuinely OK (explicit-`self` param model).
   Root: AIR always desugars `p.m(x)` → `Call{FieldAccess(p,m),[p,x]}` (self
   prepended) AND keeps `self` as a Param; backends double/triple-count.
4. **Go has no `Optional[T]` runtime** (bare `Optional[int64]`/`Some`/`None`).
   Python ALSO lacks it (out of scope here; fast-follow). Representation is
   **non-normative** (spec §18 / bock-spec.md:1625) → implementer's-call.
5. **Interpreter method bodies run in an empty env** (`interp.rs:2247`/`:2261`
   `Environment::new()`), so `Some`/`None`/`Ok`/`Err` + top-level fns are
   invisible inside any method. Contained fix: globals-bearing root frame.

## Verification gap (Q-fconf)
The harness never executes (`Expectation::Output` parsed, unconsumed);
`tools/scripts/run-conformance.sh` is absent. Wire execution conformance: compile
a fixture to each target via `bock build`, run the toolchain, capture stdout, diff
against `// EXPECT: output`. `ToolchainRegistry` (`bock-build/toolchain.rs`) only
VALIDATES today — add a `run()` that executes + captures.

## Sequencing — TWO PRs
- **PR1 — execution-conformance harness (Q-fconf), pure infra.** `run()` on the
  toolchain registry (per-target: node / tsc+node / python3 / rustc+exec / go run);
  a `compiler/tests/execution.rs` `[[test]]` driving discover→build→run→diff for
  `Output` fixtures, **skip-if-absent** + `BOCK_CONFORMANCE_REQUIRE` override;
  `tools/scripts/run-conformance.sh` + fix the 2 stale references; known-good
  fixtures proving the runner green. NO codegen changes, NO ci.yml change (runs
  under `cargo test --workspace`, skip-if-absent). → builds defects into red tests.
- **PR2 — codegen + interp fixes**, verified green by PR1's harness: red fixtures
  per defect → fix #5 (interp env) → #1 (statement match arms, all 5) → #2 (Go
  stmt-switch routing via a `match_has_statement_arm` predicate) → #4 (Go Optional
  runtime + ctor + tag-match) → #3 (self decl + call-site self-arg drop on Rust/
  Go/Python; option A = codegen-side, mirroring the JS channel-method precedent).

## Scope boundary
In-scope (unblocks core.iter + restores credible parity): #1 **statement-position**
only (all 5), #2 Go routing, #3 Rust/Go/Python, #4 Go Optional, #5 interp, Q-fconf.
**Defer (rabbit holes):** full **expression-position** statement-arm lowering
(temp-hoist desugar); Python `Optional` runtime (fast-follow); the AIR-side
self-desugar consolidation; Q-self-subst (separate checker bug — keep fixtures on
concrete types to avoid entanglement).

## Decision classification
Overwhelmingly **implementer's-call** (codegen/interp correctness + the harness
design). No spec gates: `Optional` per-target representation is non-normative;
`&self`/`&mut self` for plain/`mut self` is the pragmatic v1 lowering. `core.iter`
protocol shape (generic vs assoc-type, lazy/eager, normative combinators) is
escalated separately as **DQ12** (iter resumes after this workstream).

### Critical files
`bock-codegen/src/{go,rs,py,js,ts}.rs` (#1/#2/#3/#4), `bock-interp/src/interp.rs`
+ `env.rs` (#5), `bock-build/src/toolchain.rs` (Q-fconf `run()`),
`compiler/tests/` (`execution.rs` + Cargo.toml), `tools/scripts/run-conformance.sh`.
