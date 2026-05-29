<!-- DO NOT EDIT — generated from tracking/ by tools/scripts/gen-tracking-views.sh -->

# Status

## Active work

Live summary derived from `tracking/queue.md` (items per section):

- Ready: 5
- v1-blocking: 1
- Blocked: 5
- Deferred: 1

## Build status (as of main, 2026-05-29)

| What | State |
|------|-------|
| `cargo test --workspace` | passing (~2241 tests, 0 failed — per #79) |
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

`stdlib/` is **empty** (0 modules; prelude ≈ 9 builtins + a few
type-checker intrinsics). The §18 core library is not yet implemented
— see `divergences.md` DV1 and `milestones.md` MS-stdlib.

## Phase history

A (Foundation Lock) · B (Module System) · C (Effect Codegen) ·
D (Generics) · E (Stdlib *Bridging* — the checker↔`bock-core` method
registry, **not** the stdlib modules) · F (AI Pipeline). All complete.

## Migration notes

Migrated from the internal `aura-dev` tree (commit `38ef9fe`). The
Aura→Bock rename is recorded in the spec changelogs; historical
changelog content preserves the Aura name verbatim. Active spec,
source, examples, extension, and docs are all under the Bock identity.
