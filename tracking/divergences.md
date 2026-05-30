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
  **impl-does:** **2/11 landed** — `core.error` (#103) + `core.compare`
  (#104); 9 remain. Loading mechanism (embedded source-compiled) works.
- **Classification:** spec-ahead-of-impl
- **Disposition:** implement the remaining 9 (→ queue `Q-stdlib`, SCOPED via
  DQ5). §18.3 v1-status reconciled in #100. Fan-out paused on `Q-bridge`/DQ6
  (DV4). Remains open until all 11 land.
- **Status:** open (2/11 landed; fan-out paused on Q-bridge/DQ6)

### DV4 — stdlib trait impls can't cover primitive types
- **§:** §18.3 (+§18.2) · **spec-says:** `core.compare`'s `Comparable`/
  `Equatable` (and analogous traits across modules) apply to the language's
  values, primitives included · **impl-does:** primitive receivers resolve
  methods via the hardcoded intrinsic table in
  `bock-types/src/checker.rs::resolve_method_return_type` and never consult
  the user/stdlib trait-impl table, so `impl Comparable for Int` + a call site
  → E4001 (#104). Stdlib traits cover only stdlib-defined types today.
- **Classification:** gap (missing checker↔bock-core bridge)
- **Disposition:** fix-impl → `queue.md` Q-bridge, gated on Design's DQ6
  (carries a stdlib-impl-vs-intrinsic precedence/coherence question). A
  near-universal prerequisite for a *useful* core stdlib.
- **Status:** open

### DV5 — §18.2 prelude vs §18.3 import for fundamental traits
- **§:** §18.2 / §18.3 · **spec-says:** §18.2 lists `Comparable`/`Equatable`
  as PRELUDE ("always available without import"); §18.3 lists them as
  `core.compare` members · **impl-does:** matches the named-import model —
  bare `Ordering`/`Less` without `use core.compare` → E1001; no prelude
  injection. Internally inconsistent spec.
- **Classification:** gap (spec internal inconsistency / undecided import model)
- **Disposition:** decide via `design-questions.md` DQ9 (prelude vs import for
  the fundamental traits), then reconcile §18.2/§18.3. Surfaced #104.
- **Status:** open

---

## Resolved (this session / spec-revision — kept for traceability)

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
