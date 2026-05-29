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
- **Property claims:** "one language, many targets" (JS/TS/Py/Rust/Go
  codegen parity on examples); "effects on every function"; "targeted
  output, not a runtime".
- **Release actions (all escalate — external/irreversible):** CI live;
  VS Code extension → marketplace; `bock` → crates.io + GitHub
  Releases; docs site deploy; announcement post.
- **Mapped items:** `Q-stdlib` (core stdlib — v1-blocking), `D4`, `D5`,
  `ItemB` (project-mode codegen), `Q-20.1-xref`, `Q-cl-dates`,
  `Q-cl-0515`, `Q-fconf`, `Q-vscode-test`.
- **Acceptance:** conformance passes per target; all 20 examples
  `check`/`build`/`test` clean on ≥ JS+Py+Rust; clippy clean.

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
collections/test), pilot-first. Links: DV1, Q-stdlib, DQ5, #100.
