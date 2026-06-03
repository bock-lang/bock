#!/usr/bin/env bash
# Examples exec audit — build + run every example project on every target.
#
# WHY THIS EXISTS
# ---------------
# The conformance suite (tools/scripts/run-conformance.sh) exercises hand-picked
# fixtures, which proved too narrow: real-world-pattern codegen bugs slipped past
# it because the 20 example projects under examples/ were never built+run across
# the five targets in CI. This script closes that gap: it transpiles AND runs
# every example × {js, ts, python, rust, go}, then prints a PASS/FAIL matrix.
#
# It is INFORMATIONAL by default (exits 0 even with failures) so it can land as a
# non-blocking signal while the codegen clusters it surfaces are still being
# fixed. A baseline file records the currently-passing (example, target) pairs;
# the script warns on regression vs the baseline. Flipping the gate to *blocking*
# later is a one-flag change (see RATCHET below).
#
# OUT-OF-TREE BUILD CAVEAT
# ------------------------
# Each example is copied to a temp dir OUTSIDE the repo before building. The
# repo root is a cargo workspace (members = compiler/crates/*); a rust project
# generated *inside* the repo tree makes `cargo run` walk up, discover that root
# workspace, and fail with a "current package believes it's in a workspace" error
# that masks the example's real rust status. Building under /tmp side-steps it.
#
# RUN COMMANDS (per target, cwd = build/<target>/)
#   js     -> node main.js
#   ts     -> node --experimental-strip-types main.ts   (Node >= 22 strips types natively)
#   python -> python3 main.py
#   rust   -> cargo run -q
#   go     -> go run .
#
# A target counts as PASS only if it BUILDS cleanly AND (when a runtime for it is
# present) RUNS to a zero exit. If the runtime is absent, the target is reported
# BUILD-ONLY (still counts toward the baseline as a build pass), not FAIL.
#
# ENVIRONMENT
#   BOCK_EXAMPLES_REQUIRE   RATCHET. Empty/unset (default) => informational, always
#                           exits 0. Set to a comma-separated list of target ids
#                           (e.g. "js,python") or "all" => the script exits NON-ZERO
#                           if any (example, target) pair in the required set is
#                           below its baseline status. This is the flag CI flips to
#                           make the gate blocking once a cluster of fixes lands.
#   BOCK_EXAMPLES_TARGETS   Comma-separated targets to audit (default: js,ts,python,rust,go).
#   BOCK_EXAMPLES_FILTER    Substring; only audit example paths containing it.
#   BOCK_EXAMPLES_UPDATE_BASELINE  If "1", rewrite the baseline file from this run's
#                           results instead of comparing. Use after a cluster lands.
#   BOCK_BIN                Path to a prebuilt `bock` binary (skips cargo build).
#
# EXIT STATUS
#   0  informational mode (default), OR strict mode with no regressions.
#   1  strict mode (BOCK_EXAMPLES_REQUIRE set) with >=1 regression vs baseline.
#   2  setup error (no bock binary, no examples found, etc.).

set -uo pipefail

# ── Resolve repo root from this script's location (tools/scripts/<this>) ──────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BASELINE_FILE="$REPO_ROOT/tools/examples-exec-baseline.txt"

ALL_TARGETS="js ts python rust go"
TARGETS="${BOCK_EXAMPLES_TARGETS:-}"
if [[ -n "$TARGETS" ]]; then
  TARGETS="${TARGETS//,/ }"
else
  TARGETS="$ALL_TARGETS"
fi

REQUIRE_RAW="${BOCK_EXAMPLES_REQUIRE:-}"
STRICT=0
REQUIRE_TARGETS=""
if [[ -n "$REQUIRE_RAW" ]]; then
  STRICT=1
  if [[ "$REQUIRE_RAW" == "all" ]]; then
    REQUIRE_TARGETS="$TARGETS"
  else
    REQUIRE_TARGETS="${REQUIRE_RAW//,/ }"
  fi
fi

