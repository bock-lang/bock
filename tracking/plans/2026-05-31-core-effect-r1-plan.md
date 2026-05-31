# Plan: `core.effect` (v1) — de-risking + Design questions

**Date:** 2026-05-31
**For:** Q-stdlib R1 — `core.effect` (the 5th of 11 v1 core modules)
**Status:** scoped; Design questions (DQ25) escalated; feasibility probe dispatched; build PENDING Design's floor decision
**Designed by:** Plan agent (2026-05-31)

> `core.effect`'s v1 surface is **under-specified** (§18.3:1728 = "Effect system
> primitives" only; no §18.3.x subsection). The effect *machinery* (§10) is fully
> implemented (effects.rs ~1112 lines; effect codegen ×5; 7 effect conformance
> fixtures) and cross-module-wired at the resolve layer. The risk is (a) the SURFACE
> (Design) and (b) cross-module effect **execution** on Rust/Go — never proven (all
> effect fixtures are check-only; none carry a `// EXPECT: targets` directive). This
> is the core.iter lesson: a gap that only surfaces at cross-target *execution*.

## Investigation findings (grounded)

1. **Under-specified surface** — §18.3:1728 vs the fully-enumerated §18.3.1 core.time.
   "What does the floor contain" is a genuine Design question (→ DQ25).
2. **Machinery implemented + resolve-layer cross-module-wired** — `seed_imports.rs`
   handles `ExportKind::Effect` (ops + composite components); `resolve.rs` seeds
   effect info for Named + Glob imports so importing-module `with` clauses inject ops.
3. **Codegen bundling structurally supports it** — `reachable_modules` (generator.rs)
   bundles any module reached by a real `use` edge in dep order; all 5 backends build a
   single EmitCtx and emit bundled modules in order, so `effect_ops` (op→effect map)
   persists to the user call site where the `log(...)→handler.log(...)` rewrite consumes it.
4. **BUT cross-module effect EXECUTION never proven on Rust/Go** — exec harness scans
   only `conformance/exec/`; every effect fixture is in `conformance/effects/` (check-only,
   no `targets`). Specific hazard: backends spawn fresh sub-contexts (rs.rs:2917/2128,
   py.rs:1973, ts.rs:2213, go sub-ctxs) that do NOT inherit `effect_ops`/`current_handler_vars`
   — so an op called inside interpolation / an impl body / a sub-context may fail to rewrite
   on Rust/Go. Exactly the class of defect that bit core.iter only at execution.
5. **No effect name in the §18.2 prelude** (spec:1708) — so on the safe default `core.effect`
   needs NO `seed_imports.rs` change (symbols via explicit `use core.effect.{...}`). **This
   avoids the P4-hygiene conflict on seed_imports.rs/checker.rs.**

## Design questions → Design (DQ25; do NOT decide here)

Q1. Floor = effect-system **primitives** (vocabulary + one worked handler pattern) vs a
   library of concrete effects? *Rec default: primitives-only* (§10.4:897 fixes one v1 handler
   form; §18.3:1716 "minimum-useful subset").
Q2. Include a standard **`Log`** effect (`fn log(message: String) -> Void`) as the canonical
   executable example — **conditioned on the feasibility probe passing ×5**? *Rec: yes iff
   feasible; else Reserve.* THE most consequential question (decides if the floor has a runnable effect).
Q3. Do ambient `Panic`/`Allocate` (§10.5:934, "always available without declaration") need any
   module surface? *Rec: no — stay compiler-intrinsic.*
Q4. `Clock`/`Cancel` — confirm `core.effect` owns NEITHER (§18.3.1:1741 core.time owns `Clock`;
   `Cancel` §13.5 partly Reserved). *Rec: out.*
Q5. Effect-handler utility traits/types? *Rec: none in v1 (no spec-defined Handler supertrait).*
Q6. Composite effects (§10.1:857)? *Rec: not in the floor (premature with ≤1 effect).*
Q7. Explicitly Reserved-for-v1.x in core.effect docs: adaptive handlers (§10.8), lambda handler
   constructors (§10.4:917), Layer-3 project defaults (§10.3:893). *Rec: restate inline.*
