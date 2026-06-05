#!/usr/bin/env bash
# Performance-regression measure — wall-times of the heavy build/test phases.
#
# WHY THIS EXISTS
# ---------------
# Compile + test time is the dominant cost of every CI run and every local
# edit/build/test loop, and it creeps upward silently: a new dependency, a
# monomorphisation blow-up, or a fixture added to the conformance suite each add
# seconds nobody notices until a green run takes ten minutes. This script puts a
# coarse, repeatable number on the three heaviest phases so a regression shows up
# as a signal instead of a vibe.
#
# It is INFORMATIONAL by default (exits 0 and just prints) so it can land as a
# non-blocking gate. A baseline file (tools/perf-baseline.txt) records the
# measured wall-times; strict mode compares this run's times against the baseline
# and fails if any phase is slower than baseline * tolerance. Flipping the gate
# to *blocking* later is a one-flag change (see RATCHET below).
#
# RATIOS, NOT ABSOLUTES
# ---------------------
# Absolute milliseconds are meaningless across machines and noisy even on one
# (a shared CI runner's wall-time varies run to run). The strict check therefore
# compares the *ratio* current/baseline against a tolerance, never an absolute
# threshold. The DOMINANT signal is the conformance-execution test time: it is
# the largest single cost and the one most sensitive to codegen/fixture growth.
# Build/clippy times are recorded too but are noisier (cache state dependent).
#
# PHASES MEASURED
#   build-clean    cargo build -p bock --bin bock          (after cargo clean -p bock)
#   build-incr     cargo build -p bock --bin bock          (immediate rebuild, no-op)
#   clippy         cargo clippy --workspace --all-targets
#   conf-exec      cargo test -p bock-test-harness --test execution  (DOMINANT)
#
# For conf-exec the script prefers libtest's own "finished in Ns" figure (the
# pure test-execution time, excluding compile) when it can be parsed from the
# output, and falls back to the wall-clock of the whole `cargo test` invocation
# otherwise. Both are recorded; the "finished in" figure is the comparison key
# because it is the most stable (compile time is cache-dependent).
#
# ENVIRONMENT
#   BOCK_PERF_UPDATE_BASELINE   If "1", (re)write tools/perf-baseline.txt from
#                               this run's measurements instead of comparing.
#                               Use on a known-good machine/commit to re-baseline.
#   BOCK_PERF_TOLERANCE         STRICT-mode ratio tolerance (default 1.5). A phase
#                               is a regression when current > baseline * tolerance.
#   BOCK_PERF_STRICT            If "1", enable strict mode: exit non-zero on any
#                               phase regression vs baseline. Default (unset/0) is
#                               informational: print only, always exit 0.
#   BOCK_PERF_PHASES            Comma/space list to restrict measured phases
#                               (default: build-clean build-incr clippy conf-exec).
#
# EXIT STATUS
#   0  informational mode (default), OR strict mode with no regressions.
#   1  strict mode (BOCK_PERF_STRICT=1) with >=1 phase over baseline*tolerance.
#   2  setup error (cargo missing, measurement failed, etc.).
#
# OUT OF SCOPE (follow-up): a criterion micro-benchmark corpus on the hot
# compiler paths (lexer / parser / typecheck / codegen) would give stable
# per-operation numbers immune to CI scheduling noise. That needs a dedicated
# `benches` crate added to the cargo workspace (a manifest change) and is left to
# a separate PR; this script measures whole-phase wall-times only.

set -uo pipefail

# ── Resolve repo root from this script's location (tools/scripts/<this>) ──────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
BASELINE_FILE="$REPO_ROOT/tools/perf-baseline.txt"
MANIFEST="$REPO_ROOT/Cargo.toml"

ALL_PHASES="build-clean build-incr clippy conf-exec"
PHASES_RAW="${BOCK_PERF_PHASES:-}"
if [[ -n "$PHASES_RAW" ]]; then
  PHASES="${PHASES_RAW//,/ }"
else
  PHASES="$ALL_PHASES"
fi

UPDATE_BASELINE="${BOCK_PERF_UPDATE_BASELINE:-0}"
STRICT="${BOCK_PERF_STRICT:-0}"
TOLERANCE="${BOCK_PERF_TOLERANCE:-1.5}"