FILTER="${BOCK_EXAMPLES_FILTER:-}"
UPDATE_BASELINE="${BOCK_EXAMPLES_UPDATE_BASELINE:-0}"

# ── Scratch namespace (honour the session convention; tolerate unset) ─────────
NS="${BOCK_TEST_NAMESPACE:-examples-exec-$$}"
WORK_ROOT="/tmp/${NS}-examples-exec"
rm -rf "$WORK_ROOT"
mkdir -p "$WORK_ROOT"
trap 'rm -rf "$WORK_ROOT"' EXIT

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
echo "bock: $BOCK"
echo

# ── Detect available runtimes (absent runtime => BUILD-ONLY, not FAIL) ────────
have() { command -v "$1" >/dev/null 2>&1; }
declare -A RUNTIME_OK
RUNTIME_OK[js]=$( have node && echo 1 || echo 0 )
RUNTIME_OK[ts]=$( have node && echo 1 || echo 0 )
RUNTIME_OK[python]=$( { have python3 || have python; } && echo 1 || echo 0 )
RUNTIME_OK[rust]=$( have cargo && echo 1 || echo 0 )
RUNTIME_OK[go]=$( have go && echo 1 || echo 0 )
PY=python3; have python3 || PY=python

# ── Discover examples ─────────────────────────────────────────────────────────
mapfile -t PROJECTS < <(find "$REPO_ROOT/examples" -name bock.project | sort)
if [[ -n "$FILTER" ]]; then
  filtered=()
  for p in "${PROJECTS[@]}"; do [[ "$p" == *"$FILTER"* ]] && filtered+=("$p"); done
  PROJECTS=("${filtered[@]}")
fi
if [[ "${#PROJECTS[@]}" -eq 0 ]]; then
  echo "ERROR: no examples found under $REPO_ROOT/examples (filter='$FILTER')" >&2
  exit 2
fi

# ── Load baseline into an assoc array: BASELINE["name|target"]=pass|build ─────
# Format per non-comment line: <example-path> <target> <status>  (whitespace-
# flexible; awk normalises it). Comment lines (leading #) are skipped.
declare -A BASELINE
if [[ -f "$BASELINE_FILE" ]]; then
  while read -r ex tg st; do
    [[ -z "${ex:-}" ]] && continue
    BASELINE["$ex|$tg"]="$st"
  done < <(grep -v '^[[:space:]]*#' "$BASELINE_FILE" | awk 'NF>=3 {print $1, $2, $3}')
fi

# ── Status helpers ────────────────────────────────────────────────────────────
# Per (example,target) we emit one of:
#   pass   -> built AND ran clean (or built clean with no runtime: BUILD-ONLY)
#   build  -> built clean but RUN failed
#   fail   -> build failed
# For baseline/ratchet purposes the ordering is fail < build < pass.
rank() { case "$1" in pass) echo 2;; build) echo 1;; *) echo 0;; esac; }

declare -A RESULT     # RESULT["name|target"] = pass|build|fail
declare -A RAN        # RAN["name|target"]    = ran|builtonly|skip(build-failed)
REGRESSIONS=()
NEW_PASSES=()

echo "== examples-exec audit =="
echo "targets:  $TARGETS"
echo "examples: ${#PROJECTS[@]}"
if [[ "$STRICT" -eq 1 ]]; then
  echo "MODE:     STRICT (require: $REQUIRE_TARGETS) — regressions vs baseline FAIL the run"
else
  echo "MODE:     informational (always exits 0; regressions only warn)"
fi
echo

run_target() {
  # $1 = build dir for the target, $2 = target id. Echoes nothing; returns 0/!0.
  local dir="$1" tg="$2"
  case "$tg" in
    js)     ( cd "$dir" && node main.js >/dev/null 2>&1 ) ;;
    ts)     ( cd "$dir" && node --experimental-strip-types main.ts >/dev/null 2>&1 ) ;;
    python) ( cd "$dir" && "$PY" main.py >/dev/null 2>&1 ) ;;
    rust)   ( cd "$dir" && cargo run -q >/dev/null 2>&1 ) ;;
    go)     ( cd "$dir" && go run . >/dev/null 2>&1 ) ;;
    *)      return 1 ;;
  esac
}

