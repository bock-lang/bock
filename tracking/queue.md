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

_Last reconciled: 2026-06-03 15:24 — **MS-examples-hardening UNDERWAY: clusters A+B+C + the gate LANDED.** #204
(informational examples-exec gate) + #205 (Q-list-method-codegen A · Q-list-concat-codegen B · Q-const-enum-naming C,
all 5; conformance 455/0 w/ 5 new fixtures) merged; main a5fbb28. **Post-fix matrix (gate re-run): runtime-working
js 2→7 · ts 2→4 · py 7→9 / 20; rust 2, go 1 unchanged (blocked on E/F/G/D); 0 regressions.** NEW HIGH finding
**Q-impl-body-typecheck** (checker doesn't type-check impl/class method bodies → bounds A/B's reach to free-fn sites +
misses method type errors). Cluster C: const part done, enum-variant/trait-name residue is now RUNTIME (→ K). Remaining
MS-examples-hardening leverage order: Q-impl-body-typecheck, Q-rust-cargo-workspace, Q-go-enum-return-boxing (E),
Q-rust-move-codegen (F), Q-rust-string-num-methods (G), Q-js-effect-export (J), Q-py-circular-import (K),
Q-match-exprpos (D, deep), Q-examples-codegen-misc. Follow-up: refresh the examples-exec baseline (ratchet). — Earlier
2026-06-03 13:44: **EXAMPLES-EXEC AUDIT COMPLETE + operator decisions** (see audit.md 2026-06-03 13:44). The full 20×5 audit (built in /tmp, project mode) gives the TRUE matrix: js 10/20 compile·2/10 run,
ts 2/20·2/2, py 15/20·7/15, **rust 3/20·2/3 (in-repo 0/20 — workspace bug masks), go 1/20·1/1** — hello-world the only
all-5. Worse than the digest's 6-example sample, and **rust/go fail on REAL codegen, not just the env bug** (proven:
fizzbuzz-rust passes in /tmp, fails in-repo). **~9 evidence-confirmed root-cause clusters** filed below:
Q-list-method-codegen (A, HIGH, all 5 — receiver dup'd as first arg), Q-list-concat-codegen (B), Q-const-enum-naming
(C), Q-match-exprpos (D — UN-DEFERRED, broadened; subsumes the now-diagnosed Q-chat-protocol-allfail),
Q-go-enum-return-boxing (E), Q-rust-move-codegen (F), Q-rust-string-num-methods (G), Q-js-effect-export (J),
Q-py-circular-import (K), Q-examples-codegen-misc (minor); plus Q-rust-cargo-workspace (L, masking-only) +
Q-examples-exec-coverage (M, the gate). **OPERATOR DECIDED:** v1.0 = **leverage-order, ALL 5 targets at the
'examples green' bar** (not tiered; go/rust long poles accepted); gate = **informational-first → blocking**. → see
MS-examples-hardening. gitignore cleanup → **PR #202** (merging). NEXT: fix A first (engineer session) + build the
informational gate (parallel, disjoint files). — Earlier 2026-06-03: (**★ ItemB COMPLETE — MS-projectmode DONE (S0–S8) ★** — per-module native
output on all 5 [DV13]; project mode real [scaffolder-owned manifests/configs/README + transpiled @test files per
framework], source mode bare [DV18]; config tables parsed; core.error fixed ×5 [#193]. 430 exec pairs / 0 failed
REQUIRE=all. ItemB (the ProjectMode milestone) complete + js/ts/python CI-certified [#196] + rust/go formatter-clean
[#198]. **⚠ BUT (FOUND 2026-06-03): an examples-compile audit shows the conformance fixtures are TOO NARROW — the
real-world examples largely DON'T compile in project mode (ts 0/6, rust 0/6, go 0/6; js/py "OK" = syntax-only).**
Root causes: **Q-list-method-codegen** (List `.map()`-with-closure mislowered, all 5, §20.4) · **Q-rust-cargo-workspace**
· chat-protocol. Meta gap: **Q-examples-exec-coverage** (examples not exec-tested ×5). **v1.0 is further out than
the green-conformance picture implied** — an "examples-hardening" workstream is needed before release. **OPERATOR
DIRECTION PENDING** (recommended: examples-exec audit first → fix clusters). Release actions [escalate]; ItemD unblocked
but escalates. Q-formatter-clean-tree: rust/go DONE [#198], js/ts/python deferred. Plan: `plans/2026-06-02-itemB-per-module-projectmode-plan.md`. Quality-sweep Wave 1 also landed: **Q-conformance-clean-rebuild + Q-time-int64
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
- **[Q-match-exprpos] Expression-position statement-arm match/if lowering (all 5)** — impl ·
  ready · **UN-DEFERRED 2026-06-03 (audit) — now v1.0-scope (MS-examples-hardening), deep** · `compiler/crates/bock-codegen/` ·
  — · links #121, #127, #176, MS-examples-hardening · note: #121 fixed statement-POSITION matches with statement arms
  (all 5). The expression-position case (`let x = match … { _ => return }` yielding a value) needs a temp-hoist desugar.
  **#176 re-confirmed** broken on go/py/js/ts (Rust correct); the **20×5 audit (13:44) shows it's BROADER + higher-impact
  than the deferral assumed (~6 examples)** and is the root cause of the former Q-chat-protocol-allfail: on js/py the
  IIFE/lambda wrapping produces **unbalanced parens** (`Unexpected token ')'` / `'(' was never closed`) and a **duplicate
  `default` clause** on js, not just a captured transfer. The correct fix threads an "assign-to-target" mode through each
  backend's match/if-arm emitter — **cross-cutting across 4 backends** (still deep), but operator held all 5 at the v1.0
  bar, so it's in scope. Examples: chat-protocol, context-audit, guessing-game, pattern-lab, ownership-demo, type-zoo.
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
- **[Q-go-error-message] Go: `core.error.SimpleError` field/method name collision** — bug · **DONE (#191)** ·
  `bock-codegen/src/go.rs` · note: fixed in S6b — `go_method_name` disambiguates a public method colliding with a
  same-named record field to `<Name>Method` (applied at trait interface + receiver + call sites; field stays
  `Message`). Locked by a `go.rs` unit test + `conformance/exec/exec_core_error.bock` (rust+go). The js/ts/python
  variants of the same collision split out → **Q-error-message-jstspy** below.
- **[Q-error-message-jstspy] `core.error.message()` field/method collision also breaks js/ts/python** — bug · ready ·
  `bock-codegen/src/{js,ts,py}.rs` · — · links #191 · note: FOUND in S6b. The same `SimpleError { message }` field +
  `message()` method collision is **pre-existing on js/ts/python** (structural shadowing — TS: "Duplicate identifier
  'message'"; JS: instance field shadows the prototype method → `.message()` "not a function"; Python: dataclass field
  overwrites the method). Go fixed (#191); the `exec_core_error.bock` fixture is restricted to rust+go to keep
  conformance green. Each backend needs its own disambiguation (not just a name suffix). **Quality signal:** the v1
  stdlib was "complete" but `core.error.message()` was never exercised cross-target — a name-collision codegen pattern
  that may recur for other stdlib field/method pairs. Worth a pre-v1.0 fix; not on the ItemB critical path.
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
- **[ItemB] Per-module output + project-mode codegen + config tables** — impl · **★ DONE — MS-projectmode COMPLETE
  (S0–S8: #181/#182/#184/#185/#186/#188/#190/#191/#193/#194 + S8 close) → DV13+DV18 CLOSED; project mode real on all 5 ★** ·
  `compiler/crates/bock-codegen/`,
  `bock-cli/src/build.rs`, `bock-build/src/toolchain.rs`, `compiler/tests/execution.rs` · — · links #28, #132,
  DV13, DQ19, MS-projectmode · plan: `plans/2026-06-02-itemB-per-module-projectmode-plan.md` · note: **v1.0's last
  engineering milestone.** Owner decided (eyes-open) the v1 output is the **per-module native tree** (DQ19 →
  re-opens DV13: native per-target cross-file imports that compile+run) AND **config tables pulled into v1**.
  Staged **S0–S8** (sequential through S0→S4; S6 fans out by target):
  - **S0** — spec/tracking reconcile (DQ19 resolved, config tables un-reserved). **DONE → #181.**
  - **S1** — native imports + harness multi-file run, **PILOT = python**. **DONE → #182** (425 exec pairs / 0
    failed under REQUIRE=all; python emits a per-module native-import tree + runs as a multi-file project via the
    `emits_per_module_tree(target)` harness predicate [python-only]; js/ts/rust/go unchanged/bundling). Notes for
    fan-out: python run plan needed NO change (PEP 420 namespace pkgs resolve from build-dir root) — js/ts need an
    ESM run affordance, rust/go need a manifest; output paths key on the declared `module` path (not source-mirrored);
    per-module emission loses bundling's single-context visibility (re-seed via `seed_effect_registries` /
    `implicit_imports_for`).
  - **S2** — js then ts native ESM imports. **DONE → #184** (js: per-module ESM + minimal `package.json
    {"type":"module"}` run affordance; ts: `tsc→node`, no toolchain.rs change).
  - **S3** — rust + go native imports + minimal manifest. **DONE → #185** (rust: `src/`-rooted cargo crate +
    `mod`/`use crate::`, run `cargo run`; go: flat `package main` + `go.mod`, run `go run .`; run-plans reworked
    to validate/run at project level). FOUND → **Q-go-error-message** below.
  - **S4** — retire dead bundling code (**DV13 CLOSED**). **DONE → #186** — removed the multi-module bundling
    concatenator (trait-default `generate_project`, `bundle_output_path`, `append_entry_invocation`,
    `go::generate_bundle`, the always-true `emits_per_module_tree` predicate; ~170 net lines). KEPT (load-bearing,
    NOT bundling): the single-module self-contained emit (`generate_module` + `per_module` flag) used by ~250 unit
    tests — reframed terminology. **All 5 targets now emit per-module native trees as the sole path.**
  - **S5** — scaffolding framework + `bock.project` config parsing. **DONE → #188** — `Scaffolder` trait in
    `bock-codegen/src/scaffold.rs`; project-mode hook in `build.rs` gated on `!source_only`; `[targets.<T>]` /
    `[targets.<T>.scaffolding]` parsing + validation against the §20.6.2 v1 matrix (unknown value → error naming
    options; 26 unit tests); per-target bodies STUBBED (placeholder README) for S6. Flagged **DV18** (below).
  - **S6** — per-target scaffolders. **DONE** (split S6a/S6b):
    - **S6a → #190** — project-mode output ARCHITECTURE + **DV18 CLOSED**: codegen emits only per-module source;
      the `Scaffolder` owns the manifests (project mode only); `--source-only` is now bare; the conformance harness
      builds in project mode + runs the project. (NOTE: orchestrator finished this PR — the engineer session stalled
      after doing the work; I re-ran the gate, fixed a fmt drift, committed/merged.)
    - **S6b → #191** — enriched per-target scaffolders ×5 (rich manifests w/ framework refs + defaults, formatter
      configs, opt-in linter configs, README first-contact w/ package-manager hints; 41 unit tests) + **fixed
      Q-go-error-message** (go field/method collision via `go_method_name`; locked by `exec_core_error.bock`).
      Required side-fix: TS run plan `tsc main.ts` → `tsc -p .` (scaffolded tsconfig). 427 exec pairs / 0 failed.
      Deep-config that changes CODE (test-file codegen per framework) → S7.
  - **S7** — transpiled tests + formatter-clean gate. **DONE → #194** — Bock `@test` fns transpile to per-target
    test files (Vitest|Jest / pytest|unittest / cargo test / go test), framework-branched, wired into the scaffolded
    project; assertion lowering. **rust+go RUN-verified** (`cargo test`/`go test` pass on the emitted project);
    js/ts/python **compile-verified** (`tsc`/`node --check`/`py_compile`) — their runners (vitest/jest/pytest) +
    formatters (prettier/black) are absent on host/CI. Formatter-clean gate enforced for **rust (`rustfmt --check`)
    + go (`gofmt -l`)** + 2 codegen-hygiene fixes. 430/0. FOUND → Q-ci-projectmode-tooling, Q-go-gofmt-listclosure
    (below). Q-error-message-jstspy was done standalone (#193).
  - **S8** — internal docs + close. **DONE → this PR** — fixed `docs/src/getting-started.md` stale build-output
    path (`.bock/build/` → `build/<target>/`) + documented project-mode default (scaffolded project w/ manifest +
    transpiled tests); tooling.md/project-schema.md already updated by S5–S7. mdbook clean. Tracking closed (this PR).
  INVARIANT (held every PR): `run-conformance.sh REQUIRE=all` green (**430/430**). `--deliverable`/`--no-tests`
  stay v1.x. **★ ItemB COMPLETE (S0–S8) — DV13 + DV18 CLOSED; project mode real on all 5. ★** Remaining for v1.0
  release-readiness: Q-ci-projectmode-tooling **DONE (#196 — js/ts/python project-mode CI-certified)**; remaining =
  Q-formatter-clean-tree (full emitted tree formatter-clean ×5 per §20.6.2). **ItemD now UNBLOCKED** (external — escalates).
- **[ItemD] /get-started project-mode evolution** — docs · **READY-but-ESCALATES (UNBLOCKED 2026-06-03 — ItemB done)** ·
  `docs/`, `website/` · — · note: external-facing copy (website get-started) — **escalate for approval before any
  website change**; do not action autonomously. Now that project mode is real, the website get-started can evolve to
  show the scaffolded-project flow (`bock build` → `npm test`/`cargo run`).
- **[Q-ci-projectmode-tooling] CI provisions js/ts/python test+format tooling** — chore/test-infra · **DONE (#196)** ·
  `.github/workflows/ci.yml` · note: CI ubuntu lane now installs prettier/black/ruff/pytest + node and sets
  **`BOCK_PROJECTMODE_REQUIRE=all`** so the transpiled-test verification RUN-verifies + formatter-gates **all 5**
  (macos/windows stay skip-if-absent). ★ **Key finding: js/ts/python transpiled tests PASS as-emitted** — NO
  execution-codegen bugs; the only fixes were formatter-cleanliness of the emitted *test files* (js/ts tag-predicate
  parens; py blank-line spacing). Also added the missing `rustfmt` component to the test toolchain (surfaced by
  require=all on beta). **js/ts/python project-mode is now CI-certified.** Remaining formatter gap → Q-formatter-clean-tree.
- **[Q-formatter-clean-tree] Full emitted tree formatter-clean (§20.6.2)** — bug · **rust/go DONE (#198); js/ts/python
  DEFERRED** · `compiler/crates/bock-codegen/` · — · links #194, #196, #198, §20.6.2 · note: §20.6.2 mandates **rust+go**
  formatter-cleanliness as the universal baseline → **DONE (#198)**: project-mode build runs a post-emit `gofmt -w`
  (go) / `rustfmt` (rust) pass over the full tree (their formatters ship with the toolchain; go has no source-map
  conflict); full-tree `gofmt -l`/`rustfmt --check` gates added. **js/ts/python full-clean DEFERRED**: prettier/black
  *reflow* long lines (not hand-matchable in codegen) AND post-emit prettier would break the js/ts **source maps**;
  those formatters are user-OPTIONAL per §20.6.2 (not the baseline). Pursue later via either cheap codegen wins
  (redundant parens, py blank-lines) and/or post-emit formatting with source-map regeneration. v1.x-leaning.
- **[Q-list-method-codegen] List `.map()`/`.filter()` method-with-closure mislowered (all 5)** — bug ·
  **DONE → #205 (all 5)** · `compiler/crates/bock-codegen/` · — · links §20.4, MS-examples-hardening, #205,
  Q-impl-body-typecheck · note: FIXED by #205 — new `FUNCTIONAL_LIST_METHODS` + `desugared_list_functional_method`
  recogniser in generator.rs wired into each backend's Call arm; native idioms per target (JS/TS array methods; py
  builtins + gated runtime prelude; rust iter-adapter chains; go for-range func literals). 5 new conformance fixtures
  (×5, 25 exec pairs). **CAVEAT (reaches free-fn call sites; method-body sites bounded by Q-impl-body-typecheck —
  the checker doesn't type-check impl/class method bodies so the recv_kind stamp isn't applied there).** Original detail:
  EXACT root cause was
  a List functional METHOD with a closure is lowered with the **free-function calling convention** — the receiver is
  emitted as an explicit first argument: `data.map(data, (dp) => …)` (verified in TS output). Effect per target: **TS**
  array-not-assignable-to-callback + implicit-any params; **rust** `no method 'map'/'filter' on Vec` (needs
  `.iter().map().collect()`); **go** `found 'map'` syntax-error (`map` keyword) + `.filter` undefined; **js** runtime
  "object is not a function" / "nodes.map is not a function"; **python** `'list' object has no attribute 'map'/'filter'`.
  BROADEST single bug — ~10 examples (data-pipeline, markdown-parser, task-api, inventory-system, ownership-demo,
  ml-data-prep, react-components, systems-allocator, type-zoo, todo-list). Distinct from `core.iter`'s FREE functions
  (conformance-tested + pass) — which is why conformance is 430/0 green while real programs fail. Checks clean ⇒ §20.4
  transpiler bug. Fix the method-call lowering to use each target's native chain (no dup receiver, typed closure params).
- **[Q-rust-cargo-workspace] Generated `Cargo.toml` doesn't opt out of a parent workspace** — bug · ready ·
  `compiler/crates/bock-codegen/` (rust scaffolder) · — · links MS-examples-hardening · note: FOUND 2026-06-03;
  **CONFIRMED MASKING-ONLY (audit 13:44).** Project-mode rust build fails `current package believes it's in a workspace
  when it's not` when `build/rust/Cargo.toml` sits inside a parent cargo workspace (reproduced: fizzbuzz-rust passes in
  /tmp, fails in-repo). Fix: emit an empty `[workspace]` table in the generated `Cargo.toml`. Purely additive — fixing
  recovers 3/20 rust examples in-repo; the other 17 fail on genuine rust codegen bugs (F/G/A/B/D). Cheap; do early.
- **[Q-examples-exec-coverage] Exec-test all ~20 examples on all 5 targets in CI (the gate)** — chore/test-infra ·
  **DONE (informational) → #204; ratchet-to-blocking pending** · `tools/scripts/examples-exec-audit.sh`,
  `tools/examples-exec-baseline.txt`, `.github/workflows/examples-exec.yml` · — · links MS-examples-hardening, #204 ·
  note: LANDED #204 — a script (out-of-tree build ×5 + run) + a `continue-on-error` CI job + a checked-in baseline that
  warns on regression (strict mode `BOCK_EXAMPLES_REQUIRE` exits 1). **FOLLOW-UP (ratchet step): refresh the baseline now
  that A/B/C landed** (post-fix matrix 15:24: js ran 7/20·ts 4/20·py 9/20·rust 2·go 1, +7 vs baseline, 0 regressions) so
  the newly-passing pairs are protected; flip to required per-target as more clusters land. [historical detail below] ·
  — · links milestones (MS-examples-hardening, v1.0 acceptance) · note: FOUND 2026-06-03; the 20×5 audit (13:44) is the
  prototype. The 20 `examples/` aren't built+run on all 5, so real-world-pattern codegen bugs slipped past the narrow
  conformance fixtures (430/0 green while real programs fail). Build the gate: for each example × target, project-mode
  `bock build` (compile) + run where possible (ts via `node --experimental-strip-types`; rust `cargo run`; go `go run .`;
  js `node`; py `python3`). **Land NON-BLOCKING (reports the matrix per PR), then ratchet per-target pass-thresholds
  upward to required as clusters land** (operator decision). Can run parallel to the cluster fixes (disjoint files).
  Note the in-repo cargo-workspace interaction (Q-rust-cargo-workspace) — fix it or build rust examples out-of-tree.
- **[Q-list-concat-codegen] List `+` concatenation emitted as native `+` (ts/rust/go)** — bug ·
  **DONE → #205** · `compiler/crates/bock-codegen/` (+ `bock-types/checker.rs` stamp) · — · links MS-examples-hardening,
  §20.4, #205, Q-impl-body-typecheck · note: FIXED by #205 — checker stamps `LIST_CONCAT_META_KEY` on a List `+`
  (`infer_binop`, checks result OR either operand for a concrete `List` to close the open-result-var case);
  `generator::is_list_concat` reads it + a list-literal syntactic fallback; per-target concat idioms (`[...a,...b]` js/ts,
  clone+extend rust, append-helper go, native `+` py). **CAVEAT: a bare `self.a + self.b` list concat INSIDE an impl
  method won't get the checker stamp (Q-impl-body-typecheck); the syntactic fallback covers the common `xs + [..]` shape.**
  Original: FOUND 2026-06-03 (audit). Bock list
  append/concat via `+` (`(self.items + [todo])`) lowers to a native `+` op: **ts** `Operator '+' cannot be applied to
  T[]`, **rust** E0369 `cannot add Vec<T> to Vec<T>`, **go** `operator + not defined on []T`. js silently does the wrong
  thing (string-concat), python coincidentally works (list `+`). Lower to each target's concat idiom (spread / `extend` /
  `append`). Examples: todo-list, expense-tracker, ownership-demo, systems-allocator.
- **[Q-const-enum-naming] Const / enum-variant identifier def↔use mangling mismatch (all 5)** — bug ·
  **CONST part DONE → #205; enum-variant/trait-name residue now RUNTIME (not build)** · `compiler/crates/bock-codegen/` ·
  — · links MS-examples-hardening, #205, Q-py-circular-import · note: #205 fixed the **const** def↔use mismatch
  (`collect_const_names` registry; consts emitted verbatim at def + use across all backends) — fizzbuzz now compiles on
  js/ts/py/rust. **POST-FIX MATRIX (15:24): the enum-variant (`Category_Electronics`) + trait/protocol-name
  (`Allocatable`) cases now BUILD but RUN-FAIL** on js/py (inventory-system, systems-allocator moved from build-error to
  runtime-error), folded into Q-py-circular-import (K) + a trait-symbol-not-emitted residue — **no remaining BUILD-level
  work here**. Original: FOUND 2026-06-03 (audit). A constant or
  enum-variant name is emitted with one casing at the DEFINITION and another at the USE site: TS defines `FIZZ_NUM` but
  references `fizzNUM` (`Did you mean 'FIZZ_NUM'?`); `Category_Electronics`/`Allocatable` referenced-but-undefined; python
  references `FIZZ_NUM` but never emits the def at module scope. Normalize the identifier transform so def and use agree
  (and ensure module-scope consts are emitted). Examples: fizzbuzz, inventory-system, systems-allocator. Likely cheap.
- **[Q-impl-body-typecheck] Checker does not type-check impl/class method BODIES** — bug · ready ·
  **HIGH — correctness gap + bounds the meta-stamp codegen fixes** · `compiler/crates/bock-types/` (checker.rs) · — ·
  links #205, Q-list-method-codegen, Q-list-concat-codegen, MS-examples-hardening · note: FOUND 2026-06-03 (during #205,
  orchestrator-verified at checker.rs:1375). `check_item` dispatches only `FnDecl`/`ConstDecl`; `ImplBlock`/`ClassDecl`
  fall into the `_ => record Void` arm. Method SIGNATURES are registered (`collect_sig`) but method BODIES are **never
  type-checked**. Two consequences: (1) type errors inside impl/class methods are silently missed (correctness/UX);
  (2) the checker's codegen meta-stamps (`recv_kind`, the new `list_concat`) aren't applied inside method bodies — so the
  #205 A/B fixes fully reach only FREE-function call sites; `.map()`/`+`-concat **inside a method** relies on codegen
  syntactic fallbacks (B's list-literal fallback covers `xs + [..]`; a bare `self.a + self.b` would still mislower).
  Pre-existing (the stdlib/conformance pass because codegen has structural fallbacks). Likely accounts for a chunk of the
  still-failing examples whose list ops live in methods (e.g. todo-list's class). Fix: recurse `check_item` into
  impl/class method bodies (type-check each as a function with `self` bound to the target type). **Spec intends method
  bodies to be checked — this is an impl bug, not a spec divergence (stays in-queue).** Probably high-leverage next.
- **[Q-go-enum-return-boxing] Go: enum variant not boxed into sealed-trait interface on return** — bug · ready ·
  `compiler/crates/bock-codegen/` (go) · — · links MS-examples-hardening, #168 · note: FOUND 2026-06-03 (audit).
  Returning a variant where the sealed-trait interface type is expected isn't boxed: `cannot use MessageTypeText{}
  (struct) as __bockResult value in return`, `too many return values have (int, error) want (interface{})`, `interface{}
  does not implement Route (missing method isRoute)`. Box the variant to its interface at return sites. Examples:
  chat-protocol, microservice, effect-showcase, calculator (go).
- **[Q-rust-move-codegen] Rust: codegen produces borrow/move violations** — bug · ready ·
  `compiler/crates/bock-codegen/` (rust) · — · links MS-examples-hardening · note: FOUND 2026-06-03 (audit). Generated
  rust moves a value then reuses it: E0382 `use of moved value: op`/`borrow of moved value: key`; E0425 `cannot find
  value val/val2` (move-renamed binding). Needs clone/borrow insertion or by-ref lowering for reused bindings (pairs with
  the #149 Rust by-value-reuse follow-up). Examples: calculator, effect-showcase, ownership-demo (rust).
- **[Q-rust-string-num-methods] Rust: String / numeric method-lowering gaps** — bug · ready ·
  `compiler/crates/bock-codegen/` (rust) · — · links MS-examples-hardening, Q-r2-codegen-residue(d) · note: FOUND
  2026-06-03 (audit). `no method 'slice' found for String`, `no method 'to_float' found for i64`, `&str` vs `String`
  mismatches. Map Bock String/numeric methods to rust equivalents (slicing, numeric conversion) with correct owned/borrowed
  types. Examples: microservice, markdown-parser, inventory-system (rust).
- **[Q-js-effect-export] JS: effect-group/stack export referenced but not emitted** — bug · ready ·
  `compiler/crates/bock-codegen/` (js) · — · links MS-examples-hardening, #155, #157 · note: FOUND 2026-06-03 (audit).
  `SyntaxError: Export 'AppEffects'/'ApiEffects'/'ServiceStack' is not defined in module` — an effect-group/stack symbol
  is in the ESM export list but never emitted as a binding. Emit the binding (or drop it from the export list). Examples:
  effect-showcase, task-api, microservice (js).
- **[Q-py-circular-import] Python: multi-module emit produces a circular import** — bug · ready ·
  `compiler/crates/bock-codegen/` (python) · — · links MS-examples-hardening, #182 (per-module python) · note: FOUND
  2026-06-03 (audit). `ImportError: cannot import name 'Category' from partially initialized module 'models' (circular
  import)` — the per-module python emit creates an import cycle across the project's modules. Break the cycle (lazy/local
  import, or module-ordering). Example: inventory-system (python).
- **[Q-examples-codegen-misc] Examples audit: minor / per-example codegen + stub-quality gaps** — bug · ready (low-pri,
  triage individually) · `compiler/crates/bock-codegen/`, `examples/` · — · links MS-examples-hardening · note: FOUND
  2026-06-03 (audit). Smaller items surfaced: (a) `todo`/unimplemented expression in return position → `return throw …`
  (js) / `return raise …` (py), invalid syntax — partly example **stub-quality** (guessing-game has unfinished bodies);
  (b) reserved-word / identifier collisions — `eval` (calculator js, `Invalid use of 'eval'`), redeclared `list`
  (todo-list js); (c) `Char` type unmapped on ts/rust/go (type-zoo); (d) go unused-var strictness `declared and not used`
  (guessing-game); (e) local `step2` binding not emitted (calculator go/py `undefined: step2`); (f) **[from #205]**
  `.for_each` with a BLOCK / mutating / `println` closure body fails on go/python (the pre-existing
  statement-closure-body gap — `for_each` lowering itself is correct on rust/js/python; excluded from the all-5 fixture);
  (g) **[from #205]** chained `.map(..).reduce(..)` over a record-field projection mislowers on go (nested-IIFE inference
  gap; binding the projection to a typed `let` first works ×5). Triage each as its own fix or example correction.
- **[Q-chat-protocol-allfail] `chat-protocol` fails build on all 5 — DIAGNOSED → folded into Q-match-exprpos** — bug ·
  **RESOLVED-AS-DUP (diagnosed 2026-06-03 13:44)** · — · links Q-match-exprpos · note: the all-5 failure (js `Unexpected
  token ')'`, py `'(' was never closed`, ts `Expression expected`, go enum-return) is the **expression-position
  control-flow lowering** producing unbalanced parens on js/py + the go-enum-return cluster — NOT a distinct root cause.
  Folded into **Q-match-exprpos** (D, un-deferred) and **Q-go-enum-return-boxing** (E). No separate work item.

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
