# Tracking Consolidation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Consolidate all forward-looking tracking into a single in-repo hub under `tracking/` (granular single-purpose files), with `ROADMAP.md` + `STATUS.md` generated from it, after reconciling the implementation-chat inventory against repo truth and retiring the duplicate/scattered trackers.

**Architecture:** Five canonical hub files under `tracking/` (`queue.md`, `divergences.md`, `design-questions.md`, `milestones.md`, `snapshot.md`), each owning one question, with a boundaries table in `tracking/CLAUDE.md`. A bash generator renders `ROADMAP.md` + `STATUS.md` from the hub; CI verifies they are in sync via a read-only `--check`. The stale `tracking/tracking.md` and the `docs/INVENTORY.md` / `docs/SPEC-ALIGNMENT.md` analysis docs are retired after their content migrates into the hub.

**Tech Stack:** Markdown; POSIX/bash (`gen-tracking-views.sh`, sibling of `tools/scripts/gen-changelog.sh`); GitHub Actions (read-only `--check` step in `ci.yml`).

**Spec:** `tracking/designs/2026-05-29-tracking-consolidation-design.md` (approved). Read it before executing.

**Execution note:** these are `tracking/` changes — the orchestrator owns them. Land each PR via the `chore/tracking-<UTC>` → PR → merge → re-sync flow (no direct commits to `main`). Engineer sub-agents may be used for PR 2 (the generator, a self-contained code artifact); PRs 1 and 3 are content/reconciliation work best done with orchestrator context.

---

## Reconciliation inputs (repo-verified this session — the executor inherits these facts)

**Already landed (do NOT re-queue):** Block 1 H3 #73 / H2 #74 / H1 #75 / C1 #76; D3 tooling reference #90; D6 changelog-generator #82; §20.1 reconciliation #92; Item A source-mirroring filenames #28; Item E LSP `--stdio` #26; the agentic migration (tracking/, orchestrator contract, operating model — in-repo and in use). Impl-chat reconciliation flags **4** (D3 done; D4 below) and **5** (A/E merged) and **6** (Block 1 landed; prompts were reconstructed) are resolved. Flag **8** (tracking.md duplicate) confirmed — retired in PR 3.

**Open residue → becomes hub items (the executor verifies + files these):**
- **queue.md (actionable):** D4 stdlib reference (blocked: stdlib empty); D5 → Item B (project-mode codegen, phased) → Item D chain; D2-polish; the §20.1 cross-ref doc-sync (§17.2/§15/§10.8/§10.4); changelog hygiene (flag 1 — 0515 factual-error disposition; flag 2 — K04 batch-changelog date-alignment; flag 7 — filename/content date-drift sweep across `spec/changelogs/`); F-conf (wire conformance execution + create `tools/scripts/run-conformance.sh`, referenced-but-missing in CLAUDE.md + `/project:run-conformance`); vscode test-infra (no test script/files); the `@performance` E8003 example fix.
- **design-questions.md (open, → Design):** the still-open D1-refresh (18-row) / D2-FOUND (7-row) spec decisions not yet resolved by the spec-revision artifact (flag 3 — map them); the parked `bock check` default-strictness question; the `@performance` example-vs-§11.4-syntax question.
- **divergences.md:** the **stdlib gap** (§18 presents core modules as v1; `stdlib/` is empty + the implementation is unscheduled) — classification `spec-ahead-of-impl`, disposition links to a milestones/queue decision; plus any still-open `docs/SPEC-ALIGNMENT.md` rows.
- **milestones.md:** v1.0/1.1/1.2/v2 from `ROADMAP.md`, with the **stdlib-implementation scheduling gap** captured (it currently has no milestone home).

---

## File Structure

- Create: `tracking/divergences.md`, `tracking/design-questions.md`, `tracking/milestones.md`, `tracking/snapshot.md`, `tools/scripts/gen-tracking-views.sh`
- Restructure: `tracking/queue.md` (to the new schema)
- Generate (overwrite): `ROADMAP.md`, `STATUS.md`
- Modify: `tracking/CLAUDE.md` (boundaries table), `.github/workflows/ci.yml` (add `--check` step), root `CLAUDE.md` ("Where to Find What" refs)
- Delete: `tracking/tracking.md`, `docs/INVENTORY.md`, `docs/SPEC-ALIGNMENT.md`

---

## PR 1 — Reconcile + seed the hub

Branch `chore/tracking-<UTC>`. Goal: create the four new hub files and restructure `queue.md`, populated from the reconciliation; the old files still exist (retired in PR 3) and `ROADMAP.md`/`STATUS.md` are untouched (regenerated in PR 2).

### Task 1.1: Build the reconciliation worksheet

