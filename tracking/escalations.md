# Escalation Register

Items the orchestrator surfaces for the human's decision. The
orchestrator escalates immediately for blocking/high-severity
items; lower-severity items may batch into the daily digest.

Entry format:

```
## [YYYY-MM-DD HH:MM UTC] <short title>

**Type:** strategic | external | resource | conflict | failure | scope
**Severity:** low | medium | high | blocking
**Trigger:** <what caused this to escalate>
**Context:** <what the human needs to decide>
**Options:** <what the orchestrator considered>
**Recommendation:** <orchestrator's informal read>
**Awaiting:** <what response is needed>
**Status:** pending | responded | resolved
```

Human response format (appended to the entry):

```
**Response (YYYY-MM-DD HH:MM UTC):**
<decision and reasoning>

**Authorized actions:**
<what the orchestrator may now do>
```

---

## [2026-05-29 20:24 UTC] Core-spec design questions → Design Chat (DQ2–DQ5)

**Type:** design (core-spec)
**Severity:** low (non-blocking — filed and moving on per the Design-authority rule)
**Trigger:** the tracking-consolidation reconciliation surfaced four
undecided questions that touch core specification. Per the core-spec
rule, the orchestrator files them for the Design Chat rather than
deciding them, and continues other work.
**Context (each is in `design-questions.md`, with links):**
- **DQ2 §11.4** — does `@performance(max_latency: 100, …)` accept bare
  integers, or require unit-suffixed literals (`100.ms`)? (blocks the
  context-audit example fix Q-perf-example)
- **DQ3 §13.3** — are bounded channels (`Channel.new(buffer: N)`) v1 or
  Reserved for v1.x? (resolves the DV2 changelog-vs-spec divergence)
- **DQ4 §13.4** — which of `Mutex/RwLock/Atomic/WaitGroup/OnceCell` are
  v1 vs Reserved? (DV2)
- **DQ5 §18.3** — the v1 core-module SCOPE for the stdlib (Q-stdlib is
  decided v1-blocking; this is which modules / what surface).
**Options:** Design Chat decides each; orchestrator then reconciles
spec/divergences and unblocks the linked queue items.
**Recommendation:** none offered on the merits (core-spec is Design's
call). DQ5 is the most leverage — it scopes the v1 stdlib (Q-stdlib).
**Awaiting:** Design Chat (with the operator) decisions, routed back here.
**Status:** resolved

**Response (2026-05-29 22:44 UTC):** Design Chat (with the operator) decided
all of them:
- **DQ2 (§11.4 @performance):** require unit-suffixed literals; bare ints
  stay E8003. Time `.ns/.us/.ms/.s/.min/.h`; memory `.b/.kb/.mb/.gb/.tb`
  (decimal).
- **DQ3 + DQ4 (§13.3/§13.4 concurrency):** both Reserved for v1.x; they
  bundle with `core.concurrency`. (The escalation said "four questions";
  DQ3 and DQ4 were grouped into one concurrency question in the Design
  prompt — no question was dropped, the count was a grouping artifact.)
- **DQ5 (§18.3 stdlib scope):** 11 v1 modules at minimum-useful surface
  (option/result/collections/string/iter/compare/convert/error/effect/
  time/test); 4 Reserved for v1.x (types/math/memory/concurrency).

**Authorized actions:** the orchestrator reconciled the spec **and** the
implementation in #100 (changelog `20260529-2251-specs-changes.md`):
`design-questions.md` DQ2–DQ5 → decided; `divergences.md` DV2 resolved +
DV3 added (a parens-vs-literal impl divergence found and fixed in the same
PR); `queue.md` Q-stdlib scoped + unblocked and Q-perf-example closed;
`milestones.md` MS-stdlib scope recorded. The stdlib implementation (R1/R2/
R3) follows, starting with a one-module pilot.
