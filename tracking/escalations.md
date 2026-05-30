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

## [2026-05-29 23:57 UTC] Stdlib pilot → 3 core-spec questions (DQ6–DQ8)

**Type:** design (core-spec)
**Severity:** low (non-blocking — filed; the pilot proceeds on safe defaults)
**Trigger:** the stdlib loading + `core.error` pilot plan
(`plans/2026-05-29-stdlib-loading-error-pilot-plan.md`) surfaced three
questions touching core specification. Per the core-spec rule the orchestrator
files them and continues; the pilot is dispatched on safe defaults.
**Context (each in `design-questions.md`):**
- **DQ6 §18** — should §18 normatively state the core-module implementation
  model (Bock source + per-target runtime shims, embedded in the compiler)? The
  model is currently only a tracking-level Design note; the spec doesn't state
  it and `stdlib/CLAUDE.md`'s shim path is wrong.
- **DQ7 §18.3** — the canonical v1 `core.error` surface (does `Error` carry
  `cause()`; §18.5 / `Displayable` participation?). Pilot ships the minimal
  surface.
- **DQ8 §12/§18** — does v1 require module-qualified `use core.error` access?
  `seed_imports` skips `ImportItems::Module` (a type-checker change affecting all
  11 modules). Pilot relies on named imports (supported).
**Options:** Design decides each; the orchestrator then reconciles §18 (+ a
changelog) and, for DQ8, schedules the type-checker change if required.
**Recommendation:** none on the merits (core-spec is Design's call). DQ6 is the
highest-leverage — it makes the implementation model normative for all 11 modules.
**Awaiting:** Design Chat (with the operator) decisions, routed back here.
**Status:** resolved

**Update (2026-05-30 00:31 UTC) — `core.compare` (#104) added evidence + a 4th question:**
- **DQ6 gained its crux:** #104 proved stdlib trait impls cannot cover primitive
  types until a checker↔bock-core bridge exists (`impl Comparable for Int` →
  E4001). Building it raises a **precedence/coherence** ruling Design must make
  (stdlib trait impl vs primitive intrinsic; may user code impl core traits for
  primitives?). This is the part of the impl model that gates a *useful* stdlib
  (→ `queue.md` Q-bridge, `divergences.md` DV4). The interim #103 stdlib-strictness
  policy also wants ratification here.
- **DQ9 added:** §18.2 (prelude) vs §18.3 (import-required) for `Comparable`/
  `Equatable` — an internal spec inconsistency (DV5). The impl matches named-import.
- **Highest leverage now:** DQ6 — the module fan-out is paused on it. DQ7/DQ8/DQ9
  remain non-blocking (pilot/modules proceed on safe defaults).

**Response (2026-05-30 01:53 UTC):** Design decided all four (full text in
`design-questions.md` DQ6–DQ9). DQ6: compiler-provided canonical primitive
conformances in the trait-impl table (the bridge), sealed (no user impl of a core
trait for a primitive); mechanism stays non-normative; strictness is per-package.
DQ7: `core.error` v1 = `message()` only (supersedes the May-29 `source` lean —
trait-object dependency). DQ8: named imports sufficient for v1; module-qualified
deferred. DQ9: prelude = "defined in core.*, re-exported"; §18.2 amended (+Ordering).
**Authorized actions:** spec reconciled in #106; the bridge (Q1a) dispatched as
`feat/stdlib-primitive-bridge`; prelude-injection (Q-prelude-inject) + bare-import
rejection (Q-import-reject) queued. The bridge plan surfaced **DQ10** (below) +
a latent bug (`divergences.md` DV6: bounds unenforced in production).

## [2026-05-30 02:13 UTC] Q-bridge plan → normative primitive-conformance matrix (DQ10)

**Type:** design (core-spec)
**Severity:** low (non-blocking — bridge proceeds on a proposed matrix)
**Trigger:** the Q-bridge plan needs to know which (primitive × core-trait)
conformances are normative; §18.2/§18.5 name the traits but never pin the matrix.
**Context (in `design-questions.md` DQ10):** is `Bool: Comparable` normative? May
`Float` conform to `Equatable`/`Hashable` given `NaN != NaN` breaks their laws?
The bridge implements a proposed matrix (Equatable: Int/Float/String/Bool/Char;
Comparable: minus Bool; Displayable: all; Hashable: minus Float) and proceeds on
it; Design ratifies/refines (additive). Also flags §18.5 operator-gating for *user*
types as an unimplemented follow-up.
**Recommendation:** none on the merits (core-spec is Design's). The proposed matrix
follows Rust/Swift precedent (no `Float: Hashable`; conservative on `Bool` ordering).
**Awaiting:** Design ratification of the normative matrix; non-blocking.
**Status:** pending

## [2026-05-30 03:37 UTC] core.convert design questions (DQ11)

**Type:** design (core-spec)
**Severity:** low (non-blocking — `core.convert` shipped the floor in #110)
**Trigger:** standing up `core.convert` + parameterized-trait resolution raised
four design questions; the impl shipped a minimum-useful default for each and
escalates for ratification.
**Context (in `design-questions.md` DQ11):** (1) the normative primitive-
conversion matrix (parallels DQ10); (2) whether canonical conversions are sealed
(shipped unsealed); (3) the `TryFrom` error type (shipped fixed `ConvertError`);
(4) whether `TryInto` exists in v1 (omitted). All additive/refineable.
**Recommendation:** none on the merits (Design's call). Defaults follow the §18.3
surface + Rust/Swift precedent; refining any is non-breaking.
**Awaiting:** Design ratification; non-blocking (R1 proceeds).
**Status:** pending
