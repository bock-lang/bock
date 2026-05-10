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

## Session Workflow

All Claude Code sessions use the worktree-based pattern via
`/project:session <branch> [owned-files...]`. This isolates each
session from the main checkout and from concurrent sessions.

### Stale-main protection

The slash command's pre-flight always fetches and fast-forwards
local `main` to `origin/main` before creating the worktree, then
bases the new branch on the now-current `origin/main`. Sessions
therefore cannot build from a stale main: the worktree's base
commit is always whatever `origin/main` reports at session start.

If you want to verify before invoking, run `git -C
/opt/claude-projects/bock log -1 --format='%h %s' origin/main`
and `git -C /opt/claude-projects/bock fetch origin main` to
compare. The slash command does both automatically.

### What this means for session prompts

- Don't `cd /opt/claude-projects/bock` in the session body. The
  slash command places you in a worktree at
  `/opt/claude-projects/bock-worktrees/<branch-slug>` and exports
  `$WORKTREE` pointing there. Use the current working directory
  or `$WORKTREE`.
- Don't manually create branches with `git checkout -b`. The
  slash command does this.
- Don't manually push or open PRs at the end. The slash
  command's teardown handles it on success.
- For scratch directories under `/tmp/`, prefix with
  `$BOCK_TEST_NAMESPACE`: `mkdir -p
  /tmp/$BOCK_TEST_NAMESPACE-test-build` rather than
  `/tmp/test-build`. Prevents collisions across concurrent
  sessions.

### Cargo target sharing

`CARGO_TARGET_DIR` is set per-branch under
`~/.cargo/cache/bock-target/<branch-slug>/`. Sessions on the
same branch reuse build artifacts. Different branches stay
isolated. Trade-off: disk space for compile speed.

To wipe build state for a branch:

```bash
rm -rf ~/.cargo/cache/bock-target/<branch-slug>
```

### Worktree cleanup

Successful sessions clean up their worktree automatically. On
failure (test failure, push rejection, gh auth issue), the
worktree persists at
`/opt/claude-projects/bock-worktrees/<branch-slug>` for
inspection. Resume work there or clean up manually:

```bash
git worktree remove /opt/claude-projects/bock-worktrees/<slug>
git branch -D <branch>                        # if abandoning
rm -rf ~/.cargo/cache/bock-target/<slug>      # reclaim disk
```

## GitHub Operations (gh CLI)

Claude Code has authenticated access to the `gh` CLI. The
following operations are permitted, restricted, or prohibited:

### Permitted (no confirmation needed)

Read operations and the standard session-completion writes:

- `gh auth status`
- `gh run list`, `gh run view`, `gh run watch`, `gh run download`
- `gh pr list`, `gh pr view`, `gh pr checks`, `gh pr diff`
- `gh pr create` (on feature branches only — never on `main`)
- `gh pr comment` (own session's PR)
- `gh pr ready` (draft → ready, own session's PR)
- `gh issue list`, `gh issue view`, `gh issue comment`
- `gh api` with GET method
- `gh repo view`
- `gh release list`, `gh release view`
- `gh workflow list`, `gh workflow view`

### Restricted (surface to human, do NOT execute autonomously)

These are reversible but materially affect the canonical state.
Surface as `PROPOSED:` in the session output and let the human
run them:

- `gh pr merge` — affects main; merge ceremony belongs to the human
- `gh pr close`, `gh pr reopen`
- `gh issue close`, `gh issue reopen`
- `gh release create`, `gh release upload`, `gh release edit`
- `gh api` with `POST`, `PATCH`, `PUT`, `DELETE` (except the
  endpoints implicit in the permitted commands above)
- `gh workflow run` (manually-triggered workflow dispatches)
- `gh pr review` with `--approve` or `--request-changes` on
  PRs the same session opened (self-review)

### Prohibited (never, under any circumstance)

These are destructive, irreversible, or escalate access:

- `gh repo delete` — any repo, including bock-lang/bock
- `gh repo edit` — changing visibility, default branch, settings
- `gh repo create` — creating new repos in the bock-lang org
- `gh secret set`, `gh secret delete`, `gh secret list`
- `gh variable set`, `gh variable delete`
- `gh org` — any org-level operations (members, teams, settings)
- `gh ruleset` — modifying branch protection / rulesets
- Force pushes (`git push --force`, `--force-with-lease`)
- Deletions of remote refs other than the session's own
  newly-created feature branch when explicitly resetting
- Approving or merging PRs the same session created
- `gh auth refresh --scopes` to expand token scope

### When in doubt

If an operation isn't clearly listed above and could affect more
than the session's own branch and PR: don't run it. Surface as
`PROPOSED: gh <command>` with rationale. The human decides.

## Concurrent Sessions

Multiple sessions on different branches run safely under the
worktree pattern. Each session gets its own worktree, its own
`CARGO_TARGET_DIR`, and its own `BOCK_TEST_NAMESPACE`. There's
no shared mutable state between concurrent sessions on
different branches.

Sessions still must respect ownership: a session declares its
owned files / directories at the top of the prompt, and only
modifies those. Tracking files (STATUS.md, ROADMAP.md, this
CLAUDE.md) are off-limits to all sessions; the merge coordinator
updates them after PRs land.

The legacy `/project:parallel` command is deprecated; use
`/project:session` for all new work.

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