if ! command -v cargo >/dev/null 2>&1; then
  echo "ERROR: cargo not found on PATH" >&2
  exit 2
fi

# ── Helpers ───────────────────────────────────────────────────────────────────

# now_ms: monotonic-ish wall-clock in integer milliseconds.
now_ms() {
  # %s%3N is GNU date; portable enough for ubuntu-latest + this repo's machines.
  date +%s%3N
}

# fmt_s: render a millisecond integer as seconds with two decimals (e.g. 12.34).
fmt_s() {
  local ms="$1"
  awk -v ms="$ms" 'BEGIN { printf "%.2f", ms / 1000.0 }'
}

# ratio: current_ms / baseline_ms to two decimals (baseline 0 => "n/a").
ratio() {
  local cur="$1" base="$2"
  awk -v c="$cur" -v b="$base" 'BEGIN {
    if (b + 0 <= 0) { print "n/a"; exit }
    printf "%.2f", c / b
  }'
}

# over_tolerance: 1 if cur > base * tol (both ms), else 0. base<=0 => 0 (no data).
over_tolerance() {
  local cur="$1" base="$2" tol="$3"
  awk -v c="$cur" -v b="$base" -v t="$tol" 'BEGIN {
    if (b + 0 <= 0) { print 0; exit }
    print (c + 0 > (b + 0) * (t + 0)) ? 1 : 0
  }'
}

# ── Measurement results (parallel assoc arrays keyed by phase id) ─────────────
declare -A MEASURED_MS   # MEASURED_MS[phase] = wall-time in ms (comparison key)
declare -A NOTE          # NOTE[phase]        = human note (e.g. "clean", "no-op")

# measure: run a phase command, record its wall-time. Args: phase, note, cmd...
# The command's stdout/stderr is suppressed (we only want timing); a non-zero
# exit is recorded as a measurement failure (ms=-1) but never aborts the script.
measure() {
  local phase="$1" note="$2"; shift 2
  local start end
  start="$(now_ms)"
  if "$@" >/dev/null 2>&1; then
    end="$(now_ms)"
    MEASURED_MS["$phase"]=$((end - start))
  else
    end="$(now_ms)"
    MEASURED_MS["$phase"]=$((end - start))
    note="$note (cmd exited non-zero)"
  fi
  NOTE["$phase"]="$note"
}

# measure_conf_exec: special-cased because we want libtest's own "finished in Ns"
# figure (pure execution, compile excluded) as the comparison key when available,
# falling back to wall-clock. Records the wall-clock under conf-exec-wall too.
measure_conf_exec() {
  local log start end wall_ms fin_s fin_ms
  log="$(mktemp "${TMPDIR:-/tmp}/${BOCK_TEST_NAMESPACE:-perf}-confexec.XXXXXX.log")"
  start="$(now_ms)"
  cargo test --manifest-path "$MANIFEST" -p bock-test-harness --test execution \
    >"$log" 2>&1
  end="$(now_ms)"
  wall_ms=$((end - start))

  # libtest prints e.g.: "test result: ok. 42 passed; ...; finished in 7.83s".
  # Sum every "finished in Ns" (there can be one per test binary) for the figure.
  fin_s="$(grep -oE 'finished in [0-9]+(\.[0-9]+)?s' "$log" \
    | grep -oE '[0-9]+(\.[0-9]+)?' \
    | awk '{ sum += $1 } END { if (NR > 0) printf "%.3f", sum; else print "" }')"
  rm -f "$log"

  if [[ -n "$fin_s" ]]; then
    fin_ms="$(awk -v s="$fin_s" 'BEGIN { printf "%d", s * 1000 }')"
    MEASURED_MS["conf-exec"]="$fin_ms"
    NOTE["conf-exec"]="libtest finished-in (wall ${wall_ms}ms incl. compile)"
  else
    # Could not parse — fall back to wall-clock (still a usable signal).
    MEASURED_MS["conf-exec"]="$wall_ms"
    NOTE["conf-exec"]="wall-clock (could not parse libtest finished-in)"
  fi
}

