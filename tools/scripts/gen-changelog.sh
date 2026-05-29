#!/bin/bash
# Regenerate the `## Unreleased` section of CHANGELOG.md from git history.
#
# USAGE
#   tools/scripts/gen-changelog.sh            # rewrite CHANGELOG.md in place
#   tools/scripts/gen-changelog.sh --check    # exit 1 if Unreleased is stale
#                                             # (no write); for CI verification
#   tools/scripts/gen-changelog.sh --stdout   # print regenerated file, no write
#
# WHAT IT DOES
#   This is the CANONICAL way to update the Unreleased changelog. Git history
#   is the source of truth: CI never writes the changelog (writing to a
#   ruleset-protected `main` from CI is both impossible and a supply-chain
#   risk). Instead, a maintainer/orchestrator runs this and lands the result
#   through the normal human-reviewed pull-request flow.
#
#   The script:
#     1. Parses CHANGELOG.md and collects every PR number `(#NN)` already
#        recorded under a RELEASED section (any `## vX...` heading).
#     2. Walks first-parent history and collects each squash-merge subject
#        ending in `(#NN)` whose number is NOT already in a released section.
#        Those are the Unreleased entries.
#     3. Rewrites ONLY the `## Unreleased` block (header line + boilerplate
#        note + entries). Released sections and the file header are untouched.
#
#   Tag-independence: there are no git tags today, so the script walks full
#   history and relies on the released-PR set for its boundary. If a release
#   tag (`v*`) ever exists, the most recent one is used as the history base
#   for speed; the released-PR filter still guarantees correctness either way.
#
#   Boundary with the v0.0.1 baseline: the pre-squash era used
#   `Merge pull request #NN from ...` merge subjects. Those commits were
#   bulk-migrated and are summarized in the `## v0.0.1` section, so the
#   script deliberately matches ONLY the trailing `(#NN)` squash-merge
#   convention and ignores `Merge pull request #NN` subjects.
#
#   Tracking-PR filter: pure internal `tracking:`-prefixed PRs (queue/audit
#   coordination) are EXCLUDED from this consumer-facing changelog. Everything
#   else — feat/fix/spec/test/docs/chore — is included.
#
#   Idempotent: running it twice produces no diff.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

MODE="write"
case "${1:-}" in
  --check)  MODE="check" ;;
  --stdout) MODE="stdout" ;;
  "")       MODE="write" ;;
  *)
    echo "usage: $0 [--check|--stdout]" >&2
    exit 2
    ;;
esac

CHANGELOG="CHANGELOG.md"
if [ ! -f "$CHANGELOG" ]; then
  echo "error: $CHANGELOG not found (run from anywhere in the repo)" >&2
  exit 1
fi

# Pick a history base: the most recent v* tag if any exist, else walk full
# history. The released-PR filter is what guarantees correctness; the base is
# only an efficiency hint.
BASE=""
if LATEST_TAG="$(git describe --tags --abbrev=0 --match 'v*' 2>/dev/null)"; then
  BASE="$LATEST_TAG"
fi

# Collect first-parent subjects (newest first) into a temp file.
LOG_FILE="$(mktemp)"
OUT_FILE="$(mktemp)"
trap 'rm -f "$LOG_FILE" "$OUT_FILE"' EXIT
if [ -n "$BASE" ]; then
  git log --first-parent --pretty=%s "${BASE}..HEAD" >"$LOG_FILE"
else
  git log --first-parent --pretty=%s HEAD >"$LOG_FILE"
fi

CHANGELOG_PATH="$CHANGELOG" LOG_PATH="$LOG_FILE" python3 - >"$OUT_FILE" <<'PY'
import os
import re

changelog_path = os.environ["CHANGELOG_PATH"]
log_path = os.environ["LOG_PATH"]

with open(changelog_path, encoding="utf-8") as fh:
    text = fh.read()

lines = text.splitlines()

# Split the file into: header (everything before the first `## ` heading),
# then a sequence of sections each beginning with a `## ` heading.
header = []
sections = []  # list of (heading_line, [body_lines])
current = None
for line in lines:
    if line.startswith("## "):
        if current is not None:
            sections.append(current)
        current = [line, []]
    elif current is None:
        header.append(line)
    else:
        current[1].append(line)
