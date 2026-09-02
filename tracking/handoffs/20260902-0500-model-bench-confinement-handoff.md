# Handoff — model-bench agent confinement (→ operator, needs root)

**Filed:** 2026-09-02 05:00 UTC
**Queue item:** Q-model-bench
**Blocks:** every scored run. Not a hardening nicety — the benchmark
currently cannot measure two of its four axes soundly.
**PR:** #507 (plumbing, scrub, tripwire — landed/landing)

## The one thing being asked for

Create a low-privilege unix user the benchmarked agent runs as, so the
**kernel** enforces the scratch-tree boundary:

```bash
useradd -r -m benchagent
setfacl -R  -m u:benchagent:rwX /home/claude/bench-scratch/bock
setfacl -R -d -m u:benchagent:rwX /home/claude/bench-scratch/bock
# benchagent must have NO write access to /opt/claude-projects/**
# and must be able to exec: claude, node, cargo, git
```

Then every run adds `--run-as-user benchagent`. The flag is already
implemented and tested; nothing else changes.

## Why a session cannot do this

Confirmed 2026-09-02 from the session shell (uid 1001 `claude`):

- `sudo` — not installed
- `bwrap` — not installed
- unprivileged user namespaces — `unshare -Ur` fails with EPERM, and
  there is no `/proc/sys/kernel/unprivileged_userns_clone` to flip

So there is no route to an OS-enforced boundary without root. This is
environmental, not editorial.

## What went wrong (the finding that prompted this)

`tiel-coder`'s pre-flight, first tool call, cwd = scratch clone:

    Read(/opt/claude-projects/bock/compiler/crates/bock-source/src/lib.rs)

An absolute path into the **live repo**. The read succeeded. Killed
before any `Edit`; live repo verified clean (`git status` empty,
`lib.rs` undiffed, HEAD still `b0aeede`).

The model did not invent the path — **the pinned tree's own `CLAUDE.md`
names the live repo six times**, and Claude Code injects `CLAUDE.md`
into the system prompt verbatim. The harness was handing every model a
signpost to the real repo and then scoring it on whether it followed
one.

Scoring reads `git status` in the *scratch* clone, so an edit anywhere
else never appears in `changed_files` — scored as though the model did
nothing. The scope and destruction axes were unmeasurable.

## Already mitigated in #507 (do not re-do)

- **Scrub** — outside paths rewritten out of the scratch clone's
  `CLAUDE.md` before every run, committed as one harness-owned commit
  so the model's diff is still measured against the tree it was handed.
  Removes the invitation. Verified: 0 live-path refs remain.
- **Tripwire** — `--protect PATH` fingerprints `HEAD` + `git status`
  around each run; any change vetoes the run. Detects, does not prevent.

Neither is confinement. Both are in place now, so tripwire-only running
is *possible* — the decision on whether to bench before `benchagent`
exists is the operator's.

## Consequence for existing data

Every scored row taken before #507 has an **unvalidated scope axis**,
including flash-next-c's `completion=1` t1 row (2026-09-01). Its diff
was in-scratch and looks genuine, but the axis was never enforced. Do
not carry those rows into a campaign table without a footnote.

## Also worth an operator minute

- `tools/model-bench/**/__pycache__/*.pyc` is **checked into the repo**
  (landed with #497), so every harness run dirties the tree and adds
  binary churn to unrelated diffs. Wants a `git rm -r --cached` plus a
  `.gitignore` line.
- Killing a run: `pkill -f harness.run` orphans the `claude -p` child
  and `shim.server`, which keep running with `--dangerously-skip-
  permissions`. Kill by PID.

## Not blocked on this

`tiel-coder` passes the transport half of the pre-flight (clean
`tool_calls`, reasoning extracted into `reasoning_content` despite
`reasoning_format = none` — do **not** add `--reasoning-format auto` to
that entry on the strength of the flag list alone). First measured
figures, rocm / b10752 / UD-Q6_K_XL / -c 65536:

| | tiel-coder | flash-next-c |
|---|---|---|
| prefill | ~564 tok/s | ~230–400 tok/s |
| decode | ~45 tok/s | ~24 tok/s |
| MTP acceptance | 57.6% aggregate (517/898) | not measured |

MTP acceptance is strongly per-turn — ~91% and ~88% on the two short
turns, **53.5% (426/796)** on the long one. Quote the spread, not the
aggregate.
