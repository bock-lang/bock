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
inconsistently; OPEN/FOUND findings live in multiple places; deferred
work is split across STATUS, ROADMAP, and the queue; and there is a
literal duplicate queue.

**Key insight:** the drift came from *duplication + an off-repo
tracker + no single owner* — not from having multiple files. The fix
is therefore not "fewer files" but **granular, single-purpose files
with explicit non-overlapping boundaries, one owner, all in-repo.**

## Decisions

1. **Scope — one planning hub.** A single in-repo source of truth
   owns all *forward-looking* state: work items, spec-vs-impl
   divergences, open design questions, milestones, and the current
   project snapshot. Only `audit.md` (history), `CHANGELOG.md` (change
   records), and GitHub Issues (public intake, post-launch) stay
   separate.
2. **Medium — in-repo markdown under `tracking/`**, orchestrator-owned.
   The SoT travels with the code, diffs in PRs, and is what the
   orchestrator already reads at every block start. This directly
   removes the off-repo blind-tracker failure mode.
3. **Structure — a granular set of single-purpose files + generated
   human views.** Each hub file answers exactly one question; human-
   facing `ROADMAP.md` and `STATUS.md` are *generated from* the hub so
   they cannot drift. `STATUS.md` is **fully generated**.

## Architecture

### Hub files — canonical SoT (orchestrator-owned; one purpose each)

| File | Owns (one question) | Absorbs |
|------|---------------------|---------|
| `tracking/queue.md` | What work is to-be / being done? (actionable items) | the live queue + STATUS "Deferred Items" + actionable doc-gaps |
| `tracking/divergences.md` | Where does the implementation differ from the spec, and what's the disposition? | `docs/SPEC-ALIGNMENT.md` |
| `tracking/design-questions.md` | What design decisions are open and need Design? | the open D1-refresh / D2-FOUND rows + OPENs |
| `tracking/milestones.md` | What ships in which version? | `ROADMAP.md` content |
| `tracking/snapshot.md` | What is the current project state? (build/test, phase history, migration notes) | `STATUS.md` static content |

### Unchanged (each already single-purpose)

| File | Owns |
|------|------|
| `tracking/audit.md` | What happened / was decided? (append-only log + digests) |
| `tracking/escalations.md` | What needs the human's decision? |
| `tracking/routing.md` | How is work routed / conflict-avoided? |
| `tracking/CLAUDE.md` | What are the conventions + each file's boundary? (updated — see below) |

### Retired (content migrated, then files removed)

- `tracking/tracking.md` — the stale duplicate queue. **Deleted.**
- `docs/SPEC-ALIGNMENT.md` — migrated into `divergences.md` (open
  rows) / `design-questions.md` (undecided rows), then **deleted**.
- `docs/INVENTORY.md` — still-actionable doc gaps become `queue.md`
  items (type `docs`); the one-time inventory matrix is **deleted**.
  (Both were already slated for deletion under the D5 phase.)

### Generated at repo root (DO NOT hand-edit)

- `ROADMAP.md` — rendered from `tracking/milestones.md`.
- `STATUS.md` — rendered from `tracking/snapshot.md` plus a live
  active/blocked/deferred summary derived from `tracking/queue.md`.
  All STATUS content originates in the hub — nothing is hand-authored
  in the generated file.

## File boundaries (the rule that prevents re-fragmentation)

`tracking/CLAUDE.md` gains a **boundaries table** that states, for
each file, the one question it owns and the disambiguation rule for
the fuzzy edges:

- **Actionable → `queue.md`.** Anything someone will *do* (impl, spec
  edit, docs, chore, bugfix) is a queue item — including FOUND-bugs
  once triaged and actionable doc gaps. Raw FOUND/OPEN tags live
  transiently in PR descriptions; the orchestrator triages them into
  the right hub file.
- **`divergences.md` vs `design-questions.md`.** A *divergence* is a
  factual mismatch ("spec §X says A, impl does B") with a disposition
  (reconcile-spec / fix-impl / accept). A *design-question* is an
  *undecided* choice ("what should the behavior be?") needing a Design
  call. A divergence whose disposition requires a decision *links to*
  a design-question; it is not duplicated.
- **`milestones.md`** holds only version→item mapping and themes, not
  item detail (detail lives in `queue.md`, referenced by ID).
- **`snapshot.md`** holds only present-state facts, never future work.

Every item has a stable ID and is **named once**, referenced by ID
elsewhere (queue ↔ divergences ↔ design-questions ↔ milestones ↔
audit). No item's content is duplicated across files.

## Item schemas

- **queue item:** `[ID] title — type(impl|spec|docs|chore|bug) ·
  status(ready|in-flight|blocked|deferred) · owned-files ·
  blocked-by · links(spec §, PR#, commit) · note` + a dependency-graph
  block.
- **divergence:** `[ID] spec § · spec-says / impl-does ·
  classification(spec-stale | spec-ahead-of-impl | impl-bug | gap) ·
  disposition(reconcile-spec→link | fix-impl→queue ID | accept) ·
  status(open | resolved→link)`.
- **design-question:** `[ID] question · spec § · context · options ·
  recommendation · status(open | routed-to-Design | decided→link)`.
- **milestone:** version · theme · list of queue/design-question IDs.

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
- **Explicit boundaries** (above) mean every item has exactly one
  home — no "same item in three files."
- **"Repo wins":** at block start the orchestrator reconciles the hub
  against repo state and fixes divergence in the hub.
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
   the residue becomes hub items in the right file.
2. **Resolve / verify the residue:** check Item A (build-output
   filename) and Item E (LSP `--stdio`) for branches/PRs (flag 5);
   map still-open D1-refresh / D2-FOUND rows into `design-questions.md`
   (flag 3); changelog hygiene → `queue.md` (flag 1 — 0515 factual
   error, flag 2 — K04 date alignment, flag 7 — timestamp drift);
   confirm and remove the `tracking.md` duplicate (flag 8).
3. **Migrate** `docs/SPEC-ALIGNMENT.md` → `divergences.md` /
   `design-questions.md`, and still-actionable `docs/INVENTORY.md`
   doc-gaps → `queue.md`; delete both docs files and `tracking.md`.
4. **Build** `gen-tracking-views.sh`, generate `ROADMAP.md` +
   `STATUS.md`, wire `--check` into CI, and update `tracking/CLAUDE.md`
   with the boundaries table.

Lands as a few small, reviewable PRs, sequenced so the hub is
populated before old files are deleted and before the generator
overwrites the root docs.

## Success criteria

- A single forward-looking SoT lives in `tracking/`, as granular
  single-purpose files with explicit boundaries; `ROADMAP.md` and
  `STATUS.md` are generated and CI-`--check`-clean.
- No duplicate or scattered work trackers remain; `tracking.md`,
  `docs/INVENTORY.md`, `docs/SPEC-ALIGNMENT.md` are gone.
- The implementation-chat inventory is fully reconciled against the
  repo (repo wins); each of its 8 flags is resolved or queued.
- `tracking/CLAUDE.md` documents the boundaries table and the
  "repo wins" reconciliation rule; every hub item has exactly one home.

## Out of scope / non-goals

- **GitHub Issues migration.** Public bug/feature intake via Issues
  remains a post-launch concern.
- **Resolving the spec design-decisions themselves.** Open rows become
  `design-questions.md` entries routed to Design — this centralizes
  their tracking, it does not decide them.
- **Code `TODO/FIXME` and `branding/` inventories.** Left in place;
  may be referenced from the hub but are not migrated.