# ── Run requested phases ──────────────────────────────────────────────────────
echo "== perf-measure =="
echo "repo:      $REPO_ROOT"
echo "phases:    $PHASES"
if [[ "$STRICT" == "1" ]]; then
  echo "MODE:      STRICT (tolerance ${TOLERANCE}x) — regressions vs baseline FAIL the run"
else
  echo "MODE:      informational (always exits 0; regressions only warn)"
fi
echo

for phase in $PHASES; do
  case "$phase" in
    build-clean)
      echo "-- build-clean: cargo clean -p bock then cargo build -p bock --bin bock"
      cargo clean --manifest-path "$MANIFEST" -p bock >/dev/null 2>&1 || true
      measure build-clean "clean rebuild of bock bin" \
        cargo build --manifest-path "$MANIFEST" -p bock --bin bock
      ;;
    build-incr)
      echo "-- build-incr: cargo build -p bock --bin bock (immediate re-run, no-op)"
      # Ensure it's built first so this measures the up-to-date no-op path.
      cargo build --manifest-path "$MANIFEST" -p bock --bin bock >/dev/null 2>&1 || true
      measure build-incr "no-op incremental (already up to date)" \
        cargo build --manifest-path "$MANIFEST" -p bock --bin bock
      ;;
    clippy)
      echo "-- clippy: cargo clippy --workspace --all-targets"
      measure clippy "workspace, all targets" \
        cargo clippy --manifest-path "$MANIFEST" --workspace --all-targets
      ;;
    conf-exec)
      echo "-- conf-exec: cargo test -p bock-test-harness --test execution (DOMINANT)"
      measure_conf_exec
      ;;
    *)
      echo "WARN: unknown phase '$phase' — skipping" >&2
      ;;
  esac
done
echo

# ── Load baseline into BASELINE_MS[phase] ─────────────────────────────────────
# Format per non-comment line: <phase> <ms>  (whitespace-flexible).
declare -A BASELINE_MS
if [[ -f "$BASELINE_FILE" ]]; then
  while read -r ph ms _rest; do
    [[ -z "${ph:-}" ]] && continue
    BASELINE_MS["$ph"]="$ms"
  done < <(grep -v '^[[:space:]]*#' "$BASELINE_FILE" | awk 'NF>=2 {print $1, $2}')
fi

# ── Detect regressions (in the MAIN shell, so the array survives) ─────────────
# NB: this loop must NOT run inside a command substitution — a subshell's
# REGRESSIONS+=() mutations are discarded, which would silently neuter the gate.
REGRESSIONS=()
for phase in $PHASES; do
  cur="${MEASURED_MS[$phase]:-}"
  [[ -z "$cur" ]] && continue
  base="${BASELINE_MS[$phase]:-}"
  [[ -z "$base" ]] && continue
  if [[ "$(over_tolerance "$cur" "$base" "$TOLERANCE")" == "1" ]]; then
    REGRESSIONS+=("$phase: base=$(fmt_s "$base")s now=$(fmt_s "$cur")s ratio=$(ratio "$cur" "$base")x")
  fi
done

# ── Print the table ───────────────────────────────────────────────────────────
{
  printf '%-13s %10s %10s %8s  %s\n' "phase" "this(s)" "base(s)" "ratio" "note"
  printf '%-13s %10s %10s %8s  %s\n' "-----" "-------" "-------" "-----" "----"
  for phase in $PHASES; do
    cur="${MEASURED_MS[$phase]:-}"
    [[ -z "$cur" ]] && continue
    base="${BASELINE_MS[$phase]:-}"
    if [[ -n "$base" ]]; then
      r="$(ratio "$cur" "$base")"
      base_disp="$(fmt_s "$base")"
    else
      r="-"
      base_disp="-"
    fi
    flag=""
    if [[ -n "$base" && "$(over_tolerance "$cur" "$base" "$TOLERANCE")" == "1" ]]; then
      flag="  <== REGRESSION (> ${TOLERANCE}x)"
    fi
    printf '%-13s %10s %10s %8s  %s%s\n' \
      "$phase" "$(fmt_s "$cur")" "$base_disp" "$r" "${NOTE[$phase]:-}" "$flag"
  done
}
echo

