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
 
## Expanded prompt template
 
```
# Session: $1
 
**Branch:** $1
**Owned files:** $2 $3 $4 $5 $6 $7 $8 $9
**Worktree:** /opt/claude-projects/bock-worktrees/<slug>
 
## Pre-flight (run BEFORE any session work)
 
```bash
set -e
 
# Confirm GitHub CLI authenticated
gh auth status >/dev/null 2>&1 || {
  echo "ERROR: gh CLI not authenticated. Run: gh auth login"
  exit 1
}
 
# Confirm main checkout is clean
cd /opt/claude-projects/bock
if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "ERROR: main checkout has uncommitted changes."
  echo "  Either commit/stash them, or use a separate path."
  git status --short
  exit 1
fi
 
# Ensure main is up to date
git fetch origin main
git checkout main
git pull --ff-only origin main
 
# Compute branch slug (replace / with -)
BRANCH="$1"
SLUG=$(echo "$BRANCH" | tr '/' '-')
WORKTREE="/opt/claude-projects/bock-worktrees/$SLUG"
 
# Refuse to clobber an existing worktree
if [ -d "$WORKTREE" ]; then
  echo "ERROR: Worktree already exists at $WORKTREE."
  echo "  Either resume work there, or remove with:"
  echo "    git worktree remove $WORKTREE"
  exit 1
fi
 
# Create the worktree with a fresh branch tracking main
mkdir -p /opt/claude-projects/bock-worktrees
git worktree add "$WORKTREE" -b "$BRANCH" origin/main
 
# Symlink local Claude settings into the worktree so permissions
# transfer. settings.local.json is gitignored, so the worktree
# wouldn't otherwise have the user's allow list. The .claude/
# directory itself exists in the worktree because .claude/commands/
# is committed.
CANONICAL_LOCAL=/opt/claude-projects/bock/.claude/settings.local.json
if [ -e "$CANONICAL_LOCAL" ]; then
  mkdir -p "$WORKTREE/.claude"
  ln -sf "$CANONICAL_LOCAL" "$WORKTREE/.claude/settings.local.json"
fi
 
# Set up the per-branch shared cargo cache
mkdir -p ~/.cargo/cache/bock-target/$SLUG
export CARGO_TARGET_DIR=~/.cargo/cache/bock-target/$SLUG
 
# Set up scratch namespace for tests that use /tmp
export BOCK_TEST_NAMESPACE=$SLUG
 
# Move into the worktree for the rest of the session
cd "$WORKTREE"
 
echo "============================================="
echo "Session ready in $WORKTREE"
echo "Branch: $BRANCH"
echo "CARGO_TARGET_DIR: $CARGO_TARGET_DIR"
echo "Scratch namespace: $BOCK_TEST_NAMESPACE"
echo "============================================="
```
 
## Session body
 
[The actual session prompt body follows here. Reference the
worktree as $WORKTREE or use the cwd. Use $BOCK_TEST_NAMESPACE
when creating scratch directories under /tmp.]
 
## Teardown (run AFTER session work completes)
 
```bash
# Confirm we have commits to push (session might have produced none)
cd "$WORKTREE"
COMMIT_COUNT=$(git rev-list --count main..HEAD 2>/dev/null || echo 0)
if [ "$COMMIT_COUNT" -eq 0 ]; then
  echo "WARN: No commits on $BRANCH. Worktree preserved at $WORKTREE."
  echo "  If this was intentional (no-op session), clean up manually:"
  echo "    git worktree remove $WORKTREE"
  echo "    git branch -D $BRANCH"
  exit 0
fi
 
# Pre-push verification gate: run the exact commands CI runs.
# Catches "passes locally, fails in CI" before we push and waste
# a CI run + create a broken PR. The gate is non-optional;
# sessions that need to push WIP/draft work can use a dedicated
# draft flow, not bypass this.
echo "============================================="
echo "Running pre-push verification (matches CI)..."
echo "============================================="
 
