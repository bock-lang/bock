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

## [2026-05-30 19:41 UTC] §20.6.1 build-output: single-file bundling divergence (DQ19 → Design)

**Type:** design (core-spec)
**Severity:** low (non-blocking — bundling works on all 5; Phase 0 landed)
**Trigger:** Phase 0 Item A (#132) emits cross-module programs as a single bundled `main.<ext>`, diverging from
spec §20.6.1's one-file-per-module output. The single-file run model (conformance harness + toolchain run plans
run one `main.<ext>`) made bundling the pragmatic path; per CLAUDE.md the spec was NOT silently changed (a
non-normative §20.6.1 note + changelog were added pending Design).
**Context (design-questions.md DQ19):** is single-file bundling the v1 execution model (per-module tree → a
future "library build" mode), or should §20.6.1 be preserved (requiring multi-file run/harness support)?
**Recommendation:** none on merits (Design's). Bundling is the lower-friction v1 path given the run model.
**Awaiting:** Design ratification; non-blocking.
**Status:** **RESOLVED 2026-06-02 (owner): per-module native tree is the v1 output model** (both app + library
builds), NOT bundling — chosen eyes-open after the orchestrator surfaced that this re-opens DV13 (the foundational
cross-module-execution gap the 420-pair stdlib rests on via bundling). Owner also pulled the project-mode config
tables (`[targets.<T>]`/`.scaffolding`) forward into v1. Realized by the ItemB milestone (S0 spec/tracking
reconcile → S1–S4 native imports + harness rework, pilot python → S5–S8 scaffolding + config tables + tests).
Spec reconciled in S0 (changelogs `20260602-1608-per-module-output-dq19.md`, `20260602-1608-projectmode-config-tables-v1.md`).

## [2026-05-31 21:20 UTC] core.iter shipped surface refinements (DQ24) → Design

**Type:** design (core-spec)
**Severity:** low (non-blocking — `core.iter` R1 shipped on the floor, exercised ×5; refinements are additive)
**Trigger:** `core.iter` R1 landed (#151 module + for→Iterable desugar; #152 Rust/Go codegen — all 5×5). Three
surface choices refine DQ12 and want Design ratification per the core-spec rule (filed, queue continues).
**Context (design-questions.md DQ24):** (1) the **combinator set** — shipped 6 (`to_list`/`count`/`fold`/`map`/
`filter`/`take`), omitted `enumerate`, excluded mutating/`zip`/`flat_map`/lazy — is this the normative v1 surface?
(2) the concrete `ListIterator` satisfies `Iterator` via an **inherent `next`**, not an `impl Iterator[T] for
ListIterator` (dropped — caused a Go duplicate-`Next`; `Iterable` detection keys on `Iterable`). Acceptable, or must
the trait impl exist? (3) §6.5's associated-type `Collection`/`Iterator[Item=…]` **example** is inert and reads as
misleading vs the shipped generic `Iterator[T]`/`Iterable[T]` (DQ12) — clarify §6.5 or leave as illustration?
**Recommendation:** none on the merits (Design's call). The shipped floor follows DQ12/DQ14/DQ15 + Rust/Swift
precedent; all three are additive/reversible.
**Awaiting:** Design ratification; non-blocking (R1 continues to `effect`).
**Status:** pending

> Standing non-blocking Design queue (filed; queue not blocked on any): DQ10, DQ11, DQ12, DQ13, DQ16-residue,
> DQ17, DQ18, DQ19, DQ20, DQ21, DQ22, DQ23, DQ24, DQ25, Bool-interp spelling. The orchestrator proceeds on safe defaults
> and reconciles spec/divergences as Design decides.

## [2026-05-31 22:35 UTC] core.effect v1 surface — 8 questions (DQ25) → Design

**Type:** design (core-spec)
**Severity:** medium (the floor BUILD waits on Q1/Q2 — but the queue is NOT blocked: feasibility probe + scoping proceed in parallel)
**Trigger:** the operator chose "scope core.effect" (the next R1 module). The Plan agent found `core.effect`'s
v1 surface is genuinely **under-specified** (§18.3:1728 = "Effect system primitives" only; no §18.3.x subsection).
Per the core-spec rule the orchestrator FILES the surface questions for Design rather than deciding them.
**Context (design-questions.md DQ25 — 8 sub-questions):** the headline ones — **Q1** primitives-only vs a
library of concrete effects (rec primitives-only); **Q2** ship a standard `Log` effect as the executable
example, conditioned on cross-module effect execution proving feasible ×5 (rec yes-iff-feasible) — the most
consequential, since it decides whether the floor contains a *runnable* effect; **Q8** what is the
"representative example that runs" (§18.3:1716 acceptance bar) for a primitives-only floor. Q3-Q7 (ambient
effects / Clock-Cancel ownership / handler utility traits / composites / Reserved-v1.x) have low-controversy recs.
**The effect MACHINERY is implemented** (§10; effects.rs ~1112 lines; effect codegen ×5; 7 fixtures) and
resolve-layer cross-module-wired — this is a SURFACE decision + a cross-module-EXECUTION feasibility gap on
Rust/Go (never proven; all effect fixtures are check-only). A feasibility probe is running to inform Q2/Q8.
**Recommendation:** none on the merits (Design's call); the plan gives a recommended minimum-useful default per
question. Q2 should be decided WITH the feasibility-probe result (the orchestrator will route that result here).
**Awaiting:** Design (+ owner) decision on Q1/Q2 (the floor contents) — the rest are additive. The owner is
present in-chat and may answer directly, which the orchestrator then reconciles into the spec/floor.
**Status:** pending

**Probe result (2026-05-31 22:55 UTC) — informs Q2/Q8:** the cross-module effect feasibility probe verdict:
the `with`-clause form (declare op in module A; perform inside `fn ... with <Effect>` bodies; handle via
`handling (Effect with h()) { }`; `use A.{Effect, handler}`) **executes correctly on ALL 5 targets** (P1). So
**Q2 = an executable `Log` effect IS shippable ×5 in Variant-A form — via the `with`-clause surface** (avoiding
value-returning ops inside `${...}` on Rust until Q-effect-interp-rust lands). BUT the probe also surfaced a
**new core-spec divergence, DV16** (filed): bare effect-op calls (`log("...")`) don't resolve even same-module
(E1001), so the §10.2 bare-op/Layer-1-direct/Layer-2 ergonomic surface is non-functional, and the entire
`effects/` conformance suite is inert (never checked/executed). **This couples to Q1/Q2:** if Design intends
bare-op invocation as a v1 form, the checker has a real bug to fix (and core.effect's `Log` would otherwise present
only the `with`-clause surface); if the `with`-clause is the intended v1 form, §10.2 + the `effects/` fixtures need
correcting. **New question for Design alongside DQ25: is bare effect-op invocation a v1 form (→ fix the checker), or
is the `with`-clause the v1 form (→ correct the spec/fixtures)?** Sequencing of the effect-foundation fixes
(DV16 / Q-effect-conformance-wiring / Q-effect-interp-rust) vs. shipping core.effect on the working subset is an
operator call (surfaced in-chat).

**Foundation hardened (2026-06-01 01:31 UTC) — strengthens Q2/Q8:** the operator chose "harden the effect
foundation first"; a Plan pass confirmed the §10.4 gap was a fixable resolver/checker bug (NOT a v1-scope limit),
and **#155 landed it: ALL §10 invocation forms now execute ×5** — §10.2 `with`-clause (incl. op-in-interpolation,
the Rust fix), §10.4 canonical bare-op-in-`handling`, §10.3 Layer-1 innermost-shadow + Layer-2 module handler,
cross-module — and the previously-inert `effects/` suite now runs (6 `exec_effect_*` fixtures ×5). DV16 RESOLVED.
**So Q2 is now strongly YES** — an executable `core.effect` `Log` is shippable ×5 via the *canonical* §10.4 bare-op
surface (no `with`-clause-only constraint anymore). **The core.effect floor BUILD is now gated ONLY on Design/owner
answering Q1/Q2** (primitives-only floor + an executable `Log`?). Orchestrator recommendation stands: primitives-
only + a single `Log` effect (`fn log(message: String) -> Void`) + a `ConsoleLog` record handler + constructor.
Residue (non-blocking): Q-effect-op-node-lowering (E1001-vs-E8020), Q-effect-import-unused (cosmetic W1001).

**RESOLVED (2026-06-01 03:39 UTC):** the owner DECIDED the floor = **primitives + a `Log` effect** (the orchestrator's
recommendation). Reconciled in **#157** (the module + `spec/changelogs/...core-effect-v1-surface.md`); DQ25 marked
decided in design-questions.md. core.effect = 5/11; R1 COMPLETE. (Building it surfaced + fixed two latent gaps in
#157: the `effect`-keyword module-path parser rejection, and a nondeterministic interpreter module-registration order
— both fixed ×5-clean; the residual interpreter flat-op-map limitation → Q-interp-effect-op-collision, low-pri.)
**Status:** resolved.

## [2026-06-05 07:34 UTC] Two method/trait codegen semantics decisions (surfaced by examples-greening)

**Type:** strategic (language semantics)
**Severity:** medium (each blocks one example on a subset of targets; NOT blocking the 84/100 progress)
**Trigger:** the examples-greening push got runtime-working examples to 84/100; the last two stubborn blockers are not
codegen bugs but genuine semantics questions that the implementation should not decide unilaterally (per CLAUDE.md
"spec divergence is OPEN, not silent"). Both are filed as design-questions DQ27/DQ28.

**Context — two decisions:**
1. **DQ27 / Q-method-collision-inherent-trait.** A `class` with an inherent method AND a same-named trait method —
   `examples/target-optimized/react-components` has `impl Button { fn render … }` plus `impl Component for Button {
   fn render = self.render() }`. On overload-less targets (js, ts) both bind to one name, so the trait one overwrites
   the inherent and `self.render()` recurses infinitely; **the reference interpreter also stack-overflows** (so it's a
   language-semantics gap, not merely codegen). This wave got react-components passing on **python/rust/go** (rust native;
   go via exported-name + removing the self-recursive forwarder; python via its class model), but **js/ts still loop**.
2. **DQ28 / Q-go-method-generics.** Bock allows type params on a method (`Box[T].map[U]`); Go forbids type params on
   methods. Closing type-zoo on go needs a decision: monomorphize, lower the method to a free function, or restrict the
   surface. (Other targets handle it.)

**Options (orchestrator's informal read):**
- DQ27: (a) an inherent method auto-satisfies a same-signature trait requirement → the explicit delegating `impl` is
  redundant / a checker error (simplest, and arguably the intended model); (b) name-mangle trait methods distinctly from
  inherent on overload-less targets (codegen-heavy); (c) forbid same-name inherent+trait at check time. **Recommend (a)**
  — it matches how rust/python/go already behave and makes the example well-formed (or flags it). The interpreter overflow
  (Q-interp-method-collision) should be fixed regardless.
- DQ28: **Recommend monomorphization at codegen** (Go) or free-function lowering; low urgency (one example, one target).

**Recommendation:** non-blocking — keep both OPEN for a Design ruling; the rest of examples-hardening can proceed. I did
NOT change spec or the examples. The js/ts react-components and type-zoo/go reds are parked on these.
**Awaiting:** owner/Design ruling on DQ27 (method/trait resolution) and DQ28 (go method generics).
**Status:** resolved

**Response (2026-06-05, owner via Design handoff `spec/changelogs/20260605-1445-...` — now folded into the hub):** Both
decided. **DQ27** = the **single-method-namespace rule** (option a; an inherent method satisfies a same-signature trait
requirement, and a duplicate same-name definition is an **E4012** coherence error). **DQ28** = keep the language surface; the
Go backend lowers method-level type params via **free-function lowering**. Reconciled in **#258** (DQ27: checker E4012 +
react-components fixed to an empty `impl Component for Button {}` + spec §6.4/6.5/6.7 + changelog) and **#256** (DQ28: go.rs
free-fn lowering). **react-components now runs on all 5**; type-zoo/go's method-generics blocker cleared. Residual filed:
**Q-checker-method-generic-call-infer** (checker can't infer `U` for a `b.map(dbl)` call). Design also handed a Tier A–D
prioritization for the rest of the open queue (folded into design-questions.md; **DQ23** + **DQ20** are next-highest leverage).

## [2026-06-08 22:52 UTC] DQ29 — Equatable `==`/`!=` operator-gating for user types

**Type:** scope
**Severity:** low
**Trigger:** wave-6 follow-up tried to gate `==`/`!=` behind `Equatable` for user types (mirror of #296's `Comparable` gate). The engineer investigated and STOPPED (PR #300, doc-only): records/enums get free structural `==` at codegen but have NO checker-visible `Equatable` conformance, and `@derive` is v1.x-reserved — a strict gate would reject idiomatic `record == record` with no v1 escape. That's a design decision, not impl-completeness.
**Context:** §18.5 says implementing the trait gates the operator. For `Comparable` this landed clean (#296/#299). For `Equatable` it collides with the undefined "does structural record/enum equality count as `Equatable` conformance?" question (the `(core trait, user type)` quadrant §18.5 leaves unspecified).
**Options:** **(R1)** structurally auto-conform records/enums to `Equatable`, then gate `==`/`!=`; **(R2)** defer `==`/`!=` gating to the v1.x `@derive` era (leave `==`/`!=` ungated for now); **(R3)** strict gate requiring explicit `impl Equatable` — **rejected** (breaks idiomatic record equality, no v1 escape hatch).
**Recommendation:** none on the merits (Design's call). Note R1 and R2 both keep current code working; R1 also enables the gate now. The impl is ready to wire (same `infer_binop` mechanism as #296) once ruled. Non-blocking — `==`/`!=` stays ungated meanwhile (status quo), so nothing regresses.
**Awaiting:** Design ruling on DQ29 (R1 / R2 / other).
**Status:** pending
