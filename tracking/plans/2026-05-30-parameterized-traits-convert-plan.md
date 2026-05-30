# Implementation Plan: Parameterized-Trait Resolution + `core.convert` (Q-stdlib R1)

**Status:** approved + IMPLEMENTED (#110, main 04dd167). The infra-then-module
step for `core.convert` (3rd v1 module). Branch `feat/stdlib-convert`.

**Goal:** resolve parameterized traits (`From[T]`/`Into[T]`/`TryFrom[T]` keyed by
both implementing type and type argument, with the `From⇒Into` blanket), then
ship `core.convert` on top.

## The central gap (was)
The trait type-argument (`From[Int]` → `[Int]`) was **dropped at parse time**:
`parse_impl_header` kept only the path; `TypePath` had no args; AIR keyed the
trait by bare path. So `impl From[Int] for Float` and `impl From[String] for
Float` were indistinguishable. The #108 `ImplTable` keyed `(trait, type)` only.

## As built (#110)
- **Front end:** added `trait_args` to `ImplBlock` (AST + AIR) and captured the
  parsed args; `TraitDecl` correctly left alone (it parameterizes via
  `generic_params`). Ripple: 5 files / 9 construction sites (under the STOP
  threshold); the other 63 match sites use `..`.
- **Table:** `TraitRef.args`; a second `param_trait_impl_index` keyed
  `(trait, arg_key, target_key)` alongside #108's untouched index; 3-tuple
  coherence (E4010); `register_trait_impl_inner(args)` routes by emptiness.
- **Blanket:** `From[T] for U ⇒ Into[U] for T` synthesized in a second pass,
  `is_derived`, **skip-if-occupied** (explicit `Into` wins). No `TryInto`.
- **Resolution:** user `From`/`Into`, the blanket, **return-type-driven `.into()`**
  (new **E4012** replacing a prior *unsound* fresh-var fallthrough), `from`/
  `try_from`. Canonical primitive conversions: `Int→Float`, signed widening,
  `Float32→Float`, `Char→String`, `TryFrom[String] for Int/Float`. Narrowing
  excluded (lossy → TryFrom or deferred).
- **`where (T: Into[U])`:** arg-IMPRECISE fallback (the bound's `[U]` is dropped
  at parse — same root as DV7); satisfied when `T` has `Into` for some target.
  Verified positive + negative (E4005).
- `stdlib/core/convert/convert.bock`: `From`/`Into`/`TryFrom`/`Displayable` +
  `ConvertError`. Full gate green (incl. `cargo doc -D warnings`).

## Escalated to Design (DQ11 — shipped the floor, non-blocking)
Normative primitive-conversion matrix (parallels DQ10); whether canonical
conversions are sealed; `TryFrom` error type (shipped fixed `ConvertError`);
`TryInto` existence (omitted).

## OPEN findings (→ queue/divergences)
- Cross-module `.into()` doesn't resolve (impls don't cross module boundaries in
  the per-module checker; `.from()`/trait-methods do via method seeding) →
  `queue.md` Q-xmod-impl / `divergences.md` DV8 (pairs with DV7).
- Primitive associated calls (`Float.from(3)`) don't resolve (`.into()` is the
  primitive path) → Q-prim-assoc.
- Interpreter dispatch gaps (user associated fns, bodyless blanket `.into()`,
  builtin-shadowed `to_string`) — type-check + codegen handle them →
  folds into the interpreter-execution-gaps picture (Q-interp-enum).
- `Type.from(x)` uses dotted form (`::` doesn't parse in Bock) — informational.
