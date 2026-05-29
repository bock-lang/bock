# Design Questions — open decisions

**The one question:** what should the behavior be? (undecided choices)

Not factual mismatches (→ `divergences.md`) and not actionable work
(→ `queue.md`). A divergence whose disposition needs a decision links
here. Migrated from the open D1-refresh / D2-FOUND rows (most are now
decided — see `## Decided`).

**Core-spec rule (see orchestrator contract — Design authority):**
questions touching *core specification* (language semantics, type/effect/
context rules, stdlib surface, the §1.3 target set) are **escalated to
Design and filed in `escalations.md`** — the orchestrator does NOT decide
them, even with the operator present; Design Chat is the authoritative
core-spec voice. The orchestrator uses discretion on what's "core spec",
files the escalation, and **moves on** (doesn't block other work).
Non-core questions (e.g. CLI shape, which §20.1 declares non-normative)
the orchestrator may iterate on with the operator directly.

Schema: `[ID] question · § · context · status(open | escalated→Design |
decided→link)`

---

## Orchestrator design questions (non-core — iterate with operator)

### DQ1 — `bock check` default strictness
- **Question:** should `bock check` default to the project's configured
  strictness instead of requiring explicit `--strict`?
- **§:** §20.1 (CLI shape — non-normative per §20.1) · **context:**
  O1/O2 landed (#87) keeping `--strict` explicit, mirroring `bock build`.
  Non-core; parked as a smaller follow-up.
- **Status:** open

## Decided by Design (core spec — 2026-05-29; reconciled in #100)

These touched core specification (§11/§13/§18 language + stdlib surface),
were escalated to the Design Chat, and the Design Chat (with the operator)
decided them on 2026-05-29. The orchestrator reconciled the spec **and**
the implementation in #100 (changelog `20260529-2251-specs-changes.md`).

### DQ2 — `@performance` budget literal syntax
- **Question:** should `@performance(max_latency: 100, ...)` accept bare
  integers, or require unit-suffixed literals (`100.ms`, `50.mb`)?
- **§:** §11.4 · **context:** the context-audit example used bare ints
  → E8003.
- **Decision:** require unit-suffixed literals; bare ints stay E8003.
  Time units `.ns/.us/.ms/.s/.min/.h`; memory units `.b/.kb/.mb/.gb/.tb`
  (decimal scaling). §11.4 normative paragraph added. Reconciling this
  also exposed and fixed an impl divergence (interpreter required the
  parenthesized `100.ms()` form; now accepts the no-parens literal) — see
  `divergences.md` DV3. Closes `queue.md Q-perf-example`.
- **Status:** decided → Design 2026-05-29; reconciled #100.

### DQ3 — §13.3 channels: bounded-channel v1 status
- **Question:** are bounded channels (`Channel.new(buffer: N)`) v1, or
  Reserved for v1.x?
- **§:** §13.3 · **context:** see `divergences.md` DV2.
- **Decision:** Reserved for v1.x (bundles with `core.concurrency`, itself
  Reserved per DQ5). §13.3 leading note added; example preserved as design
  intent. Resolves DV2.
- **Status:** decided → Design 2026-05-29; reconciled #100.

### DQ4 — §13.4 synchronization primitives: v1 vs Reserved
- **Question:** which of `Mutex/RwLock/Atomic/WaitGroup/OnceCell` ship
  in v1 vs Reserved for v1.x?
- **§:** §13.4 · **context:** same unapplied-0449-claim as DQ3 (DV2).
- **Decision:** all Reserved for v1.x (bundle with `core.concurrency`).
  §13.4 leading note added; enumeration preserved as design intent.
  Resolves DV2.
- **Status:** decided → Design 2026-05-29; reconciled #100.

### DQ5 — §18.3 core-module scope for v1
- **Question:** which of the ~15 §18.3 `core.*` modules are in the v1
  stdlib, and at what surface? (Q-stdlib is decided v1-blocking; this is
  its SCOPE.)
- **§:** §18.3 · **context:** seeds `queue.md Q-stdlib` phase planning.
- **Decision:** **11 v1 modules** — `core.option`, `result`, `collections`,
  `string`, `iter`, `compare`, `convert`, `error`, `effect`, `time`,
  `test` — each at its **minimum-useful subset**; **4 Reserved for v1.x** —
  `core.types`, `math`, `memory`, `concurrency`. §18.3 reframed with the
  tiering + acceptance criterion (conformance + a representative example
  compile/run on every shipping target). Scopes `queue.md Q-stdlib` into
  three rounds: R1 effect/error/compare/convert/iter · R2
  option/result/string/time · R3 collections/test.
- **Status:** decided → Design 2026-05-29; reconciled #100.

---

## Decided (D1-refresh / D2-FOUND — resolved by the spec-revision cycle)

22 of the 25 routed rows are resolved (links in `divergences.md`
"Resolved"): §13.5, §14.1/2, §16.3/4, §20.3, §20.5, §10.3/4/6, §18.5,
§19.7, §20.6, §1.3, §15, §17.6, §12.2 (×2), §4.7, §7.6, §6.1, §11
module-level annotations. Evidence: the per-section changelogs + the
20260514-0548 spec-revision artifact (confirmed applied to the live
spec). Only DQ1 (non-core) remains open; DQ2–DQ5 were decided by Design
2026-05-29 and reconciled in #100 (see "Decided by Design" above).
