# Contributing

This section is for people working *on* the Bock compiler, standard
library, and docs — as opposed to people writing Bock programs. It
covers how the repository is laid out, how to build and test it, and
how language and spec changes move through review.

Contributions of all sizes are welcome — a typo fix and a new language
feature follow the same path, just at different scales.

## Where to look first

- **[`CONTRIBUTING.md`](https://github.com/bock-lang/bock/blob/main/CONTRIBUTING.md)**
  at the repository root — legal basics, code of conduct, and the
  governance model for language-level changes.
- **[Architecture](./architecture.md)** — a map of the compiler
  pipeline and the seventeen `bock-*` crates. Start here if you don't
  yet know which crate owns the thing you want to change.
- **[Development workflow](./workflow.md)** — build, test, the
  pre-PR verification gate, and the conformance suite.
- **[Spec changes](./spec-changes.md)** — the process for changing the
  language itself, plus how the changelog is generated.
- **`STATUS.md`** shows the current build/test state and what is in
  flight; **`ROADMAP.md`** lays out planned work. Both are generated —
  see [Spec changes](./spec-changes.md#generated-files) for what that
  means.

## How decisions are made

Small changes — bug fixes, doc corrections, new conformance fixtures —
don't need a proposal; open a pull request. Changes to the grammar,
type or effect rules, or any public surface (CLI flags, error messages,
stdlib signatures) are deliberate: open an issue describing the change
first, so the design can be discussed before code is written. The
[Spec changes](./spec-changes.md) page describes the full path for a
normative change.

The **specification defines** the language; the **docs explain** it.
When the two could drift, link to the spec for the normative rule and
keep the prose here as the friendly explanation. A discovered
divergence between the implementation and the spec is surfaced for
design review, never silently resolved in either direction.

## Code reviews

Every pull request must clear the same bar before it merges:

- The full test suite passes (`cargo test --workspace`).
- Clippy is clean at the level CI uses
  (`cargo clippy --workspace --all-targets -- -D warnings`).
- The PR description explains the *why*, not just the *what*.

The complete set of checks — and how to run exactly what CI runs,
locally, before you push — is on the [Development
workflow](./workflow.md#the-pre-pr-verification-gate) page. Running the
real gate locally is the single most effective way to avoid a
"passes locally, fails in CI" round trip.
