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
- **§:** §18.3 · **spec-says:** ~15 `core.*` modules (collections,
  string, math, iter, …) ship in v1 · **impl-does:** `stdlib/` empty
  (0 modules; prelude ≈ 9 builtins + a few type-checker intrinsics)
- **Classification:** spec-ahead-of-impl
- **Disposition:** schedule the implementation (→ queue `Q-stdlib`,
  needs `milestones.md` MS-stdlib); reconcile §18's v1-status once
  scheduled. Acknowledged everywhere but scheduled nowhere — this is
  the gap the 2026-05-29 stdlib audit surfaced.
- **Status:** open

### DV2 — §13.3/§13.4 concurrency: changelog asserts Reserved, spec doesn't
- **§:** §13.3 (channels), §13.4 (sync primitives) · **spec-says:**
  live spec lists `Channel`, `Mutex/RwLock/Atomic/WaitGroup/OnceCell`
  as plain v1 surface, NO Reserved marker · **changelog-asserts:**
  `20260514-0449` claims they were "Reserved for v1.x per the D1+D2
  batch" — but no such batch changelog exists and the assertion was
  never applied to the spec.
- **Classification:** gap (unapplied decision / changelog-vs-spec)
- **Disposition:** decide via `design-questions.md` DQ3/DQ4, then
  either mark the spec Reserved or correct the 0449 cross-reference.
- **Status:** open

---

## Resolved (this session / spec-revision — kept for traceability)

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
