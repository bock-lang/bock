#!/usr/bin/env bash
# Run the Bock conformance suite.
#
# This wraps the two halves of the conformance harness that live in the
# `bock-test-harness` crate (compiler/tests):
#
#   1. Directive tests — parse the `// TEST:` / `// EXPECT:` directives on every
#      fixture under compiler/tests/conformance/ and assert they load and check
#      as declared (the harness lib unit tests).
#
#   2. Execution tests — for every fixture carrying `// EXPECT: output "..."`,
#      compile it with `bock build -t <target> --source-only` and *run* the
#      emitted program on each installed target toolchain (js/ts/python/rust/go),
#      comparing trimmed stdout to the expected output. Targets whose toolchain
#      is absent are skipped and reported, not failed.
#
# Environment:
#   BOCK_CONFORMANCE_REQUIRE   Comma-separated target ids (or `all`) that must be
#                              present; an absent required target fails the run.
#                              Intended for CI lanes that install toolchains.
#                              Example: BOCK_CONFORMANCE_REQUIRE=js,python,rust
#
# Exit status is non-zero if any directive or execution test fails.

set -euo pipefail

# Resolve the repo root from this script's location (tools/scripts/<this>).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
cd "$REPO_ROOT"

echo "== Bock conformance suite =="
echo "repo: $REPO_ROOT"
if [[ -n "${BOCK_CONFORMANCE_REQUIRE:-}" ]]; then
  echo "required targets: ${BOCK_CONFORMANCE_REQUIRE}"
else
  echo "required targets: none (missing toolchains are skipped)"
fi
echo

# Force a fresh `bock` binary so the execution tests don't reuse a stale sibling
# binary with an out-of-date embedded stdlib. bock-cli/build.rs declares
# rerun-if-changed on the stdlib tree, but a freshly-added nested subdir/module
# isn't in the tracked file set from the previous build, so cargo can skip
# re-running it. Touching the build script forces a re-walk + re-embed; the
# rebuild refreshes the sibling binary that execution.rs::bock_binary() runs.
echo "-- forcing fresh bock build (busts stale embedded-stdlib binary) --"
touch compiler/crates/bock-cli/build.rs
cargo build -p bock --bin bock
echo

# 1) Directive / parsing conformance (harness lib unit tests).
echo "-- directive + parsing conformance --"
cargo test -p bock-test-harness --lib

echo
# 2) Cross-target execution conformance. `--nocapture` surfaces the per-run
#    pass/skip/fail summary printed by the test.
echo "-- cross-target execution conformance --"
cargo test -p bock-test-harness --test execution -- --nocapture

echo
echo "== conformance suite passed =="
