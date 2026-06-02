# Implementation Plan: ItemB — Per-Module Native Output + Project-Mode Scaffolding + Config Tables

**Date:** 2026-06-02
**For:** ItemB (project-mode codegen), expanded per two owner decisions (2026-06-02)
**Status:** DRAFT — orchestrator-authored; pending owner sign-off on staging before dispatch
**Designed by:** Orchestrator, grounded in the live build (main 8305bf7) + #28/#132 history

> **Owner decisions (2026-06-02), eyes-open:**
> 1. **DQ19 → per-module tree is the v1 output model** (NOT bundling). This *reverses*
>    the #132 single-file-bundling default and **re-opens DV13** — the foundational
>    cross-module *execution* gap. Per-target **native** cross-file imports must be made
>    to compile-and-run on all 5 targets, and the conformance harness reworked to
>    build+run a multi-file project, **before** scaffolding lands.
> 2. **Config tables pulled forward into v1** — `[targets.<T>]` deep config (test
>    framework, formatter) + `[targets.<T>.scaffolding]` shallow config (linter,
>    package manager) parsed and codegen-branched in v1. **Un-reserves** spec surfaces
>    (§20.6.2, §20.7, Appendix A.3) → requires spec changelogs.
>
> Still **deferred to v1.x** (unchanged; not in scope here): `--deliverable`,
> `--no-tests` (cheap once tests transpile — flag for a revisit, but out of scope).

## 0. Grounding (verified against the live tree)

- **Today's `bock build`** (`bock-cli/src/build.rs:225-321`) writes transpiled source
  only — mirrored filenames per §20.6.1 + optional sourcemaps — then optionally invokes
  the toolchain per emitted file. **No scaffolding** (no `package.json`/`Cargo.toml`/
  `go.mod`/`pyproject`/README; tests not transpiled). The default is effectively *source
  mode*; §20.6.2 project mode is the ItemB delta.
- **Bundling (the thing being reverted):** `generate_project` (per-target overrides in
  `bock-codegen/src/{js,ts,py,rs,go}.rs`) concatenates every `use`-reachable module —
  including imported `core.*` — into ONE entry file, dropping import statements
  (changelog `spec/changelogs/2026-05-30-single-file-bundling.md`). This is why
  cross-module programs run today; **the entire v1 stdlib (11 modules ×5, 420 exec
  pairs) depends on it.** Reverting means making native per-target imports actually
  compile+run for every cross-module program, including all stdlib usage.
- **Conformance harness** (`compiler/tests/execution.rs`): runs
  `bock build -t <T> --source-only`, then executes the single emitted `main.<ext>` via
  `ToolchainRegistry::run`. It already supports multi-file *fixtures* (`// FILE:` markers
  → `harness/mod.rs::split_file_sections`), but *output* is bundled and the *run* is
  single-file. Rework = run the per-module tree via a project runner.
- **Rust/Go cannot run a multi-file program without a manifest** (`Cargo.toml` for
  `mod`/`use`; `go.mod` + package for cross-file imports). So for those two targets the
  DV13 re-open is **inseparable** from the minimum scaffolding — their Stage-1 work
  co-delivers the minimal manifest.
- **Single-fixer-per-crate / hot-file rule** (`routing.md`): `bock-codegen` shared files
  (`generator.rs`, `lib.rs`) and `bock-cli/src/build.rs` are sequential. Per-target
  emitter files (`js.rs`/`ts.rs`/`py.rs`/`rs.rs`/`go.rs`) are disjoint → safe for
  **concurrent foreground** fan-out (background sub-agents cannot write — see project
  memory `background-subagents-cannot-write`).

## Invariant for EVERY PR in this milestone

`./tools/scripts/run-conformance.sh` with `BOCK_CONFORMANCE_REQUIRE=all` stays **420/420
green** (or higher as fixtures are added). Until a target's native-import path is proven,
the bundling path stays available behind a flag and the harness migrates **target by
target** — no big-bang cutover. The four-command pre-PR gate (fmt/clippy/test/doc) is
clean on every PR.

---

## Stage S0 — Spec reconciliation + tracking (orchestrator, no engineer)

Spec leads implementation (project rule: "the spec is normative"). Land first.

- **Changelog A — DQ19 resolved:** revise §20.6.1 — remove the "OPEN — under Design
  review" bundling implementation note; restore the per-module mirrored tree as the
  normative + *realized* v1 output; note bundling is retired as the default (kept
  internally only as a transition fallback, removed at S4). Type: breaking change
  (output layout, reverting the 2026-05-30 note).
- **Changelog B — config tables un-reserved:** §20.6.2 (deep/shallow matrices now v1),
  §20.7 + Appendix A.3 (the `[targets.<T>]` / `[targets.<T>.scaffolding]` tables are now
  parsed in v1), and the §20.1 / §15.3 / §19.7 "Reserved for v1.x" cross-refs for these
  tables. `--deliverable` / `--no-tests` remain Reserved.
- **Tracking:** DQ19 → DECIDED (owner 2026-06-02) in `design-questions.md`; DV13 → status
  re-opened (per-module tree pursued) in `divergences.md`; `milestones.md` — new
  v1-blocking milestone entry; `queue.md` — restructure ItemB into S1–S8; `audit.md`
  decision entry. Regenerate STATUS/ROADMAP.