for proj in "${PROJECTS[@]}"; do
  proj_dir="$(dirname "$proj")"
  # Stable, readable name relative to examples/ (e.g. fundamentals/hello-world).
  name="${proj_dir#"$REPO_ROOT"/examples/}"
  slug="${name//\//-}"
  work="$WORK_ROOT/$slug"
  rm -rf "$work"; mkdir -p "$work"
  cp -r "$proj_dir/." "$work/"
  rm -rf "$work/build" "$work/.bock"

  printf '%-34s' "$name"
  for tg in $TARGETS; do
    key="$name|$tg"
    blog="$work/.build-$tg.log"
    # `bock build` discovers the project by scanning cwd for bock.project, so
    # the build must run with cwd = the copied example dir (in a subshell so the
    # caller's cwd is untouched).
    if ( cd "$work" && "$BOCK" build -t "$tg" ) >"$blog" 2>&1; then
      bdir="$work/build/$tg"
      if [[ "${RUNTIME_OK[$tg]}" == "1" ]]; then
        if run_target "$bdir" "$tg"; then
          RESULT["$key"]="pass"; RAN["$key"]="ran"; printf ' %-9s' "PASS"
        else
          RESULT["$key"]="build"; RAN["$key"]="ran"; printf ' %-9s' "run-FAIL"
        fi
      else
        # No runtime to execute against — credit the clean build.
        RESULT["$key"]="build"; RAN["$key"]="builtonly"; printf ' %-9s' "BUILD?"
      fi
    else
      RESULT["$key"]="fail"; RAN["$key"]="skip"; printf ' %-9s' "FAIL"
    fi

    # Baseline comparison (only meaningful when a baseline entry exists).
    base="${BASELINE[$key]:-}"
    cur="${RESULT[$key]}"
    if [[ -n "$base" ]]; then
      if [[ "$(rank "$cur")" -lt "$(rank "$base")" ]]; then
        # Don't count a regression caused solely by a missing runtime
        # (builtonly can't reach 'pass'); only when the build itself regressed
        # or a run that previously passed now fails with the runtime present.
        if [[ "${RAN[$key]}" != "builtonly" || "$cur" == "fail" ]]; then
          REGRESSIONS+=("$key: baseline=$base now=$cur")
        fi
      elif [[ "$(rank "$cur")" -gt "$(rank "$base")" ]]; then
        NEW_PASSES+=("$key: baseline=$base now=$cur")
      fi
    fi
  done
  printf '\n'
done

echo
echo "Legend: PASS=built+ran  run-FAIL=built but run errored  BUILD?=built (no runtime to run)  FAIL=build error"
echo

# ── Tallies ───────────────────────────────────────────────────────────────────
echo "== per-target tally (build-clean / total) =="
for tg in $TARGETS; do
  built=0; ran=0; total=0
  for proj in "${PROJECTS[@]}"; do
    proj_dir="$(dirname "$proj")"; name="${proj_dir#"$REPO_ROOT"/examples/}"
    key="$name|$tg"; total=$((total+1))
    case "${RESULT[$key]:-fail}" in
      pass)  built=$((built+1)); ran=$((ran+1));;
      build) built=$((built+1));;
    esac
  done
  printf '  %-7s build %2d/%2d   ran %2d/%2d\n' "$tg" "$built" "$total" "$ran" "$total"
done
echo

