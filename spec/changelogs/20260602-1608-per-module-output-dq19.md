# Per-module native output is the v1 build layout (DQ19 resolved)

**Date:** 2026-06-02
**Affects:** §20.6.1 (Output Layout)
**Type:** breaking change (output layout) — reverses the 2026-05-30 single-file-bundling default

## Change

DQ19 is resolved (owner, 2026-06-02): the **per-module mirrored tree** specified by
§20.6.1 is the v1 build output for both application and library builds. A cross-module
program — one module `use`-ing another, including the embedded `core.*` stdlib — compiles
and runs by emitting **each reached module to its own target file** and wiring the files
with the target's **native** import mechanism:

- **js/ts** — ES module `import`/`export` between the emitted files
- **python** — package imports (`from <module> import <names>`)
- **rust** — `mod <m>;` + `use crate::<m>::<x>;` (a real Cargo crate)
- **go** — one Go module across files with package-path imports (a real `go.mod`)

The build artifact is then run through the target's normal runner (`node`, `python`,
`cargo run`, `go run .`) — not a single hand-invoked `main.<ext>`.

This **supersedes the transitional single-file bundling** introduced 2026-05-30
(changelog `2026-05-30-single-file-bundling.md`), which concatenated every `use`-reachable
module into one entry file. Bundling existed only because the run model and conformance
harness ran a single `main.<ext>` before native cross-file imports compiled+ran on each
target; it is retired as the default once native per-target imports run on all five targets.

## Rationale

§20.6.1's normative one-file-per-module layout was preserved over bundling because the
per-module tree is what makes Bock's output a real, idiomatic target-language project — the
prerequisite for project mode (§20.6.2), where users run `npm test` / `cargo test` /
`pytest` / `go test` over emitted files in the target's own toolchain. Bundling produced a
single opaque blob that, while runnable, is not a project a target-language developer would
recognize or integrate. Resolving DQ19 toward the per-module tree closes DV13 properly
(native cross-module execution) rather than routing around it.

## Migration

No source changes. Build output for a multi-module project becomes a per-module tree under
`build/<target>/` (mirroring `src/`) plus the minimum target manifest needed to run it
(`Cargo.toml`, `go.mod`, `package.json`), rather than a single bundled `build/<target>/main.<ext>`.

## Implementation sequencing

Realized by the ItemB milestone (`tracking/plans/2026-06-02-itemB-per-module-projectmode-plan.md`):
native per-target imports + a per-module-tree conformance run are delivered target-by-target
(pilot: python), keeping the bundling path behind a flag until all five run natively, then
bundling is removed (DV13 CLOSED).
