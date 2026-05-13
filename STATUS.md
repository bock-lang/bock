# Status

**Last updated:** 2026-05-13
**Phase:** M.4 complete — big-bang migration landed.

## Build Status

| What                        | State           |
| --------------------------- | --------------- |
| `cargo check --workspace`   | Clean           |
| `cargo test --workspace`    | 2209 / 0 / 26 (passed/failed/ignored) |
| Conformance fixtures        | 12 imported, parseable |
| `bock check` on examples    | 20 / 20 exit 0 (matching aura-dev baseline) |
| VS Code extension           | typechecks, esbuild + vsce package OK |
| Docs (mdBook)               | builds clean    |

CI badges will land once GitHub Actions runs against the migration
branch on `main`:

- `main` build: TBD
- Conformance suite: TBD
- Docs deploy: TBD

## What Works Today

- **Compiler pipeline end-to-end.** All 17 crates from the prior
  `aura-dev` tree have been imported under `bock-*` names. The CLI
  binary `bock` exposes `new`, `check`, `build`, `run`, `test`,
  `fmt`, `doc`, `pkg`, `repl`, `cache`.
- **Targets.** Codegen for JS, TS, Python, Rust, Go is live and
  exercised by the example projects.
- **Type system.** Bidirectional inference, generics, trait-style
  constraints, effect inference.
- **Conformance suite.** 12 fixtures across `effects/`, `interp/`,
  `parse/`, `time/`, `types/` under `compiler/tests/conformance/`,
  with the harness as `compiler/tests/` workspace member.
- **Examples.** 20 example projects across `fundamentals/`,
  `real-world/`, `spec-exercisers/`, `target-optimized/`.
- **Spec.** Primary spec at `spec/bock-spec.md`; historical
  changelogs in `spec/changelogs/` with Aura naming preserved as
  historical record.
- **VS Code extension.** Builds to a working `.vsix` (1003 KB).
  Vocabulary at `extensions/vscode/assets/vocab.json` is regenerated
  from the compiler via `tools/scripts/sync-vocab.sh`.
- **Documentation.** mdBook skeleton at `docs/` with intro,
  getting-started, language-guide stubs, and reference index.
- **Website.** Single-page landing at `website/`.

## Phase History

- **Phase A — Foundation Lock.** Complete in aura-dev pre-migration.
  Checker, interpreter, and codegen aligned. Zero open FC/FG bugs.
- **Phase B — Module System.** Complete. Cross-file imports through
  module registry; project marker `bock.project`.
- **Phase C — Effect Codegen.** Complete. Effect set lowering per
  target.
- **Phase D — Generics.** Complete. Generic functions, types, and
  trait-style constraints.
- **Phase E — Stdlib Bridging.** Complete. Shared method registry
  between checker and `bock-core`.
- **Phase F — AI Pipeline.** Complete. `bock-ai` provider abstraction
  with Anthropic and OpenAI-compatible drivers.

## Deferred Items

These are known-incomplete and tracked for v1.1 / v1.2:

- **Cancel runtime.** Effect cancellation is parsed and type-checked
  but the runtime hooks for cooperative cancellation are not wired
  into the interpreter.
- **AUDIT-006.** Outstanding finding from the implementation audit;
  details under the relevant changelog entry.
- **`std.time.SystemClock`.** The trait is in place but the live
  implementation is stubbed for cross-target consistency. Tests use
  the deterministic `MockClock`.

## Known Issues

None tracked at this time. Bug intake will route through GitHub
Issues once the public repository is live.

## Migration Notes

This repository was migrated from the internal `aura-dev` working
tree (commit `38ef9fe`). The rename from "Aura" to "Bock" is
documented in `spec/changelogs/20260420-1700-specs-changes.md` and
`spec/changelogs/20260423-1830-specs-changes.md`. Historical
changelog content preserves the Aura name verbatim; the active
spec, source, examples, extension, and docs are all under the Bock
identity.
