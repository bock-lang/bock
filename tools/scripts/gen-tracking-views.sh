#!/usr/bin/env bash
# Regenerate ROADMAP.md and STATUS.md from the tracking/ hub.
#
# USAGE
#   tools/scripts/gen-tracking-views.sh            # write ROADMAP.md + STATUS.md
#   tools/scripts/gen-tracking-views.sh --check    # exit non-zero if either
#                                                  # generated file is stale
#                                                  # (read-only); for CI
#   tools/scripts/gen-tracking-views.sh --stdout   # print both, write nothing
#
# WHAT IT DOES
#   ROADMAP.md and STATUS.md are GENERATED views over the tracking/ hub. The
#   hub is the source of truth; these two files must never be hand-edited.
#   This script is the canonical regenerator — a maintainer/orchestrator runs
#   it and lands the result through the normal human-reviewed PR flow. CI only
#   runs `--check` (read-only): it regenerates into a temp file and diffs, and
#   never writes a ruleset-protected `main` (impossible + a supply-chain risk).
#
#   ROADMAP.md  = header + `# Roadmap` + the milestone body of
#                 `tracking/milestones.md` (everything from its first `## `
#                 heading to EOF — drops that file's H1 + generated-from note).
#
#   STATUS.md   = header + `# Status` + a live queue summary (a count of items
#                 per `## ` section in `tracking/queue.md`, where each item is
#                 a `- **[ID] ...` line) + the body of `tracking/snapshot.md`
#                 (everything from its first `## ` heading to EOF). Every byte
#                 of STATUS thus comes from the hub.
#
#   Idempotent: running it twice produces no diff.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
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

ROADMAP="ROADMAP.md"
STATUS="STATUS.md"
MILESTONES="tracking/milestones.md"
SNAPSHOT="tracking/snapshot.md"
QUEUE="tracking/queue.md"

for f in "$MILESTONES" "$SNAPSHOT" "$QUEUE"; do
  if [ ! -f "$f" ]; then
    echo "error: $f not found (run from anywhere in the repo)" >&2
    exit 1
  fi
done

HEADER='<!-- DO NOT EDIT — generated from tracking/ by tools/scripts/gen-tracking-views.sh -->'

# A private temp workspace, removed on exit (covers the case where a render
# function aborts under `set -e` before its own cleanup runs).
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Emit ROADMAP.md to stdout.
render_roadmap() {
  printf '%s\n\n# Roadmap\n\n' "$HEADER"
  # Milestone body: from the first `## ` heading to EOF. This drops
  # milestones.md's own H1 + "the one question" / generated-from note block.
  sed -n '/^## /,$p' "$MILESTONES"
}

# Emit the queue summary block (one `- <section>: <n>` line per `## ` section
# in queue.md, counting `- **[ID] ...` item lines within each section). The
# format is section-based (## Ready / ## v1-blocking / ## Blocked / ##
# Deferred), so the summary is derived structurally rather than from any
# status(...) literal. Sections with zero items (e.g. the trailing
# "## Dependency graph") are skipped.
render_queue_summary() {
  printf '## Active work\n\n'
  printf 'Live summary derived from `tracking/queue.md` (items per section):\n\n'
  awk '
    # On each `## ` heading, flush the previous section (only if it actually
    # held items — this drops appendix sections like "## Dependency graph"
    # that carry no `- **[ID]` items), then start the new section.
    /^## / {
      if (section != "" && count > 0) { printf "- %s: %d\n", section, count }
      section = substr($0, 4)
      count = 0
      next
    }
    /^- \*\*\[/ { count++ }
    END {
      if (section != "" && count > 0) { printf "- %s: %d\n", section, count }
    }
  ' "$QUEUE"
}

# Emit STATUS.md to stdout.
render_status() {
  printf '%s\n\n# Status\n\n' "$HEADER"
  render_queue_summary
  printf '\n'
  # Snapshot body: from the first `## ` heading to EOF (present-state facts).
  sed -n '/^## /,$p' "$SNAPSHOT"
}

# write_or_check TARGET RENDERFN
#   write : overwrite TARGET (idempotent — no-op message if already current)
#   check : diff TARGET against a fresh render; non-zero + message if stale
#   stdout: print the fresh render
# Returns non-zero only on a --check staleness mismatch.
write_or_check() {
  local target="$1" renderfn="$2" tmp
  tmp="$(mktemp "$WORK/view.XXXXXX")"
  "$renderfn" > "$tmp"
  case "$MODE" in
    stdout)
      cat "$tmp"
      return 0
      ;;
    check)
      if diff -u "$target" "$tmp" >/dev/null 2>&1; then
        echo "$target is in sync with tracking/."
        return 0
      else
        echo "error: $target is stale (out of sync with tracking/)." >&2
        echo "       run tools/scripts/gen-tracking-views.sh and commit the result." >&2
        diff -u "$target" "$tmp" >&2 || true
        return 1
      fi
      ;;
    write)
      if [ -f "$target" ] && diff -u "$target" "$tmp" >/dev/null 2>&1; then
        echo "$target already up to date; no change."
      else
        cp "$tmp" "$target"
        echo "$target regenerated from tracking/."
      fi
      return 0
      ;;
  esac
}

rc=0
write_or_check "$ROADMAP" render_roadmap || rc=1
write_or_check "$STATUS"  render_status  || rc=1
exit "$rc"
