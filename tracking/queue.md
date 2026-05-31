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

_Last reconciled: 2026-05-30 vs main c9a241e (post #129 + the codegen-completeness audit:
#123-#129 merged; core.iter PAUSED behind the codegen-completeness milestone; repo wins). See audit.md._

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

- **[Q-codegen-completeness] Codegen completeness across all 5 backends** — impl ·
  **v1-BLOCKING MILESTONE** (operator-decided 2026-05-30 "proceed comprehensive fix"; ~10-15 PRs, phased,
  mostly `compiler/crates/bock-codegen/` → SEQUENTIAL per crate-granularity) · links DV12-DV15, DV10/DV11,
  DQ14/DQ15/DQ18, #129, the 3-agent audit (audit.md 2026-05-30 18:00) · note: the audit established the v1
  codegen substrate is materially incomplete for the stdlib's real needs (all-5-green slice is narrow).
  PHASES: **P0 foundations DONE** — tail-`if`-in-loop (#131, DV15); cross-module `use` via single-file
  bundling of reachable modules (#132, DV13); user-enum codegen / variant registry (#133, DV14). [§20.6.1
  bundling-divergence → DQ19/Design.] **P1 stdlib types DONE** (#135 Python lambdas/generics · #136 Go/TS/Rust generics [DV12 resolved] · #137
  recv_kind annotation + primitive-bridge · #138 Result runtime + Optional/Result methods; `expr?` deferred → DQ20). **P2 traits+match DONE** (#140 trait self/defaults/bounded-dispatch — `use core.compare` runs ×5 · #141
  Self-subst · #142 match guards/or/nested/tuple). **P3 Go collection
  typing DONE** (#144 Go List/Map/Set element typing + record-spread + Self-in-plain-impl · #145 Map/Set method
  dispatch + literals + range()). Collections work ×5.
  **P4 polish** — tuple `.N` parser; Optional-interp; Int/Int + Bool-interp harmonize; mutating-List guard
  (DQ18). SUBSUMES prior codegen follow-ups (Q-match-exprpos, Q-go-list-literal, Q-ts-generic-impl,
  Q-self-subst, Q-prim-assoc). Q-list-codegen READ-ONLY methods DONE (#129); mutating → P4. **Phases 0-3 DONE (#131-#145); collections work ×5;
  the codegen substrate is essentially built. Phase 4 (polish) NEXT** (design in flight, Plan agent a0f6b8f2):
  codegen-only = tuple `.N` parser, expr-position match (Q-match-exprpos), Go nested-payload typed-arith [#142],
  TS Self-in-plain-impl [#141], Int/Int + Bool-interp harmonize; design-gated = DQ18 (mutating List/Map/Set),
  DQ20 (`expr?`), DQ22 (bare `m.contains`). **Likely NONE of P4 gates R1** (iter uses concat not push; no expr?),
  so Q-stdlib R1 (iter, effect) can resume after/alongside P4. Operator chose "continue P3→P4" (P2/P3 checkpoint).
- **[Q-stdlib] Implement the core standard library** — impl ·
  **v1-BLOCKING** (3/11 landed — but those 3 are check-only, NOT executed cross-module [DV13]; R1 PAUSED
  behind **Q-codegen-completeness**: the for→Iterable desugar is PROVEN [T1 green ×5], but the stdlib needs
  cross-module wiring + user-enums + generics + List-backed iter codegen first) ·
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
[LANDED: … #121 (DV9) · #123 vscode-CI · #124 TS codegen · #125 changelog ·
 #126 Py-Optional+Go-typed-payload · #127 Go match-in-loop · #129 read-only List methods]
Q-codegen-completeness (MILESTONE: cross-module + user-enums + generics + Result + traits + Go-typing + …
  — v1-BLOCKING, phased P0→P4, mostly bock-codegen → SEQUENTIAL) ──┐ gates ↓
Q-stdlib R1 (iter, effect) → R2 (option/result/string/time) → R3 (collections/test) ──→ D4 ──→ D5 ──→ ItemB (P1 → P2-5 → P6) ──→ ItemD
  ⮑ R1/R2/R3 ALL blocked behind Q-codegen-completeness (audit: cross-module + enums + generics broken; the 3 "landed" modules are check-only)
  ⮑ iter: for→Iterable desugar PROVEN (T1 ×5); resumes after the milestone's P0/P1
(decided-ready: Q-import-reject [DQ8])
(subsumed by Q-codegen-completeness: Q-self-subst, Q-prim-assoc, Q-match-exprpos, Q-go-list-literal, Q-ts-generic-impl)
(separate bugs: Q-xmod-bounds, Q-xmod-impl, Q-interp-enum)
```

**Critical path to v1.0 (2026-05-30, updated):** the Optional-payload codegen family is CLOSED across all 5
(#124/#126/#127) and the for→Iterable desugar is PROVEN — but `core.iter` (a sensitive probe) exposed that
the v1 codegen substrate is materially incomplete: a **3-agent audit** found **cross-module `use` and
user-defined enums broken on ALL 5**, and Result/generics/closures/Optional-methods broken on 3-4/5
(audit.md 2026-05-30 18:00). The "5-target parity" #114-#121 restored was real only for a narrow slice; the
3 "landed" stdlib modules are **check-only, never executed cross-module**. Operator decided (2026-05-30): a
**codegen-completeness MILESTONE** (`Q-codegen-completeness`, v1-BLOCKING, ~10-15 PRs, phased P0-P4, mostly
bock-codegen → sequential) — fix comprehensively, THEN resume the stdlib. Updated path:
**Q-codegen-completeness (P0 cross-module+enums+tail-`if` → P1 stdlib-types → P2 traits+match → P3 Go-typing
→ P4 polish) → Q-stdlib R1 (iter, effect) → R2 → R3 → D4 → D5 → ItemB**. Phase-0 design in flight.
