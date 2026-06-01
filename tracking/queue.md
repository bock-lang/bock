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

_Last reconciled: 2026-06-01 vs main 6a48848 (**D5 contributor docs DONE [#174]** → next critical path = ItemB
project-mode codegen, now UNBLOCKED. Quality-sweep Wave 1 also landed: **Q-conformance-clean-rebuild + Q-time-int64
[#175]**; **Q-r2-codegen-residue (c) builtin-vs-user-method shadowing [#176, ×5]** + pinned Q-go-list-literal /
Q-r2-(b) / Q-ts-generic-impl (verified already-fixed). New FOUND triaged: Q-allcaps-record-parse (parser),
Q-arch-doc-drift (ARCHITECTURE.md/compiler-CLAUDE.md/CONTRIBUTING.md crate-name drift). Q-match-exprpos still
deferred (deep). — earlier: D4 [#172]; ★ v1 STDLIB COMPLETE 11/11 ×5 ★. #123-#176 merged; repo wins). See audit.md._

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
  ready (deferred — deep) · `compiler/crates/bock-codegen/` · — · links #121, #127, #176 · note: #121 fixed
  statement-POSITION matches with statement arms (all 5). The expression-position case
  (`let x = match … { _ => return }` yielding a value) needs a temp-hoist desugar on
  Go/Py/JS/TS. **#176 re-confirmed** it is genuinely broken on go/py/js/ts (Rust correct): an expr-position match/if
  bound to a `let` with a control-flow arm captures the transfer inside the IIFE/lambda. The correct fix threads an
  "assign-to-target" mode through each backend's match-arm emitter — **cross-cutting across 4 backends**, so deferred
  (too deep for the residue sweep). Off the for-desugar path.
- **[Q-stdlib-fmtcheck] Enable `fmt --check` on stdlib `.bock`** — chore · ready ·
  `.github/workflows/`, `stdlib/` · — · links #119 · note: now that `bock fmt`
  emits valid Bock (#119), the stdlib `.bock` files (hand-authored to avoid the old
  mangling) can be `bock fmt`'d + `--check`'d in CI. Format them once + add a check.
- **[Q-go-list-literal] Go `for x in [literal]` element typing** — bug · **DONE (#176)** · note: verified
  already-fixed — Go emits `for _, x := range []int64{...}` (typed slice + typed range var); pinned by the existing
  `go_typed_list_iter.bock` fixture. (No code change; #176 confirmed + pinned.)
- **[Q-ts-generic-impl] TS generic impl-target `self` typing** — bug · **DONE (#176)** · note: verified
  already-fixed — TS emits `self: Box<T>` / `-> Box<T>`, compiles `--strict` clean; pinned by new
  `ts_generic_impl_self.bock` fixture. (No code change; #176 confirmed + pinned.)
- **[Q-iter-interp-mutself] Interpreter hangs on a `mut self` iterator drive** — bug · ready ·
  interpreter crate · — · links #151, #152 · note: a `loop { match it.next() }` drive over a
  `ListIterator` HANGS under the tree-walking interpreter — `mut self` cursor mutations don't persist
  across method calls, so `next()` never advances and `None` is never reached. Compiled targets (all 5)
  are fine; only `bock run` (interpreter) is affected. Pre-existing (the proven `generic_iter_concrete_match.bock`
  hangs identically) — NOT introduced by core.iter; surfaced by it. The `stdlib_iter.rs` smoke uses a single
  `next()` to avoid it. Fix: persist `mut self` field mutations across interpreter method-call frames.
  Same family as Q-interp-enum.
- **[Q-effect-op-node-lowering] Unhandled bare effect-op surfaces E1001, not E8020** — bug/diagnostic-quality ·
  ready (low-pri) · `compiler/crates/bock-air/` (lower.rs / verify_capabilities.rs) · — · links DV16, #155 · note:
  a genuinely-unhandled bare op (no handler, no `with`) surfaces resolver **E1001** "undefined name" rather than the
  capability-pass **E8020** "effect operation has no handler" — because `EffectOp` AIR nodes are constructed ONLY in
  test code, so the E8020 check (`verify_capabilities.rs:476`) never fires on surface bare-op `Call`s. #155 kept
  E1001 for v1 (correct compile-time error per §10.3; the code is non-normative). To unify: lower recognized bare
  unhandled op `Call`s into `EffectOp` nodes so E8020 fires with the proper message. Non-urgent UX polish.
- **[Q-effect-import-unused] Imported effect used only in `handling`/`with` position flagged W1001 unused** — bug ·
  ready (cosmetic, low-pri) · `compiler/crates/bock-air|bock-types/` · — · links #155 · note: when an imported
  effect (`use m.{Log}`) is referenced only in an effect position (`handling (Log with …)` / `fn … with Log`), the
  import binding isn't marked used → cosmetic `W1001 unused import`. Doesn't fail check/exec. Mark effect-position
  references as uses.
  (DONE this block → #155: Q-effect-interp-rust [Rust interpolation effect-op rewrite] + Q-effect-conformance-wiring
  [the inert effects/ suite now executes ×5]; DV16 RESOLVED.)
- **[Q-interp-effect-op-collision] Interpreter flat op-name→effect map can't disambiguate same-named ops** — bug ·
  ready (low-pri) · interpreter / `bock-cli/src/run.rs` · — · links #157 · note: the interpreter resolves bare effect
  ops through a FLAT op-name→effect-name map, so two effects sharing an op name (e.g. a user `effect Logger { fn log }`
  + the embedded `core.effect.Log { fn log }`) collide — only last-writer-wins. #157 made registration deterministic
  (topological order → user effects shadow core), which is correct + sufficient for v1, but full qualification (a
  program using BOTH same-named ops) is unsupported on the interpreter. Codegen (all 5 targets) is UNAFFECTED (each
  program compiles in isolation with proper module scoping). Low-pri interpreter-only limitation.
- **[Q-clock-handler-routing] `Instant.now`/`sleep` bypass the Clock effect handler** — bug · ready · `bock-codegen` ·
  — · links #160 · note: the time host primitives are inlined per-target and bypass the installed `Clock` handler, so
  `std.testing.MockClock` virtual-time (§18.4) is not achievable — `sleep` always hits real host. Route now/sleep/
  elapsed through the `Clock` handler. Codegen change; the time SURFACE works ×5 (core.time done) — this is the
  testability gap. Pairs with Q-time-shim-path.
- **[Q-conformance-clean-rebuild] Conformance harness doesn't force a clean `bock` rebuild** — chore/test-infra ·
  **DONE (#175)** · note: `run-conformance.sh` now `touch`es `compiler/crates/bock-cli/build.rs` + runs
  `cargo build -p bock --bin bock` before the tests, forcing a stdlib re-embed so `execution.rs::bock_binary()` can't
  reuse a stale sibling binary. Root cause confirmed: the build.rs `rerun-if-changed` on the stdlib tree misses a
  newly-added nested subdir. Local-verification false-REDs resolved.
- **[Q-r2-codegen-residue] R2 surfaced minor codegen/parser gaps** — bug · **mostly DONE** · links #163, #176 · note:
  (b) `List[String]` RECORD FIELD on Go → **DONE** (already-fixed by #168; pinned by `record_field_collection_concat.bock`
  in #176); (c) built-in `len`/`is_empty` lowering shadowing same-named user-record methods → **DONE (#176, ×5)** — was
  genuinely broken on all 5; root cause was `desugared_list_method` matching by name alone, fixed by gating on the
  checker's `recv_kind` stamp (+ `raw_recv_kind` reader, 2 unit tests, `user_method_shadows_builtin.bock`). (a) split out
  → **Q-allcaps-record-parse** (parser, separate). (d) String `reverse`/`char_at`/`slice` remain design-deferred (no
  cross-target char primitive; `s.reverse()` checks clean today) — tracked here, → DQ.
- **[Q-time-int64] §18.3.1 `Int64` realized as `Int`** — docs/spec · **DONE (#175)** · note: §18.3.1 prose now
  clarifies the time surface uses `Int` (i64-backed, full `Int64` range; no separate `Int64` surface type), reconciling
  the storage-width wording with the `Int` signatures. Verified wording-only (not a behavioral divergence). Changelog
  `spec/changelogs/20260601-1940-impl-changes.md`.
- **[Q-allcaps-record-parse] ALLCAPS record name not parsed as struct literal** — bug · ready ·
  `compiler/crates/bock-parser/` · — · links #163, #176 · note: an ALLCAPS (≥2-letter) record name in struct-literal
  position (`SB { ... }`) is not parsed as a struct literal → `E1001`. Split from Q-r2-codegen-residue (a); confirmed
  still present by #176 (out of that PR's codegen scope). Parser fix.
- **[Q-arch-doc-drift] ARCHITECTURE.md / compiler-CLAUDE.md / CONTRIBUTING.md crate-name drift** — docs/chore · ready ·
  `ARCHITECTURE.md`, `compiler/CLAUDE.md`, `CONTRIBUTING.md` · — · links #174 · note: D5 (#174) found the root
  `ARCHITECTURE.md` and `compiler/CLAUDE.md` name crates that **don't exist** (`bock-checker`, `bock-codegen-{js,ts,py,rs,go}`)
  and omit the real ones (type-checking is `bock-types`; all codegen is the single `bock-codegen`). Root `CONTRIBUTING.md`
  also describes conformance as `<name>.bock`/`<name>.expected` pairs, but the harness is `// TEST:`/`// EXPECT:`
  directive-driven. The D5 docs page documents reality + notes the divergence; reconcile these three source files to the
  real 17-crate workspace. (CLAUDE.md files are orchestrator/merge-coordinator territory.)

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
  Q-self-subst, Q-prim-assoc). Q-list-codegen READ-ONLY methods DONE (#129); mutating → P4. **Phases 0-3 + P4-codegen DONE (#131-#149); the codegen
  substrate is essentially built (cross-module, enums, generics incl. container/trait, Optional/Result, traits,
  match, collections, primitive-bridge; ~275 exec ×5).** P4-codegen landed: #147 tuple-`.N` diagnostic, #148 TS
  Self-in-plain-impl + expr-position match, #149 generic-container/trait residue (GAP-A/B/C/D — the 4 gaps
  core.iter's v5 STOP exposed; the systematic audit under-covered them). **6th PROBE CLOSED (#152):** core.iter's
  real generic-combinator surface exposed Rust/Go codegen residue (transitive `T: Clone`, Go generic-record-construct
  / concat-arg typed literals / generic-trait interface header / lambda specialization) — fixed, ~300 exec ×5. The
  codegen substrate is now exercised by a full generic stdlib module on all 5. **REMAINING:** (a) ~~core.iter~~ DONE
  (#151/#152); (b) **Q-codegen-completeness P4-hygiene** (bock-types: mutating-collection guarding diagnostic
  [DQ18 v1-floor] + bare-`m.contains` [DQ22] — both checker.rs); (c) design-gated → Design: DQ23 (Int/Int §3.6 NEW),
  DQ18 (mutating lowering), DQ20 (`expr?`), DQ22, DQ21, Bool-interp spelling; (d) Go nested-runtime-payload arith
  [#142 residual] + Rust by-value-reuse [#149 OPEN]. NONE of these gate the R1 effect floor.
- **[Q-stdlib] Implement the core standard library** — impl ·
  **★ DONE — v1 STDLIB COMPLETE, 11/11 modules ×5 ★** (was v1-BLOCKING; now satisfied). R1: iter [#151/#152],
  effect-foundation [#155], effect [#157]. R2: option [#159/#162/#165], result [#161/#165], string [#162/#163], time
  [#160 builtin]. **R3: test [#169 — both free + fluent assert APIs, DQ26], collections [#170 — SortedSet + utils].**
  All ×5. Enabling codegen across the batch: #162 (String methods + keyword escaping + Optional-T:Clone + bundle
  determinism), #164 (dep_graph determinism), #165 (Go generic Optional/Result), #167 (bock test core-loading),
  #168 (generic List[T]-over-user-types + sealed-trait bounds on primitives), #170 (collections Go/Rust residue).
  405 exec pairs ×5. **UNBLOCKS D4** (stdlib reference docs). NO further stdlib work for v1 ·
  `stdlib/`, `compiler/tests/conformance/stdlib/` · — · links DV1, MS-stdlib, DQ5,
  #100 · note: v1 = **11 core modules** at minimum-useful surface (option, result,
  collections, string, iter, compare, convert, error, effect, time, test). Each =
  `stdlib/core/<m>/` source + per-target shims + conformance fixtures, compile/run
  on every target. **Landed:** loading mechanism + `core.error` (#103); `core.compare`
  (#104); the primitive-conformance bridge (#108); `core.convert` + parameterized
  traits (#110); **`core.iter`** (#151 generic `Iterator[T]`/`Iterable[T]` + concrete `ListIterator[T]`
  + 6 eager List-returning combinators + the for→Iterable checker desugar; #152 Rust/Go codegen — all 5×5);
  **`core.effect`** (#157 `Log` effect + `ConsoleLog` handler + `console_log()`; the effect foundation #155 + the
  `effect`-keyword module-path parser fix + the interpreter determinism fix — all 5×5);
  **`core.option`** (#159 utilities; #162 keyword-escape + Rust T:Clone; #165 Go — ×5); **`core.result`** (#161
  utilities; #165 Go — ×5); **`core.string`** (#162 String-method codegen layer; #163 utilities + StringBuilder — ×5);
  **`core.time`** (already a compiler builtin — Duration/Instant/Clock/sleep; #160 conformance floor pins §18.3.1 ×5).
  **Codegen gate CLEARED:** Q-fconf execution conformance (#114/#115)
  + Q-codegen-fixes (#121, DV9) + the codegen-completeness milestone (#131-#152) — 5-target parity real + tested.
  **R1+R2+R3 ALL COMPLETE — v1 stdlib DONE (11/11 ×5).** R3: test #169 (DQ26 both-API floor), collections #170
  (SortedSet + utils). No remaining stdlib work for v1. Plans (all executed): `plans/2026-05-31-core-iter-r1-plan.md`,
  `plans/2026-05-31-effect-foundation-plan.md`, `plans/2026-05-31-core-effect-r1-plan.md`.
  `core.types/math/memory/concurrency` Reserved for v1.x.
  Plans: `plans/2026-05-29-stdlib-loading-error-pilot-plan.md`,
  `plans/2026-05-30-primitive-conformance-bridge-plan.md`,
  `plans/2026-05-30-codegen-correctness-conformance-plan.md` (done).

## Blocked

- **[D4] Stdlib reference docs** — docs · **DONE → #172** · `docs/src/reference/` · note: shipped the v1 stdlib
  reference — landing (`reference/stdlib.md`, replacing the outdated `std.*` stub) + 11 per-module pages
  (`reference/stdlib/core-*.md`) generated from the `///`/`//!` comments via `bock doc stdlib/core` then curated to
  user-facing prose; `core.time` (builtin) hand-written from §18.3.1. SUMMARY wired; `mdbook build docs` clean.
- **[D5] Contributor docs + cleanup** — docs · **DONE → #174** · `docs/src/contributing/` · note: shipped a proper
  nested Contributing section — `index` (overview/where-to-look/reviews), `architecture` (real 17-crate workspace +
  pipeline), `workflow` (canonical 4-command pre-PR gate + directive-driven conformance), `spec-changes` (spec process +
  generated changelog/STATUS/ROADMAP). Replaced the thin flat `contributing.md`; SUMMARY rewired; `mdbook build docs`
  clean. FOUNDs filed → Q-arch-doc-drift. **D5 was the last gate before ItemB → ItemB now UNBLOCKED.**
- **[D2-polish] D2 language-reference final polish** — docs · blocked ·
  `docs/src/language/` · blocked-by: (D2-FOUND mostly resolved — verify)
  · note: most D2-FOUND rows resolved per spec revision; confirm residue.
- **[ItemB] Project-mode codegen (Phases 1-6)** — impl · **READY (UNBLOCKED 2026-06-01 — D5 done)** ·
  `compiler/crates/bock-codegen/` · — · links #28 · note: **next critical-path item.**
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
Q-stdlib R1 (iter ✓ #151/#152 · effect NEXT) → R2 (option/result/string/time) → R3 (collections/test) ──→ D4 ──→ D5 ──→ ItemB (P1 → P2-5 → P6) ──→ ItemD
  ⮑ codegen-completeness milestone #131-#152 essentially DONE — substrate complete + now EXERCISED by a full generic stdlib module (core.iter) on all 5
  ⮑ iter DONE on all 5: module + for→Iterable checker desugar (#151) + Rust/Go generic-combinator codegen (#152), ~300 exec ×5
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