- Lands as one tracking+spec PR (`chore/tracking-*` + spec changelogs).

## Stage S1 — Native cross-module imports + harness multi-file run: PILOT = Python

Pilot the riskiest mechanic (harness rework + native imports) on the **simplest runtime**
(Python: no compile step, multi-file runs via `python main.py` with `build/py` on path).

- **Codegen (`py.rs`):** emit each module to its own `build/py/<path>.py` with real
  `from <module> import <names>` instead of concatenating; entry stays `main.py`.
- **Run model (`bock-build/src/toolchain.rs` Python run plan):** run the entry with the
  output dir as package root (sys.path / `python -m`).
- **Harness (`execution.rs`):** add a per-module-tree run path; keep the single-file path
  for not-yet-migrated targets; migrate **python only** this PR. Prove the cross-module +
  stdlib-using exec fixtures green on python via native imports.
- Acceptance: all python exec pairs green via native multi-file; other 4 targets still
  green via bundling fallback; full suite 420/420.

## Stage S2 — Fan-out native imports: JS, then TS

- **JS (`js.rs`):** ESM relative imports (`import { x } from "./foo.js"`); minimal run
  affordance (`"type":"module"` shim or `.mjs`) so `node main.js` resolves imports.
- **TS (`ts.rs`):** ES imports + `tsc`/`tsx` run plan.
- Migrate harness js→ts in step; keep 420 green.

## Stage S3 — Native imports for Rust + Go (entangled with the minimal manifest)

These targets *require* a manifest to run multi-file — so the minimum `Cargo.toml` /
`go.mod` is co-delivered here (the rest of scaffolding is S5–S7).

- **Rust (`rs.rs`):** emit `mod <m>;` + `use crate::<m>::<x>;` across files; emit a
  minimal `Cargo.toml`; run via `cargo run`. (Replaces flatten-to-crate-root bundling.)
- **Go (`go.rs`):** one package across files with module-path imports; minimal `go.mod`;
  run via `go run .`.
- Migrate harness rust→go; keep 420 green.

## Stage S4 — Flip the default; retire bundling

- Default `bock build` = per-module tree (project-mode foundation). `--source-only` =
  bare per-module source, no scaffolding. Remove/quarantine the bundling path now that
  all 5 run natively. Harness fully on the multi-file path; **420/420 native, no
  fallback.** This is the DV13-CLOSED milestone.

## Stage S5 — Scaffolding framework (ItemB Phase 1)

- A `Scaffolder` abstraction (shared `generator.rs`/new module): manifest model, README
  first-contact templating, test-emission hook, formatter-config emission.
- `bock.project` parsing of `[targets.<T>]` + `[targets.<T>.scaffolding]` with
  unknown-value validation (error → spec's documented options per target). Locate/extend
  the existing `bock.project` parser (shared with `bock new`).

## Stage S6 — Per-target scaffolders + deep-config branches (ItemB Phases 2-5, fan-out)

Disjoint emitter files → **concurrent foreground** sessions OK; shared framework changes
serialize through S5.

- **JS/TS:** `package.json` (+ `tsconfig.json`), **Vitest|Jest** test-codegen branch,
  Prettier config, ESLint (shallow), package-manager README hint (npm|pnpm|yarn).
- **Python:** `pyproject.toml`, **pytest|unittest** branch, **Black|Ruff format** branch,
  Ruff-check|Pylint (shallow), pip|Poetry|uv README hint.
- **Rust:** extend S3 `Cargo.toml`; cargo-test scaffolding; Clippy (shallow); rustfmt is
  universal/always-on.
- **Go:** extend S3 `go.mod`; stdlib-testing scaffolding; golangci-lint (shallow); gofmt
  universal/always-on.

## Stage S7 — Transpiled tests + formatter-clean gate (ItemB Phase 6 + release-readiness)

- Bock `@test` functions → idiomatic target test files so `npm test` / `cargo test` /
  `pytest` / `go test` execute them (§20.6.2 default-on test inclusion — the validation
  surface).
- Output passes `rustfmt --check` / `gofmt -l` / `prettier --check` / `black --check`
  cleanly on first generation (the §20.6.2 codegen-formatter agreement). May surface
  codegen-hygiene fixes.

## Stage S8 — Internal docs + tracking close

- Update `docs/` build reference (modes, config tables) + the `bock build` CLI surface.
- **External** `/get-started` + website copy = **ItemD** (escalates — do NOT touch here).
- Tracking: ItemB DONE, DV13 CLOSED, milestone satisfied; regenerate views.

---

## Sequencing & risk

- **Critical path is sequential** through S0→S4 (foundational, mostly shared
  `bock-codegen`/`build.rs`/harness). S6 fans out by target. S5 gates S6.
- **Top risk:** S1–S4 regressing the 420-pair stdlib. Mitigation: bundling fallback
  retained behind a flag, target-by-target harness migration, full-conformance gate on
  every PR, pilot-first (python) to de-risk the harness mechanic before fan-out.
- **Size:** ~20–30 PRs. This is the bulk of remaining v1.0 engineering.
- **Pilot-target rationale:** python (no compile, simplest multi-file run) → fastest
  feedback on the harness rework; rust/go last (manifest-entangled, heaviest).
