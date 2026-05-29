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

_Last reconciled: 2026-05-29 vs main fd2250e (+ impl-chat inventory;
repo wins). Most prior items landed this session — see audit.md._

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

## v1-blocking

- **[Q-stdlib] Implement the core standard library** — impl ·
  **v1-BLOCKING** · `stdlib/` · blocked-by: DQ5 (core-module scope —
  escalated to Design) · links DV1, MS-stdlib · note: **DECIDED a v1
  deliverable** (operator, 2026-05-29). §18.3 lists ~15 core.* modules;
  `stdlib/` is empty (0 modules; prelude = ~9 builtins). Large, phased.
  The work is scheduled for v1; the precise core-module SCOPE is a
  core-spec question escalated to Design (DQ5 / escalations.md) —
  phase planning proceeds when that returns. Don't block other work on it.

## Blocked

- **[D4] Stdlib reference docs** — docs · blocked · `docs/src/reference/`
  · blocked-by: Q-stdlib · note: scaffolding-only until stdlib lands
  (a stub exists); the real reference cycle follows the implementation.
- **[Q-perf-example] Fix @performance literal in context-audit example**
  — bug · blocked · `examples/spec-exercisers/context-audit/` ·
  blocked-by: DQ2 · note: `@performance(max_latency: 100, ...)` bare
  ints → E8003 (checker wants unit-suffixed). Fix once DQ2 decides the
  §11.4 literal syntax. Pre-existing; ungated.
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
Q-stdlib ──→ D4 ──→ D5 ──→ ItemB (P1 → P2-5 fan-out → P6) ──→ ItemD
DQ2 ──→ Q-perf-example
(independent / ready: Q-cl-dates, Q-cl-0515, Q-20.1-xref, Q-vscode-test, Q-fconf)
```

**Critical path to v1.0:** Q-stdlib (now DECIDED v1-blocking) → D4 →
D5 → ItemB. The earlier "ship what's done" vs §18-full-stdlib tension is
resolved in favor of shipping the core stdlib in v1; the remaining open
piece is its core-module SCOPE (DQ5, escalated to Design — see DV1).
