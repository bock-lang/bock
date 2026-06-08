#!/usr/bin/env bash
# stdlib fmt-check gate — assert the embedded core stdlib stays `bock fmt`-clean.
#
# WHY THIS EXISTS
# ---------------
# The `stdlib/core/**/*.bock` sources are compiled and embedded into the `bock`
# binary. Now that `bock fmt` emits valid Bock (#119), these files are normalised
# once and this gate keeps them from drifting: it runs `bock fmt --check` over the
# gated set and fails (exit 1) if any file would be reformatted.
#
# WHY A SCRIPT (NOT JUST `bock fmt --check`)
# ------------------------------------------
# `bock fmt` has no path/include/exclude flags — it recursively scans the current
# directory. Run from the repo root it would also walk `examples/`, conformance
# fixtures, etc. — far more than the stdlib we want to gate. So we stage exactly
# the gated files into a temp tree (preserving the relative layout `bock fmt`
# reports) and run the check there.
#
# EXCLUDED FILES
# --------------
#   (none)
#
# Historically two files were excluded for OPEN `bock-fmt` bugs:
#   stdlib/core/collections/collections.bock
#   stdlib/core/iter/iter.bock
# Both contain `match` arms whose body is a bare control-flow statement (e.g.
# `None => break`); `bock fmt` used to rewrite those to `None => break,` — a
# trailing comma the Bock parser rejected with E2020 "expected expression,
# found `,`". `collections.bock` additionally panicked the formatter on its long
# box-drawing divider comments (a UTF-8 byte-vs-char-boundary slice bug in
# `wrap_long_lines`/`find_break_point`). Both bugs are fixed (Q-bockfmt-cfarm-comma,
# Q-bockfmt-utf8-panic); the files were reformatted and folded back into the gate.
#
# ENVIRONMENT
#   BOCK_BIN   Path to a prebuilt `bock` binary (skips `cargo build`).
#
# EXIT STATUS
#   0  every gated file is already `bock fmt`-clean.
#   1  at least one gated file would be reformatted (drift).
#   2  setup error (no bock binary, no gated files found).

set -euo pipefail

# ── Resolve repo root from this script's location (tools/scripts/<this>) ──────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
STDLIB_DIR="$REPO_ROOT/stdlib"

# Files excluded from the gate (OPEN bock-fmt bugs — see header). Paths are
# relative to the repo root. Currently empty: the two previously-excluded files
# (iter.bock, collections.bock) are fixed and folded back into the gate.
EXCLUDE=()
is_excluded() {
  local rel="$1"
  # `${EXCLUDE[@]+...}` guards against `set -u` tripping on an empty array
  # (bash < 4.4 treats an unset-or-empty array expansion as an unbound var).
  for e in ${EXCLUDE[@]+"${EXCLUDE[@]}"}; do
    [[ "$rel" == "$e" ]] && return 0
  done
  return 1
}

# ── Locate / build the bock binary ────────────────────────────────────────────
BOCK="${BOCK_BIN:-}"
if [[ -z "$BOCK" ]]; then
  echo "== building bock (cargo build -p bock --bin bock) =="
  if ! cargo build --manifest-path "$REPO_ROOT/Cargo.toml" -p bock --bin bock; then
    echo "ERROR: failed to build bock" >&2
    exit 2
  fi
  # CARGO_TARGET_DIR may be redirected (per-branch session caches); ask cargo.
  TARGET_DIR="$(cargo metadata --manifest-path "$REPO_ROOT/Cargo.toml" --format-version 1 2>/dev/null \
    | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
  TARGET_DIR="${TARGET_DIR:-$REPO_ROOT/target}"
  BOCK="$TARGET_DIR/debug/bock"
fi
if [[ ! -x "$BOCK" ]]; then
  echo "ERROR: bock binary not found/executable at: $BOCK" >&2
  exit 2
fi

# ── Stage the gated files into a temp tree, preserving relative layout ─────────
NS="${BOCK_TEST_NAMESPACE:-stdlib-fmt-check-$$}"
STAGE="$(mktemp -d "/tmp/${NS}-stdlib-fmt-check.XXXXXX")"
trap 'rm -rf "$STAGE"' EXIT

gated=0
excluded=0
while IFS= read -r -d '' f; do
  rel="${f#"$REPO_ROOT"/}"
  if is_excluded "$rel"; then
    excluded=$((excluded + 1))
    continue
  fi
  dest="$STAGE/${rel#stdlib/}"
  mkdir -p "$(dirname "$dest")"
  cp "$f" "$dest"
  gated=$((gated + 1))
done < <(find "$STDLIB_DIR" -name '*.bock' -print0 | sort -z)

if [[ "$gated" -eq 0 ]]; then
  echo "ERROR: no gated .bock files found under $STDLIB_DIR" >&2
  exit 2
fi

echo "== stdlib fmt-check =="
echo "bock:     $BOCK"
echo "gated:    $gated file(s)"
echo "excluded: $excluded file(s) (OPEN bock-fmt bugs — see script header)"
echo

# ── Run the check from the staged tree (bock fmt scans cwd recursively) ───────
# `bock fmt --check` exits 1 (and lists "Would reformat: <path>") on any drift,
# 0 when every file is already clean. Surface its output verbatim.
if ( cd "$STAGE" && "$BOCK" fmt --check ); then
  echo
  echo "stdlib fmt-check: clean."
  exit 0
else
  rc=$?
  echo
  echo "stdlib fmt-check: FAILED — run \`bock fmt\` inside stdlib/ to fix, then commit." >&2
  echo "(paths above are shown relative to a staging dir; the real files live under stdlib/.)" >&2
  exit "$rc"
fi
