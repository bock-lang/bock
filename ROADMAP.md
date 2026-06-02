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
  the stdlib on every shipping target.

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

## MS-projectmode — v1-BLOCKING (in flight, from 2026-06-02) — per-module output + project mode + config tables
**v1.0's last engineering milestone** (ItemB, expanded). Two owner decisions 2026-06-02 (eyes-open, after the
orchestrator surfaced the cost): (1) **DQ19 → per-module native tree is the v1 output model** (not bundling) —
re-opens **DV13** (native per-target cross-file imports that compile+run); (2) **config tables pulled forward
into v1** (`[targets.<T>]` deep + `[targets.<T>.scaffolding]` shallow) — un-reserved from v1.x (spec §20.6.2/
§20.7/A.3 reconciled). **Plan:** `plans/2026-06-02-itemB-per-module-projectmode-plan.md`, staged **S0–S8**: S0
spec/tracking reconcile (this entry) → S1 native imports + harness multi-file run, **pilot python** → S2 js/ts →
S3 rust/go (with minimal manifest — those targets can't run multi-file without it) → S4 flip default + retire
bundling (**DV13 CLOSED**) → S5 scaffolding framework + `bock.project` config parsing → S6 per-target scaffolders
+ deep-config branches (Vitest|Jest, Black|Ruff…) → S7 transpiled tests + formatter-clean gate → S8 internal docs.
**Progress:** S0 DONE (#181 — spec/tracking reconcile); **S1 DONE (#182 — python per-module native tree + harness
multi-file run; 425 exec pairs / 0 failed)**. Next = S2 (js→ts). **Invariant:** `run-conformance.sh REQUIRE=all`
stays green every PR (now 425/425); bundling kept behind a flag until all 5 run natively; harness migrates
target-by-target (no big-bang). ~20–30 PRs. **Still v1.x:** `--deliverable`,
`--no-tests` (§20.1). External `/get-started` copy = **ItemD** (escalates). Links: ItemB, DV13, DQ19, §20.6.1/2,
§20.7, changelogs `20260602-1608-per-module-output-dq19.md` + `20260602-1608-projectmode-config-tables-v1.md`.
