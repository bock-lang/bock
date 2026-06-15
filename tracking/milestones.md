# Milestones — what ships when

**The one question:** what ships in which version?

Version → theme → mapped item IDs (detail lives in `queue.md` /
`design-questions.md`, referenced by ID). Holds mapping + themes only.
**`ROADMAP.md` is GENERATED from this file** — do not hand-edit
`ROADMAP.md`.

---

## Current phase
**M.4 — Compiler migration. Complete.** Compiler crates, conformance
suite, examples, spec, extension, and docs ported into this repo under
the Bock identity.

## v1.0 — Public Release
**Theme:** ship what's already done — verify, polish, announce.
- **★ Design-audit affirmation (2026-06-09, `designs/2026-06-09-design-audit.md` R12/R-B):** ship v1
  **on current scope** — nothing in the audit grows v1.0; all its recommendations route to v1.x,
  positioning, or marketing. The audit's lean on timing (OQ3, operator's call): release-prep now —
  the governance window is open. Repositioned identity (equivalence + auditability lead; convenience
  pitch retired) awaits operator sign-off (R1, escalations) before any external copy moves.
- **★ v1.0 timing + scope DECIDED (2026-06-15, operator walk-through):** ship on current scope, but **clear a
  defined hardening pass first** (OQ3) before the cut — **scope = everything ready except pure docs** (D2-polish
  defers to v1.2), correctness-first (the equivalence cluster leads). → **MS-v1.0-hardening** (below). Identity
  sign-off (R1) + marketing wedge (OQ1) **delegated to the marketing chat**
  (`tracking/handoffs/2026-06-15-marketing-positioning.md`); R6 ratified; OQ2 publish; OQ4 (§10.8 demo) deferred to
  v1.x. The v1.0 cut + release actions (all escalate) follow the hardening pass + the marketing copy lock.
- **★ POSITIONING RESOLVED (2026-06-15, marketing chat → operator sign-off; handoff `2026-06-15-marketing-positioning.md`):**
  OQ1 + R1 returned and landed. A **nested split, not one ICP**: **Identity = the guarantee** (provable cross-target
  equivalence + auditable/reproducible AI pipeline) carries the homepage hero + the **§1.1 spec amendment** (LANDED,
  changelog `20260615-s1.1-equivalence-amendment`); **launch wedge = SDK / library vendors** (project mode's native
  npm/pip/cargo/go-mod packages + transpiled tests ARE their product promise — fully in current scope, no FFI); **macro
  narrative = the trust-scarcity shift**, held in air-cover register only (NOT a product-capability lead — zero shipping
  users / no compliance track record would overclaim and collide with the §1.1 enterprise-procurement non-audience). The
  signed public identity sentence uses **"verifies," not "proves"** (R9-ledger-accurate; shares the verb with §1.1).
  **Website scope:** homepage stays guarantee-led; the SDK-vendor wedge gets its **own use-cases page** — a NEW pending
  route (slug TBD by marketing: `/use-cases` | `/for-sdk-authors`; 404s cleanly until copy lands, per deferred-route
  discipline). The `/get-started` copy lock (was gated on R1) is now **UNBLOCKED** → marketing locks it, implementation
  renders. OQ2 corpus reframed as equivalence-moat evidence supporting the wedge. **Boundary:** identity sentence +
  website copy stay marketing-owned + human-approved; the orchestrator landed only the §1.1 amendment + this reconcile.
  NEXT (marketing, non-blocking): draft the wedge page — it needs a **verified SDK-vendor demo** (one Bock source →
  emitted native packages ×5 + a real conformance report), an implementation delegation before the copy locks (→ `queue.md`).
- **Property claims:** "one language, many targets" (JS/TS/Py/Rust/Go
  codegen parity on examples); "effects on every function"; "targeted
  output, not a runtime".
- **Release actions (all escalate — external/irreversible):** CI live;
  VS Code extension → marketplace; `bock` → crates.io + GitHub
  Releases; docs site deploy; announcement post.
