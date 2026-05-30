# Single-file bundling for runnable cross-module builds (DV13)

**Date:** 2026-05-30
**Affects:** §20.6.1 (Output Layout)
**Type:** breaking change (output layout) — surfaced as OPEN, pending Design

## Change

`bock build` now emits a **single bundled entry file** for runnable builds
instead of the one-file-per-module mirrored tree §20.6.1 specifies. Every module
the entry program reaches through a real `use` (the transitive closure of its
`use` imports — including the embedded `core.*` stdlib when actually imported) is
concatenated, in dependency order, into the one entry file (`build/<target>/main.<ext>`).
The `use`/import statements themselves are dropped: the imported declarations live
in the same file, so they resolve directly.

A module reachable only via the implicit §18.2 prelude (not explicitly `use`d and
contributing no referenced symbol) is **not** bundled, so a program that uses no
cross-module symbol — e.g. a bare `hello_world` — emits only its own entry module,
exactly as it ran before this change.

Per target:
- **js/ts**: each module's top-level declarations are concatenated (one shared
  top-level scope; valid). Each runtime prelude (Optional/concurrency) is emitted
  at most once.
- **python**: module defs are concatenated; the `import …` preamble and runtime
  preludes are emitted once.
- **rust**: all module items are **flattened to the crate root** (so imported
  items resolve unqualified); the crate `#![allow(...)]` attribute and `use std::…`
  imports appear once.
- **go**: one `package main`, one merged/deduped `import (...)` block (the union of
  each module's `fmt`/`sync`/`time` needs), each runtime prelude at most once, then
  all module bodies.

## Rationale

The conformance harness and the build toolchain both compile + run a single
`main.<ext>`; the per-module tree's extra files were never compiled or executed, so
a cross-module program could not run on any target regardless of how imports were
emitted (DV13). Bundling collapses the program into the one file the run model
actually executes, making importing programs compile + run on all five v1 targets
(js, ts, python, rust, go). The interpreter (`bock run`) already does cross-module
resolution via the module registry and is unaffected.

## Migration

No source changes are required. Build output for a multi-module project is now a
single `build/<target>/main.<ext>` rather than a mirrored per-module tree.

## OPEN

This diverges from §20.6.1's normative one-file-per-module layout. Surfaced as
`OPEN: §20.6.1` for Design to decide whether the per-module tree returns as a
future "library build" mode, with single-file bundling as the default
"application build" mode.
