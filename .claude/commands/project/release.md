# Release

Cut a new release of Bock. Triggers the `release.yml` workflow on
tag push. Rewritten after the v1.0.0 release (2026-07-03) to match
how a release actually lands under the protect-main ruleset — the
old direct-push-to-main instructions were impossible to follow.

## Arguments

`$ARGUMENTS` — the version string (e.g. `1.1.0`). No leading `v`.
Must be full three-part semver: the tag pattern `release.yml`
matches is `v*.*.*`.

## Pre-Flight

1. **Verify `main` is green in CI** — including the **path-filtered
   workflows** (`Docs`): check `gh run list --workflow docs.yml` for
   failures on recent commits, not just the latest HEAD run. A
   website/docs breakage can hide behind a HEAD whose changes didn't
   trigger it (this masked a broken website build for 9 hours before
   v1.0.0).
2. **Verify local `main` matches `origin/main`:**
   ```
   git fetch origin main
   git status
   git log origin/main..HEAD --oneline   # should be empty
   ```
3. **Confirm publish credentials with the operator** (sessions
   cannot list secrets): `CRATES_IO_TOKEN` (the account must own the
   existing `bock-*` crates) and `VSCE_PAT` (Marketplace publisher
   `bock-lang`).
4. **Drain or explicitly defer open dependabot PRs** — operator's
   call either way.

## Release-prep (lands via PR — main is PR-only)

Do all of this on a `release/v<version>` branch in a worktree, then
PR → CI green → merge. Never push to `main` directly; the
protect-main ruleset rejects it.

1. **Version bump** in root `Cargo.toml`: `[workspace.package]`
   `version` **and every internal `version = "..."` ref in
   `[workspace.dependencies]`** (crates.io publish fails without
   them). Refresh `Cargo.lock` (`cargo check --workspace`).
2. **Extension**: bump `extensions/vscode/package.json` version
   (+ `npm install --package-lock-only`), and promote its
   `CHANGELOG.md` `Unreleased` heading. Keep `@types/vscode` ≤
   `engines.vscode` — vsce refuses to package otherwise (dependabot
   is configured not to bump it past the floor).
3. **Spec stamp**: `spec/bock-spec.md` header (Version / Date /
   Status), then run `tools/scripts/sync-vocab.sh` — the extension
   bundles a spec asset and the `assets-drift` CI guard fails if it
   drifts.
4. **Docs/README version refs**: grep for the old version string and
   stale status claims (e.g. "pre-1.0"); update whatever the release
   falsifies.
5. **CHANGELOG**: run `tools/scripts/gen-changelog.sh` to sync
   Unreleased, rename the heading to `## v<version> — <date>`, run
   the generator again (it re-adds the canonical empty Unreleased
   block), and verify `tools/scripts/gen-changelog.sh --check`
   passes — it is a hard gate in `release.yml`.
6. **Tracking hub**: update `tracking/milestones.md` (+ snapshot /
   queue as appropriate) and regenerate the views with
   `tools/scripts/gen-tracking-views.sh`. Never hand-edit
   `ROADMAP.md` / `STATUS.md` — they are generated.
7. **Publish preflight**: `cargo publish --workspace --dry-run
   --allow-dirty` must pass for every crate; for any first-time
   crate also confirm name availability/ownership on crates.io.
8. **Full pre-PR gate** (fmt / clippy / test / doc + conformance +
   extension suite). A LOCAL-only rust-lane conformance failure with
   a shifting fixture set is the known harness race
   (`Q-conformance-rust-stale-binary-reuse`) — verify against CI
   before treating it as a regression.

## Post-promotion changelog rule

Any PR merged after the `## v<version>` section is written but
before the tag exists sits below the future tag and becomes
permanently invisible to the changelog generator. Either fold its
line into the released section via a `tracking:`-prefixed PR (the
generator excludes those subjects), or make the PR self-recording
(add its own `(#NN)` line to the released section in a follow-up
commit on the same branch).

## Tag (operator gate)

Release actions escalate: get the operator's explicit go before
tagging. Then, on the merged, green `main` HEAD:

```
git tag -a v<version> -m "Release v<version>"
git push origin v<version>
```

The tag push triggers `release.yml`: changelog gate, 6-target
binaries, crates.io publish (tolerant per-crate loop), Marketplace
publish, GitHub release with per-target-named assets.

## If the release run fails

- **Do not retry blindly.** Read the failing job's log first.
- **Nothing published yet** (publish jobs skip when builds fail):
  fix on `main` via PR, then re-tag —
  `git tag -d v<version>`, `git push origin :refs/tags/v<version>`,
  re-tag the new HEAD, push.
- **Partial crates.io publish**: re-tagging does NOT recover this
  (crates.io versions are permanent, and job reruns reuse the old
  workflow definition). Use the dispatchable recovery workflow —
  the dispatch is a restricted action, so get operator approval:
  `gh workflow run publish-crates.yml -f tag=v<version>`
  (tolerant fixed-point loop; already-published crates are skipped).
- **GitHub release failed but binaries built**: complete it manually
  (operator approves `gh release create`): `gh run download <run-id>
  -R bock-lang/bock`, rename binaries to
  `bock-<target-triple>[.exe]`, smoke-test one, then
  `gh release create v<version> <assets> --generate-notes`.
- A `cargo publish --workspace` planner wedge ("no packages ready to
  publish but N remain … unexpected cargo internal error") is a
  known cargo bug — the tolerant loop exists because of it.

## Done When

- Release workflow (or its documented recovery paths) finished green
- GitHub release page shows one asset per target plus the `.vsix`
- `cargo install bock` installs the new version (actually run it)
- VS Code Marketplace shows the new extension version
- Tracking hub reconciled + views regenerated (via PR)