- [ ] **Step 1: Map the impl-chat inventory + scattered files against the repo.** For each impl-chat item and the 8 flags, record landed-vs-open using the "Reconciliation inputs" above plus these verifications:
  - Flag 3 (D1-refresh/D2-FOUND): `ls spec/changelogs/2026051*` and read the spec-revision artifact `spec/changelogs/20260514-0548-spec-revision-artifact.md`; for each of the 25 decision rows, mark resolved (changelog exists) vs open. The open set seeds `design-questions.md`.
  - Flag 2 + 7 (changelog dates): `for f in spec/changelogs/2026*.md; do echo "$f"; grep -m1 '^\*\*Date:\*\*' "$f"; done` and flag any filename-date ≠ content-`Date:` mismatch → a `queue.md` hygiene item.
  - Flag 1 (0515 error): confirm `spec/changelogs/20260513-0515*.md` still contains the unparseable handler example → a `queue.md` item with the disposition decision (leave/annotate/replace).
- [ ] **Step 2: Verify no double-counting.** Cross-check the residue against `tracking/queue.md` and `audit.md` so nothing already-done is re-queued.
- [ ] **Step 3: Record the worksheet** inline in the PR description (not a committed file — it is transient reconciliation output).

### Task 1.2: Create `tracking/divergences.md`

- [ ] **Step 1: Create the file** with this header + schema + the migrated rows:

```markdown
# Divergences — spec ↔ implementation

Where the implementation differs from the spec, with disposition.
One question: *where does impl differ from spec, and what do we do?*
Each row has a stable ID; actionable fixes link to a queue item; spec
reconciliations link to the changelog. NOT a work list (see queue.md)
and NOT for undecided behavior (see design-questions.md).

Schema: `[ID] spec § · spec-says / impl-does · classification
(spec-stale | spec-ahead-of-impl | impl-bug | gap) ·
disposition(reconcile-spec→link | fix-impl→queue ID | accept) ·
status(open | resolved→link)`

## Open

### DV1 — stdlib core modules unimplemented
- **Spec §:** §18.3 · **spec-says:** core.* modules ship in v1 ·
  **impl-does:** `stdlib/` empty (0 modules); prelude = ~9 builtins
- **Classification:** spec-ahead-of-impl
- **Disposition:** schedule stdlib implementation (see milestones MS-stdlib /
  queue Q-stdlib); reconcile §18 v1-status once scheduled
- **Status:** open

## Resolved

<!-- migrate the resolved rows from docs/SPEC-ALIGNMENT.md here, each
linking to its changelog/PR (e.g. §20.1 → #92, §1.5 → #73). -->
```

