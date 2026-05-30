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

## [2026-05-30 04:19 UTC] core.iter protocol shape (DQ12)

**Type:** design (core-spec)
**Severity:** low (non-blocking — iter is paused on Q-codegen-fixes anyway; this
ratifies in parallel)
**Trigger:** §18.3 doesn't pin the `Iterator`/`Iterable` protocol shape; the
`core.iter` plan chose a minimum-useful floor and escalates the surface.
**Context (in `design-questions.md` DQ12):** generic vs associated-type protocol
(planned generic — assoc-types are inert today); next()/iter() signatures; lazy vs
eager (planned eager floor); which combinators are normative; whether `for`
requires `Iterable` for built-ins (planned: native fast path for built-ins).
**Recommendation:** none on merits (Design's). The floor is implementable now;
ratify the normative surface before/as iter resumes post-codegen-workstream.
**Awaiting:** Design ratification; non-blocking.
**Status:** pending

## [2026-05-30 07:15 UTC] §18.2 prelude membership — TryFrom/Error (DQ13)

**Type:** design (core-spec)
**Severity:** low (non-blocking; reversible in v1-dev)
**Trigger:** the prelude-injection impl (#120) preludes `TryFrom` + `Error`, which
aren't in §18.2's literal trait list (the orchestrator's dispatch prompt named them).
**Context (design-questions.md DQ13):** amend §18.2 to include `TryFrom`/`Error`
(both core-defined, fundamental), or drop them from the prelude (require `use`)?
**Recommendation:** none on merits (Design's call); both are defensible and the change
is one-line either way.
**Awaiting:** Design ratification; non-blocking.
**Status:** pending

## [2026-05-30 15:24 UTC] core.iter blocked on the List-codegen substrate — scope/roadmap (operator) + R1 floor (Design, DQ16)

**Type:** scope (operator) + design core-spec (Design)
**Severity:** high / blocking — `core.iter` (a v1 critical-path module) cannot proceed; reframes the stdlib path.
**Trigger:** `core.iter` (R1) passed its T1 codegen gate on all 5 targets (the for→Iterable desugar
shape is PROVEN) but stopped one layer deeper: the DQ12 R1 floor (a `ListIterator[T]` over `List[T]` +
6 List-returning combinators) requires **List built-in method codegen** (`.len()`/`.get(i)`/`.push(x)`/…),
which **does not exist on ANY backend** — the codegen emits the calls verbatim and no target lowers them
(verified empirically on all 5 + by source). See `divergences.md` DV10, `queue.md` Q-list-codegen. Latent
until now because the 3 landed modules (error/compare/convert) were List-free.
**Context — TWO coupled decisions:**
- **(operator — scope/roadmap):** "Implement List built-in method codegen across 5 backends" is a
  substantial, foundational workstream (not a routine fix) gating `core.iter`, `core.collections` (R3),
  and every List-using module — it reframes the v1 stdlib critical path. Priority/sequencing? Authorize a
  plan-first codegen workstream now, or defer and re-sequence R1?
- **(Design — core-spec, DQ16):** keep the R1 `core.iter` floor List-backed (block on the List-codegen
  workstream), or redefine it to a **List-free iterator surface** (Counter/Range-style, Int/Float +
  arithmetic — codegen-PROVEN today via `optional_match_in_loop.bock`), shipping iter sooner but omitting
  the combinators until List codegen lands?
**Options the orchestrator sees:** (a) operator authorizes the List-codegen workstream + Design keeps the
List-backed floor → plan + build List codegen, then resume full core.iter; (b) Design redefines the floor
List-free → ship a reduced core.iter now, List-codegen + combinators follow; (c) defer core.iter,
re-sequence R1 (`effect` first) while List-codegen is scoped/built.
**Recommendation (informal; the calls are the operator's + Design's):** List built-in method codegen is
needed for v1 regardless (`core.collections` is a v1 module), so authorizing the workstream (a) is likely
unavoidable; whether to ALSO ship a List-free iter floor first (b) is a sequencing/UX call. The
for→Iterable desugar work is complete and proven on all 5 either way — no rework lost.
**Also filed this block (non-blocking → Design):** DQ14 (`Iterable.iter()` return-type limit), DQ15
(concrete vs generic-bound combinators), DQ17 (canonical Optional codegen representation — normative?).
**Awaiting:** operator decision on Q-list-codegen scope/priority + Design decision on DQ16 (core.iter floor).
**Status:** responded

**Response (2026-05-30):** Operator chose **"Build List codegen first"** (AskUserQuestion) — DQ16 resolved:
keep core.iter's List-backed floor; build the codegen prerequisite (no spec change).
**Authorized actions:** dispatched the read-only List-codegen workstream → **#129** merged (len/get/is_empty/
contains/first/last/concat/index_of/join, all 5). Mutating methods deferred → DQ18. (Superseded in scope by the
codegen-completeness milestone below.)

## [2026-05-30 18:00 UTC] Codegen substrate materially incomplete → codegen-completeness milestone (operator-decided)

**Type:** scope / roadmap (operator) + core-spec (Design, via DQ16)
**Severity:** high — reframes the v1 critical path; ~10-15 PR milestone.
**Trigger:** core.iter v4 stopped at a 4th codegen layer (generic-record codegen broken 4/5; DV12). A 3-agent
codegen audit (all 5 targets, 280+ compile+run points) then established the v1 codegen substrate is materially
incomplete for the stdlib's real needs: **cross-module `use` and user-defined enums broken on ALL 5** (DV13/DV14);
Result/generics/closures/Optional-methods broken on 3-4/5. The 3 "landed" stdlib modules are check-only, never
executed cross-module. "5-target parity" was aspirational.
**Context — decisions made (AskUserQuestion ×2):**
- **#2:** "Systematic codegen-completeness push" — stop the reactive probe-and-fix loop; a dedicated milestone
  (audit + comprehensive fix), THEN resume stdlib.
- **#3:** "Proceed — comprehensive fix" (over reduce-target-set / reduce-stdlib-scope) — full 5-target parity +
  full v1 stdlib, phased P0-P4, ~10-15 PRs, checkpointing between phases.
**Authorized actions:** established `Q-codegen-completeness` (v1-blocking milestone, phased), paused Q-stdlib R1
behind it, dispatched the Phase-0 design (cross-module wiring + user-enum codegen + tail-`if`), and will dispatch
the phased fixes (sequential per crate-granularity), checkpointing between phases. Reduce-target-set (§1.3) and
reduce-stdlib-scope remain available if the magnitude warrants a later pivot.
**Status:** responded (decisions #2 + #3 made; milestone underway)
