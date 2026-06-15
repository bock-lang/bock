# Tracking — Claude Conventions

This subtree is the orchestrator's working memory. The files here
are project state, committed to the repo, read by the orchestrator
at the start of every work block.

## Files — the planning hub

```
tracking/
  queue.md            Active work items (the one work list).
  divergences.md      Spec ↔ impl factual mismatches + disposition.
  design-questions.md Open design decisions (core-spec → escalated).
  milestones.md       Version milestones; item→version mapping.
  snapshot.md         Current project-state facts (build, what works).
  routing.md          Routing rules + conflict-avoidance.
  audit.md            Decision log (reasoning per action) + digests.
  escalations.md      Items awaiting the human / Design, with responses.
  designs/, plans/    Approved design specs + implementation plans.
  handoffs/           Cross-chat handoffs (→ Marketing / Design) + their resolutions.
```

**Handoff filenames** follow `YYYYMMDD-HHMM-<descriptor>-handoff.md` (UTC, the
project-instructions handoff convention) — e.g.
`20260615-0412-marketing-positioning-handoff.md`. A `> ✅ RESOLVED <date>` banner
+ the signed decisions land at the top of the same file when the handoff returns.

`ROADMAP.md` and `STATUS.md` at the repo root are **generated** from
this hub by `tools/scripts/gen-tracking-views.sh` (milestones → ROADMAP;
snapshot + queue summary → STATUS). Do NOT hand-edit them; the
`Tracking Views` CI workflow `--check`s they stay in sync. Run the
generator after changing the hub.

## Hub file boundaries (every item has exactly one home)

| File | The one question it owns |
|------|--------------------------|
| `queue.md` | What work is to-be / being done? (actionable) |
| `divergences.md` | Where does impl differ from spec, and what's the disposition? |
| `design-questions.md` | What design decisions are open? |
| `milestones.md` | What ships in which version? |
| `snapshot.md` | What is the current project state? |
| `audit.md` | What happened / was decided? (history) |
| `escalations.md` | What needs the human / Design? |

Disambiguation: **actionable → `queue.md`** (incl. triaged FOUND-bugs
and actionable doc gaps); **factual mismatch → `divergences.md`**;
**undecided behavior → `design-questions.md`** (core-spec ones are
escalated to Design, never decided here — see orchestrator.md "Design
authority"); **version mapping → `milestones.md`** (mapping only;
detail by ID in queue); **present-state → `snapshot.md`** (never future
work). Items carry stable IDs and are named once, referenced by ID
elsewhere — never duplicated across files. Raw OPEN/FOUND tags arrive
via PR descriptions; the orchestrator triages them into the right file.

## Authority

These files are the orchestrator's working memory, not an
independent source of truth. **The repo wins.** If `queue.md`
disagrees with repo state (a PR merged that the queue still shows
as pending), the repo is canonical — the orchestrator reconciles
the queue against the repo at startup.

`routing.md` rules are stable conventions; changes to them are a
process decision, not routine orchestrator activity. The
orchestrator follows routing.md; it does not rewrite the rules
without surfacing the change.

## Integration

These files land on `main` like everything else: via a
`chore/tracking-<UTC>` branch and PR, never a direct commit to local
`main` (which is ruleset-protected, PR-only). The orchestrator batches
tracking writes across a block and merges one tracking PR at a natural
boundary — block completion, daily digest, or session end — then
re-syncs local `main` with a fast-forward. Because `tracking/` touches
no code, these PRs never conflict with feature PRs. Full mechanism:
`.claude/agents/orchestrator.md` (Main integration & tracking PRs).

## Timestamps

All dated entries use UTC via `date -u`. Audit entries, digests,
and escalation timestamps follow the project timestamp discipline
(see root CLAUDE.md / the operating model).

## Not for engineer sessions

Engineer sessions do not read or write `tracking/`. This is
orchestrator-only working memory. Engineer sessions receive their
scope through their session prompt and surface results through
PR descriptions (OPEN/FOUND tags), which the orchestrator then
folds into `tracking/`.
