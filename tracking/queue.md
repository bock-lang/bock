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

_Last reconciled: 2026-05-30 vs main 70f1b80 (post the core.iter codegen-residue
block: #123-#127 merged; core.iter BLOCKED on Q-list-codegen + DQ16; repo wins). See audit.md._

---

## Ready

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
- **[Q-match-exprpos] Expression-position statement-arm match lowering** — impl ·
  ready · `compiler/crates/bock-codegen/` · — · links #121, #127 · note: #121 fixed
  statement-POSITION matches with statement arms (all 5). The expression-position case
  (`let x = match … { _ => return }` yielding a value) needs a temp-hoist desugar on
  Go/Py/JS/TS. #127 found the Go variant: an expr-position Optional/`if` lowers to a
  `func() interface{}{…}()` IIFE whose `interface{}` result can't be assigned to a
  concrete type (incl. block-tail `Some` in an `if` branch). Off the for-desugar path.
- **[Q-stdlib-fmtcheck] Enable `fmt --check` on stdlib `.bock`** — chore · ready ·
  `.github/workflows/`, `stdlib/` · — · links #119 · note: now that `bock fmt`
  emits valid Bock (#119), the stdlib `.bock` files (hand-authored to avoid the old
  mangling) can be `bock fmt`'d + `--check`'d in CI. Format them once + add a check.
- **[Q-go-list-literal] Go native `for x in [literal]` element typing** — bug · ready ·
  `compiler/crates/bock-codegen/` (go.rs) · — · links #127, DV11 · note: `for x in [1,2,3]`
  emits `for _, x := range []interface{}{...}`, so `x` is `interface{}` and typed use fails.
  Emit a typed slice + typed range var. Same `interface{}` family as #127's Optional fixes;
  js/python/rust fine. Found by core.iter v3.
- **[Q-ts-generic-impl] TS generic impl-target `self` typing drops generic args** — bug · ready ·
  `compiler/crates/bock-codegen/` (ts.rs) · — · links #124 · note: `impl Box[T]` types `self` as
  `Box` not `Box<T>` (`type_expr_to_string` drops generic args) — imprecise, not implicit-any; no
  fixture exercises it. Minor follow-up from #124.

## v1-blocking

- **[Q-list-codegen] List built-in method codegen across all 5 backends** — impl ·
  **v1-BLOCKING** · `compiler/crates/bock-codegen/` (js/ts/py/rs/go + generator) ·
  ESCALATED (scope/roadmap — operator; escalations.md 2026-05-30 15:24) · links DV10, DQ16,
  #127 · note: List built-in methods (`.len()`/`.get(i)`/`.push(x)`/`is_empty`/…) DO NOT
  codegen on ANY target — emitted verbatim (`recv.len()`), no backend lowers them to native
  ops (verified all 5 + by source: no List-method dispatch in bock-codegen). Gates core.iter's
  List-backed `ListIterator`+combinators AND core.collections (R3) AND any List-using module.
  Substantial workstream — plan-first. Surfaced by core.iter v3 (2026-05-30); latent because
  the 3 landed modules (error/compare/convert) were List-free.
- **[Q-stdlib] Implement the core standard library** — impl ·
  **v1-BLOCKING** (3/11 landed; R1 `iter` BLOCKED on Q-list-codegen + DQ16 — the for→Iterable
  desugar is PROVEN [T1 green on all 5 targets], but the DQ12 List-backed module surface needs
  List-method codegen first) ·
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
[LANDED: … #121 Q-codegen-fixes (DV9) · #123 vscode-CI-test (Q-ci-vscode-test) ·
 #124 Q-ts-codegen · #125 changelog-hygiene (Q-cl-dates/Q-cl-0515) ·
 #126 Q-py-optional + Go-typed-payload · #127 Go match-in-loop codegen]
Q-list-codegen (List built-in method codegen ×5 — NEW v1-BLOCKING, ESCALATED) ──┐ gates ↓
Q-stdlib R1 (iter ⟂ effect) → R2 (option/result/string/time) → R3 (collections/test) ──→ D4 ──→ D5 ──→ ItemB (P1 → P2-5 → P6) ──→ ItemD
  ⮑ iter: for→Iterable desugar PROVEN (T1 green ×5); BLOCKED on Q-list-codegen + DQ16 (floor: List-backed vs List-free)
  ⮑ collections (R3) + any List-using module also depend on Q-list-codegen
(decided-ready: Q-import-reject [DQ8])
(bugs/follow-ups: Q-self-subst, Q-xmod-bounds, Q-xmod-impl, Q-prim-assoc, Q-interp-enum, Q-match-exprpos [+Go expr-pos #127], Q-go-list-literal, Q-ts-generic-impl)
```

**Critical path to v1.0 (2026-05-30, updated):** the Optional-payload codegen family is now
CLOSED across all 5 targets (#124 TS · #126 Python+Go-typed-payload · #127 Go match-in-loop)
and the for→Iterable desugar is PROVEN (T1 green ×5) — but `core.iter` revealed a deeper gate:
**List built-in methods don't codegen on any backend (Q-list-codegen, v1-BLOCKING, ESCALATED)**,
which also gates `core.collections` and every List-using module. core.iter additionally awaits
Design on **DQ16** (List-backed vs List-free R1 floor). Updated path: **resolve Q-list-codegen
(+ DQ16) → Q-stdlib R1 (iter, effect) → R2 → R3 → D4 → D5 → ItemB**. The "5-target parity"
#114-#121 restored was real for the constructs tested but rested on fixtures that never
exercised realistic desugar shapes (method-call scrutinees, statement-position match-in-loop,
mut-self iterators, List methods); conformance is now materially deeper (55+ exec pairs). Quality
follow-ups (Q-xmod-*, Q-go-list-literal, Q-ts-generic-impl, Q-match-exprpos) land alongside.
