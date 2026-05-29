# Work Queue

Orchestrator working memory. Read at the start of every work
block; update as work moves. Committed to the repo as project
state.

Last updated: 2026-05-29 13:26 UTC (Block 1 DISPATCHED — H3/H2/H1 launched as
engineer sessions; C1 sequenced after H1 lands. Repo HEAD 4210186; no Block 1
drift. See audit.md 13:26 STARTUP + DISPATCH entry.)

**Block 1 status (in flight):**
- H3 `spec/paradigm-cleanup` — dispatched
- H2 `test/effect-handler-conformance` — dispatched (per-target fan-out
  contingent on harness reality; see audit)
- H1 `fix/check-exit-code` — dispatched
- C1 `feat/check-aspect-flags` — queued, dispatch after H1 merges

---

## Ready to dispatch

Work that's unblocked and ready for engineering sessions.

### Block 1 (first agentic block — the three handoffs + §20.1.1)

| ID | Work | Type | Sub-agents? | Conflict notes |
|----|------|------|-------------|----------------|
| H3 | §1.5 paradigm cleanup | spec edit | No | None. Solo spec session |
| H2 | Effect handler conformance fixtures | test coverage | Yes (Pattern 1: per-target) | None. Touches conformance/ only |
| H1 | `bock check` exit-code bug | bug fix | No | **Touches bock-cli/main.rs — conflicts with §20.1.1** |
| C1 | §20.1.1 `--only`/`--brief` flag alignment | impl | No | **Touches bock-cli/main.rs — conflicts with H1** |

**Coordination for H1 + C1:** Both modify
`compiler/crates/bock-cli/src/main.rs`. Do NOT run concurrently.
Either:
- Combine into one bock-cli session (exit-code fix + flag
  alignment), one PR, OR
- Sequence: run one, merge, rebase the other.

Implementation chat's read favored sequential for cleaner
per-PR review; Orchestrator's call within that guidance.

**H1 owned-files (confirmed):** the whole
`compiler/crates/bock-cli/` crate. Exit-code logic is in
`src/check.rs`; the audit spans other command sources in `src/`
(build, test, run, fmt). CLI integration tests are colocated at
`compiler/crates/bock-cli/tests/` — there is **no** central
`compiler/tests/cli/`, and the H1 handoff's instruction to create
one is wrong. The exit-code tests extend the existing
`tests/check_command.rs`. Both H1 and C1 touch this crate, so
conflict-avoidance applies — never concurrent.

**H3 timestamp note:** The drop-in changelog hard-codes
`20260515-0434`. Use `date -u` at session execution; if landing
meaningfully after that time, re-timestamp filename and content
date.

---

## Blocked

Work waiting on dependencies.

| ID | Work | Blocked on | Notes |
|----|------|------------|-------|
| D3 | Tooling reference (docs) | §20.3/§20.5/§20.6/§20.1.1 resolutions | Spec resolutions landed in revision; §20.1.1 needs C1 to land first |
| D4 | Stdlib reference (docs) | §18.3/§18.5/§19.7 resolutions | Spec resolutions landed; verify before dispatch |
| D5 | Contributor docs + cleanup | D2-D4 complete | Deletes INVENTORY.md/SPEC-ALIGNMENT.md — grep for refs before deletion |
| D2-polish | D2 final polish | D2-FOUND items resolved | Most resolved in spec revision; verify remaining |
| ItemB-P1 | Project mode codegen Phase 1 | Documentation buildout D5 | 8-12h scope; per-target Phases 2-5 parallelize via sub-agents after P1 |
| ItemD | /get-started project-mode evolution | Item B Phase 6 | Deferred; external-facing (escalate for copy) |

---

## Independent (can run anytime)

| ID | Work | Type | Sub-agents? | Notes |
|----|------|------|-------------|-------|
| D6 | Changelog backfill | docs | No | Independent after D1 resolved (it is). CHANGELOG.md regeneration |

---

## Deferred (no action; tracked for completeness)

| ID | Work | Trigger |
|----|------|---------|
| ItemC | /get-started AI configuration section | Real-world AI usage characterization (post-launch) |

---

## Dependency graph

```
Block 1 (H3, H2, H1+C1) ──┐
                          │ C1 unblocks ──→ D3
                          │
D3 ─┐                     │
D4 ─┼──→ D5 ──→ ItemB-P1 ──→ ItemB-P2..5 (sub-agent fan-out)
D2-polish ─┘                              ──→ ItemB-P6 ──→ ItemD

D6 (independent) ─── runs anytime
```

**Critical path to v1.0:** Block 1 → D3/D4 → D5 → Item B → v1.0
release actions (all escalate).

---

## Escalation-pending

(none at seed — populated as the Orchestrator surfaces items)

---

## Recently landed (for orchestrator context, not action)

- Agentic infrastructure (PR #70, 2026-05-29) — tracking/,
  orchestrator contract, operating model. Coordination layer
  only; touches no Block 1 work.
- Spec changelog re-add (PR #69, 2026-05-29) —
  20260514-0548-spec-revision-artifact.md.
- D1+D2 spec alignment consolidation: 14 changelogs + spec
  revision artifact (20260514-0548) — 21 sections updated
- §10.4 handler form verification report (20260514-0540)
- §13.5 cancellation restructure (20260514-0449)
- §15 annotation taxonomy (20260514-0408)
- K04 spec consolidation (20260512-1700) — spec/sections/ deleted

---

## Notes for the orchestrator

- **v1.0 is the active milestone.** "Ship what's already done."
  Coordinating toward release means external/irreversible actions
  ahead (publish, announce, deploy) — all escalate.
- **bock-cli is a hot file.** H1 and C1 both touch main.rs.
  Sequence or combine; never concurrent. This is the conflict-
  avoidance rule's first real test.
- **Conformance sub-agents must surface per-target FOUND tags
  explicitly.** Don't let "4/5 passed" bury a per-target codegen
  failure. The parent aggregates but does not hide.
- **D5 deletion sweep.** When D5 runs, grep for INVENTORY.md /
  SPEC-ALIGNMENT.md references before deleting them; expand scope
  or coordinate per the owned-files discipline.
