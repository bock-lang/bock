# Update Status

Refresh `STATUS.md` to reflect the current state of the repo.

## Steps

1. **Scan build health:**
   ```
   cargo check --workspace 2>&1 | tail -5
   cargo test --workspace --no-run 2>&1 | tail -5
   ```

2. **Count tests:**
   ```
   cargo test --workspace -- --list 2>/dev/null | grep -c ': test'
   ```

3. **Check CI status** (if `gh` is available):
   ```
   gh run list --branch main --limit 5
   ```

4. **Count open issues by label** (if `gh` is available):
   ```
   gh issue list --label bug --state open --json number | jq length
   gh issue list --label enhancement --state open --json number | jq length
   ```

5. **Read current `STATUS.md`** and identify what changed.

6. **Rewrite `STATUS.md`** with these sections, updated to today:
   - **Last updated:** today's date
   - **Build status:** CI badges and current `main` health
   - **What works today:** tested and verified capabilities
   - **Known issues:** open bugs with severity and tracking links
   - **Deferred items:** things explicitly punted to later milestones

7. **Commit on its own:**
   ```
   git add STATUS.md
   git commit -m "status: refresh for <date>"
   ```

## Rules

- `STATUS.md` describes **today**, not aspirations. Aspirations go
  in `ROADMAP.md`.
- Don't claim something works without verifying. If it's listed,
  it should have a passing test.
- Keep entries short. One line per item where possible.
