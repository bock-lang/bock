# `bock check --strict` + context-validation scope

**Date:** 2026-05-29
**Affects:** §20.1 (`bock check`), §20.1.1 (`bock check` Aspect Surface), §2 (Language Overview), §11.2 (`@context`), §11.7 (`@domain`), §11.8 (Context Composition), §15.3 (Application sites)
**Type:** addition / clarification

## Change

Two implementation-driven amendments to the `bock check` surface:

1. **`--strict` flag (§20.1, addition).** `bock check` gains a
   `--strict` flag that forces **production** strictness for the
   check, mirroring `bock build --strict`. Without it, the check
   runs at **development** strictness (the prior, unchanged
   default). The flag is a binary override, not the full
   sketch/development/production ladder — it does not read the
   project's configured strictness (that remains an explicit
   future option, surfaced as OPEN below). The exit-code rule is
   stated normatively for the first time: `bock check` exits
   non-zero **if and only if** the check produces at least one
   error; warnings never fail the check. Therefore completeness
   gaps are warnings (exit 0) at the default development
   strictness and errors (non-zero exit) under `--strict`.

2. **`context` aspect scope (§20.1.1, clarification).** The
   `context` aspect now explicitly runs two compiler-verified §11
   passes: capability (`@requires`) verification **and** the
   context-validation pass. The latter checks annotation
   consistency (security-level monotonicity, performance-budget
   validity, known capability/security names) plus **completeness**
   (public items and modules carrying `@context`), with
   completeness gated by strictness per the §1.4 ladder
   (sketch -> consistency only; development -> completeness
   warnings; production -> completeness errors). The clarification
   also records that PII/security context **composition** --
   cross-module leak detection -- is **Reserved for v1.x** (a
   dedicated security pass) and is *not* part of the `context`
   aspect in v1.

3. **v1 context-completeness is per-item; module-level
   completeness is Reserved for v1.x (§11.2, §11.7, §11.8,
   §15.3, §20.1.1 — reconciliation).** Module-level annotations
   on the `module` declaration are Reserved for v1.x (§15.3):
   v1 has no syntax to attach `@context` (or any annotation) to a
   `module`, and the parser rejects it. The context-completeness
   checks therefore cannot apply a *module-level* requirement in
   v1 — it would be unsatisfiable, firing on every module
   regardless of authored intent and making `bock check --strict`
   impossible to pass. The decision (design-decided on PR #87):
   in v1 the **module-level** completeness requirement is
   **dropped**; v1 completeness is **per-item** (every public
   declaration must carry `@context`), which is satisfiable. The
   module-level rule ships in v1.x alongside the Reserved
   module-level annotation syntax.

   Spec reconciliation accompanying the decision:
   - **§20.1.1** now states the `context` aspect checks **per-item**
     completeness (public declarations carrying `@context`), not
     "public items and modules", and cross-references §15.3/§11.8
     for why module-level completeness is Reserved.
   - **§11.8** gains an explicit "v1 context-completeness is
     per-item" note.
   - **§15.3** gains a forward note that the unsatisfiable
     module-level requirement is why v1 completeness is per-item.
   - **§11.2** (`@context`) and **§11.7** (`@domain`) examples
     previously showed `@annotation ... module <path>` —
     module-level application that §15.3 Reserves for v1.x and that
     v1 parsers reject. Both are rewritten to show the v1-valid
     per-declaration form and to mark the module-level form clearly
     as **Reserved for v1.x** (cross-referencing §15.3). The §2
     Language Overview example's dangling module-level `@context`
     block is likewise replaced with a clarifying comment.

## Rationale

The implementation (H1's `CheckOutcome`) already mapped errors ->
non-zero exit, but `bock check` hardcoded development strictness
with no way to escalate, and the §11 context-validation pass
(`bock_air::validate_context`) was implemented but unwired. O1
adds the `--strict` escalation; O2 wires `validate_context` into
the `context` aspect with the strictness mapping
Sketch->Lax, Development->Standard, Production->Strict. Together
they give `bock check --strict` a meaningful production gate:
completeness gaps that are advisory in development become blocking
in production, matching the develop -> ship workflow `bock build
--strict` already serves. Context **composition**
(`bock_air::compose_context`) is deliberately deferred so the v1
`context` aspect stays scoped to per-module, compiler-verifiable
checks; cross-module PII/security analysis is a larger design that
belongs in its own security pass.

## Migration

No source migration. Behavioral notes for tooling/CI:

- `bock check` (no flag) is unchanged in pass/fail terms: errors
  fail, warnings do not. Newly, the default check now *surfaces*
  context-validation completeness **warnings** for public items
  and modules missing `@context` (previously these were not
  reported by `bock check`). These do not change the exit code.
- `bock check --strict` is new and will fail (non-zero) on code
  with missing-context completeness gaps that previously passed
  under the implicit development strictness. This is the intended
  production gate.

## OPEN / FOUND

- **OPEN: §20.1 -- should `bock check` default to the project's
  configured strictness?** This change keeps `--strict` as an
  explicit override (matching `bock build`) and does *not* read
  `bock.project`'s strictness default. If `check` should instead
  honor the project's configured level by default, that is a
  separate design decision.
- **RESOLVED (was FOUND): module-level `@context` completeness
  was unsatisfiable in v1.** The original PR #87 noted that
  `bock_air::interpret_context` never populates a `Module` node's
  context block (annotations attach only to item nodes), so the
  production-mode module-completeness rules (E8014 in
  `validate_context`, E8022 in `bock_air::verify_capabilities`)
  fired on *every* module under `--strict` regardless of any
  module-level `@context`. The root cause is deeper than an
  interpretation gap: v1 has **no syntax** to attach `@context` to
  a `module` at all — that surface is Reserved for v1.x (§15.3).
  A module-level completeness requirement is therefore
  intrinsically unsatisfiable in v1. **Resolution (this
  follow-up):** drop the module-level completeness requirement in
  v1 rather than try to satisfy it. The E8014 module arm in
  `validate_context` and the E8022 emission in
  `bock_air::verify_capabilities` are removed (the latter's
  `is_complete()` no longer counts the module dimension; module
  counters remain as informational reporting only). Per-item
  completeness (E8013/W8013 on public items; E8023 on public
  functions) is unchanged. `bock check --strict` is now
  satisfiable: a module whose public items each carry `@context`
  passes clean. The module-level completeness rule returns in v1.x
  with the Reserved module-level annotation syntax.
