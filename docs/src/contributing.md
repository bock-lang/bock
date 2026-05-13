# Contributing

Bock is open source. Contributions of all sizes are welcome — typo
fixes through full language features.

## Where to Look First

- `CONTRIBUTING.md` at the repo root lists the legal and
  development-environment basics.
- `ARCHITECTURE.md` is a 30-minute orientation to the compiler
  pipeline and crate dependency graph.
- `STATUS.md` shows current build/test state and what's in flight.
- `ROADMAP.md` lays out planned work.

## Local Development

```bash
cargo build              # compile the workspace
cargo test               # full test suite (~2200 tests)
cargo clippy --workspace -- -D warnings
cargo fmt --all
```

Per-crate development:

```bash
cargo test -p bock-checker
cargo doc -p bock-air --open
```

## Adding a Spec Change

Spec changes are deliberate. Process:

1. Open an issue describing the proposed change.
2. Add a changelog entry under `spec/changelogs/` with the
   `YYYYMMDD-HHMM-specs-changes.md` filename convention.
3. Update the affected section(s) of `spec/bock-spec.md`.
4. Add a conformance fixture under
   `compiler/tests/conformance/<category>/` exercising the new
   behavior.

## Code Reviews

PRs require:

- Full test suite passing.
- `cargo clippy -D warnings` clean.
- A brief PR description explaining the *why*, not just the *what*.

For larger changes, propose an outline in an issue first.
