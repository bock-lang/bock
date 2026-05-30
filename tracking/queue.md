# Queue — active work

**The one question:** what work is to-be / being done?

Orchestrator-owned. Actionable items only (impl / spec / docs / chore /
bug). Factual spec↔impl mismatches live in `divergences.md`; undecided
behavior in `design-questions.md`; version mapping in `milestones.md`;
present-state in `snapshot.md`. Each item has a stable ID, named once
here and referenced elsewhere. Raw OPEN/FOUND tags arrive via PR
descriptions; the orchestrator triages them into the right file.

Schema: `[ID] title — type · status · owned-files · blocked-by ·
links · note`. Status ∈ {ready, in-flight, blocked, deferred}.

_Last reconciled: 2026-05-29 vs main 7b478fb (post-#100 Design Q1/Q2/Q3
reconciliation; repo wins). See audit.md._

---

## Ready

- **[Q-cl-dates] Changelog date hygiene** — chore · ready ·
  `spec/changelogs/` · — · note: 8 filename-date vs content-`Date:`
  mismatches (20260420-1400, 20260423-1830, 20260506-0900,
  20260506-1630, 20260510-2100, 20260510-2300, 20260512-1700,
  20260512-1700-k04-handoff) + 1 missing-date file (20260304-1629).
  Decide filename-wins vs content-wins and align; preserve history.
- **[Q-cl-0515] Fix 0515 changelog factual error** — chore · ready ·
  `spec/changelogs/20260513-0515-specs-changes.md` · — · note: replace
  the non-parsing `with Logger as { log: (msg) => print(msg) }`
  "works today" example (lines 13, 44-48, 81) with a working Form A
  snippet (`record … impl … handling (Log with …)`) per the 0540
  verification. Changelog-body only; spec §10.4 is correct.
- **[Q-20.1-xref] §20.1 cross-reference doc-sync** — spec · ready ·
  `spec/bock-spec.md` · — · links #92 · note: §17.2 (`--optimize`),
  §15 (`--no-tests`), §10.8/§10.4 (`override --promote <id>`) still
  cite pre-reconciliation forms; align to the §20.1 surface from #92.
  Editorial; each already points at §20.1 as normative.
- **[Q-vscode-test] VS Code extension test infra** — chore · ready ·
  `extensions/vscode/` · — · note: no `test` script and no test files;
  current gate is compile + lint only. Add a minimal test setup.
- **[Q-fconf] Wire conformance execution + run-conformance.sh** —
  impl · ready · `compiler/tests/`, `tools/scripts/`,
  `.claude/commands/project/run-conformance.md` · — · note: the
  harness only parses/discovers fixtures (no compiler-phase / per-target
  execution); `tools/scripts/run-conformance.sh` is referenced by
  CLAUDE.md + the `/project:run-conformance` skill but does NOT exist.
  Create the runner + wire fixture execution + fix both references.
- **[Q-fmt-bock] `bock fmt` emits invalid Bock** — bug · ready ·
  `compiler/crates/bock-cli/` (fmt path) · — · note: `bock fmt` strips `///`
  doc comments and rewrites `public`→`pub` (not valid Bock), mangling stdlib
  `.bock` sources (error.bock/compare.bock are hand-authored to avoid it). A
  formatter producing invalid output is a real CLI bug. Found #104.
- **[Q-interp-enum] interpreter: cross-module enum variant in stdlib impl body**
  — bug · ready · interpreter crate (`bock run` path) · — · note: `bock run` of
  a `main` calling a stdlib impl method whose body constructs an imported enum
  variant (e.g. `Ordering.Less`) → "undefined variable: Less"; the interpreter
  lacks the imported enum's variants in scope inside a cross-module stdlib impl
  body. Type-check + codegen handle it; execution doesn't. Found #104; relates to
  the execution story (Q-fconf).
- **[Q-self-subst] checker: `Self` not substituted in impl method sigs** — bug ·
  ready · `compiler/crates/bock-types/` · — · note: an impl writing
  `fn compare(self, other: Self)` → E4001 at call sites (`Point vs Self`); the
  checker doesn't substitute `Self`→concrete in impl method signatures. Workaround
  (used by core.compare): write the concrete operand type in impls, declare `Self`
  in the trait. Narrow gap; low urgency. Found #104.

## v1-blocking

