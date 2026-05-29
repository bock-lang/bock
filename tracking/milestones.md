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
- **Mapped items:** `D4`, `D5`, `ItemB` (project-mode codegen),
  `Q-20.1-xref`, `Q-cl-dates`, `Q-cl-0515`, `Q-fconf`, `Q-vscode-test`.
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

## MS-stdlib — UNSCHEDULED (needs a milestone decision)
The **core** standard library (`core.collections/string/math/iter/…`,
§18.3) is presented by the spec as v1, but is unimplemented
(`stdlib/` empty) and has **no milestone home**: v1.0 is "ship what's
done" (implies it's done — it isn't) and v2 is *expansion* (HTTP/logging,
not the core modules). Decision needed: is the core stdlib a v1.0
deliverable (and thus a v1.0 blocker), a new milestone (e.g. v1.0.x /
v1.1), or explicitly Reserved with §18 reconciled to match?
Links: `divergences.md` DV1, `queue.md` Q-stdlib, `design-questions.md`
DQ5. **This is the highest-leverage open planning decision.**
