# Roadmap

This file tracks forward-looking plans. For the present state of
the repo, see [`STATUS.md`](STATUS.md).

## Current Phase

**M.4 — Compiler migration.** Complete. The compiler crates,
stdlib, conformance suite, examples, spec, extension, and docs
have all been ported from the internal `aura-dev` working tree
into this repository under the Bock identity.

## v1.0 — Public Release

**Theme:** Ship what's already done. Verify, polish, announce.

- Property claims locked in:
  - "One language, many targets" — codegen parity across JS, TS,
    Python, Rust, Go on the example projects.
  - "Effects on every function" — effect inference + reporting in
    the CLI and editor.
  - "Targeted output, not a runtime" — generated code is idiomatic
    in each target with no Bock-specific runtime.
- CI live on GitHub Actions: build + test + clippy + fmt + mdbook.
- VS Code extension published to the marketplace.
- `bock` binary released via crates.io and GitHub Releases.
- Documentation site deployed.
- Announcement post.

**Acceptance criteria:**
- Conformance suite passes on every supported target.
- All 20 example projects `bock check`, `bock build`, and (where
  applicable) `bock test` clean on at least JS + Python + Rust.
- `cargo clippy --workspace -- -D warnings` clean.

## v1.1 — Editor and Tooling Polish

**Theme:** Make the editor a delight; close interpreter gaps.

- AIR tree view in the VS Code extension (visualize the
  intermediate representation per file).
- Target preview (live transpile-and-show in side panel).
- Standalone language server (LSP), decoupled from the VS Code
  extension so other editors can consume it.
- Incremental compilation with persistent build cache.
- Diagnostics quick-fixes (error → suggested edit).
- Hover-card improvements: effect set, target equivalence per
  symbol.

## v1.2 — Closing Deferred Loose Ends

**Theme:** Finish the items deferred from v1.0.

- **Cancel runtime.** Wire cooperative cancellation through the
  interpreter and into each codegen target.
- **AUDIT-006.** Address the outstanding implementation audit
  finding.
- **`std.time.SystemClock` live impl.** Replace the stub with a
  per-target live clock that still respects the deterministic
  test path.
- Documentation depth: complete the language-guide stubs left
  brief in v1.0.

## v2 — Ecosystem Growth

**Theme:** From compiler to ecosystem.

- Stdlib expansion: HTTP server primitives, structured logging,
  config loading, async streaming.
- Additional codegen targets (candidates: Swift, Kotlin, C#) — one
  at a time, evaluated against current generation patterns.
- Package registry and dependency resolution beyond the local
  lockfile.
- Macro system (syntactic, hygienic).
- Self-hosting: compile the Bock compiler itself with Bock.
- Native compilation via LLVM backend.
- WebAssembly target with first-class browser bindings.
- Distributed type-checking for monorepo scale.

The order of v2 items is intentionally not fixed; individual items
will graduate to versioned milestones as designs solidify and
contributor capacity allows.