# ── Baseline update mode ───────────────────────────────────────────────────────
if [[ "$UPDATE_BASELINE" == "1" ]]; then
  {
    echo "# examples-exec baseline — currently-passing (example, target) pairs."
    echo "# Format: <example-path>  <target>  <status>   status in {pass, build}."
    echo "#   pass  = builds AND runs clean"
    echo "#   build = builds clean (run failed, or no runtime present at record time)"
    echo "# Regenerate: BOCK_EXAMPLES_UPDATE_BASELINE=1 tools/scripts/examples-exec-audit.sh"
    echo "# The strict ratchet (BOCK_EXAMPLES_REQUIRE=...) fails CI if any listed"
    echo "# pair drops below its recorded status. Update this file when a codegen"
    echo "# cluster lands so the ratchet can only tighten, never loosen silently."
    echo "#"
    for proj in "${PROJECTS[@]}"; do
      proj_dir="$(dirname "$proj")"; name="${proj_dir#"$REPO_ROOT"/examples/}"
      for tg in $TARGETS; do
        key="$name|$tg"; st="${RESULT[$key]:-fail}"
        [[ "$st" == "fail" ]] && continue
        printf '%s\t%s\t%s\n' "$name" "$tg" "$st"
      done
    done
  } > "$BASELINE_FILE"
  echo "Wrote baseline -> $BASELINE_FILE"
  echo
fi

# ── Regression / new-pass reporting ────────────────────────────────────────────
if [[ "${#NEW_PASSES[@]}" -gt 0 ]]; then
  echo "== improvements vs baseline (consider re-recording the baseline) =="
  printf '  + %s\n' "${NEW_PASSES[@]}"
  echo
fi

if [[ "${#REGRESSIONS[@]}" -gt 0 ]]; then
  echo "== REGRESSIONS vs baseline =="
  printf '  - %s\n' "${REGRESSIONS[@]}"
  echo
else
  echo "No regressions vs baseline."
  echo
fi

# ── GitHub step summary (rendered in the Actions UI when running in CI) ────────
if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  {
    echo "### examples-exec matrix"
    echo
    printf '| example |'; for tg in $TARGETS; do printf ' %s |' "$tg"; done; printf '\n'
    # NB: a literal '---|' as printf's *format* makes printf parse the leading
    # '--' as an option terminator ("invalid option"); pass it as an argument.
    printf '%s' '|---|'; for tg in $TARGETS; do printf '%s' '---|'; done; printf '\n'
    for proj in "${PROJECTS[@]}"; do
      proj_dir="$(dirname "$proj")"; name="${proj_dir#"$REPO_ROOT"/examples/}"
      printf '| %s |' "$name"
      for tg in $TARGETS; do
        key="$name|$tg"
        case "${RESULT[$key]:-fail}" in
          pass)  cell="✅";;
          build) [[ "${RAN[$key]:-}" == "builtonly" ]] && cell="🔨" || cell="⚠️";;
          *)     cell="❌";;
        esac
        printf ' %s |' "$cell"
      done
      printf '\n'
    done
    echo
    echo "Legend: ✅ built+ran · ⚠️ built, run failed · 🔨 built (no runtime) · ❌ build failed"
    if [[ "${#REGRESSIONS[@]}" -gt 0 ]]; then
      echo
      echo "**Regressions vs baseline:** ${#REGRESSIONS[@]}"
    fi
  } >> "$GITHUB_STEP_SUMMARY"
fi

# ── Exit decision ──────────────────────────────────────────────────────────────
if [[ "$STRICT" -eq 1 ]]; then
  # Only regressions within the required target set are fatal.
  fatal=0
  for r in "${REGRESSIONS[@]:-}"; do
    [[ -z "$r" ]] && continue
    rt="${r#*|}"; rt="${rt%%:*}"          # extract target id from "name|target: ..."
    for want in $REQUIRE_TARGETS; do
      [[ "$rt" == "$want" ]] && fatal=1
    done
  done
  if [[ "$fatal" -eq 1 ]]; then
    echo "STRICT mode: regression in required targets ($REQUIRE_TARGETS) — failing."
    exit 1
  fi
  echo "STRICT mode: no regressions in required targets ($REQUIRE_TARGETS)."
  exit 0
fi

echo "informational mode — exit 0 regardless of failures."
exit 0
