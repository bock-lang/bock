# Divergences — spec ↔ implementation

**The one question:** where does the implementation differ from the
spec, and what's the disposition?

Factual mismatches only. Undecided behavior → `design-questions.md`;
actionable fixes → `queue.md` (linked by ID). Each row links its
resolution. Migrated from the retired `docs/SPEC-ALIGNMENT.md` (repo
wins; resolved rows carry the landing PR/changelog).

Schema: `[ID] spec § · spec-says / impl-does · classification
(spec-stale | spec-ahead-of-impl | impl-bug | gap) ·
disposition(reconcile-spec→link | fix-impl→queue ID | accept) ·
status(open | resolved→link)`

---

## Open

### DV1 — core stdlib modules unimplemented
- **§:** §18.3 · **spec-says:** 11 v1 `core.*` modules ship in v1 ·
  **impl-does:** **3/11 landed** — `core.error` (#103), `core.compare` (#104),
  `core.convert` (#110); 8 remain. Loading mechanism (embedded source-compiled) works.
- **Classification:** spec-ahead-of-impl
- **Disposition:** implement the remaining 8 (→ queue `Q-stdlib`, SCOPED via
  DQ5). §18.3 v1-status reconciled in #100. R1 `iter` is now BLOCKED on **DV10**
  (List-method codegen, all backends) + DQ16 (floor); the for→Iterable desugar itself
  is proven (T1 green ×5).
- **Status:** open (3/11 landed; iter blocked on Q-list-codegen + DQ16)

### DV7 — cross-module where-bound enforcement gap
- **§:** — (impl correctness; §18.5 trait conformance across modules) ·
  **spec-says:** `where (T: Comparable)` bounds hold for imported generic
  functions too · **impl-does:** `ExportedSymbol` carries only a function's type
  string, not its trait bounds, so `seed_imported_generic_fn` sets
  `where_clause: vec![]` — bounds on **imported** generic fns aren't enforced.
  Locally-defined bounded fns now enforce correctly (#108).
- **Classification:** gap (export ABI omits trait bounds)
- **Disposition:** fix-impl → `queue.md` Q-xmod-bounds (thread where-clauses
  through the export ABI). Pre-existing; surfaced by Q-bridge (#108). Orthogonal
  to the bridge; not blocking the fan-out (stdlib generics are defined locally).
- **Status:** open

### DV8 — cross-module trait-impl resolution for `.into()`
- **§:** — (impl correctness; cross-module impl visibility) · **spec-says:** a
  `From`/`Into` impl is usable wherever the trait + types are in scope, across
  modules · **impl-does:** `.into()` resolves via the impl-table, which isn't
  seeded across module boundaries, so an `impl From[A] for B` in one module isn't
  visible to `.into()` in another (`.from()` + trait methods DO cross modules via
  method seeding). Surfaced by `core.convert` (#110).
- **Classification:** gap (impl-table not seeded cross-module)
- **Disposition:** fix-impl → `queue.md` Q-xmod-impl. Pairs with DV7 as the
  cross-module-impl-surface theme. Not blocking (fixtures/stdlib co-locate impl
  with use).
- **Status:** open

### DV10 — List built-in methods do not codegen on any target
- **§:** §18.3 (collections) / general · **spec-says:** `List[T]` values have built-in
  methods (`len`/`get`/`push`/`is_empty`/…) usable in Bock, lowered to native ops on every
  target · **impl-does:** codegen emits the calls VERBATIM (`recv.len()`); NO backend lowers
  List built-ins → failure on all 5 (js `len is not a function`; py `no attribute 'len'`;
  rust type errors; go `no field or method len`). Verified empirically + by source (no
  List-method dispatch anywhere in bock-codegen).
- **Classification:** gap (List built-in method codegen unimplemented, all backends)
- **Disposition:** fix-impl → `queue.md` Q-list-codegen (v1-BLOCKING; substantial workstream).
  Gates core.iter (List-backed floor), core.collections, any List-using module. ESCALATED
  (scope/roadmap) + DQ16 (floor). Surfaced by core.iter v3 (2026-05-30); latent because the
  3 landed modules were List-free.
- **Status:** open

### DV11 — Go native `for x in [list]` element typing
- **§:** — (impl correctness) · **spec-says:** the loop var of `for x in [1,2,3]` has the
  list's element type · **impl-does:** Go codegen emits `for _, x := range []interface{}{…}`,
  so `x` is `interface{}` and typed use fails (`sum + x` mismatched types). js/python/rust ok.
- **Classification:** impl-bug (Go list-literal element typing; `interface{}` family, cf. #127)
- **Disposition:** fix-impl → `queue.md` Q-go-list-literal (emit a typed slice + typed range
  var). Surfaced by core.iter v3's native-fast-path fixture.
- **Status:** open

---

## Resolved (this session / spec-revision — kept for traceability)

- **DV9 codegen incomplete on multiple targets (the v1 "parity" gap)** — impl-bug:
  general Bock constructs failed codegen on Rust/Go/Python (statement-bodied `match`
  arms on all 5 backends; Go match-as-IIFE; `self`-methods on Rust/Go/Python; Go/
  Python `Optional` runtime; interpreter method-body empty env) — so "5-target
  parity" was false + untested (the conformance suite never executed). RESOLVED via
  a two-PR workstream: **Q-fconf** execution conformance (#114/#115 — compile + run
  fixtures per target, diff stdout) + **Q-codegen-fixes** (#121 — all 6 defects
  fixed, 32/32 fixture×target pairs green under REQUIRE=all). Parity is now real +
  tested. Residue: Python `Optional` (Q-py-optional) RESOLVED #126; TS self/Optional
  (Q-ts-codegen) RESOLVED #124; expr-position statement-arm match (Q-match-exprpos) still
  open (+ a Go expr-position IIFE variant, #127). NB: "parity" rested on fixtures that never
  exercised method-call scrutinees / statement-position match-in-loop / mut-self iterators /
  List methods — deeper Optional-payload layers surfaced + closed by core.iter (#124/#126/#127);
  the List-method codegen gate (DV10) remains. resolved → #114/#115/#121.

- **DV4 stdlib trait impls can't cover primitive types** — gap (missing
  checker↔bock-core bridge) → Design DQ6 ruled the model; the compiler now
  registers canonical primitive conformances into the trait-impl table (sealed),
  so primitives satisfy core-trait bounds + resolve trait methods, codegen
  unchanged (no dynamic dispatch). resolved → #108.
- **DV6 trait bounds unenforced in the production pipeline** — impl-bug (latent;
  `impl_table` was `None`, so `where`-bounds were never checked) → #108 wires
  `ImplTable::build_from` into `check_module`; all 2275 baseline tests stayed
  green (no code relied on the unenforced bounds) + bounds now enforced
  (non-conforming type → E4005). resolved → #108. (Cross-module case → DV7.)
- **DV5 §18.2 prelude vs §18.3 import for fundamental traits** — gap (spec
  internal inconsistency) → Design DQ9 ruled the model "defined in core.*,
  re-exported into the prelude" (§18.2/§18.3 consistent); §18.2 amended to add
  `Ordering`/`Less`/`Equal`/`Greater`. Spec reconciled → #106. Impl side (prelude
  injection) tracked as `queue.md` Q-prelude-inject.
- **DV2 §13.3/§13.4 concurrency Reserved status** — gap (the
  `20260514-0449` changelog asserted channels + sync primitives were
  "Reserved per the D1+D2 batch", but no such batch existed and the spec
  carried no Reserved marker) → channels (§13.3) and sync primitives
  (§13.4) marked Reserved-for-v1.x per Design DQ3/DQ4 (they bundle with
  `core.concurrency`, Reserved per DQ5); the 0449 cross-ref corrected.
  resolved → #100.
- **DV3 §11.4 `@performance` literal form** — impl-bug: the annotation
  interpreter required the parenthesized method-call form `100.ms()` and
  rejected the canonical no-parens literal `100.ms`, contradicting the
  §11.4 Q3 decision ("a literal, not a method call"); it also lacked the
  `.min`/`.h`/`.tb` units in Design's normative set. fix-impl → taught the
  interpreter to accept the no-parens `FieldAccess` literal form (keeping
  the parens form as a lenient alias) and added the missing units; bare
  ints still → E8003. resolved → #100. (Surfaced while reconciling DQ2.)
- **§20.1 CLI + §20.7/Appendix A target tables** — spec-ahead-of-impl →
  reconciled (Reserved-for-v1.x). resolved → #92.
- **§1.5 paradigm modes / `[paradigm]`** — spec-ahead-of-impl →
  Reserved-for-v1.x. resolved → #73.
- **§20.1.1 `bock check` flags (--only/--brief)** — direct
  contradiction → impl aligned to spec. resolved → #76 (F04).
- **§11.7/§11.8/§15.3 module-level annotations / context-completeness**
  — reconciled (module-level Reserved-for-v1.x; v1 completeness
  per-item). resolved → #87, #73.
- **§18.5 trait-language integration, §19.7 stability tiers, §14.1/2
  FFI, §16.3/4 AIR serialization, §20.3 LSP, §20.5 debugger, §10.3/4/6
  effects, §1.3 targets, §17.6 capability, §12.2 imports, §4.7
  refinements, §7.6 tuple-indexing, §6.1 defaults** — resolved via the
  D1-refresh/D2-FOUND cycle + the 20260514-0548 spec-revision artifact
  (see that artifact + per-section changelogs).

_(Full pre-consolidation analysis history is in git: the retired
`docs/SPEC-ALIGNMENT.md`.)_
