# Run Conformance Suite

Execute the language conformance tests — directive checks plus
cross-target execution — and report pass/skip/fail.

## What the suite is

The conformance harness lives in the `bock-test-harness` crate
(`compiler/tests/`) and has two halves:

1. **Directive tests** (harness lib unit tests). Every fixture under
   `compiler/tests/conformance/` carries `// TEST:` / `// EXPECT:`
   directives; these tests assert the fixtures load and that their
   declared expectations parse.

2. **Execution tests** (`compiler/tests/execution.rs`). Every fixture
   carrying `// EXPECT: output "..."` is compiled with
   `bock build -t <target> --source-only` and the emitted program is
   *run* on each installed target toolchain (js/ts/python/rust/go),
   comparing trimmed stdout to the expected output. Targets whose
   toolchain is absent are **skipped** and reported, not failed.

## Steps

1. **Run the suite via the wrapper script:**
   ```
   ./tools/scripts/run-conformance.sh
   ```
   It runs both halves and prints a per-run pass/skip/fail summary.

2. **Require specific toolchains (CI lanes).** To turn an absent
   toolchain into a hard failure instead of a skip, set the
   comma-separated `BOCK_CONFORMANCE_REQUIRE` (or `all`):
   ```
   BOCK_CONFORMANCE_REQUIRE=js,python,rust ./tools/scripts/run-conformance.sh
   ```

3. **Read the execution summary.** The execution test prints, e.g.:
   ```
   === conformance execution summary ===
     passed:  10 (exec_hello_world::js, ...)
     skipped: 0 (toolchain absent: none)
     failed:  0
   ```
   An all-skipped green run means no target toolchains were present —
   it is **not** real coverage. Check the skipped list.

## Fixtures

Execution fixtures live under `compiler/tests/conformance/exec/` in the
inline-directive format (directives at the top, then the program):

```
// TEST: exec_hello_world
// EXPECT: output "hello world"
module main

fn main() -> Void {
  println("hello world")
}
```

Other conformance categories (`parse/`, `types/`, `effects/`,
`context/`, `interp/`, `stdlib/`, `time/`) carry directive-only
fixtures checked by the directive tests.

## Investigating failures

- **Output mismatch.** The execution failure message prints the run
  command, the expected vs actual stdout, the exit code, and stderr.
  Reproduce by hand:
  ```
  mkdir -p /tmp/conf && cp compiler/tests/conformance/exec/<f>.bock /tmp/conf/main.bock
  (cd /tmp/conf && ./target/debug/bock build -t <target> --source-only)
  # then run build/<target>/main.<ext> with that target's toolchain
  ```
  Decide: codegen bug, or stale fixture/expectation? Fix accordingly.

- **Skips.** A skipped target just means its toolchain is not installed
  on this host. Install it, or run a lane with
  `BOCK_CONFORMANCE_REQUIRE` to force coverage.

## Done When

- The suite exits zero (or skips are understood and acceptable).
- Any new failures are root-caused (filed as issues or fixed).
- Skip count is explained (which toolchains are absent and why).
