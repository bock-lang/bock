# Design — Centralized In-Repo Planning Hub

**Date:** 2026-05-29
**Status:** Approved (design); implementation pending
**Owner:** Orchestrator, with the project owner

## Problem

Forward-looking work is tracked in many overlapping places, and the
pre-agentic workflow let the implementation chat steer tracking
*blind* — with no repo visibility — so drift and misalignment
accumulated. Inventory of where work/tracking lives today (outside
spec text):

- **Work-item trackers (fragmented):** `tracking/queue.md` (live
  queue), `tracking/tracking.md` (a **stale duplicate** of the
  queue), `docs/INVENTORY.md` (doc-gap + drift tables + impl-surface
  counts), `docs/SPEC-ALIGNMENT.md` (spec-vs-impl divergences/OPENs),
  `STATUS.md` ("Deferred Items"), `ROADMAP.md` (milestones).
- **History/records:** `tracking/audit.md`, `spec/changelogs/`,
  `CHANGELOG.md`.
- **Intake/process/other:** `.github/ISSUE_TEMPLATE/*`,
  `tracking/{routing,escalations,CLAUDE}.md`, code `TODO/FIXME`,
  `branding/*-content-inventory.md`.
- **Off-repo:** the implementation chat's in-flight list — the blind
  tracker that is the root cause of the drift.

The same logical item recurs across several surfaces, sometimes
inconsistently (e.g. the stdlib appears in INVENTORY, SPEC-ALIGNMENT,
the queue, and ROADMAP with differing framing); OPEN/FOUND findings
live in multiple places; deferred work is split across STATUS,
ROADMAP, and the queue; and there is a literal duplicate queue.

## Decisions

1. **Scope — one planning hub.** A single in-repo source of truth
   owns all *forward-looking* state: work items, findings/OPENs,
   deferred items, doc gaps, and milestones. Only `audit.md`
   (history), `CHANGELOG.md` (change records), and GitHub Issues
   (public intake, post-launch) stay separate.
2. **Medium — in-repo markdown under `tracking/`**, orchestrator-owned.
   The SoT travels with the code, diffs in PRs, and is what the
   orchestrator already reads at every block start. This directly
   removes the off-repo blind-tracker failure mode.
3. **Structure — structured set + generated human views.** The hub
   is a small set of concern-specific files; human-facing `ROADMAP.md`
   and `STATUS.md` are *generated from* the hub so they cannot drift.
   `STATUS.md` is **fully generated** (its static content moves into
   the hub).

## Architecture

### Hub files — canonical SoT (hand-maintained by the orchestrator)

- `tracking/queue.md` — the single **active-work** list.
- `tracking/findings.md` — **divergences + OPENs + the spec
  design-decision queue**. Absorbs `docs/SPEC-ALIGNMENT.md` and the
  open D1-refresh / D2-FOUND decision rows; ongoing FOUND/OPEN items
  land here.
- `tracking/milestones.md` — **milestones** (v1.0 / v1.1 / v1.2 / v2)
  plus a small static **project-status block** (build state, phase
  history, migration notes) that `STATUS.md` is rendered from.

### Unchanged

- `tracking/audit.md` (append-only decision log + daily digests),
  `tracking/escalations.md`, `tracking/routing.md`,
  `tracking/CLAUDE.md` (updated to describe the hub).

### Retired (content migrated, then files removed)

- `tracking/tracking.md` — the stale duplicate queue. **Deleted.**
- `docs/INVENTORY.md` and `docs/SPEC-ALIGNMENT.md` — content migrated
  into `queue.md` / `findings.md`, then **deleted** (they were already
  slated for deletion under the D5 contributor-docs phase).

### Generated at repo root (DO NOT hand-edit)

- `ROADMAP.md` — rendered from `tracking/milestones.md`.
- `STATUS.md` — rendered from `tracking/queue.md` (active/blocked/
  deferred summary) and the `milestones.md` static block (which holds
  the build/test snapshot, milestone progress, and phase history that
  the orchestrator updates). All STATUS content thus originates in the
  hub — nothing is hand-authored in the generated file.

## Item schemas (stable IDs, cross-referenced across hub files)

