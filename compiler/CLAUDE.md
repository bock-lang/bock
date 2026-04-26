# Compiler — Claude Conventions

This subtree is the Cargo workspace for all compiler crates plus
the conformance suite.

## Layout

```
compiler/
  crates/           One crate per compiler component (bock-*)
  tests/conformance/  Language conformance fixtures (.bock + .expected)
```

## Crate Dependency Order

Upstream → downstream:

```
bock-errors → bock-source → bock-lexer → bock-ast → bock-parser
  → bock-types → bock-checker → bock-air → bock-codegen-{js,ts,py,rs,go}
  → bock-cli
```

A lower crate must never depend on a higher one. `cargo check` from
the workspace root catches violations.

## Edit-Test Cycle

```
cargo check                       # fast typecheck after every edit
cargo test -p bock-<crate>        # focused tests for the crate you touched
cargo test --workspace            # full suite before commit
cargo clippy --workspace -- -D warnings
cargo fmt --all
```

## Rust Conventions

- Rust 2021. `unsafe_code = "forbid"` workspace-wide.
- `thiserror` for library error types; `anyhow` only in `bock-cli` and tests.
- All public types and fns get doc comments.
- No `unwrap()` or `expect()` in library crates — workspace lints
  catch this. Use `?` or explicit error handling.
- `#[must_use]` on non-trivial return values.
- `Span` and `FileId` from `bock-errors` everywhere for source locations.
- `DiagnosticBag` from `bock-errors` accumulates errors across passes.

## Adding a New Crate

Use `/project:new-crate <suffix>`. The command scaffolds the crate
with the workspace conventions baked in.

## Conformance Fixtures

When fixing a language bug, **add a fixture first** under
`compiler/tests/conformance/<category>/`. Format:

- `<name>.bock` — the source
- `<name>.expected` — expected `bock check` (or `bock build`) output
- `<name>.skip` — optional, present means "known-broken, with reason"

Run via `/project:run-conformance`.