if current is not None:
    sections.append(current)

# Boundary: PR numbers already recorded under any RELEASED (`## vX...`) section.
released_prs = set()
pr_re = re.compile(r"\(#(\d+)\)")
for heading, body in sections:
    if heading.startswith("## v"):
        for bl in body:
            for m in pr_re.finditer(bl):
                released_prs.add(int(m.group(1)))

# Tracking-PR filter: subjects whose conventional-commit prefix is `tracking`.
# Matches `tracking:` and `tracking(scope):`. Pure-internal, excluded.
tracking_re = re.compile(r"^tracking(\([^)]*\))?\s*:", re.IGNORECASE)
# Squash-merge convention: subject ends in `(#NN)`.
trailing_pr_re = re.compile(r"\(#(\d+)\)\s*$")

with open(log_path, encoding="utf-8") as fh:
    subjects = [s.rstrip("\n") for s in fh]

# Build Unreleased entries in newest-first order (git log order), skipping
# already-released PRs and tracking PRs. De-dup on PR number.
entries = []
seen = set()
for subject in subjects:
    m = trailing_pr_re.search(subject)
    if not m:
        continue  # not a squash-merge subject (e.g. old `Merge pull request`)
    num = int(m.group(1))
    if num in released_prs or num in seen:
        continue
    if tracking_re.match(subject):
        continue
    seen.add(num)
    entries.append(f"- {subject}")

# Rebuild the Unreleased block. The boilerplate note documents provenance and
# is part of the regenerated block (so it is preserved idempotently).
NOTE = (
    "The Unreleased section is generated from merged-PR history by "
    "`tools/scripts/gen-changelog.sh`. It is regenerated and committed via "
    "normal pull requests (the maintainer/orchestrator syncs it during "
    "coordination and at release time). CI never writes the changelog."
)
unreleased_body = ["", f"_{NOTE}_", ""]
unreleased_body.extend(entries)
if entries:
    unreleased_body.append("")

# Reassemble: header, Unreleased (regenerated), then all released sections
# verbatim. If no Unreleased heading existed, insert one before the first
# released section.
out = []
out.extend(header)

wrote_unreleased = False
for heading, body in sections:
    if heading.strip() == "## Unreleased":
        out.append("## Unreleased")
        out.extend(unreleased_body)
        wrote_unreleased = True
    else:
        out.append(heading)
        out.extend(body)

if not wrote_unreleased:
    # Insert a fresh Unreleased block before the first released section.
    rebuilt = list(header)
    inserted = False
    for heading, body in sections:
        if not inserted and heading.startswith("## v"):
            rebuilt.append("## Unreleased")
            rebuilt.extend(unreleased_body)
            inserted = True
        rebuilt.append(heading)
        rebuilt.extend(body)
    if not inserted:
        rebuilt.append("## Unreleased")
        rebuilt.extend(unreleased_body)
    out = rebuilt

# Normalize trailing blank lines to a single terminating newline.
while out and out[-1] == "":
    out.pop()
result = "\n".join(out) + "\n"
print(result, end="")
PY

case "$MODE" in
  stdout)
    cat "$OUT_FILE"
    ;;
  check)
    if diff -u "$CHANGELOG" "$OUT_FILE" >/dev/null; then
      echo "CHANGELOG.md Unreleased section is up to date."
    else
      echo "error: CHANGELOG.md Unreleased section is stale." >&2
      echo "       run tools/scripts/gen-changelog.sh and commit the result." >&2
      diff -u "$CHANGELOG" "$OUT_FILE" >&2 || true
      exit 1
    fi
    ;;
  write)
    if diff -u "$CHANGELOG" "$OUT_FILE" >/dev/null; then
      echo "CHANGELOG.md already up to date; no change."
    else
      cp "$OUT_FILE" "$CHANGELOG"
      echo "CHANGELOG.md Unreleased section regenerated."
    fi
    ;;
esac
