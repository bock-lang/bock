# Implementation Plan: Stdlib Loading Mechanism + `core.error` Pilot (Q-stdlib R1)

**Status:** approved (orchestrator, 2026-05-29). First step of Q-stdlib (v1
core stdlib, 11 modules, Design-scoped 2026-05-29 / DQ5). Dispatched as
engineer session `feat/stdlib-error-pilot`.

**Goal:** stand up the mechanism by which the compiler discovers and compiles
`stdlib/core/*` modules so user code's `use core.error.{...}` resolves, and
prove it end-to-end with one pure-Bock pilot module, `core.error`.

---

## Reconnaissance verification (confirmed against the code)

- `stdlib/` is empty (only `stdlib/CLAUDE.md`, which is partly stale: documents
  only the `std.*` tier and references non-existent per-target codegen crates
  `compiler/crates/bock-codegen-<target>/`).
- **core vs std is real and normative.** Spec §18.1 (`bock-spec.md:1696`): `core`
  "ships with the compiler"; `std` is package-manager-installed. §18.3 (`:1706`)
  lists the 11 v1 `core.*` modules incl. `core.error` (`:1719`, "Error base
  trait"). §18.4 (`:1810`) is the separate `std.*` tier. → `stdlib/core/error/
  error.bock` is the correct location.
- The three CLI commands share one pipeline; each hardcodes discovery from the
  project root and none know about `stdlib/`:
  `check.rs:255 run()`→`discover_bock_files(".")`; `build.rs:61 run()`→
  `discover_bock_files_recursive(".")`; `run.rs:36 run()`→`discover_bock_files`.
  All: parse all → `DepGraph` topo-sort → compile loop (`seed_imports` +
  `registry.register(collect_exports(...))`). This is the hook point.
- The registry + seeding fully support cross-module traits (`parse_trait_decl`
  bock-parser:3889; `ExportKind::Trait` exports.rs:263/280; seed_imports seeds
  traits). A Bock-source `Error` trait resolves with NO type-system change.
- "Phase E — Stdlib Bridging: Complete" is the checker↔`bock-core` interpreter
  method registry, NOT the module stdlib (snapshot.md:53). The loading wiring
  genuinely does not exist.
- Conformance harness discovers/parses inline `// TEST:`/`// EXPECT:` directives
  but does NOT execute; `Expectation::Output` parsed-but-unhandled. No
  `.expected` sidecars. `tools/scripts/run-conformance.sh` absent (Q-fconf).

---

## 1. Loading mechanism (THE central decision)

**Decision: source-compiled into the existing registry (reuse the proven
multi-file pipeline), with stdlib sources embedded in the `bock` binary.**

- Rejected (B) prebuilt/serialized registry — needs a serialization format for
  `ModuleExports`/`TypeRef` + cache invalidation; defer as a later optimization
  once all 11 modules exist and recompile cost is measured.
- Distribution: **embed via `build.rs` + `include_dir`** (compile
  `stdlib/core/**/*.bock` into the binary as `&'static str`) — hermetic, matches
  §18.1 "ships with the compiler", no install-layout discovery to get wrong —
  **plus a dev-only `$BOCK_STDLIB` path override** for a fast stdlib edit loop.

**Shape:** new `compiler/crates/bock-cli/src/stdlib.rs` (`core_sources()` returns
the embedded set) shared by all three commands; **prepend parsed stdlib sources
to the parsed-files set before the user-file loop** so they flow through the
existing dep-graph/topo/compile/`register` path and land in the registry before
user modules. `module core.error` → dep-graph derives the id → `use core.error`
matches, no special-casing.

Files: NEW `bock-cli/src/stdlib.rs`, `bock-cli/build.rs`; EDIT `main.rs`
(`mod stdlib;`), `check.rs` (~:271), `build.rs` (~:96), `run.rs` (~:55),
`Cargo.toml` (include_dir).

**STOP-and-surface gate:** if prepending stdlib sources does NOT make
`use core.error.{Error}` resolve (module-id mismatch / import-form gap / ordering),
STOP — that's a larger registry lift, not a discovery-wiring task. Verify the
named-import form first (`ImportItems::Module` qualified access is currently
unsupported in seed_imports).

## 2. `core.error` surface (minimum-useful, pure Bock)

`stdlib/core/error/error.bock`, `module core.error`: `public trait Error` with
`message(self) -> String`; `public record SimpleError { message: String }` with
`impl Error for SimpleError`; `public fn error(message: String) -> SimpleError`.
**No per-target shim** (pure trait/record/impl) — the deliberate reason to pilot
`core.error`: defers all shim infrastructure to a later module that needs a host
primitive. `cause()`/§18.5/Displayable participation deferred (escalated surface
question — minimal surface is the safe default).

## 3. Verification (given the harness execution gap)

Pilot gates on: (1) **type-check** — a fixture `use core.error.{...}` →
`// EXPECT: no_errors` passes once loading works; (2) **compile-to-target** —
`bock build -t <t> --source-only` succeeds for each of js/ts/py/rs/go (proves
codegen for the stdlib module on all 5 targets without needing 5 toolchains).
(3) **actually run** is best-effort on available toolchains. Full per-target
conformance EXECUTION (running `// EXPECT: output` + diffing) is the separate
**Q-fconf** task — explicitly DEFERRED; coupling it here would bury the
loading-mechanism risk. Verification deliverable: a `bock-cli` integration test
(runs in `cargo test --workspace`), not an untracked script.

## 4. Conformance fixtures

`compiler/tests/conformance/stdlib/error/` (inline-directive format):
`error_trait_resolves` (named import, no_errors), `error_construct_and_use`
(no_errors), `error_output_smoke` (`// EXPECT: output "boom"` — parsed-but-not-
executed today, staged for Q-fconf; commented as such).

## 5. Task breakdown (loading mechanism front-loaded)

- **T1** spike loading with one hardcoded source + a `bock-cli/tests/` integration
  test asserting `use core.error.{Error}` checks clean. **Early STOP point.**
- **T2** finalize `core.error` surface; `bock check` over `stdlib/` clean.
- **T3** wire `build.rs` + `run.rs` to the same injection.
- **T4** bundle via `build.rs`+`include_dir`; `$BOCK_STDLIB` override; prove a
  built binary resolves from a non-repo cwd.
- **T5** conformance fixtures load (no LoadError).
- **T6** cross-target `--source-only` integration test for js/ts/py/rs/go.
- **T7** `stdlib/CLAUDE.md` `core/` section.

## 6. Decision classification

**Settled by Design (not relitigable):** the 11 v1 modules at minimum-useful
surface (DQ5); `core.error` centers on the `Error` base trait (§18.3); acceptance
= conformance + a representative example compile/run on every target; R1/R2/R3.

**Orchestrator/engineer implementation decisions (made here):** source-compiled +
embedded loading + `$BOCK_STDLIB` override; the `stdlib.rs` prepend integration;
the concrete `SimpleError`/`error()` helpers; pilot verification = type-check +
`--source-only` (execution → Q-fconf); no shim infrastructure in the pilot.

**Genuine CORE-SPEC questions — ESCALATED to Design (see escalations.md /
design-questions.md DQ6–DQ8):**
1. Should §18 normatively state core modules are Bock source compiled with the
   program + per-target runtime shims for host primitives, distributed embedded
   in the compiler? (The model lives only in tracking-level Design notes; the
   spec §18 doesn't state it, and `stdlib/CLAUDE.md`'s shim path is already wrong.)
2. The canonical v1 `core.error` surface — does `Error` carry `cause(self) ->
   Optional[Error]`, and does it participate in §18.5 / `Displayable`? (§18.3 says
   only "base trait"; pilot uses the minimal surface.)
3. Does v1 require module-qualified `use core.error` access? `seed_imports`
   currently skips `ImportItems::Module`; supporting it is a type-checker change
   affecting all 11 modules. (Pilot relies on named imports, which work.)

**Flagged, not blockers:** conformance execution → Q-fconf; "don't emit stdlib
into every user project's build" → ItemB/project-mode concern; serialized-registry
caching → later perf; stale `compiler/CLAUDE.md` `.expected` description.

---

### Critical files
- `compiler/crates/bock-cli/src/check.rs` (shared compile loop; injection ~:271)
- `compiler/crates/bock-cli/src/build.rs` (codegen + `--source-only`; injection ~:96)
- `compiler/crates/bock-cli/src/run.rs` (interpreter path; injection ~:55)
- `compiler/crates/bock-types/src/seed_imports.rs` (no type-system change needed;
  `ImportItems::Module` unsupported)
- `stdlib/core/error/error.bock` (the pilot's product — does not yet exist)
