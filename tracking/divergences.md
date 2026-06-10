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

### DV23 — §10.4 prose says lambda handlers "fail at name resolution"; they fail in the type checker
- **§:** §10.4 · **spec-says:** the v1.x-reserved lambda-handler form
  "fails at name resolution" · **impl-does:** the failure mechanism is
  the type checker (was E4002, now the dedicated E6006 per #345); the
  normative outcome (the form does not compile) is preserved and
  conformance-pinned (`effects/lambda_handler_reserved.bock`).
- **Classification:** spec-stale (mechanism wording only; behavior
  conforms)
- **Disposition:** reconcile-spec — Design-routed wording update
  ("rejected during checking" or mechanism-neutral phrasing). Low
  priority; no behavior change.
- **Status:** open (FOUND by #345, 2026-06-10)

### DV22 — §8.4 checker accepts a non-diverging `guard` else
- **§:** §8.4 · **spec-says:** a `guard` else-block must diverge
  (return/propagate/abort) · **impl-does:** the checker accepts a
  non-diverging else; all five targets + the interpreter fall through
  and continue (python USED to silently truncate instead — fixed as
  part of #344, so runtime behavior is now at least uniform).
- **Classification:** impl-bug (checker under-rejects)
- **Disposition:** route to Design to confirm the §8.4 reading, then
  fix-impl → queue (checker-side divergence check; existing
  fall-through programs become compile errors — note the breakage
  surface when filing).
- **Status:** open (FOUND by the #344 statement-position audit,
  2026-06-10)

### DV21 — §6.7 fieldless enum-variant expression accepted statically, rejected at interp runtime
- **§:** §6.7 · **spec-says:** enum-variant construction/usage rules are
  checked statically · **impl-does:** a fieldless `Status.Active`
  *expression* passes `bock check` but is a runtime error in the
  interpreter — the static checker under-rejects (or the interp
  under-accepts; the spec reading favors static rejection).
- **Classification:** impl-bug
- **Disposition:** route to Design to confirm the §6.7 reading, then
  fix-impl → queue (file the Q-item on the ruling; checker-side
  rejection is the expected outcome).
- **Status:** open (FOUND by the #339 context-pack authoring probe,
  2026-06-10)

### DV20 — §21.11 qualified enum-variant patterns rejected by the parser
- **§:** §21.11 · **spec-says:** the grammar admits qualified
  enum-variant patterns (`Status.Active =>`) in match arms ·
  **impl-does:** the parser rejects the qualified form; only bare
  variant names match.
- **Classification:** spec-ahead-of-impl
- **Disposition:** route to Design — implement the qualified pattern
  form or amend §21.11; interacts with DV21's §6.7 expression-position
  question (same qualified-variant surface).
- **Status:** open (FOUND by the #339 context-pack authoring probe,
  2026-06-10)

### DV19 — §20.3 claims a v1 LSP completion provider; none is implemented
- **§:** §20.3 · **spec-says:** the v1 LSP ships completion ·
  **impl-does:** `bock lsp` registers hover, definition, push+pull
  diagnostics, find-references, validated rename, hierarchical document
  symbols, inlay hints (#324/#330) — no completion provider
  (`bock-lsp/src/server.rs` capabilities).
- **Classification:** spec-ahead-of-impl
- **Disposition:** route to Design — implement completion (→ queue
  `Q-lsp-completion`) or reconcile §20.3's v1 claim. Docs state the gap
  plainly since #331 (`docs/src/reference/tooling.md`). Non-blocking.
- **Status:** open (FOUND by the #331 docs reconcile, 2026-06-09)

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
- **Disposition:** READ-ONLY methods LANDED #129 (len/get/is_empty/contains/first/last/concat/index_of/join,
  all 5). MUTATING methods (push/etc.) deferred → DQ18 (→ Q-codegen-completeness P4). Folded into the
  Q-codegen-completeness milestone. Surfaced by core.iter v3 (2026-05-30); latent because the 3 landed
  modules were List-free.
- **Status:** resolved-for-read-only → #129 (mutating residue pending DQ18)

### DV11 — Go native `for x in [list]` element typing
- **§:** — (impl correctness) · **spec-says:** the loop var of `for x in [1,2,3]` has the
  list's element type · **impl-does:** Go codegen emits `for _, x := range []interface{}{…}`,
  so `x` is `interface{}` and typed use fails (`sum + x` mismatched types). js/python/rust ok.
- **Classification:** impl-bug (Go list-literal element typing; `interface{}` family, cf. #127)
- **Disposition:** fix-impl → `queue.md` Q-go-list-literal (emit a typed slice + typed range
  var). Surfaced by core.iter v3's native-fast-path fixture. Folded into Q-codegen-completeness P3.
- **Status:** open

### DV12 — Generic-record codegen broken on 4/5 targets
- **§:** general (a `record R[T]` with methods should compile on every target) · **impl-does:** only JS
  compiles generic records/impls. Python: no `TypeVar`/`Generic[T]` ever emitted (py.rs RecordDecl/FnDecl) —
  universal generic failure. Go: struct literal not instantiated (go.rs:~2445) + method receiver missing `[T]`
  (go.rs:~1726) + int-literal `int` vs `int64`. TS: impl interface-merge drops `<T>` (ts.rs:~1041/1050). Rust:
  bare `impl Box` not expanded to `impl<T> Box<T>` + trait-path drops args + missing `T: Clone`/`Display`.
- **Classification:** gap (generic codegen, 4/5) · **Disposition:** fix-impl → Q-codegen-completeness P1. Gates
  core.iter (generic ListIterator), core.collections, option/result. · **Status:** RESOLVED → #135 (Python
  TypeVar/Generic) + #136 (Go receiver `[T]`/instantiation, TS interface-merge `<T>`, Rust `impl<T>` + bounds;
  shared collect_generic_decls registry). Generics work on all 5. Residue (refinements, non-blocking): Rust
  generic-bounds policy + generic-enum scope + Go inference edge cases → P-follow-ups.

### DV13 — Cross-module `use` not wired into codegen (broken on ALL 5)
- **§:** §12/§18 · **impl-does:** stdlib/user modules emit as separate files but `main` never wires them (js
  comment-only; py `from core.x import` of nonexistent; rust `use core::x`; go `import`); the exec harness runs
  a single file. → **no cross-module program runs on any target**; the 3 "landed" stdlib modules
  (error/compare/convert) were `bock check` + `--source-only`-verified, never executed cross-module.
- **Classification:** gap (foundational) · **Disposition:** **CLOSED 2026-06-02** — closed *properly* via native
  per-target cross-file imports (not bundling). DQ19 decided (owner) the v1 output is the per-module native tree;
  realized across MS-projectmode S1–S4 (#182 python · #184 js/ts · #185 rust/go + manifests · #186 retired the dead
  bundling code). All 5 targets emit + run per-module native-import trees; 425 exec pairs / 0 failed REQUIRE=all. ·
  **Status:** CLOSED → #182/#184/#185/#186. Spec reconciled (§20.6.1, changelog `20260602-1608-per-module-output-dq19.md`).

### DV14 — User-defined enum codegen broken on ALL 5
- **§:** §-enum · **impl-does:** no enum-variant registry in codegen. Variant construction (`Red`→lowercased
  `red`; `Circle{..}`→bare object, never `Shape_Circle`) and match dispatch (js/ts `is_adt` ignores `RecordPat`
  → struct-payload→all `default:`; Rust unqualified paths; Python no union alias + no payload bind; Go one-line
  value-switch on undefined types) all broken. Built-in `Optional` works (bespoke lowering — the model to
  generalize).
- **Classification:** gap (foundational) · **Disposition:** RESOLVED — enum-variant registry in generator.rs
  (pre-seeds Some/None/Ok/Err) + per-target construction/match (#133). MONOMORPHIC user enums; generic enums →
  DV12/P1. · **Status:** resolved → #133

### DV15 — Tail-position statement-`if` in loop bodies mis-lowered (4/5)
- **§:** — (impl correctness) · **impl-does:** `generator.rs:~426 node_is_statement()` omits `If`, so a
  tail-position `if (c){return/break/…}` (no else, statement branch) routes through expression emission →
  `/* unsupported */` ternary (js/ts/python fail) or wrong `return` (Rust silent-wrong); Go fail.
- **Classification:** impl-bug (localized, high-leverage) · **Disposition:** RESOLVED — `node_is_statement`
  now classifies no-else/all-statement-branch `If` as a statement (#131); no backend edits needed. · **Status:**
  resolved → #131

### DV16 — Effect operation surface: bare-op calls don't resolve + the `effects/` suite is inert
- **§:** §10.2/§10.3 · **impl-does:** a bare effect-op call (`log("...")` inside a `handling` block) fails
  **E1001 undefined name 'log'** even SAME-module; the op name is neither callable bare nor importable
  (`use m.{log}` → E1006 not exported). The ONLY working invocation form is calling the op inside a
  `fn ... with <Effect>` body (handler threaded as a param) — which executes correctly cross-module on ALL 5
  (probe P1). The committed `conformance/effects/*` fixtures use bare ops and therefore ERROR on `bock check`/
  `bock run` — undetected because the **entire `effects/` suite is parse-only**: the directive harness
  (`compiler/tests/harness/mod.rs`) only parses directives; the execution harness (`execution.rs:122`) scans
  ONLY `conformance/exec/`. So `// EXPECT: no_errors`/`output` on `effects/` fixtures is inert; the effect
  system has never been checked or executed there (0 of 300 exec cases are `exec_effect_*`).
- **Classification:** spec↔impl divergence — the spec (§10.2/§10.4) clearly establishes bare-op invocation as
  the canonical form (the impl was wrong), NOT a Design question (a Plan pass confirmed: §10.4 codegen already
  established the handler binding + rewrote the bare op; only resolver/checker name-injection was missing — no
  v1-scope limitation). · **Disposition:** **RESOLVED → #155.** `resolve_handling` + a module-`handle` pass now
  inject the handled effects' ops into scope (resolve.rs); the checker `HandlingBlock` arm mirrors it; the Rust
  interpolation sub-context now propagates `effect_ops`/`current_handler_vars` (rs.rs). ALL §10 invocation forms
  now work ×5 (with-clause incl. op-in-interpolation, §10.4 bare-op-in-handling, §10.3 Layer-1 innermost-shadow +
  Layer-2 module handler, cross-module). The `effects/` suite was converted to 6 executable `exec_effect_*`
  fixtures (run ×5; the suite was inert before). · **Status:** RESOLVED → #155 (Q-effect-interp-rust +
  Q-effect-conformance-wiring both DONE). RESIDUE → queue Q-effect-op-node-lowering (unhandled bare op surfaces
  E1001 not E8020 — `EffectOp` AIR nodes are only built in test code; non-urgent; diagnostic code non-normative).

### DV17 — §18.3 lists "benchmarking" for `core.test`, but §15.4/§20.4 removed/delegated it
- **§:** §18.3 vs §15.4/§20.4 · **spec-internal inconsistency:** §18.3's `core.test` line lists "Assertions, BDD
  grouping, mocking, **benchmarking**", but §15.4 REMOVED `@benchmark` entirely (not Reserved) and §20.4 delegates
  benchmarking to target-native tools (`cargo bench`/`pytest-benchmark`/…). So core.test ships NO benchmarking surface
  (DQ26). · **Disposition:** **OPEN → Design** — amend §18.3's core.test line to read "benchmarking delegated to
  native tools (§20.4), not shipped" (one-line spec clarification; no behavior change; the impl is correct). Surfaced
  by the core.test build (#169 changelog). Non-blocking (v1 stdlib is complete). · **Status:** RESOLVED 2026-06-06 — §18.3
  core.test line corrected (DV17, in the stdlib-surface ratification batch): "benchmarking" dropped entirely, trimmed to the
  shipped "assertions (free + fluent)", BDD/mocking/property/snapshot → Reserved-v1.x; changelog `20260606-stdlib-surface-ratification`.

### DV18 — source mode emits run-affordance manifests, but §20.6.2 says source mode emits none
- **§:** §20.6.2 · **impl-does:** `--source-only` still emits the run-affordance manifests (`Cargo.toml`, `go.mod`,
  `package.json {"type":"module"}`) because codegen emits them in ALL modes — needed so the per-module tree is runnable
  and so the conformance harness (which builds `--source-only` then runs `cargo run`/`go run .`) works. §20.6.2 source
  mode is "no manifests, scaffolding, or entry-point wiring." · **Classification:** gap (transitional) · **Disposition:**
  planned resolution in **S6/S7** — migrate the conformance harness to **project-mode** builds (the mode that legitimately
  carries manifests + transpiled tests), letting source mode become truly bare. Surfaced by S5 (#188); the S5 project-mode
  scaffolding pass is correctly project-mode-only. Non-blocking (425/0 green). · **Status:** **CLOSED → #190** (S6a):
  codegen now emits only per-module source; the `Scaffolder` owns the manifests (project mode only); `--source-only`
  is bare (asserted by `build_command` + per-backend codegen tests); the conformance harness builds in project mode.

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