Q8. Acceptance bar (§18.3:1716 = conformance + a representative example compile+run ×5): for a
   primitives-only floor, what is the runnable representative example? *Rec: the cross-module
   effect exec fixture — which requires Q2 feasible.*

## Proposed floor (`stdlib/core/effect/effect.bock`) — gated on the probe

**Variant A (preferred, if feasibility passes ×5):** ship `effect Log { fn log(message: String) -> Void }`
+ `record ConsoleLog {}` + `impl Log for ConsoleLog { fn log... println("[log] ${message}") }` +
`fn console_log() -> ConsoleLog`. (The exact shapes proven in `handler_record_impl.bock`, now exported +
driven cross-module.) Only host primitive = `println` (proven ×5).

**Variant B (fallback if feasibility fails):** primitives/types + docs only (the module exists,
embedded-loads, documents the v1 handler form + Reserved items); Reserve the executable `Log` + FOUND.

## Phasing (incremental value; never STOP — core.iter discipline)

- **Phase 0 (MUST-RUN feasibility gate):** scaffold a throwaway two-file program — `public effect Log`
  in an aux module, `use`+handle from `main`, `targets js, ts, python, rust, go` — build+run on each.
  Proves/disproves cross-module effect execution on Rust/Go. Selects Variant A vs B. *(Dispatched as an
  investigation probe NOW — independent of Design, informs Q2/Q8.)*
- **Phase 1 (always lands):** `effect.bock` (Variant per Phase 0 + Design Q1/Q2) + directive conformance
  (`conformance/stdlib/effect/`). No seed_imports change on the default.
- **Phase 2 (Variant A only):** the cross-module exec fixture (`exec_core_effect_cross_module.bock`,
  analog of `exec_for_user_iterable`) + any Rust/Go sub-context `effect_ops`-propagation codegen fix
  (the core.iter rust/go fast-follow analog).

**Fallback:** if cross-module effects are an unclosed codegen gap → ship Variant B + a precise FOUND
(which backend / which sub-context drops `effect_ops`); do NOT STOP.

## Conformance
- Directive (`conformance/stdlib/effect/`): `effect_module_no_errors.bock`, `effect_console_log_output.bock`.
- Exec (`conformance/exec/`): `exec_core_effect_cross_module.bock` (cross-module `use core.effect.{Log,...}`
  + `with Log` propagation + record-impl handler + `println` interpolation — ×5). Variant A only.

## Owned-files (engineer session)
- `stdlib/core/effect/effect.bock` *(new)*
- `compiler/tests/conformance/stdlib/effect/*.bock` *(new dir)*
- `compiler/tests/conformance/exec/exec_core_effect_cross_module.bock` *(new)*
- **Only if Phase-0 surfaces a defect:** `compiler/crates/bock-codegen/src/{rs,go,py,ts}.rs` (sub-context
  `effect_ops` propagation).
**Conflicts:** NONE with P4-hygiene on the safe default (no seed_imports.rs/checker.rs edit). If Design Q2
later wants an effect prelude name, that seed_imports.rs edit must SEQUENCE after P4-hygiene. build.rs/stdlib.rs
need no edit (glob-embedded). Codegen backends (Phase 2) are high-contention — coordinate.

## Risk / sequencing
Riskiest = Phase 0 (cross-module effect execution on Rust/Go; the sub-context `effect_ops`-drop hazard).
Static evidence is positive (single bundle ctx, ordered emission) but no effect program has ever run on
Rust/Go. Fallback = Variant B + FOUND, never STOP. Sequence: Phase 0 → variant → Phase 1 → Phase 2.
**Escalate Q1–Q8 to Design BEFORE Phase 1** so the floor is ratified, not guessed.

## Verification (pre-PR gate)
`cargo fmt --all -- --check` · `cargo clippy --workspace --all-targets -- -D warnings` ·
`cargo test --workspace` · `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features` ·
`tools/scripts/run-conformance.sh` · `BOCK_CONFORMANCE_REQUIRE=all` exec lane (the gate that runs the
cross-module effect fixture ×5 — the one that would have caught core.iter's Rust/Go defects).