- **[Q-stdlib] Implement the core standard library** — impl ·
  **v1-BLOCKING** (2/11 landed; fan-out paused on Q-bridge/DQ6) · `stdlib/`,
  `compiler/tests/conformance/stdlib/` · — · links DV1, MS-stdlib, DQ5,
  #100 · note: **DECIDED a v1 deliverable** (operator, 2026-05-29) and
  **SCOPE decided by Design 2026-05-29** (DQ5; §18.3 tiering reconciled in
  #100). v1 = **11 core modules** at minimum-useful surface: `option,
  result, collections, string, iter, compare, convert, error, effect,
  time, test`. Each module = `stdlib/core/<m>/` source + per-target runtime
  shims + conformance fixtures that compile/run on every shipping target.
  Three rounds: **R1** effect/error/compare/convert/iter · **R2**
  option/result/string/time · **R3** collections/test. Start with a
  **one-module pilot** to validate the per-module pattern AND the
  conformance-harness execution gap (Q-fconf) before fanning out.
  `core.types/math/memory/concurrency` are Reserved for v1.x. **Progress:**
  the loading mechanism + `core.error` landed (#103); `core.compare` landed
  (#104, validating generics — work with a `Self`-substitution caveat, see
  Q-self-subst). **Fan-out PAUSED** pending **Q-bridge** + Design's DQ6: #104
  confirmed stdlib trait impls cannot cover primitive types until the checker↔
  bock-core bridge exists, so further trait modules (convert/iter/effect) have
  low value until then. Plan: `plans/2026-05-29-stdlib-loading-error-pilot-plan.md`.

- **[Q-bridge] checker↔bock-core trait-impl bridge for primitives** — impl ·
  **v1-BLOCKING** · `compiler/crates/bock-types/` · blocked-by: DQ6 · links DV4,
  Q-stdlib · note: primitive receivers resolve methods via the hardcoded
  intrinsic table in `checker.rs::resolve_method_return_type` and never consult
  the user/stdlib trait-impl table, so `impl Comparable for Int` + a call site →
  E4001 (#104). Stdlib traits can't cover primitives until this lands — a
  near-universal prerequisite for a USEFUL core stdlib. Carries a coherence/
  precedence question (stdlib impl vs intrinsic) folded into DQ6; implement once
  Design rules the impl model.

## Blocked

- **[D4] Stdlib reference docs** — docs · blocked · `docs/src/reference/`
  · blocked-by: Q-stdlib · note: scaffolding-only until stdlib lands
  (a stub exists); the real reference cycle follows the implementation.
- **[D5] Contributor docs + cleanup** — docs · blocked · `docs/`,
  `docs/src/contributing.md` · blocked-by: D4 · note: its
  INVENTORY/SPEC-ALIGNMENT deletion scope is now ABSORBED by the
  tracking consolidation (PR3 deletes them); remaining = contributor-doc
  buildout.
- **[D2-polish] D2 language-reference final polish** — docs · blocked ·
  `docs/src/language/` · blocked-by: (D2-FOUND mostly resolved — verify)
  · note: most D2-FOUND rows resolved per spec revision; confirm residue.
- **[ItemB] Project-mode codegen (Phases 1-6)** — impl · blocked ·
  `compiler/crates/bock-codegen/` · blocked-by: D5 · links #28 · note:
  Phase 1 then per-target Phases 2-5 (sub-agent fan-out), Phase 6.
  Unblocks the §20.1-Reserved build flags (--deliverable/--no-tests).
- **[ItemD] /get-started project-mode evolution** — docs · blocked ·
  `docs/`, `website/` · blocked-by: ItemB Phase 6 · note: external-facing
  copy — escalate for approval.

## Deferred

- **[ItemC] /get-started AI configuration section** — docs · deferred ·
  trigger: real-world AI-usage characterization (post-launch).

---

## Dependency graph

```
[#103 foundation+error, #104 compare: LANDED]
DQ6 ──→ Q-bridge ──→ Q-stdlib fan-out (convert/iter/effect → R2 → R3) ──→ D4 ──→ D5 ──→ ItemB (P1 → P2-5 → P6) ──→ ItemD
(independent / ready: Q-cl-dates, Q-cl-0515, Q-20.1-xref, Q-vscode-test, Q-fconf)
(bugs, ready: Q-fmt-bock, Q-interp-enum, Q-self-subst)
```

**Critical path to v1.0:** Q-stdlib foundation + `core.error` + `core.compare`
have LANDED (#103/#104). The next gate is **Q-bridge** (← Design's **DQ6**):
#104 proved stdlib traits can't cover primitive types until the checker↔
bock-core bridge exists, so a *useful* stdlib runs through the bridge, not
around it. Module fan-out is PAUSED on it (more primitive-incapable trait
modules add little until then). After the bridge: fan out R1's remaining
modules → R2 → R3 → D4 → D5 → ItemB. The "ship what's done" vs §18-full-stdlib
tension stays resolved in favor of shipping the core stdlib in v1.
