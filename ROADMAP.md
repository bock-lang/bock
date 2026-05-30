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
- **Mapped items:** `Q-stdlib` (core stdlib — v1-blocking) + `Q-list-codegen` (List
  built-in method codegen — NEW v1-blocking; gates iter/collections; ESCALATED); the
  codegen-correctness gate **DONE** — `Q-fconf` (#114/#115) + `Q-codegen-fixes` (#121),
  DV9 closed; `D4`, `D5`, `ItemB` (project-mode codegen). Done this cycle (#117-#127):
  `Q-20.1-xref`, `Q-vscode-test`, `Q-fmt-bock`, `Q-prelude-inject`, `Q-ci-vscode-test`,
  `Q-ts-codegen`, `Q-cl-dates`, `Q-cl-0515`, `Q-py-optional` (+ Go typed-payload). The
  Optional-payload parity residue is CLOSED (#124/#126/#127); remaining parity follow-ups:
  `Q-match-exprpos`, `Q-go-list-literal`, `Q-ts-generic-impl`.
- **Acceptance:** **execution** conformance passes per target (Q-fconf, live); all
  20 examples `check`/`build`/`test` clean on ≥ JS+Py+Rust; clippy clean. UPDATE
  (2026-05-30): execution conformance (#114/#115) + the DV9 fixes (#121) + the
  Optional-payload family closed across all 5 (#124/#126/#127, 55+ exec pairs) make the
  tested-parity property solid for the constructs covered. BUT core.iter surfaced a deeper
  gate — **List built-in method codegen exists on no backend** (DV10 / Q-list-codegen,
  v1-blocking, ESCALATED) — which blocks core.iter (+ DQ16 floor) and core.collections.

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

## MS-stdlib — DECIDED: v1-blocking + SCOPE decided (operator + Design, 2026-05-29)
The **core** standard library (§18.3) ships in **v1** and **blocks v1.0**
(→ `queue.md` Q-stdlib). Resolves the "ship what's done vs §18-full-stdlib"
tension in favor of shipping it. Distinct from v2's stdlib *expansion*
(HTTP/logging/etc.). **SCOPE (Design 2026-05-29; §18.3 reframed with v1/v1.x
tiering in #100):** v1 = **11 modules** at minimum-useful surface —
`option, result, collections, string, iter, compare, convert, error,
effect, time, test`; **Reserved for v1.x** — `types, math, memory,
concurrency`. Q-stdlib implements them over three rounds (R1
effect/error/compare/convert/iter · R2 option/result/string/time · R3
collections/test), pilot-first. **UPDATE (2026-05-30):** core.iter (R1) surfaced that List
built-in methods don't codegen on any backend → `Q-list-codegen` (v1-blocking, ESCALATED) is a
prerequisite for iter's List-backed floor AND collections (R3); the for→Iterable desugar itself
is proven (T1 green ×5). core.iter floor pending Design DQ16 (List-backed vs List-free). Links:
DV1, DV10, Q-stdlib, Q-list-codegen, DQ5, DQ16, #100.
