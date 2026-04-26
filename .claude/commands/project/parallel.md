# Parallel Execution Mode

This session is running in parallel with other Claude Code sessions.
Strict file isolation is required to prevent merge conflicts.

## Setup

Parse the arguments: `$ARGUMENTS`

Expected format: `<branch-name> <owned-crate-1> [owned-crate-2] ...`

Example: `feat/effect-inference bock-checker bock-types`

1. **Switch to the specified branch.** If it doesn't exist, create it from main:
   ```
   git checkout <branch-name> 2>/dev/null || git checkout -b <branch-name> main
   ```

2. **Confirm owned crates exist:**
   ```
   for crate in <owned-crates>; do
     ls compiler/crates/$crate/Cargo.toml || echo "ERROR: crate $crate not found"
   done
   ```

3. **Print the isolation contract** before doing any work:
   ```
   PARALLEL MODE ACTIVE
   Branch: <branch-name>
   Owned crates: <owned-crates>
   Read-only: everything else
   ```

## File Ownership Rules

**This session may ONLY modify files inside the owned crates:**
- `compiler/crates/<owned-crate>/src/**`
- `compiler/crates/<owned-crate>/Cargo.toml`
- `compiler/crates/<owned-crate>/tests/**`
- New conformance fixtures in `compiler/tests/conformance/` related
  to owned functionality

**This session MUST NOT modify:**
- `CLAUDE.md`, `STATUS.md`, `ROADMAP.md`, `ARCHITECTURE.md`
- Any crate not in the owned list
- `Cargo.toml` (workspace root)
- `compiler/crates/bock-cli/` (unless explicitly owned)
- Any file in `examples/`, `spec/`, `extensions/`, `docs/`, `website/`

If a fix requires changes outside owned crates, **stop and document
the needed change** in a commit message or a `PARALLEL-NOTES.md`
file inside the owned crate. Do NOT make the change. The merge
coordinator will handle it.

## Git Rules

- All commits go to the specified branch. No sub-branches.
- Commit frequently with descriptive messages.
- Run `cargo test` (full workspace) before each commit to ensure no
  cross-crate breakage.
- Do NOT merge to main. The human operator handles merges.
- Do NOT rebase during the session. Rebase happens at merge time.

## Tracking File Protocol

- **Do NOT edit** `STATUS.md` or `ROADMAP.md`.
- If you fix a bug or discover a new one, note it in the commit
  message:
  ```
  FIXED: #142 (effect inference for closures)
  FOUND: missing diagnostic when shadowing across modules
  ```
- The merge coordinator updates tracking files on main after merge.

## Session End

Before finishing:
1. `cargo check && cargo test && cargo clippy --workspace -- -D warnings`
2. Summarize what was done, fixed, and discovered.
3. List any changes needed outside owned crates (for the coordinator).
4. Do NOT merge. Leave the branch for the operator.

## Begin

Confirm the branch and owned crates, print the isolation contract,
then proceed with the session prompt that follows.