# ── Baseline update mode ───────────────────────────────────────────────────────
if [[ "$UPDATE_BASELINE" == "1" ]]; then
  {
    echo "# perf-baseline — wall-times (milliseconds) of the heavy build/test phases."
    echo "# Generated on THIS machine; absolute numbers are machine-specific. The"
    echo "# strict gate compares RATIOS (current / baseline) against a tolerance, not"
    echo "# absolute ms, so a faster or slower machine re-baselines without churn."
    echo "# CI re-baselines on its own runners; do not treat these ms as portable."
    echo "#"
    echo "# Format: <phase>  <ms>   # note"
    echo "# Phases:"
    echo "#   build-clean  cargo build -p bock --bin bock after cargo clean -p bock"
    echo "#   build-incr   cargo build -p bock --bin bock no-op rebuild"
    echo "#   clippy       cargo clippy --workspace --all-targets"
    echo "#   conf-exec    cargo test -p bock-test-harness --test execution (DOMINANT;"
    echo "#                libtest 'finished in Ns' = pure exec, compile excluded)"
    echo "#"
    echo "# Regenerate: BOCK_PERF_UPDATE_BASELINE=1 bash tools/scripts/perf-measure.sh"
    echo "# Strict gate: BOCK_PERF_STRICT=1 [BOCK_PERF_TOLERANCE=1.5] bash .../perf-measure.sh"
    echo "#"
    for phase in $PHASES; do
      cur="${MEASURED_MS[$phase]:-}"
      [[ -z "$cur" ]] && continue
      printf '%-13s %8s   # %s\n' "$phase" "$cur" "${NOTE[$phase]:-}"
    done
  } > "$BASELINE_FILE"
  echo "Wrote baseline -> $BASELINE_FILE"
  echo
fi

# ── GitHub step summary (rendered in the Actions UI when running in CI) ────────
if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
  {
    echo "### perf-measure (informational)"
    echo
    echo "Wall-times of the heavy build/test phases. Comparison is by **ratio**"
    echo "(current / baseline), not absolute ms — CI runner wall-times are noisy."
    echo
    echo '| phase | this (s) | baseline (s) | ratio | note |'
    echo '|---|---|---|---|---|'
    for phase in $PHASES; do
      cur="${MEASURED_MS[$phase]:-}"
      [[ -z "$cur" ]] && continue
      base="${BASELINE_MS[$phase]:-}"
      if [[ -n "$base" ]]; then
        r="$(ratio "$cur" "$base")x"; base_disp="$(fmt_s "$base")"
      else
        r="-"; base_disp="-"
      fi
      flag=""
      [[ -n "$base" && "$(over_tolerance "$cur" "$base" "$TOLERANCE")" == "1" ]] \
        && flag=" ⚠️"
      printf '| %s | %s | %s | %s%s | %s |\n' \
        "$phase" "$(fmt_s "$cur")" "$base_disp" "$r" "$flag" "${NOTE[$phase]:-}"
    done
    echo
    echo "Dominant signal: **conf-exec** (conformance execution test time)."
    if [[ "${#REGRESSIONS[@]}" -gt 0 ]]; then
      echo
      echo "**Phases over ${TOLERANCE}x baseline:** ${#REGRESSIONS[@]}"
    fi
  } >> "$GITHUB_STEP_SUMMARY"
fi

# ── Regression reporting ───────────────────────────────────────────────────────
if [[ "${#REGRESSIONS[@]}" -gt 0 ]]; then
  echo "== phases over ${TOLERANCE}x baseline =="
  printf '  - %s\n' "${REGRESSIONS[@]}"
  echo
else
  echo "No phases over ${TOLERANCE}x baseline."
  echo
fi

# ── Exit decision ──────────────────────────────────────────────────────────────
if [[ "$STRICT" == "1" ]]; then
  if [[ "${#REGRESSIONS[@]}" -gt 0 ]]; then
    echo "STRICT mode: ${#REGRESSIONS[@]} phase(s) over ${TOLERANCE}x baseline — failing."
    exit 1
  fi
  echo "STRICT mode: no phase over ${TOLERANCE}x baseline."
  exit 0
fi

echo "informational mode — exit 0 regardless of timings."
exit 0
