# `bock check --strict` + context-validation scope

**Date:** 2026-05-29
**Affects:** §20.1 (`bock check`), §20.1.1 (`bock check` Aspect Surface)
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
- **FOUND: `bock_air::interpret_context` does not interpret
  module-level annotations.** `interpret_context` extracts
  annotations only from item nodes (functions, records, enums,
  classes, traits, impls, effects, type aliases, consts), not from
  the `Module` node. Consequently module-level `@context` does not
  populate the module node's context block, so the
  production-mode module-completeness rule (E8014) fires for
  *every* module under `--strict` regardless of any module-level
  `@context`. This is a pre-existing `bock-air` gap, left
  unchanged here (out of this change's scope); it should be fixed
  in a follow-up so module-level `@context` suppresses E8014.
