# examples-matrix

A small, real tool written in Bock — the "one real tool written in Bock"
dogfood deliverable (queue item `Q-dogfood-tool`, R8 from the 2026-06-09
design audit). It renders the examples × targets support matrix from
`tools/examples-exec-baseline.txt`: a fixed-width text table, a Markdown
table ready to paste into issues/PRs, per-target pass/build-only rollups,
and overall totals. Malformed baseline lines are reported, never silently
dropped.

All parsing, aggregation, and rendering is pure Bock (`src/matrix.bock`);
`src/main.bock` is a thin entry point. Output is deterministic and
byte-identical across all five shipping targets.

## Design note: no file reads in Bock v1

Bock's v1 stdlib has no file-read surface (filesystem/stdin effects are
reserved for v1.x), so the tool cannot open the baseline file at runtime.
Instead, `sync-baseline.sh` snapshots the file verbatim into a generated
module (`src/baseline.bock`, `module baseline`) as a `List[String]` — one
element per line, tabs encoded as `\t`. The Bock code then does all the
real work on that data.

`src/baseline.bock` is **generated — do not edit by hand.**

## Regenerating the baseline snapshot

After `tools/examples-exec-baseline.txt` changes (e.g. a codegen cluster
lands and the ratchet is updated), re-run:

```bash
tools/examples-matrix/sync-baseline.sh
```

and commit the regenerated `src/baseline.bock` alongside.

## Building and running

`bock check` and `bock build` discover the project from `bock.project` in
this directory:

```bash
cd tools/examples-matrix
bock check                     # must be clean (no errors, no warnings)

bock build -t js      && (cd build/js     && node main.js)
bock build -t ts      && (cd build/ts     && tsc --noEmit -p tsconfig.json \
                                          && node --experimental-strip-types main.ts)
bock build -t python  && (cd build/python && python3 main.py)
bock build -t go      && (cd build/go     && go run .)
bock build -t rust    # see caveat below before `cargo run`
```

**Rust out-of-tree caveat** (same as `tools/scripts/examples-exec-audit.sh`):
the repo root is a cargo workspace, so a generated rust crate *inside* the
repo tree makes `cargo run` walk up, discover the root workspace, and fail.
Copy the project somewhere outside the repo (e.g. `/tmp`) before building
and running the rust target:

```bash
cp -r tools/examples-matrix /tmp/examples-matrix && cd /tmp/examples-matrix
rm -rf build .bock
bock build -t rust && (cd build/rust && cargo run -q)
```

All five targets print byte-identical reports.

## Dogfooding notes

Writing this tool surfaced several real codegen defects (rust ownership
clone-insertion gaps, a Python `pass` keyword collision on record fields,
Go lambda type erasure in `map`/`filter`, a Go identifier collision with
the runtime `Lines` helper, and unescaped `%` in Go's lowered `Sprintf`).
They are recorded as FOUND items with minimal repros in the PR that added
this tool. Where a reasonable idiom existed, the code here uses it (each
such spot carries a comment naming the defect it dodges); the defects
themselves remain compiler work, not worked around silently.