- **Mapped items:** `Q-codegen-completeness` (NEW v1-blocking MILESTONE — the audit showed the codegen
  substrate is materially incomplete: cross-module/enums broken 5/5, generics/Result/closures 3-4/5; phased
  P0-P4, ~10-15 PRs) gates `Q-stdlib` (core stdlib); `D4`, `D5`, `ItemB`. Codegen-correctness gate
  (#114/#115/#121, DV9) DONE. Done this cycle (#117-#129): `Q-20.1-xref`, `Q-vscode-test`, `Q-fmt-bock`,
  `Q-prelude-inject`, `Q-ci-vscode-test`, `Q-ts-codegen`, `Q-cl-dates`, `Q-cl-0515`, `Q-py-optional`,
  read-only `Q-list-codegen` (#129). Optional-payload parity CLOSED (#124/#126/#127).
- **Acceptance:** **execution** conformance passes per target (Q-fconf, live); all
  20 examples `check`/`build`/`test` clean on ≥ JS+Py+Rust; clippy clean. UPDATE
  (2026-05-30): execution conformance (#114/#115) + DV9 fixes (#121) + the Optional-payload family
  closed across all 5 (#124/#126/#127) made tested-parity solid for the constructs covered — but a 3-agent
  **codegen audit** (prompted by core.iter) showed that was a NARROW slice: **cross-module `use` + user
  enums broken on ALL 5**, generics/Result/closures/Optional-methods on 3-4/5; the 3 "landed" stdlib modules
  are check-only. → **Q-codegen-completeness** (v1-blocking milestone, phased) must land before the stdlib.
  v1.0 acceptance now requires the codegen-completeness milestone + per-target EXECUTION (not check-only) of
  the stdlib on every shipping target. **UPDATE (2026-06-03):** the "20 examples build/test clean on ≥JS+Py+Rust"
  acceptance gate is **NOT met** — an examples-compile audit found the `real-world/*` examples largely fail project-mode
  build (ts 0/6, rust 0/6, go 0/6; js/py pass syntax-only validate). The 430/0 conformance was a NARROW slice again
  (List `.map()`-method-with-closure mislowered = Q-list-method-codegen; rust Cargo.toml workspace = Q-rust-cargo-workspace;
  examples not exec-tested = Q-examples-exec-coverage). An **examples-hardening workstream** (exec-gate the examples ×5 +
  fix the codegen clusters) is a v1.0 prerequisite. v1.0 is further out than the ItemB-complete picture implied.
  **UPDATE (2026-06-03 13:44) — full audit done + scope decided:** the complete 20×5 audit (built out-of-tree) gives the
  true matrix — **js 10/20·ts 2/20·py 15/20·rust 3/20·go 1/20 compile** (hello-world the only all-5; rust/go fail on real
  codegen, not just the env bug). **~9 root-cause clusters** identified (see MS-examples-hardening / queue). **Operator
  decided:** v1.0 **holds all 5 targets at the 'examples green' acceptance bar** (NOT tiered to ≥JS+Py+Rust — the gate
  wording is superseded), fixed in leverage order (Q-list-method-codegen first); the examples-exec gate
  (Q-examples-exec-coverage) lands **informational-first → blocking**. v1.0 acceptance = MS-examples-hardening complete
  (all 20 examples compile+run on all 5) + per-target EXECUTION of the stdlib + release actions (escalate).

## v1.1 — Editor & Tooling Polish
**Theme:** delight in the editor; close interpreter gaps.
- **★ Editor-feature slice DELIVERED EARLY (owner-directed, 2026-06-09,
  #320–#331):** AIR tree viewer, target preview, diagnostics quick-fixes,
  semantic tokens, inlay hints, symbol rename, find-references, document
  symbols, strictness picker, + hover/spec-search/decisions/annotations
  depth (detail by ID in `queue.md`). Landed on main pre-v1.0; they ship
  with whatever release vehicle v1.0 settles on.
- Remaining: standalone LSP; incremental compilation + persistent cache;
  LSP completion (DV19 → Design); strictness migration assistant
  (Q-ext-migration-assistant); further hover-card work as scoped.
- **★ Design-audit reorientation (2026-06-09 audit, R4/R5/R6/R7):**
  - **Tooling is agent-first:** `Q-mcp-server` (bock-mcp: check/build/test/
    inspect/conformance as MCP tools) **leads** the v1.x tooling list; the
    §20.3 human-facing panels ship behind it (spec note + changelog
    `20260610-design-audit-spec-touches.md`).
  - **First v1.x design pass = `Q-ai-loop-design-pass`** (agentic repair-loop /
    AI-layer composability: loop budgets, convergence, fallback policy — R5).
  - **Prioritization principle (R6, RATIFIED 2026-06-15 — operator walk-through):**
    verification features over surface-area features — property testing
    (`@property`) → runtime guardrails (§15.4) → refinement predicates (§4.7),
    ahead of new targets / FFI breadth.
  - **Runtime-AI pillar (OQ4, operator 2026-06-15): DEFER** the §10.8
    adaptive-effect-handlers end-to-end demo behind the release window; revisit
    at the first v1.x planning pass (R9 keeps §10.8 specced-unvalidated; the R6
    verification track leads).
  - **Target demand-gate (R7, affirms §1.3 posture):** a v1.x target is added
    only on concrete demand + a fully automated end-to-end conformance lane —
    never on a calendar.
  - **Model-familiarity workstream (R3/R-A, queue):** Q-context-pack ·
    Q-synthetic-corpus · Q-diagnostics-agent-audit; + Q-dogfood-tool (R8).
    Corpus publication: **OQ2 → PUBLISH** (operator, 2026-06-15) — the context
    pack + synthetic corpus ship as open artifacts.

## v1.2 — Deferred Loose Ends
**Theme:** finish what v1.0 deferred.
- Cancel runtime; AUDIT-006; `std.time.SystemClock` live impl;
  language-guide depth. (See `snapshot.md` "Deferred".)

## v2 — Ecosystem Growth
**Theme:** from compiler to ecosystem.
- Stdlib **expansion** (HTTP server primitives, structured logging,
  config loading, async streaming — note: distinct from the *core*
  stdlib, see MS-stdlib); additional targets (Swift/Kotlin/C#);
  package registry; macros; self-hosting; LLVM native; WASM;
  distributed type-checking. Order intentionally unfixed.

---

## MS-stdlib — ★ COMPLETE (2026-06-01) — v1 stdlib DONE, 11/11 modules ×5
**★ DONE:** the v1 core standard library is COMPLETE — all 11 modules execute on all 5 targets
(error/compare/convert/iter/effect/option/result/string/test/collections as Bock modules + time as a builtin).
R1+R2+R3 landed (#103-#170); the codegen-completeness milestone + a long tail of generic-codegen fixes
(#131-#170) built the substrate the stdlib needed. Q-stdlib SATISFIED → **unblocks D4** (stdlib reference docs) →
D5 → ItemB. v1.0's stdlib gate is cleared. [Historical scope/decision record below.]
The **core** standard library (§18.3) ships in **v1** and **blocks v1.0**
(→ `queue.md` Q-stdlib). Resolves the "ship what's done vs §18-full-stdlib"
tension in favor of shipping it. Distinct from v2's stdlib *expansion*
(HTTP/logging/etc.). **SCOPE (Design 2026-05-29; §18.3 reframed with v1/v1.x
tiering in #100):** v1 = **11 modules** at minimum-useful surface —
`option, result, collections, string, iter, compare, convert, error,
effect, time, test`; **Reserved for v1.x** — `types, math, memory,
concurrency`. Q-stdlib implements them over three rounds (R1
effect/error/compare/convert/iter · R2 option/result/string/time · R3
collections/test), pilot-first. **UPDATE (2026-05-30):** core.iter's pursuit (4 attempts) + a 3-agent
codegen audit established the codegen substrate is materially incomplete for the stdlib (cross-module + enums
broken 5/5; generics/Result/closures 3-4/5; the 3 "landed" modules are check-only, never executed
cross-module). Operator decided a **codegen-completeness MILESTONE** (`Q-codegen-completeness`, v1-blocking,
phased P0-P4) that must land before the stdlib resumes. Q-stdlib R1 PAUSED behind it; the for→Iterable desugar
is proven (T1 ×5) and resumes after P0/P1. DQ16 resolved (keep List-backed floor; build the prerequisite).
Links: DV1, DV10-DV15, Q-stdlib, Q-codegen-completeness, DQ5, DQ16, DQ18, #100, #129.

## MS-projectmode — ★ COMPLETE (2026-06-03) — per-module output + project mode + config tables
**★ DONE:** ItemB complete (S0–S8, #181–#194 + S8 close). All 5 targets emit per-module native-import trees (DV13);
project mode is real — the `Scaffolder` emits per-target manifests + formatter/opt-in-linter configs + README +
`@test` functions transpiled to each target's test framework, honoring `bock.project` `[targets.<T>]` config
(defaults per §20.6.2); `--source-only` is bare (DV18). 430 exec pairs / 0 failed REQUIRE=all. rust+go transpiled
tests RUN-verified; js/ts/python compile-verified (→ Q-ci-projectmode-tooling for full cert). **ItemB was v1.0's last
mapped engineering item → v1.0 engineering runway clear; what remains for v1.0 is release actions (all escalate) +
two non-blocking pre-release follow-ups (Q-ci-projectmode-tooling, Q-go-gofmt-listclosure).** [Historical record below.]
**v1.0's last engineering milestone** (ItemB, expanded). Two owner decisions 2026-06-02 (eyes-open, after the
orchestrator surfaced the cost): (1) **DQ19 → per-module native tree is the v1 output model** (not bundling) —
re-opens **DV13** (native per-target cross-file imports that compile+run); (2) **config tables pulled forward
into v1** (`[targets.<T>]` deep + `[targets.<T>.scaffolding]` shallow) — un-reserved from v1.x (spec §20.6.2/
§20.7/A.3 reconciled). **Plan:** `plans/2026-06-02-itemB-per-module-projectmode-plan.md`, staged **S0–S8**: S0
spec/tracking reconcile (this entry) → S1 native imports + harness multi-file run, **pilot python** → S2 js/ts →
S3 rust/go (with minimal manifest — those targets can't run multi-file without it) → S4 flip default + retire
bundling (**DV13 CLOSED**) → S5 scaffolding framework + `bock.project` config parsing → S6 per-target scaffolders
+ deep-config branches (Vitest|Jest, Black|Ruff…) → S7 transpiled tests + formatter-clean gate → S8 internal docs.
**Progress:** **S0–S6 DONE → DV13 + DV18 CLOSED.** S0–S4 (#181/#182/#184/#185/#186): per-module native trees on all
5, sole path. S5 #188: scaffolding framework + config parsing. S6a #190: project-mode architecture (scaffolder owns
manifests, source mode bare → DV18 closed; harness builds project-mode). S6b #191: enriched per-target scaffolders
×5 (manifests/configs/README) + go core.error fix. **427 exec pairs / 0 failed.** **Next = S7 (transpiled `@test`
files per framework + formatter-clean release gate); operator pre-S7 checkpoint pending.** **Invariant:**
`run-conformance.sh REQUIRE=all` stays green every PR (427/427). NEW FOUND → Q-error-message-jstspy (core.error
collision on js/ts/python; go fixed). S7–S8 remain. **Still v1.x:** `--deliverable`,
`--no-tests` (§20.1). External `/get-started` copy = **ItemD** (escalates). Links: ItemB, DV13, DQ19, §20.6.1/2,
§20.7, changelogs `20260602-1608-per-module-output-dq19.md` + `20260602-1608-projectmode-config-tables-v1.md`.

## MS-examples-hardening — v1.0 PREREQUISITE (opened 2026-06-03) — real-world examples compile+run ×5
**Theme:** close the gap between green conformance and green real-world programs. The 20×5 examples-exec audit
(2026-06-03 13:44) established the true matrix — **js 10/20·ts 2/20·py 15/20·rust 3/20·go 1/20 compile; hello-world the
only all-5 end-to-end** — and ~9 evidence-confirmed codegen clusters. Conformance (430/0) was a narrow slice: it
exercises the stdlib's FREE functions but not the real-world-shaped method/closure/concat/match-position patterns.
**Operator decision (2026-06-03):** v1.0 holds **all 5 targets** at the "examples green" bar (not tiered); fix in
**leverage order**; the examples-exec CI gate lands **informational-first**, ratcheting to blocking as clusters land.
- **Acceptance:** all 20 `examples/` **compile AND run** on all 5 targets in CI (the Q-examples-exec-coverage gate,
  flipped to blocking); no regressions vs the ratchet.
- **Progress (2026-06-03 23:05):** landed **17 PRs (#204–#221)**. A **5-WAY PARALLEL FAN-OUT** (#216 rust · #217 js ·
  #218 py · #219 ts · #220 go — one cluster-batch per backend, file-disjoint, `generator.rs` untouched in every one;
  combined conformance 0-failed/124-fixtures verified on merged main) **leapt the examples matrix: runtime-working
  js 7→14 · ts 5→7 · py 9→12 · rust 8→9 · go 1→7 / 20** (30→49 example-target passes; baseline ratcheted #221). go's
  all-5 bet is paying off (1→7). Per-backend clusters done; the fan-out **converged on the remaining SHARED-lowering
  work** — `Q-exprpos-shared-desugar` (the match-exprpos core; value-position diverging control-flow needs a shared AIR
  temp-hoist; go-blocking), `Q-propagate-operator-noop` (`?` no-op on js/ts/py; maybe Design), `Q-list-range-pattern-shared`,
  `Q-guard-let-shared`, `Q-let-shadow-const`. **Next = the shared-lowering session (generator.rs/AIR — NOT parallelizable).**
  [prior 20:25 progress below.]
- **Progress (2026-06-03 20:25):** landed **11 PRs (#204–#214)** — adds Q-string-num-jstspygo (#213, §18.3 String/num/
  Char/Bool methods on js/ts/py/go; conformance 476→480; **microservice ts FAIL→PASS**). An INCIDENT (merged #213 with a
  failing windows-python lane — multibyte fixture output vs Windows-Python's codepage stdout) was caught + hotfixed (#214,
  ASCII fixture; root issue filed → Q-py-windows-utf8); merge discipline tightened (gate on `mergeStateStatus=CLEAN`).
  Runtime-working examples now **js 7 · ts 5 · py 9 · rust 8 · go 1 / 20**. **OPERATOR DECISION (2026-06-03): go HOLDS the
  all-5 v1.0 bar** (not tiered) — commit to the full go chain. Remaining: J, K, **D (match-exprpos — deep, go-blocking)**,
  go-Result-payload, Q-py-windows-utf8, Q-examples-codegen-misc (14 sub-items). [prior 18:01 progress below.]
- **Progress (2026-06-03 18:01):** landed **8 PRs (#204–#211)** — gate (M), A+B+C (#205), Q-impl-body-typecheck (#207),
  **rust L/F/G (#210)**, **go E (#209)**, baseline ratchet (#211). Combined conformance **430→476** REQUIRE=all.
  Runtime-working examples: **js 2→7 · ts 2→4 · py 7→9 · RUST 2→8 · go 1→1** / 20. **rust jumped hard** (L cargo-workspace
  + F move/borrow + G String/num). **go is the lone stuck target (still 1/20)** — E (enum-return boxing) was a necessary
  prerequisite but cleared only one barrier; go examples now hit a deeper chain (§18.3 string-methods missing on go +
  match-exprpos + a Result-payload type-assert), so NO go example completes yet. G's String/num lowerings are rust-only →
  the js/ts/py/**go** split is Q-string-num-jstspygo. **Remaining (leverage order):** Q-string-num-jstspygo (unblocks go +
  js/ts/py runtime) → J (js-effect-export) → K (py-circular) → **D (match-exprpos — deep, all-backend, go-blocking)** →
  Q-examples-codegen-misc (13 sub-items). **STRATEGIC NOTE:** go (1/20) needs Q-string-num-jstspygo + D + go-Result-payload
  chained before any go example completes — worth an operator check on whether go holds the same v1.0 bar or tiers to v1.1.
- **Mapped items (queue.md), leverage order:** `Q-list-method-codegen` (A, HIGH, all 5 — receiver dup'd as 1st arg) →
  `Q-rust-cargo-workspace` (L, cheap, recovers 3 rust in-repo) → `Q-list-concat-codegen` (B) → `Q-const-enum-naming` (C)
  → `Q-go-enum-return-boxing` (E) → `Q-rust-move-codegen` (F) → `Q-rust-string-num-methods` (G) → `Q-js-effect-export`
  (J) → `Q-py-circular-import` (K) → `Q-match-exprpos` (D, deep, cross-4-backend; subsumes ex-Q-chat-protocol-allfail) →
  `Q-examples-codegen-misc` (minor/triage). Plus `Q-examples-exec-coverage` (M, the gate — built in parallel, disjoint
  files). LESSON (carried in memory `conformance-green-is-not-sufficient`): conformance fixtures must include
  real-world-shaped programs / the examples must be exec-tested — green conformance gave false confidence.

## MS-v1.0-hardening — v1.0 PREREQUISITE (opened 2026-06-15) — drain the ready queue to a clean floor before the cut
**Theme:** v1.0 engineering scope is complete (stdlib 11/11 ×5, codegen-completeness, project mode, examples green); the
operator chose (OQ3) to clear a **defined hardening pass** before cutting 1.0 rather than ship-and-patch. **Scope =
everything ready except pure docs** (D2-polish defers to v1.2). Correctness-first ordering: the equivalence cluster
leads, since "the five behave identically" is the headline guarantee.
- **Boundary principle:** a 1.0 blocker is anything that produces silent cross-target divergence, interpreter-oracle
  divergence (R11), silent-wrong output, or a target that won't compile/run a valid program. Diagnostics-credibility +
  chores ride in this pass too (operator chose the thorough boundary), but **behind** the correctness cluster.
- **Tier A–D — equivalence cluster (lead):** interp parity `Q-interp-list-concat` / `Q-interp-compare-ordering` /
  `Q-interp-float-ieee-equality` (R11); silent-wrong codegen `Q-go-tailmatch-unreachable-panic` /
  `Q-displayable-interpolation-dispatch` / `Q-bounded-comparable-codegen` / `Q-js-handling-let-redeclaration`;
  soundness/won't-compile `Q-bracket-bounds-unenforced` / `Q-prelude-impl-missing-import` /
  `Q-rust-clone-insertion-gaps` + `Q-rust-callarg-borrow-mismatch` / `Q-ts-variant-constructed-let-typing`; +
  **`Q-dq31-container-element-eq`** (the DQ31 ruling impl, NEW 2026-06-15).
- **Diagnostics-credibility:** `Q-error-catalog-completeness`, `Q-diag-structure-misc`, `Q-diag-brief-span-format`,
  `Q-errors-render-byte-col-drift`, `Q-w1001-effect-import-false-positive`.
- **Chores/cleanup:** `Q-context-pack-reconcile`, `Q-examples-matrix-undodge`, bock-core cleanup
  (`Q-core-dead-equals-registration` / `Q-core-legacy-list-builtins`), `Q-exec-output-directive-wiring`,
  `Q-ts-print-scaffold-types`.
- **Progress (2026-06-15) — WAVE 1 + WAVE 2 COMPLETE (#352–#357):** the equivalence cluster + diagnostics-credibility
  landed (14 items). Wave 1: go tail-match (#352), ts variant-let (#353), interp/core parity ×5 (#354), bracket-bounds
  soundness (#355). Wave 2: diagnostics-credibility batch (#356), DQ31 container element-eq (#357). All ×5-clean, gate +
  examples-matrix green per PR; combined-tree main CI verified. Surfaced FOUND/OPEN (queue): 4 codegen/cleanup bugs +
  3 design questions (code-renumbering, Hashable-on-collection-keys, transitive-unbounded generic bounds). **REMAINING:**
  Wave 3 (per-backend codegen families: rust-ownership, displayable-interp-dispatch, bounded-comparable,
  js-handling-let-redecl, prelude-impl-missing-import) + chores (context-pack-reconcile, examples-matrix-undodge,
  exec-output-directive-wiring, ts-print-scaffold, sync-vocab-script) + the new FOUND bugs. Not yet dispatched (the
  2026-06-15 session wound down cleanly at the Wave-2 boundary).
- **Acceptance:** the ready queue is drained to {pure docs (D2-polish) ∪ v1.x-deferred}; the combined-tree gate stays
  green every PR; the equivalence cluster lands first. After acceptance → v1.0 release-prep (spec version stamp,
  user-facing release notes, distribution — all release ACTIONS escalate) + the marketing copy lock (gated on R1/OQ1).
- **Defers (NOT in this pass):** `D2-polish` (pure docs → v1.2); the audit's v1.x workstreams (Q-mcp-server,
  Q-ai-loop-design-pass, the model-familiarity items beyond Q-context-pack-reconcile).
