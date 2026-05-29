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

## Changelog

The `## Unreleased` section of `CHANGELOG.md` is **generated from
merged-PR history**, not written by hand or by CI. Git history is the
source of truth.

To regenerate it:

```bash
tools/scripts/gen-changelog.sh            # rewrite CHANGELOG.md in place
tools/scripts/gen-changelog.sh --stdout   # preview without writing
tools/scripts/gen-changelog.sh --check    # exit non-zero if stale (CI guard)
```

How it works:

- It collects every PR number already recorded under a released
  (`## vX...`) section, then lists each squash-merge subject ending in
  `(#NN)` whose number is *not* yet released. Those become the Unreleased
  entries.
- It is **idempotent** — running it twice produces no diff — and rewrites
  only the `## Unreleased` block; released sections and the file header are
  untouched.
- Pure internal `tracking:`-prefixed PRs are excluded from this
  consumer-facing changelog.
- It is **tag-independent**: it works today (no release tags) by walking
  full history, and will prefer the latest `v*` tag as a base once one
  exists.

It is regenerated and committed via normal pull requests — the
maintainer/orchestrator syncs it during coordination and at release time.
**CI never writes the changelog.** Writing to the ruleset-protected `main`
from CI is both impossible (no direct pushes) and a supply-chain risk (a
credential that can push to `main` is exactly what recent attacks target),
so the changelog lands through the same human-reviewed PR flow as every
other change. The release workflow only *verifies* the section is in sync
(`--check`); it never writes.

## Code Reviews

PRs require:

- Full test suite passing.
- `cargo clippy -D warnings` clean.
- A brief PR description explaining the *why*, not just the *what*.

For larger changes, propose an outline in an issue first.
