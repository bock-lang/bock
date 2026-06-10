# Orchestrator Routing Rules

## Standard flows

1. **New feature**: Design changelog → Implementation session
   prompt → Engineer session → merge → STATUS.md update →
   (Marketing if user-facing; escalate for external copy)

2. **Bug fix**: FOUND tag → Implementation triage → Engineer
   session → merge

3. **Spec alignment**: Implementation audit (two-axis: normative
   walk + example execution, sub-agent fan-out optional) → batch
   by topic → Design batch resolution → per-batch changelogs land
   → downstream phases unblock

4. **Documentation phase (D-series)**: verify prerequisites
   resolved → Implementation session prompt → Engineer session
   (two-axis verification, sub-agent fan-out for example
   execution) → merge → FOUND items routed to Design

5. **Design question**: classify core-spec vs non-core (orchestrator's
   discretion; when unsure → core-spec). **Core-spec** → record in
   `design-questions.md` (`escalated → Design`) + file in
   `escalations.md` → Design Chat + owner decide → orchestrator
   reconciles spec/`divergences.md` + unblocks the linked queue items.
   **Non-core** (CLI shape, tooling, process) → iterate with the owner →
   record the decision. Never block the queue on a design decision —
   file and move on. (See orchestrator.md "Design authority".)

## Conflict-avoidance rules

- Sessions touching the same crate/file do NOT run concurrently.
  Known hot file: `compiler/crates/bock-cli/src/main.rs`
  (exit-code fix H1 + §20.1.1 flag work C1 both touch it).
  Sequence or combine; the orchestrator decides, never concurrent.
- Sessions that delete shared files grep for references first, then
  update or accept each before deletion (e.g. the tracking
  consolidation retired `docs/INVENTORY.md` / `docs/SPEC-ALIGNMENT.md`
  / `tracking/tracking.md` after migrating their content into the hub).
  Expand scope or coordinate per the owned-files discipline.
- Sessions whose verification gate scans repo-wide (e.g.,
  "no refs to X remain") must either declare the full reachable
  set as owned-files or carry explicit SCOPE EXPANSION
  permission. Predict the gate's reach at dispatch time.

## Sub-agent rules

- Model/effort floor: every sub-agent and engineer session runs
  at ≥ the orchestrator's model and effort. Override cheaper
  framework defaults. Cost concerns escalate; never downgrade
  silently.
- Per-unit fan-out surfaces each per-unit failure explicitly via
  FOUND tags. No burying failures in aggregate pass rates.
- Sub-agents stay within the parent's scope; no independent
  commits; design questions surface as OPEN; bounded fan-out
  depth (one level).

## Test placement convention

Two distinct test types live in different places — do not conflate
them when declaring owned-files or instructing sessions:

- **Crate integration tests** are colocated: `compiler/crates/
  <crate>/tests/`. There is no central `compiler/tests/cli/` or
  equivalent. CLI integration tests for `bock-cli` live at
  `compiler/crates/bock-cli/tests/` (e.g., `check_command.rs`,
  `build_command.rs`).
- **Language conformance fixtures** are central:
  `compiler/tests/conformance/<category>/` (`.bock` + `.expected`).
  This is where H2's effect-handler fixtures go.

A session prompt that instructs creating a central CLI test
directory is wrong — extend the colocated crate tests instead.

## Prioritization rules

- **Interpreter-as-oracle guard (2026-06-09 design audit, R11):**
  interpreter parity items (the `Q-interp-*` pattern) are
  **correctness work, not polish** — the interpreter is the Tier 1
  semantics oracle, and if it lags checker/codegen the equivalence
  claim silently degrades to "targets agree with each other".
  Rank interpreter-parity items with correctness bugs, not with
  ergonomic backlog, when sequencing dispatch.

## Escalation-fast triggers

- Any external-facing artifact → escalate before publish
- Any v1.0 release action → escalate
- Any target/provider/tooling change → escalate
- Any scope expansion beyond "ship what's already done" → escalate