if ! cargo fmt --all -- --check; then
  echo ""
  echo "ERROR: cargo fmt --check failed. Format drift in commit."
  echo "  Fix and amend:"
  echo "    cd $WORKTREE"
  echo "    cargo fmt --all"
  echo "    git add -A && git commit --amend --no-edit"
  echo "  Then re-run /project:session teardown or push manually."
  echo "  Worktree preserved at $WORKTREE."
  exit 1
fi
 
if ! cargo clippy --workspace --all-targets -- -D warnings; then
  echo ""
  echo "ERROR: cargo clippy failed (--workspace --all-targets -D warnings)."
  echo "  Note: --all-targets covers tests/examples/benches that"
  echo "  default 'cargo clippy' skips — this is the most common"
  echo "  source of CI surprises."
  echo "  Fix the warnings and amend:"
  echo "    cd $WORKTREE"
  echo "    # ... fix code ..."
  echo "    git add -A && git commit --amend --no-edit"
  echo "  Worktree preserved at $WORKTREE."
  exit 1
fi
 
if ! cargo test --workspace; then
  echo ""
  echo "ERROR: cargo test --workspace failed."
  echo "  Fix failing tests and amend the commit."
  echo "  Worktree preserved at $WORKTREE."
  exit 1
fi
 
echo "============================================="
echo "Verification passed. Pushing branch..."
echo "============================================="
 
# Push the branch
if ! git push -u origin "$BRANCH"; then
  echo "ERROR: git push failed."
  echo "  Worktree preserved at $WORKTREE for manual finish."
  echo "  After resolving the issue:"
  echo "    cd $WORKTREE"
  echo "    git push -u origin $BRANCH"
  echo "    gh pr create --fill"
  echo "    cd /opt/claude-projects/bock"
  echo "    git worktree remove $WORKTREE"
  exit 1
fi
 
# Open the PR
PR_URL=$(gh pr create --fill 2>&1) || {
  echo "ERROR: gh pr create failed."
  echo "  Branch is pushed; worktree preserved at $WORKTREE."
  echo "  Open the PR manually:"
  echo "    cd $WORKTREE && gh pr create --fill"
  echo "  Then clean up:"
  echo "    cd /opt/claude-projects/bock && git worktree remove $WORKTREE"
  exit 1
}
 
echo "PR created: $PR_URL"
 
# Successful completion: clean up the worktree
cd /opt/claude-projects/bock
git worktree remove "$WORKTREE"
 
echo "============================================="
echo "Session complete."
echo "  Branch: $BRANCH (pushed)"
echo "  PR: $PR_URL"
echo "  Worktree: removed"
echo "  Branch ref: still in local repo (delete manually after merge)"
echo "============================================="
```
```
 
## Notes
 
- The CARGO_TARGET_DIR persists between sessions on the same
  branch. Multiple sessions on `fix/output-filenames` share
  build artifacts; switching to a different branch starts fresh.
- The branch ref persists locally after worktree removal. After
  PR merge, delete with `git branch -D <branch>`.
- If `gh pr create --fill` doesn't capture enough context (e.g.,
  for multi-commit PRs needing a longer description), the
  session prompt can override with a custom `--title` and
  `--body` or use `--body-file` pointing at a generated summary.
- Worktrees live at `/opt/claude-projects/bock-worktrees/`. Add
  `bock-worktrees/` to your shell's directory navigation if you
  often inspect them.
- `.claude/settings.local.json` from the canonical workspace is
  symlinked into each worktree so the agent inherits the user's
  permission allow list. Updates to the canonical file are
  immediately reflected in active worktrees through the symlink.
  If the canonical file doesn't exist, the symlink step is
  skipped and the agent runs with default permissions.
- The teardown's pre-push verification gate runs `cargo fmt
  --check`, `cargo clippy --workspace --all-targets -D warnings`,
  and `cargo test --workspace` — the same commands CI runs. If
  any fail, the push is aborted and the worktree is preserved
  for the user to fix and amend. This makes "passes locally,
  fails in CI" structurally impossible for these three checks.
  Tradeoff: the gate adds time (especially clippy and tests)
  to every successful session. Cargo's incremental cache makes
  the second run fast since the session's Test step already
  warmed it.
