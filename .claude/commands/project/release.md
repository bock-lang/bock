# Release

Cut a new release of Bock. Triggers the `release.yml` workflow on
tag push.

## Arguments

`$ARGUMENTS` — the version string (e.g. `0.2.0`). No leading `v`.

## Pre-Flight

1. **Verify `main` is green** in CI. Do not release a red `main`.
2. **Verify your local checkout matches `origin/main`:**
   ```
   git fetch origin
   git status
   git log origin/main..HEAD --oneline   # should be empty
   ```

## Steps

1. **Bump the workspace version** in root `Cargo.toml`:
   ```toml
   [workspace.package]
   version = "<new-version>"
   ```

2. **Update extension version** in `extensions/vscode/package.json`
   to the same value.

3. **Update `ROADMAP.md`:** move completed items from "Next Up" to
   a new "Released — v<version>" section.

4. **Promote `Unreleased` in `CHANGELOG.md`:** rename the heading
   to `## v<version> — <date>` and add a fresh empty `## Unreleased`
   section above it.

5. **Commit:**
   ```
   git add Cargo.toml extensions/vscode/package.json ROADMAP.md CHANGELOG.md
   git commit -m "release: v<version>"
   ```

6. **Tag:**
   ```
   git tag -a v<version> -m "Release v<version>"
   ```

7. **Push** the commit and tag together. The tag push triggers
   `release.yml`:
   ```
   git push origin main
   git push origin v<version>
   ```

## After Push

- Watch the `release.yml` run. It builds binaries for all targets,
  packages the extension, publishes to crates.io and the VS Code
  Marketplace, deploys docs and website, and creates the GitHub
  release with artifacts attached.
- If any step fails, **do not retry blindly.** Investigate, fix on
  `main`, then either re-tag (`git tag -d v<version>` locally and
  on origin, then re-tag) or cut a patch release.

## Done When

- Release workflow finished green
- GitHub release page shows artifacts for every target
- `cargo install bock` installs the new version
- VS Code Marketplace shows the new extension version
