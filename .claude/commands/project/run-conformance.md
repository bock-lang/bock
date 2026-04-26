# Run Conformance Suite

Execute the language conformance tests and report pass/fail/skip
counts by category.

## Steps

1. **Build the compiler in release mode** (faster execution):
   ```
   cargo build --release --bin bock
   ```

2. **Run the conformance harness:**
   ```
   ./tools/scripts/run-conformance.sh
   ```

   The script walks `compiler/tests/conformance/` and runs every
   `.bock` fixture against the freshly built `bock` binary,
   comparing output to the adjacent `.expected` file (if present).

3. **Categories.** Fixtures are organized by language area:
   ```
   compiler/tests/conformance/
     lexer/
     parser/
     types/
     effects/
     codegen-js/
     codegen-ts/
     codegen-py/
     codegen-rs/
     codegen-go/
     stdlib/
   ```

4. **Read the report.** Output format:
   ```
   category    pass  fail  skip  total
   lexer        42     0     0     42
   parser       87     2     1     90
   ...
   ```

5. **For any failures:**
   - Read the failing fixture and its `.expected` file.
   - Run the binary directly to see actual output:
     ```
     ./target/release/bock check compiler/tests/conformance/<path>
     ```
   - Decide: bug in compiler, or stale fixture? Update accordingly.

6. **For any skips:** check the `.skip` reason file. A long-skipped
   fixture is a roadmap signal — flag it in the session summary.

## Done When

- Full report printed
- Any new failures are root-caused (filed as issues or fixed)
- Skip count hasn't grown without a reason
