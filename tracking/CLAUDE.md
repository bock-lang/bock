# Tracking — Claude Conventions

This subtree is the orchestrator's working memory. The files here
are project state, committed to the repo, read by the orchestrator
at the start of every work block.

## Files

```
tracking/
  queue.md         Work queue across all chats — status, dependencies,
                   dependency graph. The orchestrator's view of what's
                   ready, blocked, deferred.
  routing.md       Routing rules and conflict-avoidance constraints.
                   How the orchestrator decides where work goes.
  audit.md         Decision log (reasoning per action) + daily digests.
                   The human's review surface.
  escalations.md   Items awaiting the human's decision, with responses.
```

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