- [ ] **Step 2: Migrate** the still-relevant `docs/SPEC-ALIGNMENT.md` rows: open factual mismatches → `## Open`; already-reconciled ones (§20.1 #92, §1.5 #73, etc.) → `## Resolved` with links. Undecided-behavior rows go to `design-questions.md` instead (Task 1.3), not here.
- [ ] **Step 3: Verify** every SPEC-ALIGNMENT row has a destination (divergences, design-questions, or "resolved"): no row dropped.

### Task 1.3: Create `tracking/design-questions.md`

- [ ] **Step 1: Create the file** with header + schema:

```markdown
# Design Questions — open decisions for Design

Undecided behavior/semantics awaiting a Design call. One question:
*what should the behavior be?* NOT factual mismatches (see
divergences.md) and NOT actionable work (see queue.md).

Schema: `[ID] question · spec § · context · options · recommendation ·
status(open | routed-to-Design | decided→link)`

## Open

### DQ1 — bock check default strictness
- **Question:** should `bock check` default to `bock.project` strictness
  rather than requiring explicit `--strict`?
- **§:** §20.1 · **context:** O1/O2 landed #87 keeping `--strict` explicit
  (matches `bock build`). · **status:** open

### DQ2 — @performance budget literal syntax
- **Question:** should `@performance(max_latency: 100, ...)` accept bare
  ints, or must they be unit-suffixed (100.ms)? · **§:** §11.4 ·
  **context:** context-audit example uses bare ints → E8003. · **status:** open
```

- [ ] **Step 2: Add** the still-open D1-refresh / D2-FOUND rows from Task 1.1's worksheet (one DQ each, with the spec § + the decision needed). Mark any already-resolved-by-spec-revision as `decided→<changelog>` and move to a `## Decided` section.

### Task 1.4: Create `tracking/milestones.md`

- [ ] **Step 1: Create the file** by migrating `ROADMAP.md`'s milestone content (v1.0/1.1/1.2/v2 themes + bullets), reformatted to: `version · theme · [item IDs]`. Header:

```markdown
# Milestones — what ships when

Version → theme → mapped item IDs (detail lives in queue.md /
design-questions.md, referenced by ID — milestones holds mapping only).
ROADMAP.md is GENERATED from this file; do not edit ROADMAP.md by hand.
```

- [ ] **Step 2: Capture the stdlib scheduling gap** — add an explicit `MS-stdlib` line under the milestone where the core stdlib implementation should sit (or a "Unscheduled — needs a milestone decision" subsection), cross-referencing DV1 + the queue stdlib item. This is the gap the audit surfaced.

### Task 1.5: Create `tracking/snapshot.md`

- [ ] **Step 1: Create the file** with the static project-state content currently in `STATUS.md` (build/test snapshot, "What Works Today", phase history, migration notes), refreshed to current `main`. Header:

```markdown
# Snapshot — current project state

Present-state facts only (build/test status, what works, phase history,
migration notes). NO future work (see queue.md/milestones.md). STATUS.md
is GENERATED from this file plus a queue summary; do not edit STATUS.md.
```

- [ ] **Step 2: Update the build/test line** to current `main` (run `cargo test --workspace 2>&1 | tail -1` for the count; note CI is green).

### Task 1.6: Restructure `tracking/queue.md`

- [ ] **Step 1: Rewrite** `queue.md` to the schema (`[ID] title — type · status · owned-files · blocked-by · links · note` + dependency graph), seeded with the actionable residue from Task 1.1: D4, D5, Item B (phased), Item D, D2-polish, the §20.1 cross-ref doc-sync, the three changelog-hygiene items (flags 1/2/7), F-conf (+ run-conformance.sh), vscode test-infra, the @performance example fix, and a `Q-stdlib` placeholder linked to DV1/MS-stdlib. Mark statuses honestly (most `blocked`/`ready`).
- [ ] **Step 2: Remove** the "Recently landed" / digest-style content (that's audit.md's job) and any rows now represented as divergences/design-questions — queue holds only actionable work.

### Task 1.7: Verify + commit + PR

- [ ] **Step 1: Cross-reference check.** `grep -oE '\b(Q|DV|DQ|MS)[0-9-]+' tracking/*.md` — every referenced ID exists in its owning file.
- [ ] **Step 2: Commit** the four new files + restructured queue.md.

```bash
git add tracking/divergences.md tracking/design-questions.md tracking/milestones.md tracking/snapshot.md tracking/queue.md
git commit -m "tracking: seed the consolidated hub (queue/divergences/design-questions/milestones/snapshot)"
```

- [ ] **Step 3: Open PR**, with the reconciliation worksheet (Task 1.1) in the body. Orchestrator merges after review.

---

## PR 2 — Generator + generated views + CI check

Branch off updated `main` (after PR 1). Goal: `gen-tracking-views.sh` renders `ROADMAP.md` + `STATUS.md` from the hub; CI verifies via `--check`.

### Task 2.1: Write `tools/scripts/gen-tracking-views.sh`

**Files:** Create `tools/scripts/gen-tracking-views.sh` (chmod +x).

- [ ] **Step 1: Write the generator.** It renders two files by composition (the hub files are authored so generation is extract-and-assemble, not heavy parsing):

```bash
#!/usr/bin/env bash
# Regenerate ROADMAP.md and STATUS.md from the tracking/ hub.
# Usage: gen-tracking-views.sh [--check|--stdout]
#   (default) write ROADMAP.md + STATUS.md
#   --check   exit non-zero if either generated file is stale (read-only)
#   --stdout  print both to stdout, write nothing
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"
MODE="${1:-write}"

HEADER='<!-- DO NOT EDIT — generated from tracking/ by tools/scripts/gen-tracking-views.sh -->'

render_roadmap() {
  printf '%s\n\n# Roadmap\n\n' "$HEADER"
  # milestones.md body, minus its H1 + the leading "generated-from" note block.
  sed -n '/^## /,$p' tracking/milestones.md
}

render_status() {
  printf '%s\n\n# Status\n\n' "$HEADER"
  # Live queue summary computed from queue.md status fields.
  printf '## Active work\n\n'
  for s in in-flight ready blocked deferred; do
    n="$(grep -coE "status\($s" tracking/queue.md || true)"
    printf -- '- %s: %s\n' "$s" "$n"
  done
  printf '\n'
  # snapshot.md body (present-state facts).
  sed -n '/^## /,$p' tracking/snapshot.md
}

write_or_check() {  # $1=target $2=renderfn
  tmp="$(mktemp)"; "$2" > "$tmp"
  if [ "$MODE" = "--check" ]; then
    if ! diff -q "$1" "$tmp" >/dev/null 2>&1; then
      echo "STALE: $1 is out of sync with tracking/. Run tools/scripts/gen-tracking-views.sh" >&2
      rm -f "$tmp"; return 1
    fi
  elif [ "$MODE" = "--stdout" ]; then cat "$tmp"
  else mv "$tmp" "$1"; return 0
  fi
  rm -f "$tmp"
}

rc=0
write_or_check ROADMAP.md render_roadmap || rc=1
write_or_check STATUS.md  render_status  || rc=1
exit $rc
```

- [ ] **Step 2: `chmod +x tools/scripts/gen-tracking-views.sh`.**

### Task 2.2: Generate + verify idempotency

- [ ] **Step 1: Run it.** `bash tools/scripts/gen-tracking-views.sh` → writes `ROADMAP.md` + `STATUS.md`.
- [ ] **Step 2: Verify idempotent.** Run again; `git diff --quiet ROADMAP.md STATUS.md` → no change. Then `bash tools/scripts/gen-tracking-views.sh --check` → exits 0.
- [ ] **Step 3: Sanity-check** the rendered `ROADMAP.md`/`STATUS.md` read correctly (headers present, milestone + status content sensible). `shellcheck tools/scripts/gen-tracking-views.sh` if available.

### Task 2.3: Wire `--check` into CI (read-only)

**Files:** Modify `.github/workflows/ci.yml`.

- [ ] **Step 1: Add a job** (mirror the changelog-check pattern; `actions/checkout` SHA-pinned as elsewhere, `contents: read`, no token):

```yaml
  tracking-views:
    name: tracking views in sync
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd # v6.0.2
      - run: bash tools/scripts/gen-tracking-views.sh --check
```

- [ ] **Step 2: Validate** the workflow YAML (`actionlint` via a `$HOME` copy — it is snap-confined and cannot read `/opt`).

### Task 2.4: Commit + PR

- [ ] **Step 1:**

```bash
git add tools/scripts/gen-tracking-views.sh ROADMAP.md STATUS.md .github/workflows/ci.yml
git commit -m "tracking: generate ROADMAP.md + STATUS.md from the hub; CI --check"
```

- [ ] **Step 2: Open PR;** confirm its CI runs the new `tracking-views` check green.

---

## PR 3 — Retire old files + conventions

Branch off updated `main` (after PR 2). Goal: delete the retired trackers (content already migrated) and document the boundaries.

### Task 3.1: Reference sweep before deletion (D5 discipline)

- [ ] **Step 1:** `grep -rn "INVENTORY.md\|SPEC-ALIGNMENT.md\|tracking/tracking.md" --include='*.md' . | grep -v node_modules` — list every reference.
- [ ] **Step 2:** Update each surviving reference to point at the hub (notably root `CLAUDE.md` "Where to Find What" and any `docs/` cross-links). Replace `docs/INVENTORY.md`/`SPEC-ALIGNMENT.md` mentions with `tracking/` equivalents.

### Task 3.2: Delete the retired files

- [ ] **Step 1:** `git rm tracking/tracking.md docs/INVENTORY.md docs/SPEC-ALIGNMENT.md`.
- [ ] **Step 2:** Confirm nothing in `docs/src/SUMMARY.md` referenced them (mdbook would break otherwise).

### Task 3.3: Boundaries table in `tracking/CLAUDE.md`

- [ ] **Step 1:** Add a "Hub file boundaries" section to `tracking/CLAUDE.md` with the per-file one-question table and the disambiguation rules (actionable→queue; mismatch→divergences; undecided→design-questions; mapping→milestones; present-state→snapshot; ROADMAP.md/STATUS.md generated) — copy the rules from the design spec's "File boundaries" section. State that `ROADMAP.md`/`STATUS.md` are generated and must not be hand-edited.

### Task 3.4: Verify + commit + PR

- [ ] **Step 1:** `mdbook build docs` succeeds (no dangling links from the deletions); `bash tools/scripts/gen-tracking-views.sh --check` still exits 0.
- [ ] **Step 2:**

```bash
git add -A
git commit -m "tracking: retire duplicate/scattered trackers; document hub boundaries"
```

- [ ] **Step 3: Open PR.** After merge, the consolidation is complete.

---

## Self-review checklist (run before execution)
- Spec coverage: every spec section (hub files, schemas, boundaries, generated views, drift-prevention, reconciliation, success criteria) maps to a task above. ✓
- Placeholder scan: hub-file *content* is reconciliation-derived (Task 1.1 produces it); templates + schemas + the known residue are concrete. The generator, CI yaml, and commands are complete.
- Type/name consistency: file names (`divergences.md`/`design-questions.md`/`milestones.md`/`snapshot.md`), the generator name, and the CI job name are consistent across tasks.
