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

_Last reconciled: 2026-05-30 vs main 2b562e3 (post the codegen-correctness
workstream + the 5-way fan-out #114-#121; repo wins). See audit.md._

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
  snippet. Changelog-body only; spec §10.4 is correct.
- **[Q-import-reject] Reject bare module-qualified import** — bug · ready ·
  `compiler/crates/bock-parser|bock-types/` · — · links DQ8 · note: a `use` of a
  module path with neither a brace-list nor a wildcard (bare `use core.error`) is
  not a v1 form; reject with a diagnostic pointing at the braced form. Decided by
  DQ8; module-qualified access deferred to v1.x.
- **[Q-interp-enum] interpreter execution gaps for stdlib dispatch** — bug ·
  ready · interpreter crate · — · links #104, #110, #121 · note: PARTIALLY fixed
  by #121 (defect #5: method bodies now run with a globals-bearing env, so
  `Some`/`None`/top-level fns + imported enum variants resolve in method bodies —
  this likely closed the #104 `Ordering.Less` case). REMAINING (verify): the #110
  convert dispatch gaps — user associated fns, the bodyless blanket `.into()`,
  builtin-shadowed `to_string`. Re-test against #121; close or scope the residue.
- **[Q-self-subst] checker: `Self` not substituted in impl method sigs** — bug ·
  ready · `compiler/crates/bock-types/` · — · note: an impl writing
  `fn compare(self, other: Self)` → E4001 at call sites; the checker doesn't
  substitute `Self`→concrete in impl method signatures. Workaround: write the
  concrete operand type in impls. Narrow gap; low urgency. Found #104.
- **[Q-xmod-bounds] Cross-module where-bound enforcement** — bug · ready ·
  `compiler/crates/bock-types/` (export ABI) · — · links #108 · note: where-clause
  bounds on **imported** generic fns aren't enforced — `ExportedSymbol` carries no
  trait bounds. Locally-defined bounds enforce (#108); thread bounds through the
  export ABI. Pairs with Q-xmod-impl (DV7/DV8 cross-module-impl theme).
- **[Q-xmod-impl] Cross-module trait-impl resolution for `.into()`** — bug ·
  ready · `compiler/crates/bock-types/` · — · links #110, DV8 · note: `.into()`
  resolves via the impl-table, not seeded across modules — an `impl From[A] for B`
  in module X isn't visible to `.into()` in module Y. Seed the impl-table
  cross-module. Pairs with Q-xmod-bounds.
- **[Q-prim-assoc] Primitive associated calls (`Float.from(3)`)** — bug · ready ·
  `compiler/crates/bock-types/` · — · links #110 · note: the resolver doesn't
  treat a primitive type name as an expression value, so `Float.from(3)` doesn't
  resolve (`.into()` is the working primitive path). Minor usability gap.
- **[Q-ts-codegen] TypeScript codegen defects (self-methods, Optional typing)** —
  bug · ready · `compiler/crates/bock-codegen/` (ts.rs) · — · links #121 · note:
  TS self-methods emit `Point.prototype.m = function(self)` and TS `Optional`
  typing (`number | null` *type* vs the tagged-object *value*) both fail `tsc`.
  Pre-existing, surfaced by #121 (excluded from its fixtures, out of scope there).
  JS is fine (no type-checking); these are TS-specific.
- **[Q-py-optional] Python `Optional`/`Some`/`None` runtime** — bug · ready ·
  `compiler/crates/bock-codegen/` (py.rs) · — · links #121 · note: Python (like Go
  pre-#121) emits bare `Some`/`None`/`Optional[int]` with no runtime definition.
  #121 added the Go `__bockOption` runtime; do the analogous Python one (deferred
  fast-follow from #121).
- **[Q-match-exprpos] Expression-position statement-arm match lowering** — impl ·
  ready · `compiler/crates/bock-codegen/` · — · links #121 · note: #121 fixed
  statement-POSITION matches with statement arms (all 5 backends). The
  expression-position case (`let x = match … { _ => return }` yielding a value on
  non-diverging arms) needs a temp-hoist desugar on Go/Py/JS/TS. Deferred from #121.
- **[Q-ci-vscode-test] Wire `npm test` into the CI vscode job** — chore · ready ·
  `.github/workflows/ci.yml` · — · links #118 · note: #118 added the extension
  test infra + `npm test` (Mocha), but the CI `vscode extension` job runs only
  `npm ci`/`lint`/`compile`. Add a `npm test` step so the new tests gate PRs.
- **[Q-stdlib-fmtcheck] Enable `fmt --check` on stdlib `.bock`** — chore · ready ·
  `.github/workflows/`, `stdlib/` · — · links #119 · note: now that `bock fmt`
  emits valid Bock (#119), the stdlib `.bock` files (hand-authored to avoid the old
  mangling) can be `bock fmt`'d + `--check`'d in CI. Format them once + add a check.

## v1-blocking

- **[Q-stdlib] Implement the core standard library** — impl ·
  **v1-BLOCKING** (3/11 landed; R1 RESUMES with iter — codegen gate cleared) ·
  `stdlib/`, `compiler/tests/conformance/stdlib/` · — · links DV1, MS-stdlib, DQ5,
  #100 · note: v1 = **11 core modules** at minimum-useful surface (option, result,
  collections, string, iter, compare, convert, error, effect, time, test). Each =
  `stdlib/core/<m>/` source + per-target shims + conformance fixtures, compile/run
  on every target. **Landed:** loading mechanism + `core.error` (#103); `core.compare`
  (#104); the primitive-conformance bridge (#108); `core.convert` + parameterized
  traits (#110). **Codegen gate CLEARED:** Q-fconf execution conformance (#114/#115)
  + Q-codegen-fixes (#121, DV9) — 5-target parity now real + tested. **R1 RESUMES**
  with `iter` (generic `Iterator[T]`/`Iterable[T]`, eager combinator floor,
  for→Iterable desugar in the CHECKER + collection conformances; protocol shape =
  DQ12), then `effect` (effect-system bridge), then R2 (option/result/string/time),
  R3 (collections/test). `core.types/math/memory/concurrency` Reserved for v1.x.
  Plans: `plans/2026-05-29-stdlib-loading-error-pilot-plan.md`,
  `plans/2026-05-30-primitive-conformance-bridge-plan.md`,
  `plans/2026-05-30-codegen-correctness-conformance-plan.md` (done).

## Blocked

- **[D4] Stdlib reference docs** — docs · blocked · `docs/src/reference/`
  · blocked-by: Q-stdlib · note: scaffolding-only until stdlib lands
  (a stub exists); the real reference cycle follows the implementation.
- **[D5] Contributor docs + cleanup** — docs · blocked · `docs/`,
  `docs/src/contributing.md` · blocked-by: D4 · note: its
  INVENTORY/SPEC-ALIGNMENT deletion scope is now ABSORBED by the
  tracking consolidation; remaining = contributor-doc buildout.
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
[LANDED: #103 found+error · #104 compare · #108 bridge · #110 param-traits+convert ·
 #114/#115 Q-fconf exec conformance · #116 rust-cache · #117 §20.1 · #118 vscode-test ·
 #119 fmt · #120 prelude · #121 Q-codegen-fixes (DV9 closed)]
Q-stdlib R1 (iter → effect) → R2 (option/result/string/time) → R3 (collections/test) ──→ D4 ──→ D5 ──→ ItemB (P1 → P2-5 → P6) ──→ ItemD
  ⮑ iter (for→Iterable desugar in CHECKER + collection conformances; DQ12); effect (effect-system bridge)
(decided-ready: Q-import-reject [DQ8])
(bugs/follow-ups: Q-self-subst, Q-xmod-bounds, Q-xmod-impl, Q-prim-assoc, Q-interp-enum, Q-ts-codegen, Q-py-optional, Q-match-exprpos)
(ci/chore: Q-cl-dates, Q-cl-0515, Q-ci-vscode-test, Q-stdlib-fmtcheck)
```

**Critical path to v1.0 (2026-05-30):** the **codegen-correctness gate is CLOSED** —
Q-fconf execution conformance (#114/#115) + Q-codegen-fixes (#121, resolving DV9)
restored and *tested* the v1 "5-target parity" property (it was false + untested
before). 3/11 core modules landed; `core.iter` is unblocked. Remaining critical
path: **Q-stdlib R1 (iter, effect) → R2 → R3 → D4 → D5 → ItemB**. The ready
follow-ups + bugs (left column) can land alongside; the cross-module-impl gaps
(Q-xmod-*) and the TS/Python codegen gaps (Q-ts-codegen/Q-py-optional) are quality
items that should close before v1.0's parity claim is fully airtight.