- **queue item:** `[ID] title — type(impl|spec|docs|chore|bug) ·
  status(ready|in-flight|blocked|deferred) · owned-files ·
  blocked-by · links(spec §, PR#, commit) · note`, plus a
  dependency-graph block.
- **finding:** `[ID] description · kind(spec-divergence |
  design-OPEN→Design | FOUND-bug | doc-gap) · spec § ·
  status(open | routed | resolved→link) · recommendation`.
- **milestone:** theme + the list of queue/finding IDs mapped to it.

IDs are stable and cross-referenced (queue ↔ findings ↔ milestones ↔
audit), so an item is named once and referenced everywhere else.

## Generated views + generator

A generator `tools/scripts/gen-tracking-views.sh` (sibling of
`gen-changelog.sh`) renders `ROADMAP.md` and `STATUS.md` from the hub:

- Modes: default (write), `--stdout` (preview), `--check` (read-only;
  exits non-zero if a generated file is stale).
- Each generated file carries a `DO NOT EDIT — generated from
  tracking/; run tools/scripts/gen-tracking-views.sh` header.
- `--check` is wired into CI as a **read-only** step (no write-back,
  no token, no push) — identical security posture to the changelog
  generator. This is what makes the human-facing docs unable to drift.

## Drift-prevention model

- **One owner, one location:** the orchestrator owns `tracking/`;
  it is repo-co-located, diffs in PRs, and is read at every block
  start. No off-repo blind tracker can re-emerge.
- **"Repo wins":** at block start the orchestrator reconciles the hub
  against repo state and fixes divergence in the hub. This is the
  discipline that was missing pre-agentic; it is already in the
  orchestrator contract and becomes explicit here.
- **Generated human docs** (`ROADMAP.md`, `STATUS.md`) are CI-`--check`ed
  → they cannot drift from the SoT.
- **Engineer sessions never write `tracking/`.** They surface OPEN/
  FOUND in PR descriptions; the orchestrator folds them into the hub.

## One-time reconciliation + migration (first implementation step)

Seed the hub from repo-truth plus the implementation-chat inventory,
applying "repo wins":

1. **Reconcile** every impl-chat item and its 8 reconciliation flags
   against repo state. Most already landed this session (Block 1
   #73–#76, D3 #90, D6 via #82, §20.1 #92, the agentic migration);
   the residue becomes hub items.
2. **Resolve / verify the residue:** check Item A (build-output
   filename) and Item E (LSP `--stdio`) for branches/PRs (flag 5);
   map the still-open D1-refresh / D2-FOUND spec design-decisions
   (flag 3); changelog hygiene (flag 1 — 0515 factual error, flag 2 —
   K04 date alignment, flag 7 — timestamp drift); confirm and remove
   the `tracking.md` duplicate (flag 8).
3. **Migrate** `docs/INVENTORY.md` + `docs/SPEC-ALIGNMENT.md` content
   into `findings.md` (and `queue.md` where actionable); delete those
   files and `tracking/tracking.md`.
4. **Build** `gen-tracking-views.sh`, generate `ROADMAP.md` +
   `STATUS.md`, and wire `--check` into CI.

This lands as one or a few PRs (sequenced so the hub is populated
before the old files are deleted and before the generator overwrites
the root docs).

## Success criteria

- A single forward-looking SoT lives in `tracking/`; `ROADMAP.md` and
  `STATUS.md` are generated and CI-`--check`-clean.
- No duplicate or scattered work trackers remain; `tracking.md`,
  `docs/INVENTORY.md`, `docs/SPEC-ALIGNMENT.md` are gone.
- The implementation-chat inventory is fully reconciled against the
  repo (repo wins); each of its 8 flags is resolved or queued as a
  hub item.
- `tracking/CLAUDE.md` documents the hub model and the "repo wins"
  reconciliation rule.

## Out of scope / non-goals

- **GitHub Issues migration.** Public bug/feature intake via Issues
  remains a post-launch concern; not part of this consolidation.
- **Resolving the spec design-decisions themselves.** Open
  D1-refresh / D2-FOUND items become findings/OPENs in the hub,
  routed to Design — this design centralizes their tracking, it does
  not decide them.
- **Code `TODO/FIXME` and `branding/` inventories.** Left in place;
  they may be referenced from the hub but are not migrated.
