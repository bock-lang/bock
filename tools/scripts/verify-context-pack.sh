#!/usr/bin/env bash
# Verify the Bock context pack against the real compiler.
#
# WHAT IT DOES
#   Extracts every fenced ```bock code block from
#   context-pack/BOCK-CONTEXT-PACK.md, writes each to its own scratch file,
#   and runs `bock check` on it. Exits non-zero if any block fails to check.
#
#   This is the pack's drift-guard: the pack's contract is that every
#   ```bock block is true of the current implementation. Intentionally-wrong
#   snippets in the pack use ```text fences and are not extracted.
#
# USAGE
#   tools/scripts/verify-context-pack.sh            # build bock, verify
#   BOCK_BIN=/path/to/bock tools/scripts/verify-context-pack.sh
#                                                   # reuse a prebuilt binary
#
# ENVIRONMENT
#   BOCK_BIN              Path to a prebuilt `bock` binary. If unset, the
#                         script builds `cargo build -p bock --bin bock`
#                         (honoring CARGO_TARGET_DIR) and uses that.
#   BOCK_TEST_NAMESPACE   Optional prefix for the /tmp scratch dir, matching
#                         the repo's concurrent-session convention.
#
# EXIT STATUS
#   0  every ```bock block in the pack passes `bock check`
#   1  at least one block failed (each failure is printed with its block
#      number, pack line number, and the compiler diagnostics)
#   2  setup error (pack missing, bock binary not found/buildable)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
PACK="$REPO_ROOT/context-pack/BOCK-CONTEXT-PACK.md"

if [[ ! -f "$PACK" ]]; then
  echo "error: pack not found at $PACK" >&2
  exit 2
fi

# ── Resolve the bock binary ───────────────────────────────────────────────────
BOCK="${BOCK_BIN:-}"
if [[ -z "$BOCK" ]]; then
  echo "==> Building bock (cargo build -p bock --bin bock)"
  (cd "$REPO_ROOT" && cargo build -p bock --bin bock --quiet)
  TARGET_DIR="${CARGO_TARGET_DIR:-$REPO_ROOT/target}"
  BOCK="$TARGET_DIR/debug/bock"
fi
if [[ ! -x "$BOCK" ]]; then
  echo "error: bock binary not found/executable at $BOCK" >&2
  exit 2
fi
echo "==> Using bock: $BOCK ($("$BOCK" --version))"

# ── Scratch dir ───────────────────────────────────────────────────────────────
NS="${BOCK_TEST_NAMESPACE:-ctxpack}"
WORK="$(mktemp -d "/tmp/${NS}-verify-context-pack.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

# ── Extract every ```bock block into $WORK/block_<n>.bock ────────────────────
# Records the pack line number each block starts at in block_<n>.line.
awk -v dir="$WORK" '
  /^```bock[ \t]*$/ && !inblock { inblock = 1; n += 1
    file = dir "/block_" sprintf("%03d", n) ".bock"
    print NR + 1 > (dir "/block_" sprintf("%03d", n) ".line")
    next }
  /^```[ \t]*$/ && inblock { inblock = 0; close(file); next }
  inblock { print > file }
  END { print n > (dir "/count") }
' "$PACK"

COUNT="$(cat "$WORK/count")"
if [[ "$COUNT" -eq 0 ]]; then
  echo "error: no \`\`\`bock blocks found in the pack — extraction broken?" >&2
  exit 2
fi
echo "==> Extracted $COUNT bock block(s) from ${PACK#"$REPO_ROOT"/}"

# ── Check each block ──────────────────────────────────────────────────────────
fail=0
pass=0
for f in "$WORK"/block_*.bock; do
  n="$(basename "$f" .bock)"
  line="$(cat "$WORK/$n.line")"
  if out="$(cd "$WORK" && "$BOCK" check --brief "$(basename "$f")" 2>&1)"; then
    pass=$((pass + 1))
  else
    fail=$((fail + 1))
    echo ""
    echo "FAIL: $n (pack line $line)"
    echo "$out" | sed 's/^/    /'
  fi
done

echo ""
echo "verify-context-pack: $pass passed, $fail failed, $COUNT total"
if [[ "$fail" -gt 0 ]]; then
  exit 1
fi
