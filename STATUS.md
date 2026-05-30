<!-- DO NOT EDIT — generated from tracking/ by tools/scripts/gen-tracking-views.sh -->

# Status

## Active work

Live summary derived from `tracking/queue.md` (items per section):

- Ready: 10
- v1-blocking: 2
- Blocked: 5
- Deferred: 1

## Build status (as of main c9a241e, 2026-05-30)

| What | State |
|------|-------|
| `cargo test --workspace` | passing (~2370 tests, 0 failed — per #127) |
| `cargo clippy --workspace --all-targets -D warnings` | clean |
| `cargo fmt --all -- --check` | clean |
| `cargo doc --workspace --no-deps -D warnings` | clean (now in the pre-PR gate + CI) |
| `mdbook build docs` | clean |
| CI on `main` | green; cache via Swatinem/rust-cache@v2.9.1 (#116, faster) |
| Conformance | parse/discover **+ execution** — compile+run+diff stdout per target (#114/#115); `run-conformance.sh`; **55+ exec pairs** across 5 targets (deepened by #124/#126/#127 Optional-payload fixtures) |
| `bock check` on examples | 20/20 exit 0 |

## What works today

- **Compiler pipeline end-to-end** — 17 `bock-*` crates; CLI `bock`
  exposes `new`, `build`, `run`, `check` (incl. `--only`/`--brief`/
  `--strict`), `test`, `fmt`, `repl`, `inspect`, `pin`, `unpin`,
  `override`, `cache`, `promote`, `pkg`, `doc`, `model`, `lsp`.
- **Targets** — JS, TS, Python, Rust, Go codegen, now **execution-tested** for
  cross-target parity. DV9 (the parity gap the `core.iter` spike exposed) is
  CLOSED: Q-fconf execution conformance (#114/#115 — compile + run + diff stdout
  per target) + Q-codegen-fixes (#121 — statement-bodied match arms, self-methods
  on Rust/Go/Python, Go `Optional` runtime, interp method-env all fixed); 32/32
  exec fixture×target pairs green under `REQUIRE=all`. The Optional-payload residue has since
  been CLOSED across all 5 (#124 TS self/Optional · #126 Python runtime + Go typed-payload ·
  #127 Go match-in-loop; 55+ exec pairs). Remaining: Q-match-exprpos (expr-position), and the
  newly-found **List built-in method codegen gap (DV10 / Q-list-codegen)** — no backend lowers
  `List.len()`/`.get()`/`.push()` — which blocks core.iter (+ Q-go-list-literal, Q-ts-generic-impl).
- **Type system** — bidirectional inference, generics, trait-style
  constraints, effect inference.
- **Conformance** — fixtures across `effects/interp/parse/time/types`
  (+ effect-handler #74; stdlib/* + exec/* fixtures); the harness now
  **executes** `// EXPECT: output` fixtures — compiles to each target, runs the
  toolchain, diffs stdout (#114/#115); `tools/scripts/run-conformance.sh`.
- **VS Code extension** — builds to a working `.vsix`; vocab synced
  from the compiler; deps current (ESLint 10, etc., #80).
- **Docs** — mdBook with tooling reference in sync with the CLI (#90).
- **Website** — Astro static site; Cloudflare Workers deploy green
  (#85); deps current (#78).

## Standard library

The embedded source-compiled loading mechanism is **live** (#103): `core.*`
modules ship as Bock source bundled in the `bock` binary and resolve through
the module registry (hermetic; works from any cwd). **3 of 11 v1 modules
landed** — `core.error` (#103), `core.compare` (#104), `core.convert` (#110) — though these are
**check-only**: cross-module `use` codegen is broken on all 5 (DV13), so they have not executed
cross-module on any target (the conformance for them was `no_errors`/`--source-only`, not run).
The 2026-05-30 Design stdlib batch (DQ6–DQ9) is reconciled into the spec (#106);
**Q-bridge (#108)** wired the trait-impl table + canonical primitive conformances
(primitives satisfy bounds; `where`-bounds enforced; DV6 fixed); **#110** added
parameterized-trait resolution (From/Into/TryFrom + blanket + primitive
conversions). #129 landed read-only List method codegen (all 5). But `core.iter`'s pursuit (4
attempts, each stopping at a deeper codegen layer) prompted a **3-agent codegen audit** that
established the v1 codegen substrate is **materially incomplete** for the stdlib: **cross-module
`use` and user-defined enums are broken on ALL 5** (DV13/DV14 — so the 3 "landed" modules are
check-only, never executed cross-module), and Result/generics/closures/Optional-methods are broken
on 3-4/5. Operator decided (2026-05-30) a **codegen-completeness MILESTONE** (`Q-codegen-completeness`,
v1-blocking, phased P0-P4, ~10-15 PRs): fix comprehensively, THEN resume the stdlib. **Q-stdlib R1 is
PAUSED** behind it. The for→Iterable desugar is proven (T1 ×5) and resumes after the milestone's P0/P1.
Phase-0 (cross-module bundling · user-enum variant registry · tail-`if`) is designed; sequence C→A→B.
**§18.2 prelude auto-import is live** (#120): the core-defined prelude symbols
(`Ordering`/`Less`/`Equal`/`Greater`, `Comparable`/`Equatable`, `Into`/`From`/
`TryFrom`/`Displayable`, `Error`) resolve without an explicit `use` (the membership
of `TryFrom`/`Error` vs §18.2's literal list → DQ13). See DV1, MS-stdlib.

## Phase history

A (Foundation Lock) · B (Module System) · C (Effect Codegen) ·
D (Generics) · E (Stdlib *Bridging* — the checker↔`bock-core` method
registry, **not** the stdlib modules) · F (AI Pipeline). All complete.

## Migration notes

Migrated from the internal `aura-dev` tree (commit `38ef9fe`). The
Aura→Bock rename is recorded in the spec changelogs; historical
changelog content preserves the Aura name verbatim. Active spec,
source, examples, extension, and docs are all under the Bock identity.
