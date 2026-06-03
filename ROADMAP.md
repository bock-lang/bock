<!-- DO NOT EDIT — generated from tracking/ by tools/scripts/gen-tracking-views.sh -->

# Roadmap

## Current phase
**M.4 — Compiler migration. Complete.** Compiler crates, conformance
suite, examples, spec, extension, and docs ported into this repo under
the Bock identity.

## v1.0 — Public Release
**Theme:** ship what's already done — verify, polish, announce.
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
- AIR tree view; target preview; standalone LSP; incremental
  compilation + persistent cache; diagnostics quick-fixes; hover-card
  improvements. (Mapped items: TBD as scoped.)

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
- **Progress (2026-06-03 16:56):** the gate (M, #204, informational) + clusters **A+B+C (#205)** + **Q-impl-body-typecheck
  (#207)** landed. Runtime-working rose **js 2→7 · ts 2→4 · py 7→9 / 20** (rust 2, go 1 unchanged — blocked on E/F/G/D),
  0 regressions, conformance **455→460**. #207 made the checker type-check impl/class method bodies — caught a REAL latent
  `core.error` field/method value-position bug + fixed a `Self` false-positive; a correctness win (example output
  byte-identical — codegen fallbacks already covered method-body list ops). Cluster C: const done; enum-variant/trait-name
  residue now runtime (→ K). **Remaining (leverage order):** Q-rust-cargo-workspace (cheap, +3 rust), E (go-enum-boxing),
  F (rust-move), G (rust-string), J (js-effect-export), K (py-circular), D (match-exprpos, deep), Q-examples-codegen-misc.
- **Mapped items (queue.md), leverage order:** `Q-list-method-codegen` (A, HIGH, all 5 — receiver dup'd as 1st arg) →
  `Q-rust-cargo-workspace` (L, cheap, recovers 3 rust in-repo) → `Q-list-concat-codegen` (B) → `Q-const-enum-naming` (C)
  → `Q-go-enum-return-boxing` (E) → `Q-rust-move-codegen` (F) → `Q-rust-string-num-methods` (G) → `Q-js-effect-export`
  (J) → `Q-py-circular-import` (K) → `Q-match-exprpos` (D, deep, cross-4-backend; subsumes ex-Q-chat-protocol-allfail) →
  `Q-examples-codegen-misc` (minor/triage). Plus `Q-examples-exec-coverage` (M, the gate — built in parallel, disjoint
  files). LESSON (carried in memory `conformance-green-is-not-sufficient`): conformance fixtures must include
  real-world-shaped programs / the examples must be exec-tested — green conformance gave false confidence.
