# Bock Design Audit — Design, State, and Future — 2026-06-09 23:27 UTC

**From:** Design chat
**To:** Operator + Orchestrator
**Type:** Strategic design audit (not a DQ ruling; not a spec change — recommendations route onward)
**Proposed landing:** `tracking/designs/2026-06-09-design-audit.md` (persistent strategic baseline)

---

## 0. Scope and method

This audits Bock's design thesis against two grounds: (1) the verified current state of the project (synced tracking hub, main `0567568`, 2026-06-09), and (2) the AI/LLM ecosystem as of mid-2026, checked against external sources where my training cutoff (Jan 2026) could be stale. It asks four questions: does the value proposition still hold, is the project feasible as scoped, is the need real, and does the design itself — which depends on model capabilities — still fit the capability curve.

Verdicts are tagged by evidence class: **[validated]** = supported by implementation evidence or external data; **[held]** = supported by argument, unchanged; **[weakened]** / **[strengthened]** = the 2026 landscape moved against/for it; **[unvalidated]** = specced but not yet exercised; **[superseded]** = the world moved past the design's assumption.

This document deliberately includes findings uncomfortable for decisions this Design chat itself made. An audit that spares its own author is not an audit.

---

## 1. Where Bock stands (facts, June 9 2026)

- **v1.0 engineering runway is clear.** MS-stdlib complete (11/11 core modules × 5 targets). MS-projectmode complete (per-module native-import trees on all 5; scaffolding; config tables; `@test` transpiled to each target's test framework). 2,854 workspace tests passing; conformance **824 fixture×target pairs, 0 failed, REQUIRE=all across go/js/python/rust/ts**; examples 100/100 non-red, build-clean ×5. CI gates include stdlib formatting and blocking examples-exec.
- **The design queue is empty** of core-spec decisions (DQ1 non-core remains; DQ17/DQ21 closing as non-normative/impl-backlog). Every cross-target-correctness divergence found to date (Int division, Bool spelling, `?` propagation, method-namespace collisions) has been decided and fixed — the conformance architecture caught each one, which is the system doing what it was designed to do.
- **What remains for v1.0** is release actions (operator-gated) plus two non-blocking follow-ups. The remaining risk is not engineering.
- **The development model is itself a result.** The orchestrator + design-authority + tracking-hub process has run ~330 PRs of agentic development with one human, a design chat, and engineer sessions, with spec/impl divergence actively tracked and reconciled. This is operational evidence for the project's own thesis (AI agents as primary developers, humans as architects, every decision auditable) — and notably, the artifact this process produced is mostly *deterministic* software, with AI applied at development time, not runtime. That shape matters later in this audit.

**Feasibility verdict: v1 as scoped is feasible and essentially done.** The deferral decisions (concurrency, FFI, refinement predicates, trait objects, lazy iterators) are what made a five-target, conformance-gated v1 reachable by this process; in hindsight every one of them was correct. The open question is not "can we build it" — it's "does the thesis hold," which is the rest of this document.

---

## 2. The ecosystem, mid-2026

Four shifts since Bock's conception, verified against current sources:

**(a) Agents went autonomous and mainstream.** The 2026 mode is delegation, not completion: assign an issue, the agent explores the repo, edits, runs tests, opens a PR; agents run for minutes-to-hours in execution loops. Adoption is no longer early: industry reporting puts AI-assisted coding above 90% of organizations, with a large majority deploying coding agents for production code and a substantial minority trusting agents to *lead* development under human oversight. The market roughly doubled-to-tripled in two years.

**(b) Governance became the product requirement.** The same sources converge on the next bottleneck: unreviewed AI-generated code is now a named security and quality concern; organizations are being told to *increase* review, scanning, and verification as agent throughput grows; permissions, sandboxing, auditability, and policy distribution are described as core product requirements for AI coding tools going forward. The bottleneck moved from generation to verification and accountability — exactly the territory Bock's manifests, pins, capability checks, and deterministic fallback occupy.

**(c) Direct whole-project translation is not solved — and the research frontier converged on Bock's architecture.** This is the audit's sharpest external finding. Published PL research (PLDI) on validated project-scale LLM translation reports that naive LLM translation is unreliable precisely at *features lacking a direct mapping in the target language*, and that LLMs get stuck in repair loops; the fix that works is combining predefined translation rules with LLM translation, plus localized signature-level compatibility checks to catch errors early — and even then, state of the art validates roughly three-quarters of functions for I/O equivalence on projects up to ~10K lines, one language pair at a time. Read that against Bock's design: deterministic rules (Tier 2) + AI at capability gaps (§17.6) + independent verification (§17.3) + typed signatures as the check surface. Independent researchers, working forward from the failure modes of direct translation, derived Bock's compile pipeline. The architecture is not just defensible; it is being externally re-discovered.

**(d) Tooling standardized around agent-facing protocols.** MCP became the standard way agents consume tools; the agent (not the human IDE) is now a primary consumer of developer tooling. CLAUDE.md-style project primers are the established mechanism for making agents competent in a specific codebase. Open-weight models and open agent frameworks commoditized raw generation.

---

## 3. The central tension, stated honestly

Bock was conceived on the premise that AI-assisted development needs a better substrate. The ecosystem's actual trajectory was different: models got good enough to write mainstream languages directly, and the agentic loop made that workable without a new language. So the existential question is not "can we build Bock" — Section 1 settles that — but: **why would an agent (or a human directing one) write Bock instead of writing the target codebases directly?**

The honest answer has two halves.

**The half that weakened:** the convenience pitch. "One language, every platform" as a productivity claim is eroded for the single-target case — an agent that wants one TypeScript app writes one TypeScript app, and Bock adds a layer. Marketing that pretends otherwise will burn credibility, and the project's own marketing constraints already forbid it.

**The half that strengthened:** the guarantee pitch. Three claims Bock can make that no direct-generation workflow can:

1. **Provable cross-target equivalence.** The same program, conformance-tested to behave identically on every shipping target — 824 pairs, zero failures, with real divergences (integer division, Bool spelling) caught and eliminated by the mechanism. Section 2(c) shows direct translation cannot make this claim even for one language pair at modest scale.
2. **Auditable, reproducible AI involvement.** Decision manifests, confidence thresholds, pinning, promote-from-runtime, deterministic fallback, model-version-as-dependency (§19.4 — which anticipated what is now standard practice). Section 2(b) shows this is what the market now says it needs.
3. **Machine-verifiable semantics.** Effects and capabilities make "what can this AI-written function actually touch?" a compiler question with a checked answer; the strictness ladder makes rigor graduated and enforceable per package.

**The anti-fragility argument — the position that survives model progress.** Every model improvement cuts both ways: it makes direct multi-target generation more plausible (eroding the convenience moat) *and* it makes Bock's own capability-gap synthesis, repair, and selection smarter and cheaper (strengthening the product). Which effect dominates is a positioning choice. If Bock competes with agents at code generation, it loses to the tide. If Bock is the deterministic substrate agents *target* — the thing that turns agent output into a reproducible, equivalence-tested, auditable artifact — the tide lifts it. The architecture already supports the second positioning. The framing has lagged the architecture.

A second framing follows from the language's own nature: Bock source is feature-declarative — closer to an executable specification than to an implementation. In a world where models write the implementation anyway, the durable shared artifact between humans and agents is the spec-plus-tests. Bock source *is* that artifact, with a compiler that makes it executable on five platforms and a conformance suite that proves the realizations equivalent. "Executable specification with provably equivalent multi-target realization" is a frame that improves as models improve, because better models make the realization better without obsoleting the specification layer.

---

## 4. Pillar-by-pillar audit

### 4.1 Determinism, decision manifests, pins — **[strengthened, validated]**
The founding constraint ("no black boxes," deterministic fallback for every AI stage, pinned decisions in production) is the design's best-aged element. The industry arrived at Bock's position: auditability and policy are now core requirements (§2b). The build/runtime decision split, promote-to-pin path, and model-as-dependency were all ahead of practice. The project's own development reinforces it: the manifest philosophy is the same philosophy as the tracking hub that built the compiler.

### 4.2 Cross-target conformance equivalence — **[strengthened, validated]**
The moat. Operationally proven (824/0 ×5; the DQ23 division divergence is the canonical case study: found by the architecture, decided by design, fixed across three backends with negative-operand and zero-divisor fixtures). Externally validated by §2(c): equivalence at project scale is exactly what direct translation cannot deliver. **One guard:** the guarantee is only as strong as the interpreter-as-oracle (Tier 1 semantics) staying ahead of checker/codegen — the Q-interp-* lag items show the pressure. The oracle must remain a funded, first-class component, or the guarantee quietly inverts into "targets agree with each other but not with the spec."

### 4.3 Effects + capabilities — **[strengthened]**
Designed as cross-target abstraction; landed as AI governance. A function signature that declares `with Network, Storage` and `@requires(Capability.Network)` is a machine-checkable answer to "what can this code the agent wrote actually do" — the question §2(b) says organizations are now asking. The PII type-propagation rules (§11.8) are the same story for data. This pillar should move up in positioning.

### 4.4 Four-mode provider interface (§17.8) — **[held; one elevation needed]**
The vendor-neutral trait survived two years of provider churn — the decision to keep provider/model identifiers out of the spec was right. The Anthropic-provider bet on reasoning traces for manifests aged well (reasoning models are now standard). Constrained decoding makes Select's closed-set guarantee trivially enforceable. **The gap:** the interface is single-shot; 2026 practice is the loop (generate → compile → repair → re-verify). The pieces exist (Generate, Repair, the §17.7 learning loop) but the *composition* — the agentic repair loop as a first-class pipeline stage — is the deferred "AI layer composability" design pass. §2(c)'s repair-loop failure finding confirms this needs design (loop budgets, convergence detection, when to fall back), not just plumbing. **Elevate it to the first v1.x design pass.**

### 4.5 Tier structure — rules-first with AI at gaps — **[validated; one spec inconsistency found]**
Independently re-derived by external research (§2c) and by this project's own experience (the compiler was built almost entirely on the deterministic path; 824 conformance pairs run without an API key). **Audit finding:** the spec is internally inconsistent about the default. §17.2 labels Tier 1 (AI generation) "default," while §20.7 states "Bock uses rule-based code generation by default; AI configuration is opt-in" — and §20.7 matches reality and the right posture. **Ruling (this audit, Design):** rules are the default; AI activates at capability gaps when configured. §17.2's tier labels are amended accordingly ("Tier 1 — AI Generation (when configured)" / "Tier 2 — Rule-Based Generation (default and fallback)"). Small changelog; no behavior change.

### 4.6 Context system (§11) — **[split verdict]**
The *verified* annotations (@requires, @security/PII propagation, @invariant-as-future-guardrail) **held or strengthened** — they are compiler-checked semantics, which free text can never be. The *free-text* `@context` **weakened as a differentiator**: 2026 context windows ingest whole codebases plus docs, so inline prose context is now ordinary documentation rather than a unique AI affordance. It retains two real roles: structured input to runtime Select (where there is no human prompt at the failure site — still genuinely novel) and human-readable intent. Positioning should stop leading with free-text context and lead with the verified subset.

### 4.7 Adaptive effect handlers (§10.8) — **[unvalidated]**
The closed-set design remains sound (never executes generated code; degrades to deterministic via pins; the promote path means mature systems converge to no-AI-at-runtime). But it has never been exercised end-to-end — no example, no conformance fixture, no runtime Select call in anger. It is the most-cited "AI-native" feature and the least-proven one. Either build the end-to-end demonstration early in v1.x or keep it out of lead positioning until it runs. Honest status: a designed bet, not a capability.

### 4.8 Rule learning (§17.7) — **[unvalidated, direction externally confirmed]**
Post-v1 status stands. §2(c)'s "feature mapping" result is this mechanism by another name, which raises confidence in the design without changing its priority.

### 4.9 AIR — **[held internally; AI-facing format demoted]**
The four-layer IR is validated by the working codegen substrate. But the premise that AI consumes a special text IR (AIR-T, "designed for AI consumption") is **superseded**: 2026 models are at their best on source-shaped text, and every line of evidence from this project's own development (engineer sessions work on Bock source and Rust source, never on IR dumps) points the same way. Keep AIR as compiler internals; feed models source + spec context; don't invest further in AI-facing IR serialization. (§16.3/16.4 Post-v1 deferral already aligned — affirm it, with this sharper rationale.)

### 4.10 Strictness ladder + per-package strictness — **[held]**
Maps cleanly onto prototype-vs-production agent usage. The Q1d per-package ruling (dependency diagnostics never fail the consumer) forward-extended correctly.

### 4.11 Target ambition (§1.3) — **[headline weakened; posture affirmed]**
Each added target multiplies the conformance surface — the cost of the guarantee scales with target count, while §2 lowers the marginal value of a speculative target. The existing spec posture (v1.x four as ambition, not commitment) is right; harden it into an explicit gate: **a v1.x target is added when a concrete demand exists and the conformance pipeline for it is automated end-to-end, not on a calendar.**

### 4.12 v1.x LSP extensions (§20.3) — **[superseded as designed]**
The five deferred extensions (AI Context Panel, Target Preview, Capability Graph, Smart Completions, Inline Diagnostics) were designed for a human-in-IDE-with-AI-features world. The world that arrived is agent-with-tools: the highest-leverage editor-adjacent surface for an AI-first language in 2026 is an **MCP server** exposing `check / build / test / inspect / conformance` as agent tools — making every agentic environment a competent Bock environment cheaply. The recent LSP wave (nav, hover, inlay hints) serves humans well and stands; the *v1.x extension list* should be reoriented MCP-first, with the human-facing panels behind it. This supersedes part of a deferral this Design chat itself wrote in May; the May framing was already dated when written.

### 4.13 The training-data gap — **[the risk]**
Named fully in §6. Flagged here because it is a *design* concern, not only a go-to-market one: an AI-first language that frontier models have never seen inverts its own premise — models are competent in proportion to corpus exposure, and Bock's corpus is this repository. Every other verdict in this audit is conditional on closing this gap.

### 4.14 Spec-as-single-file — **[held, accidental affordance]**
The ~2,600-line consolidated spec fits comfortably in any 2026 context window. What the K04 consolidation did for maintenance, it also did for model-legibility: the spec is the seed of the context pack (§7 R3). Keep the spec single-file and context-budget-conscious as a standing constraint.

---

## 5. The repositioned value proposition

**Identity (proposed, one sentence):** Bock is the deterministic substrate for AI-built software — an executable specification language whose compiler produces idiomatic code for five platforms and *proves* the five behave identically, with every AI decision in the pipeline logged, pinned, and reproducible.

**Three moats, in lead order:** (1) provable cross-target equivalence; (2) auditable, reproducible AI involvement; (3) machine-verifiable semantics (effects, capabilities, PII propagation — growing toward invariants and refinements). None weaken as models improve.

**The concession, made explicitly:** a single-target app written by an agent does not need Bock. Say so. The credibility this buys is worth more than the audience it releases — and the project's marketing rules already require it.

**The wedge (ICP, sharpest first):**
- **SDK / library vendors** shipping one semantic surface to many ecosystems — Bock's project mode emits native packages (npm/cargo/pip/go-mod shapes) with transpiled native tests; the equivalence guarantee is precisely their product promise.
- **Polyglot platform teams** maintaining the same logic across service languages with agents doing the maintenance.
- **Audit-critical environments** adopting agent codegen under governance requirements (§2b) — the manifest/pin/capability stack is the compliance story.

**Spec touch (proposed, operator sign-off):** a one-sentence addition to §1.1 making the equivalence guarantee part of the identity statement — e.g., after the existing abstraction-hierarchy sentence: "Uniquely, Bock's multi-target output is conformance-tested for semantic equivalence: the same program is verified to behave identically on every target it ships to." Marketing owns downstream wording; Design supplies this truth basis.

---

## 6. Risk register

**R-A — Model familiarity (the dominant risk).** Models write Python from billions of examples and Bock from zero. Untreated, this caps adoption regardless of architecture. Mitigations are concrete and this project is unusually positioned to execute them: **(1) Context pack** — a curated, versioned primer (spec + idiom guide + error-code table + worked examples, CLAUDE.md-shaped) that makes any frontier model a competent Bock author at session start; the vocab-sync pipeline and single-file spec are existing primitives. **(2) Synthetic corpus** — the compiler + conformance suite can mass-produce *verified* (Bock source ↔ five target outputs ↔ expected behavior) triples; 824 passing pairs already constitute the seed. This is fine-tuning-grade data no one else can generate, publishable as a dataset. **(3) Diagnostics as agent affordance** — audit error messages for machine-actionability (the `bock check` exit-code bug was this lesson learned once; make it a standing review criterion). The deep point: for an AI-first language, *docs are training data and diagnostics are UX for agents* — reframe both workstreams accordingly.

**R-B — Relevance window.** The governance conversation (§2b) is happening now; v1 should ship into it. Engineering is done; the risk is polish-perfectionism delaying contact with reality. Audit position: ship v1 on current scope — this audit shapes v1.x, not v1.

**R-C — Overclaiming the unvalidated.** Adaptive handlers (§10.8) and rule learning (§17.7) are designed bets, not capabilities. Marketing constraints already prohibit overstatement; this audit adds a tracked validation ledger so the line is enforceable (R9).

**R-D — Conformance cost scaling.** Each target multiplies fixtures, CI time, and formatter/test-framework matrices. Mitigation is the R7 demand gate plus continued automation; never add a target whose conformance lane isn't fully automated.

**R-E — Oracle drift.** If the interpreter lags checker/codegen (the Q-interp-* pattern), "equivalence" silently degrades to "targets agree with each other." Standing investment guard (R11).

**R-F — Ecosystem cold start, partially defused.** Bock has no library ecosystem — but project mode means Bock *emits into* existing ecosystems rather than competing with them, and v1's stdlib covers the self-contained core. The FFI design pass (v1.x) is where "consume target libraries" arrives; until then the honest scope statement is "self-contained logic + emitted packages," which matches the SDK-vendor wedge exactly. One requirement forwarded to the v1.x FFI pass: native blocks puncture the equivalence guarantee per-target *by definition*, so the design must include an explicit equivalence-boundary story (how conformance marks and reports FFI-containing modules).

**R-G — Process bus factor.** One operator, one design authority. The tracking hub mitigates knowledge loss (everything is in-repo and auditable); succession/redundancy is an operator-level concern outside this audit's scope, noted for completeness.

---

## 7. Recommendations

| # | Recommendation | Route |
|---|----------------|-------|
| R1 | Reposition identity per §5 (equivalence + auditability lead; convenience pitch retired; single-target concession explicit). §1.1 one-sentence amendment proposed above. | Marketing leads wording; Design truth-basis; **operator signs** identity sentence; spec changelog for §1.1 |
| R2 | Fix the §17.2 vs §20.7 default-posture inconsistency: rules-default, AI-at-gaps-when-configured. Decided in §4.5. | Spec changelog (small); no impl change |
| R3 | Model-familiarity workstream — three queue items: context pack (versioned, shipped in-repo); synthetic corpus pipeline from conformance fixtures; diagnostics-as-agent-affordance audit + standing review criterion. | Orchestrator → queue; corpus publication call is OQ2 |
| R4 | `bock-mcp` server (check/build/test/inspect/conformance as tools) as the lead v1.x tooling item; reorient §20.3's v1.x extension list MCP-first (human panels behind it). | Roadmap/milestones + small §20.3 note (changelog) |
| R5 | Elevate the agentic repair-loop / AI-layer composability design pass to **first v1.x design item** (loop budgets, convergence, fallback policy; §2c motivates). | Design (next major pass) + milestones |
| R6 | Adopt the v1.x prioritization principle: **verification features over surface-area features** — property testing (`@property`, cheapest + highest agent synergy) → runtime guardrails (§15.4 verb-keyed) → refinement predicates (§4.7), ahead of new targets / FFI breadth. | Milestones; **operator ratifies** the principle |
| R7 | Harden the target gate: v1.x targets added only on concrete demand + fully automated conformance lane. | Milestones (affirms §1.3 posture) |
| R8 | Dogfooding milestone: one real tool written *in* Bock (candidates: conformance-report generator, examples-matrix renderer). Yields corpus, credibility, and the first genuine user. | Queue/roadmap |
| R9 | Validation ledger: mark §10.8 and §17.7 **specced-unvalidated** in tracking; positioning may not lead with unvalidated pillars until an end-to-end demonstration exists. | Tracking + standing marketing constraint |
| R10 | Land this audit at `tracking/designs/2026-06-09-design-audit.md`; revisit per frontier-model generation or ~quarterly, whichever first. | Orchestrator commits |
| R11 | Interpreter-as-oracle investment guard: interpreter parity items are correctness work, not polish; they gate the equivalence claim. | Queue prioritization rule |
| R12 | Ship v1 on current scope. Nothing in this audit grows v1; everything routes to v1.x or marketing. | Operator (release timing = OQ3) |

---

## 8. What this audit does *not* change

For the record, the load-bearing decisions that survive re-examination intact: the deterministic-fallback constraint; the conformance-equivalence architecture; the effect/capability system; the strictness ladder and per-package strictness; the 11-module v1 stdlib scope and every Reserved-for-v1.x deferral (concurrency, FFI, trait objects, refinement predicates, lazy iterators — each *reinforced* by the repositioning, since every one protects the equivalence guarantee or the v1 critical path); the vendor-neutral provider trait; spec-as-single-source-of-truth with changelog discipline; and the agentic operating model itself. The audit's changes are of emphasis, sequencing, and two small spec corrections — not of architecture.

---

## 9. Open strategic questions (operator's, not Design's)

- **OQ1 — Which wedge leads?** SDK/library vendors vs polyglot platform teams vs audit-critical orgs. Marketing strategy follows; my read is SDK vendors (the equivalence guarantee *is* their product promise) but this is a market call.
- **OQ2 — Open corpus?** Publish the context pack + synthetic corpus as open artifacts (the ecosystem play; probably the only viable one for a language) vs hold. Interacts with any future fine-tune.
- **OQ3 — v1 timing.** My lean per R12/R-B: release-prep now; the governance window is open.
- **OQ4 — The runtime-AI pillar.** Fund the §10.8 end-to-end demonstration early in v1.x, or quietly defer it and let compile-time governance carry positioning until there's pull. (R9 holds either way.)

---

## 10. Bottom line

The thesis survives, transformed. Bock conceived as "write once, AI compiles everywhere" meets a 2026 where generation is commodity and *trust* is scarce — and it happens to have spent two years building the trust machinery: provable equivalence, auditable AI decisions, verified semantics, deterministic fallback. The published research frontier independently converged on its compile architecture; the market independently converged on its governance requirements; its own development process is a working demonstration of its premise. The convenience pitch is gone — concede it. The guarantee pitch is stronger than the original ever was — lead with it. One existential risk (model familiarity) has concrete, fundable mitigations that only this project can execute, because only this project has a compiler that manufactures its own verified training data. Ship v1, reposition, close the familiarity gap, and point v1.x at verification depth and the agent interface.
