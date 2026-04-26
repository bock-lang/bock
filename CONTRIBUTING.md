# Contributing to Bock

Bock is pre-1.0 and contributions of every size are welcome — bug
reports, doc fixes, conformance fixtures, and language work alike.

## Before You Start

- Read [`ARCHITECTURE.md`](ARCHITECTURE.md) for a 30-minute tour of
  the compiler pipeline.
- Skim [`ROADMAP.md`](ROADMAP.md) and [`STATUS.md`](STATUS.md) so
  your change lines up with where the project is heading.
- All participation is governed by the
  [Code of Conduct](CODE_OF_CONDUCT.md).
- Decision-making for language-level changes is described in
  [`GOVERNANCE.md`](GOVERNANCE.md). Small changes don't need an RFC;
  grammar, semantics, and public surface changes do.

## Local Setup

You need a stable Rust toolchain (the CI matrix runs `stable` and
`beta`) and Node 20 for the VS Code extension.

```bash
# Compiler workspace
cargo build
cargo test
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check

# VS Code extension
cd extensions/vscode && npm install && npm run compile
```

## Workflow

1. **Open or claim an issue.** For non-trivial changes, get
   maintainer signal before writing code.
2. **Branch from `main`.** Use a short descriptive name
   (`fix/lexer-overflow`, `feat/effect-inference`).
3. **Write a test first.** For language bugs, add a fixture under
   `compiler/tests/conformance/` before fixing.
4. **Keep PRs focused.** One logical change per PR. Refactor and
   functional change in separate PRs when practical.
5. **Run the full local check** before pushing:
   `cargo fmt --all -- --check && cargo clippy --workspace -- -D warnings && cargo test --workspace`
6. **Open a PR** using the template. CI must be green before merge.

## Commit Messages

- Imperative mood: "Add effect inference for closures" not "Added".
- One short subject line (≤ 72 chars), blank line, then body if
  needed.
- Reference issues with `Closes #123` / `Refs #123`.
- The merge commit's PR title becomes the changelog entry — make
  it readable.

## Spec, Stdlib, and Codegen Changes

- **Language-level** (grammar / semantics / effects / public CLI):
  open an RFC under `spec/changelogs/<YYYYMMDD>-<short-name>.md`
  before implementing. See
  [`spec/CLAUDE.md`](spec/CLAUDE.md) for the changelog format.
- **Stdlib additions** must be broadly useful, stable in scope, and
  implementable on every supported codegen target. See
  [`stdlib/CLAUDE.md`](stdlib/CLAUDE.md).
- **New compiler crate**: use `/project:new-crate <suffix>` so the
  workspace conventions are scaffolded for you.

## Testing Expectations

- Unit tests live alongside the code they test.
- Integration tests live in each crate's `tests/`.
- Language-level behavior is exercised by the conformance suite at
  `compiler/tests/conformance/`. Add a `<name>.bock` fixture and a
  `<name>.expected` file capturing the expected `bock` output.

## Reviewing

- Be specific. "This name is misleading because…" beats "rename
  this".
- Distinguish blocking from optional comments.
- Approve when the change is correct and consistent with project
  conventions, even if you'd have written it differently.

## Reporting Bugs and Security Issues

- Functional bugs: use the [bug report
  template](.github/ISSUE_TEMPLATE/bug_report.md).
- Security vulnerabilities: do **not** open a public issue. See
  [`SECURITY.md`](SECURITY.md) for the disclosure process.

## Working with Claude Code

This repo includes Claude Code conventions in `CLAUDE.md` files at
the root and inside each subtree. They describe the build/test
loop, code-style rules, and slash commands available under
`.claude/commands/`. If you're driving changes through Claude Code,
read those before starting; if you're not, you can ignore them.
