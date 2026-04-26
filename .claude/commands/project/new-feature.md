# New Feature

Scaffold and guide a new feature through the standard workflow.

## Arguments

`$ARGUMENTS` — short branch suffix (e.g. `effect-inference`).

The branch name will be `feat/<suffix>`.

## Steps

1. **Create the feature branch:**
   ```
   git checkout -b feat/<suffix> main
   ```

2. **Decide: spec change?**

   If the feature changes the language surface (grammar, type system,
   effect system, public CLI), an RFC is required first.

   - Open or reference an RFC in `spec/changelogs/`.
   - Wait for maintainer signoff before writing implementation code.

   If the feature is internal (refactor, perf, new diagnostic, etc.),
   no RFC is needed — proceed.

3. **Tests first.**

   Write the failing test before the implementation. For language
   features, this means a conformance fixture in
   `compiler/tests/conformance/`. For compiler internals, a unit
   test in the relevant crate.

4. **Implement.**

   Smallest change that makes the test pass. Follow the crate
   dependency order in `ARCHITECTURE.md` — modify upstream crates
   before touching downstream ones.

5. **Docs.**

   - User-facing change: update the relevant `docs/src/` page.
   - Spec change: update `spec/sections/` and add a changelog entry.
   - Internal-only change: update doc comments only.

6. **Final checks before PR:**
   ```
   cargo fmt --all
   cargo clippy --workspace -- -D warnings
   cargo test --workspace
   ```

7. **Open the PR** using `.github/PULL_REQUEST_TEMPLATE.md`.

## Done When

- Feature branch pushed
- PR open with template filled in
- CI green
- Reviewer assigned
