# Bock Agentic Operating Model

The Bock-specific instantiation of the agentic multi-chat
playbook, extended with a sub-agent integration layer. This
document migrates Bock from human-orchestrated to agentic
operation: an Orchestrator fills the product-owner-and-merge-
coordinator role with bounded autonomy, escalating to the human
(you) at strategic checkpoints.

Assumes familiarity with the agentic playbook. This document
covers what's specific to Bock: role mapping against current
state, escalation thresholds calibrated for a pre-1.0 solo-
maintained project, the sub-agent layer, and the transition
sequence.

---

## Role mapping for Bock

```
You (strategic director)
  │ v1.0 release decisions, external-facing content,
  │ target/provider/tooling choices, roadmap priorities
  ▼
Orchestrator
  │ work routing, session scheduling, merge coordination,
  │ tracking files, audit, consistency sweeps
  ├──────────────┬──────────────────┬─────────────────┐
  ▼              ▼                  ▼                 ▼
Design        Implementation    Marketing      (consistency
(spec)        (sessions)        (positioning)   sub-agents)
                  │
                  ▼
              Engineers (Claude Code sessions in worktrees)
                  │
                  ▼
              Sub-agents (spawned within sessions for
                          parallel/focused work)
```

The three core chats keep their existing responsibilities and
tones from the human-orchestrated model. The Orchestrator is the
new coordination layer. Engineers are the existing worktree CC
sessions. Sub-agents are the new fan-out layer.

**One asymmetry to note:** the migration effectively folds the
**Implementation** chat into the Orchestrator (the Orchestrator now
dispatches/coordinates engineering directly). It does **not** fold in
the **Design** chat — Design remains the authoritative
core-specification voice alongside the owner. The Orchestrator
orchestrates and iterates on design but escalates core-spec decisions
to Design rather than deciding them (see the orchestrator contract,
"Design authority & core-specification decisions").

---

## Escalation thresholds (calibrated for Bock)

Bock is pre-1.0, solo-maintained, with churn budget available.
The Orchestrator can be moderately autonomous on routine
implementation and coordination; it escalates anything
strategic, external-facing, or scope-affecting.

### Orchestrator handles autonomously

- Scheduling and dispatching engineering sessions for
  already-decided work (changelogs landed, design resolved)
