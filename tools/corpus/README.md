# Synthetic Corpus Pipeline

Generates a fine-tuning-grade dataset of **verified** triples —
*(Bock source ↔ its five target outputs ↔ expected behavior)* — from the
conformance fixtures under `compiler/tests/conformance/`. This implements the
synthetic-corpus workstream from the 2026-06-09 design audit (R3(2)/R-A): the
compiler plus the conformance suite can mass-produce training data for Bock
that no one else can generate, with the ~824 passing fixture×target pairs as
the seed.

## ⚠ Publication gate (OQ2)

**The generated corpus must NOT be committed or published.** Whether the
corpus (and the context pack) ships as an open artifact is an **operator-gated
open question — OQ2** in `tracking/designs/2026-06-09-design-audit.md`. Until
the operator resolves OQ2, only the *pipeline* lives in the repo. The output
directory (`tools/corpus/out/`) is gitignored to enforce this; do not work
around it.

## Running

```bash
# 1. Build the CLI (the generator shells out to it):
cargo build -p bock --bin bock

# 2. Generate (from anywhere; paths are self-resolving):
tools/corpus/generate.py
```

Output lands in `tools/corpus/out/`:

- `corpus.jsonl` — one record per fixture (schema below), sorted by id.
- `manifest.json` — provenance + run statistics: counts by kind, per-target
  ok/failed pair counts, skipped fixtures with reasons, verification failures,
  and warnings (e.g. unrecognized `// EXPECT:` directives).

The `bock` binary is resolved from `--bock-bin`, else `$BOCK_BIN`, else
`$CARGO_TARGET_DIR/debug/bock`, else `compiler/target/debug/bock`. Useful
flags: `--targets js,python` (subset), `--jobs N` (parallelism, default = CPU
count), `--out-dir DIR`, `--repo-root DIR`.

Exit status is `0` only for a fully-clean run; any fixture load failure,
transpile failure, or diagnostic-verification failure in a harness-enforced
category (see below) exits `1` (the corpus and manifest are still written,
with failures marked in place — nothing is silently dropped). Warnings —
unrecognized `// EXPECT:` directives and diagnostic drift in categories the
harness does not enforce — do not fail the run but always appear in both the
manifest and the console summary.

## What "verified" means

Every record is checked at generation time; the verification depends on the
fixture's `kind`:

| kind | declared via | verification at generation time | runtime authority |
|---|---|---|---|
| `execution` | `// EXPECT: output "..."` | `bock build -t <t> --source-only` must succeed for every allowed target (runs the full frontend) | for fixtures under `exec/`, the conformance execution suite (`tools/scripts/run-conformance.sh`) runs these programs on all five toolchains in CI and pins stdout to `expected.output`; the few output-declaring fixtures in other dirs (`interp/`, `stdlib/*`, `time/`) are pinned by their owning crate tests, not the cross-target suite |
| `static` | `// EXPECT: no_errors` (or no expectation) | per-target build must succeed — proves the source checks clean and transpiles | n/a (no runtime claim) |
| `diagnostic` | `// EXPECT: error E<code> at <l>:<c>` | `bock check <fixture>` must **fail** and surface every declared code and `line:col` (same assertions as `compiler/tests/execution.rs`); the captured compiler output is embedded | n/a (invalid-by-design source; not transpiled) |

Diagnostic verification **scope** mirrors the harness. `execution.rs` drives
only `conformance/effects/` and `conformance/types-diagnostics/` through
`bock check`, so declared-vs-actual drift in those two categories fails the
run (corpus and CI would disagree). A diagnostic fixture anywhere else
carries a declaration **no CI test enforces**; if the live compiler disagrees
with it, the pipeline emits a manifest warning and keeps the record with
`diagnostic.verified: false` instead of failing — visible, never silently
dropped. Consumers should filter on `diagnostic.verified` regardless; the
embedded `diagnostic.output` is always the live compiler's actual response.

Consumers wanting only positive transpilation pairs should filter per-target
entries on `status == "ok"`. Diagnostic records are negative examples:
"this source is invalid Bock and the compiler says *exactly this*" — also
training-grade, deliberately included.

## Record schema (`schema_version: 1.0.0`)

One JSON object per line in `corpus.jsonl`:

```jsonc
{
  "schema_version": "1.0.0",
  "id": "exec/hello_world",                  // fixture path sans .bock, unique
  "test_name": "exec_hello_world",           // the fixture's `// TEST:` name
  "category": "exec",                        // top-level conformance dir
  "fixture_path": "compiler/tests/conformance/exec/hello_world.bock",
  "kind": "execution",                       // "execution" | "static" | "diagnostic"

  "source": {
    "entry_path": "main.bock",               // how the build laid out the entry
    "entry": "module main\n\nfn main() ...", // entry-module Bock source
                                             // (directives stripped, doc comments kept)
    "aux_files": [                           // `// FILE:` sections of multi-file
      { "path": "helper.bock", "content": "module helper\n..." }
    ]
  },

  "expected": {
    "output": "hello world",                 // expected stdout (execution), else null
    "no_errors": false,                      // declared `no_errors`
    "errors": [                              // declared diagnostics (diagnostic kind)
      { "code": "E0205", "line": 3, "col": 10 }
    ],
    "allowed_targets": ["js","ts","python","rust","go"],  // `targets` directive, default all
    "unrecognized_directives": []            // EXPECT values neither we nor the harness parse
  },

  // Per-target transpilation, keyed by target id; only allowed targets appear.
  // Empty object for diagnostic records.
  "targets": {
    "js": {
      "status": "ok",                        // "ok" | "build_failed"
      "files": [                             // emitted tree under build/js/
                                             // (sourcemaps *.map excluded)
        { "path": "main.js", "content": "..." }
      ]
      // on "build_failed": "stderr" (tail of compiler output) instead of "files"
    },
    "ts": { "...": "..." }, "python": { "...": "..." },
    "rust": { "...": "..." }, "go": { "...": "..." }
  },

  // Only on diagnostic records: the live `bock check` evidence.
  "diagnostic": {
    "command": "bock check type_mismatch.bock",
    "exit_code": 1,
    "output": "error[E0205]: ... at 3:10 ...",
    "verified": true                         // all declared codes + locations surfaced
  },

  "bock_commit": "<git sha the corpus was generated from>"
}
```

Notes:

- The emitted `targets.<t>.files` are the complete per-module native trees
  `bock build --source-only` writes (entry `main.<ext>` — `src/main.rs` for
  rust — plus reached module files and the shared runtime, e.g.
  `_bock_runtime.py`). Sourcemaps are excluded as noise.
- `expected.output` comparison semantics match the conformance harness:
  trailing newlines are insignificant.
- Diagnostic fixtures' `line:col` refer to the fixture file **as on disk**
  (including the directive comment lines), which is why `diagnostic` evidence
  is collected against `fixture_path`, not the stripped `source.entry`.
- Records are deterministic for a given commit + compiler binary except
  `manifest.json`'s `generated_at` timestamp.

## How it relates to the conformance harness

The directive grammar (`// TEST:` / `// EXPECT:` `no_errors` / `error E<code>
at <l>:<c>` / `output "..."` / `targets a, b` / multi-file `// FILE:`
sections) is parsed exactly as `compiler/tests/harness/` parses it. If the
harness grammar grows a directive, mirror it in `generate.py` — the
`unrecognized_directives` field will flag the gap in the meantime. Likewise,
if `execution.rs` wires a new conformance directory through `bock check`,
add its category to `HARNESS_WIRED_DIAGNOSTIC_CATEGORIES` in `generate.py`
so drift there becomes run-failing again.
