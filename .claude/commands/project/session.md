# Session: <branch> [owned-files...]

Worktree-based isolated CC session. Creates a fresh worktree,
runs the session in isolation, opens a PR on success, cleans up.

## Args

- `$1` — branch name (e.g., `fix/output-filenames`,
  `feat/website-deploy`). Required. Convention:
  `<type>/<short-description>` where type is `fix`, `feat`,
  `chore`, `docs`, `refactor`, `test`.
- `$2..$N` — owned files / directories (e.g., `bock-codegen`,
  `extensions/vscode/`). Optional but recommended for clarity in
  the PR description.

## What this command does

When you invoke `/project:session fix/output-filenames bock-codegen`,
the expanded prompt instructs CC to:

1. **Pre-flight:** verify `gh` CLI auth, ensure main is clean,
   fetch latest main
2. **Setup:** create a worktree at
   `/opt/claude-projects/bock-worktrees/<branch-slug>` with the
   new branch tracking main, set `CARGO_TARGET_DIR` to a
   per-branch shared cache, set `BOCK_TEST_NAMESPACE` for scratch
   isolation
3. **Run** the session work (whatever prompt body follows the
   slash command invocation)
4. **Teardown on success:** push branch, open PR, remove
   worktree, return final state
5. **Teardown on failure:** leave worktree intact, log the path
   and the failure, do NOT push or open PR