- Coordinating merges for PRs with clean verification gates
  (`cargo fmt --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `cargo test --workspace`, `mdbook build`)
- Updating tracking files (STATUS.md, follow-up lists; ROADMAP.md
  only per a design decision, never unilaterally)
- Routing OPEN→Design, FOUND→Implementation triage
- Running consistency sweeps (changelog cross-refs, timestamp
  coherence, tracking file alignment)
- Batching and sequencing work to avoid conflicts (e.g., the
  §20.1.1 / exit-code coordination on bock-cli)

### Orchestrator escalates to you

| Trigger | Why |
|---------|-----|
| **v1.0 release actions** | Announcement, crates.io publish, marketplace publish, site deploy — all external and irreversible |
| **External-facing content** | Website copy, README intros, announcement posts, social bios. Marketing drafts; you approve publication |
| **Target additions/changes** | The §1.3 "5 ship, 4 planned" is settled; any change to that is strategic (the AIR/codegen architecture pivots on it) |
| **Provider/tooling choices** | AI provider selection, new third-party deps, new codegen targets |
| **Roadmap reprioritization** | Changing what v1.0 vs v1.1 vs v1.2 contains |
| **Scope expansion** | Work surfacing new requirements beyond what v1.0 means ("ship what's already done") |
| **Cross-role conflict** | Design says X, Implementation says X is infeasible, and the handoff patterns don't resolve it |
| **Repeated session failure** | Sessions failing verification after the retry threshold |

### Retry thresholds before escalation

| Session type | Retries |
|--------------|---------|
| Mechanical (rename, format, lint fix, doc edit) | 1 |
| Spec-content edit | 2 |
| Feature implementation | 2 |
| Cross-cutting refactor | 1 (escalate fast) |
| Verification/audit | 1 (failure usually means scope problem) |

---

## Sub-agent integration layer

This is the new capability beyond the agentic playbook. Sub-
agents are spawned within a context — an engineer session or the
orchestrator — to handle parallelizable, bounded, independent
work and report structured results the parent aggregates.

### The sub-agent contract

A sub-agent:

- Receives a **focused task** with explicit inputs and an
  expected output structure
- Operates **within the parent's scope** — it does not expand
  owned-files, touch files outside the parent's declaration, or
  alter shared state
- Reports **structured results** the parent can aggregate
  mechanically (a table, a matrix, a pass/fail list)
- Does **not commit independently** — the parent session owns
  the commit; sub-agents produce findings and artifacts the
  parent assembles
- Does **not make design decisions** — design questions surface
  as OPEN to the parent, which routes to Design chat via the
  Orchestrator
- Has **bounded fan-out depth** — sub-agents do not spawn their
  own sub-agents beyond one level, to avoid runaway recursion
  and unaccountable work

### Model and effort floor

Sub-agent model and effort selection defaults to the spawning
context's discretion. This has a failure mode observed across
projects: parents select lower-tier models or reduced effort for
delegated work as an implicit cost optimization. The work
sub-agents do is often the substantive work (verification,
codegen, analysis); degrading it while the coordination layer
stays strong is backwards.

**Rule: no agent anywhere in the delegation tree runs below the
orchestrator's model version or effort level.**

- The orchestrator's model and effort (set by you) is the global
  floor for the entire tree
- Any sub-agent or engineer session: model version ≥ the
  orchestrator's, effort level ≥ the orchestrator's
- The floor is transitive — a sub-agent spawned by an engineer
  session you dispatched is also bound by it (orchestrator →
  engineer → sub-agent; the floor holds at every step)
- Delegating to an equal-or-higher configuration is allowed;
  delegating to a lower one is not
- Override framework defaults that select lower models or reduced
  effort for delegated work — this floor takes precedence over
  the default delegation heuristic

The orchestrator does not get to make delegated work cheaper by
making it weaker. If cost genuinely becomes a concern, it
surfaces as an escalation (a resource decision for you), not a
silent downgrade of sub-agent capability. A weaker sub-agent
producing a result the orchestrator then trusts is worse than an
honest escalation about cost, because the quality loss is
invisible until it lands in the repo.

### When to spawn sub-agents

Spawn when work is:

- **Parallelizable** — independent units that can run
  concurrently (per-target, per-fixture, per-section)
- **Bounded** — each unit has clear inputs and a clear done
  condition
- **Aggregatable** — results combine cleanly into the parent's
  deliverable

Do NOT spawn when work is:

- **Sequential** — unit B needs unit A's output (run in the
  parent, in order)
- **Shared-state** — units mutate common state (race conditions;
  keep in the parent)
- **Design-bearing** — the work requires a design decision
  (route to Design, not a sub-agent)

### Bock-specific sub-agent patterns

**Pattern 1 — Per-target fan-out.** Codegen and conformance work
across five targets (js, ts, python, rust, go) parallelizes
cleanly. One sub-agent per target verifies the target's output
or runs the target's fixtures; the parent aggregates a pass/fail
matrix.

*Applies to:* Handoff 2 (conformance fixtures — one sub-agent per
target executing the six fixtures, returning the per-target
pass/fail matrix); Item B per-target phases (Phases 2-5 each
target's project-mode scaffolding); cross-target verification in
any codegen-touching session.

**Pattern 2 — Two-axis spec alignment.** Spec-alignment audits
have two complementary axes that the D1/D2 work established:
normative review (read spec, compare to impl) and example
execution (run every example through the tool). These are
independent and parallelizable. One sub-agent walks normative
claims; another extracts and executes examples; the parent merges
into the divergence matrix.

*Applies to:* Future spec-alignment refreshes; the
documentation-buildout phases (D3 verifies CLI examples against
`bock --help`; D4 verifies stdlib examples). Each phase can fan
out the two axes.

**Pattern 3 — Consistency sweep.** The Orchestrator's periodic
consistency checks (changelog cross-reference integrity,
timestamp coherence, tracking file alignment) are bounded,
read-mostly work. A consistency sub-agent runs the sweep and
returns a findings list while the Orchestrator handles other
coordination.

*Applies to:* The Orchestrator's periodic sweeps; the
cross-reference verification that K04 surfaced as necessary.

**Pattern 4 — Multi-crate blast-radius analysis.** When a change
touches the compiler crate graph (bock-errors → bock-source → …
→ bock-cli), understanding the blast radius means inspecting
multiple crates. Sub-agents inspect different crates in parallel
and report what each would need to change; the parent assembles
the impact map before the session edits anything.

*Applies to:* Cross-cutting refactors; verification sessions that
need to understand where a behavior lives (like the §10.4
verification, which walked parser/AST/type-checker/runtime — those
four could have been four parallel sub-agents).

### Sub-agent output contract

Sub-agents return structured results in a format the parent
parses. Standard shape:

```
## Sub-agent: <task identifier>
**Status:** complete | blocked | partial
**Inputs processed:** <what was examined>
**Findings:**
<structured result — table, matrix, or list>
**Surfaced items:**
- OPEN: <design questions, if any>
- FOUND: <bugs, if any>
**Citations:** <file:line references>
```

The parent aggregates these into its own deliverable (the PR
description, the verification report, the alignment matrix).

### Sub-agents and the worktree pattern

Sub-agents run **within** an engineer session's worktree. They
share the parent's `$WORKTREE` and `$BOCK_TEST_NAMESPACE` but
must namespace their own scratch under
`$BOCK_TEST_NAMESPACE/<sub-agent-id>/` to avoid collisions.
Sub-agents do not create branches, do not push, do not open PRs
— the parent session's teardown handles all of that. A sub-agent
that needs to write a file writes it to the worktree; the parent
includes it in the commit.

---

## Orchestrator workspace

The Orchestrator maintains a `tracking/` directory. These files
are the Orchestrator's working memory and your review surface.

```
tracking/
├─ queue.md          ← work queue across all chats, with status
│                       and dependencies
├─ audit.md          ← decision log (reasoning per action)
├─ escalations.md    ← items awaiting your response
└─ routing.md        ← Bock-specific routing rules
```

`tracking/` is committed to the repo (it's project state, not
scratch). The Orchestrator reads these at the start of every
work block and writes to them as it coordinates. Those writes land
via tracking PRs (`chore/tracking-<UTC>` branch → PR → merge), never
direct commits to the ruleset-protected `main`; see the orchestrator
contract (Main integration & tracking PRs).

### routing.md (Bock-specific rules)

```markdown
# Orchestrator Routing Rules

## Standard flows

1. **New feature**: Design changelog → Implementation session
   prompt → Engineer session → merge → STATUS.md update →
   (Marketing if user-facing, escalate for external copy)

2. **Bug fix**: FOUND tag → Implementation triage → Engineer
   session → merge

3. **Spec alignment**: Implementation audit (two-axis, possibly
   sub-agent fan-out) → batch by topic → Design batch resolution
   → per-batch changelogs land → downstream phases unblock

4. **Documentation phase (D-series)**: verify prerequisites
   resolved → Implementation session prompt → Engineer session
   (two-axis verification, sub-agent fan-out for example
   execution) → merge → FOUND items routed to Design

## Conflict-avoidance rules

- Sessions touching the same crate/file do not run concurrently.
  Known hot file: compiler/crates/bock-cli/src/main.rs (exit-code
  fix + §20.1.1 flag work both touch it). Sequence or combine.
- Documentation phases that delete shared scratch (D5 deletes
  INVENTORY.md / SPEC-ALIGNMENT.md) grep for references before
  deletion; expand scope or coordinate.

## Escalation-fast triggers

- Any external-facing artifact → escalate before publish
- Any v1.0 release action → escalate
- Any target/provider/tooling change → escalate
```

---

## Transition sequence

Migrate incrementally. Don't flip the whole project at once;
validate the model on a bounded first block, then scale.

### Step 1 — Set up the Orchestrator workspace

Create `tracking/` with the four files. Seed `queue.md` with
current in-flight work (the companion `bock-work-queue-seed.md`
artifact is ready to drop in). `audit.md` and `escalations.md`
start empty with their entry-format headers. `routing.md` gets
the Bock rules above.

### Step 2 — Stand up the Orchestrator as a Claude Code session

The orchestrator runs as a Claude Code session (not a chat
instance) with persistent access to the repo and `tracking/`,
auto-dispatching engineer sessions and sub-agents. Its operating
contract is `.claude/agents/orchestrator.md`, which combines the
shared content (authoritative sources, timestamps, DO NOT) + the
Orchestrator role definition + the Bock escalation thresholds +
the sub-agent integration layer + the startup protocol that reads
`tracking/`.

Launch the session pointed at `.claude/agents/orchestrator.md`.
Set its model and effort deliberately — that configuration is the
global floor for every engineer session and sub-agent beneath it
(see "Model and effort floor").

### Step 3 — First agentic block: the three handoffs + §20.1.1

The current ready-to-dispatch work is the ideal first block. It
exercises the full machinery on real, bounded work:

- **Handoff 3 (§1.5 cleanup)** — solo spec session, no
  conflict, no sub-agents. Simplest; validates basic
  orchestration.
- **Handoff 2 (conformance fixtures)** — exercises sub-agent
  fan-out (Pattern 1: one sub-agent per target executing the six
  fixtures). Validates the sub-agent layer.
- **Handoff 1 + §20.1.1 (bock-cli)** — exercises the
  conflict-avoidance rule (both touch main.rs; sequence or
  combine). Validates coordination.

The Orchestrator coordinates all three, escalating nothing
(none of this is strategic), and produces an audit trail you
review at the end of the block.

### Step 4 — Review the first block

Read the audit log. Check:
- Did the Orchestrator route correctly?
- Did it respect the conflict-avoidance rule on bock-cli?
- Did the conformance sub-agents fan out and aggregate cleanly?
- Was the reasoning per decision sound?
- Did it escalate anything it shouldn't have, or fail to escalate
  something it should have?

Adjust escalation thresholds and routing rules based on what you
see.

### Step 5 — Scale up

Subsequent blocks: the documentation phases (D3/D4 now that the
spec resolutions landed; D5 after D2-D4; D6 independent), then
Item B (after D5). The Orchestrator coordinates progressively
larger scope with progressively less human touch as the patterns
prove themselves.

---

## What stays the same

The migration changes the coordination layer, not the
fundamentals:

- Spec is still the single source of truth; Design still owns it
- Worktree sessions with verification gates still execute the work
- Timestamp discipline (`date -u`), owned-files discipline,
  two-state handoffs, historical preservation — all unchanged
- The engineering tone (skeptical, direct, no superlatives)
  applies to Orchestrator, Design, and Implementation
- Marketing's accuracy constraints are unchanged

The Orchestrator does what you've been doing (routing, merging,
tracking, sweeping) with bounded autonomy and an audit trail,
freeing you to operate as strategic director rather than
continuous coordinator.

---

## Bock-specific risks to watch

- **v1.0 is close.** "Ship what's already done" means the
  Orchestrator is coordinating toward a release with external,
  irreversible actions (publish, announce, deploy). Escalation
  discipline on external-facing actions matters more here than
  in early-phase projects. Keep those thresholds tight.

- **The marketing chat touches external surfaces.** Website copy,
  README, announcement. In agentic mode, Marketing drafts but
  never publishes; publication always escalates. Don't let the
  Orchestrator treat "update the website copy" as a routine
  merge — it's external-facing even when it feels like a code
  change (the website source lives in the repo, but the published
  site is audience-facing).

- **Sub-agent fan-out on codegen can mask per-target failures.**
  When five sub-agents verify five targets in parallel, an
  aggregated "4/5 passed" is easy to wave through. The parent
  must surface each FOUND per-target failure explicitly, not bury
  it in an aggregate. The conformance handoff already models this
  (FOUND tags per target); keep that discipline.

- **The orchestrator forgetting its own decisions.** Long
  coordination runs risk the Orchestrator losing track of
  earlier decisions. The audit log + reading `tracking/` at every
  block start is the mitigation. If the Orchestrator's reasoning
  starts referencing state that doesn't match `tracking/`, that's
  the drift signal — refresh and re-ground.
