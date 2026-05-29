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

## Escalated to Design (core spec — filed in `escalations.md`, not decided here)

These touch core specification (§11/§13/§18 language + stdlib surface).
Filed and routed to the Design Chat; work proceeds elsewhere meanwhile.

### DQ2 — `@performance` budget literal syntax
- **Question:** should `@performance(max_latency: 100, ...)` accept bare
  integers, or require unit-suffixed literals (`100.ms`, `50.mb`)?
- **§:** §11.4 · **context:** the context-audit example uses bare ints
  → E8003. Blocks `queue.md Q-perf-example`.
- **Status:** escalated → Design (escalations.md)

### DQ3 — §13.3 channels: bounded-channel v1 status
- **Question:** are bounded channels (`Channel.new(buffer: N)`) v1, or
  Reserved for v1.x?
- **§:** §13.3 · **context:** see `divergences.md` DV2 — spec lists
  channels as plain v1, but the 0449 changelog asserts a never-applied
  Reserved decision. The decision then lets DV2 reconcile spec vs changelog.
- **Status:** escalated → Design (escalations.md)

### DQ4 — §13.4 synchronization primitives: v1 vs Reserved
- **Question:** which of `Mutex/RwLock/Atomic/WaitGroup/OnceCell` ship
  in v1 vs Reserved for v1.x?
- **§:** §13.4 · **context:** same unapplied-0449-claim as DQ3 (DV2).
- **Status:** escalated → Design (escalations.md)

### DQ5 — §18.3 core-module scope for v1
- **Question:** which of the ~15 §18.3 `core.*` modules are in the v1
  stdlib, and at what surface? (Q-stdlib is decided v1-blocking; this is
  its SCOPE.)
- **§:** §18.3 · **context:** seeds `queue.md Q-stdlib` phase planning;
  also confirm whether the historical D1-refresh §18.3 row was substantive
  or just the resolved core.time expansion.
- **Status:** escalated → Design (escalations.md)

---

## Decided (D1-refresh / D2-FOUND — resolved by the spec-revision cycle)

22 of the 25 routed rows are resolved (links in `divergences.md`
"Resolved"): §13.5, §14.1/2, §16.3/4, §20.3, §20.5, §10.3/4/6, §18.5,
§19.7, §20.6, §1.3, §15, §17.6, §12.2 (×2), §4.7, §7.6, §6.1, §11
module-level annotations. Evidence: the per-section changelogs + the
20260514-0548 spec-revision artifact (confirmed applied to the live
spec). Only DQ2-DQ5 (escalated) + DQ1 (non-core) remain.
