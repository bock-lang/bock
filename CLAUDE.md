# Bock Programming Language

Bock is a feature-declarative, target-agnostic language that compiles
to JS, TS, Python, Rust, Go, and more. This repo contains the
compiler (Rust), standard library, language specification, VS Code
extension, and documentation.

## Repo Layout

```
compiler/        Cargo workspace — compiler crates + conformance tests
stdlib/          Bock standard library packages (std.*)
spec/            Language specification — sections + changelogs
extensions/      Editor integrations (VS Code lives here)
examples/        Example Bock projects
docs/            mdBook documentation source
website/         Marketing/landing site source
tools/scripts/   Dev scripts (vocab sync, release helpers, etc.)
.claude/         Project commands and conventions for Claude Code
.github/         CI workflows, issue/PR templates, dependabot
```

## Build Commands

```bash
# From repo root (workspace-aware):
cargo build                               # build all compiler crates
cargo test                                # run all tests
cargo clippy --workspace -- -D warnings   # lint workspace
cargo fmt --all -- --check                # format check

# From compiler/ (equivalent):
cd compiler && cargo build

# Extension:
cd extensions/vscode && npm install && npm run compile
```

## Testing Commands

```bash
cargo test                                # all unit + integration tests
cargo test -p bock-lexer                  # one crate
./tools/scripts/run-conformance.sh        # language conformance suite
cd extensions/vscode && npm test          # extension tests
```

## Where to Find What

- **Language reference:** `spec/sections/`
- **Implementation playbook:** `docs/src/contributing/playbook.md`
- **Architecture overview:** `ARCHITECTURE.md` (start here for new contributors)
- **Current state:** `STATUS.md`
- **Forward plans:** `ROADMAP.md`

## Tracking File Alignment

`STATUS.md` and `ROADMAP.md` describe the same project at different
time horizons. When a milestone completes, both must be updated in
the same PR. If they disagree, the most recent commit on each file
wins until reconciled. Do not let drift persist across sessions.

## Parallel Sessions

Use `/project:parallel <branch> <crate1> [crate2] ...` when running
multiple Claude Code sessions simultaneously. Each session may only
modify its owned crates. See `.claude/commands/project/parallel.md`
for the full protocol.

## Code Style

### Rust (compiler/, stdlib build tools)

- Rust 2021 edition
- `thiserror` for library error types, `anyhow` only in CLI/tests
- All public types and functions get doc comments
- No `unwrap()` in library crates — use `?` or explicit handling
- `cargo fmt` and `cargo clippy -D warnings` must pass

### TypeScript (extensions/vscode/)

- Strict mode on (`"strict": true` in tsconfig.json)
- ESLint clean, no `any` without justification
- One module per language feature

### Bock (.bock files in stdlib/, examples/, conformance tests)

- 2-space indent
- `module <name>` declaration at top of every file intended for cross-file `use`
- `public` keyword required for exported items (default is private)
- Records, enums, match arms: newline-separated
- `if (cond)`, `(x) => expr` — parens required
- See `spec/sections/` for the authoritative grammar

## Multi-File / Build Conventions for Bock

- Cross-file imports go through the module registry; every importable
  file declares `module <name>` at the top
- `bock check` takes file paths; no args scans cwd
- `bock build -t <target> --source-only` emits transpiled source
  without invoking the target toolchain
- Project root marker is `bock.project` (TOML)
- Build cache lives in `.bock/` (gitignored)
