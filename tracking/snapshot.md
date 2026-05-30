# Snapshot — current project state

**The one question:** what is the current project state?

Present-state facts only — build/test status, what works, phase
history, migration notes. NO future work (that's `queue.md` /
`milestones.md`). **`STATUS.md` is GENERATED** from this file plus a
live active/blocked/deferred summary derived from `queue.md` — do not
hand-edit `STATUS.md`. Orchestrator refreshes the build line as it
changes.

---

## Build status (as of main, 2026-05-29)

| What | State |
|------|-------|
| `cargo test --workspace` | passing (~2296 tests, 0 failed — per #108) |
| `cargo clippy --workspace --all-targets -D warnings` | clean |
| `cargo fmt --all -- --check` | clean |
| `mdbook build docs` | clean |
| CI on `main` | green (build matrix · clippy · rustfmt · cargo doc · mdbook · vscode · pages) |
| Conformance fixtures | parse/discover only — execution not wired (queue Q-fconf) |
| `bock check` on examples | 20/20 exit 0 (the context-audit `@performance` E8003 fixed in #100) |

## What works today

- **Compiler pipeline end-to-end** — 17 `bock-*` crates; CLI `bock`
  exposes `new`, `build`, `run`, `check` (incl. `--only`/`--brief`/
  `--strict`), `test`, `fmt`, `repl`, `inspect`, `pin`, `unpin`,
  `override`, `cache`, `promote`, `pkg`, `doc`, `model`, `lsp`.
- **Targets** — JS, TS, Python, Rust, Go codegen, exercised by the
  example projects (source-mirrored output paths, #28).
- **Type system** — bidirectional inference, generics, trait-style
  constraints, effect inference.
- **Conformance** — fixtures across `effects/interp/parse/time/types`
  (+ effect-handler fixtures, #74); harness currently validates by
  parse/discovery (see Q-fconf).
- **VS Code extension** — builds to a working `.vsix`; vocab synced
  from the compiler; deps current (ESLint 10, etc., #80).
- **Docs** — mdBook with tooling reference in sync with the CLI (#90).
- **Website** — Astro static site; Cloudflare Workers deploy green
  (#85); deps current (#78).

## Standard library

The embedded source-compiled loading mechanism is **live** (#103): `core.*`
modules ship as Bock source bundled in the `bock` binary and resolve through
the module registry (hermetic; works from any cwd). **3 of 11 v1 modules
landed** — `core.error` (#103), `core.compare` (#104), `core.convert` (#110).
The 2026-05-30 Design stdlib batch (DQ6–DQ9) is reconciled into the spec (#106);
**Q-bridge (#108)** wired the trait-impl table + canonical primitive conformances
(primitives satisfy bounds; `where`-bounds enforced; DV6 fixed); **#110** added
parameterized-trait resolution (From/Into/TryFrom + blanket + primitive
conversions). R1 remaining: `iter` (collection conformances), `effect`
(effect-system bridge). The prelude (≈9 builtins + type-checker intrinsics) is
unchanged pending prelude injection (Q-prelude-inject). See DV1, DV4, MS-stdlib.

## Phase history

A (Foundation Lock) · B (Module System) · C (Effect Codegen) ·
D (Generics) · E (Stdlib *Bridging* — the checker↔`bock-core` method
registry, **not** the stdlib modules) · F (AI Pipeline). All complete.

## Migration notes

Migrated from the internal `aura-dev` tree (commit `38ef9fe`). The
Aura→Bock rename is recorded in the spec changelogs; historical
changelog content preserves the Aura name verbatim. Active spec,
source, examples, extension, and docs are all under the Bock identity.
