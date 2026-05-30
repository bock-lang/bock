# Standard Library — Claude Conventions

This subtree holds Bock's standard library, organized as `std.*`
packages.

## The v1 Core (`core.*`)

The **v1 core** standard library lives under `stdlib/core/` and is
distinct from the broader `std.*` packages above. It is the minimal,
always-available foundation (`core.error`, and siblings as they land).

**The normative contract is spec §18.1:** `core` ships with the
compiler, is small and stable, and works on every target. Everything
in this section below — the embedded-source-compiled loading mechanism,
the per-target shim layout, the `$BOCK_STDLIB` override — is an
**implementation strategy** for satisfying that contract, not part of
the contract itself. It may evolve (for example, toward distributing
pre-compiled AIR alongside source per spec §16.4) without a spec
change, provided §18.1 still holds.

### Layout

```
stdlib/
  core/
    error/
      error.bock     module core.error  (the pilot module)
    <module>/
      <module>.bock  module core.<module>
```

One directory per core module; the module's public surface lives in
`<module>/<module>.bock` and declares `module core.<module>` at the top.

### Loading model (current implementation strategy)

**Core ships embedded in the compiler.** The `bock` binary embeds every
`stdlib/core/**/*.bock` source at build time (via the crate build script
`compiler/crates/bock-cli/build.rs`, surfaced through
`compiler/crates/bock-cli/src/stdlib.rs::core_sources`). At `check`,
`build`, and `run` time, the CLI **prepends the parsed core sources to
the parsed-files set before the user-file loop**, so they flow through
the exact same multi-file pipeline (dependency sort → per-module compile
→ register in the `ModuleRegistry`) and land in the registry before any
user module. A user's `use core.<module>.{...}` (named import form) then
resolves with no special-casing.

Because core is embedded, a built `bock` resolves `core.*` from any
working directory with no filesystem access — inside or outside the
repo. A dev-only `$BOCK_STDLIB=<dir>` environment override reads the
core sources from disk instead, for iterating on stdlib sources without
a recompile.

Diagnostics: core modules compile like any other module, but their
**non-error** diagnostics (e.g. development-mode context-annotation
recommendations) are **not surfaced** to the user — they describe
internal stdlib code the user did not author. Core **errors** still
surface (they are compiler defects).

### Shims

**Shims only where a host primitive is needed** — deferred until a
module needs one. `core.error` is pure Bock (a trait + record + impl +
constructor), so it needs **no** per-target runtime shim; that is why it
is the pilot for the embedded source-compiled loading mechanism. When a
future core module requires a host primitive, its per-target shim is
added at that point (see the note on shim paths below).

### Conformance

Core fixtures live under `compiler/tests/conformance/stdlib/<module>/`
in the harness's inline-directive format (`// TEST:` / `// EXPECT:
no_errors` / `// EXPECT: output "..."`), not as separate `.expected`
files. Cross-target source-emission is additionally verified by
`compiler/crates/bock-cli/tests/stdlib_error_targets.rs`. Full
conformance **execution** across targets is the separate Q-fconf task.

## Layout

```
stdlib/
  std/
    io/        I/O primitives
    collections/
    async/
    errors/
    time/
    ai/        AI provider abstraction (per spec §17.8)
    ...
```

Each package directory contains:

- `<package>.bock` — public surface
- `<package>_internal.bock` (optional) — implementation helpers, private
- `tests/` — package tests
- `README.md` — short description, design notes, examples

## Adding a New `std.*` Package

1. **Justify the addition.** A stdlib package must be:
   - Broadly useful (not domain-specific)
   - Stable in scope (additions, not churn)
   - Implementable on every supported codegen target

   If it doesn't meet all three, it belongs in a community package,
   not stdlib.

2. **Open an RFC** in `spec/changelogs/` describing the surface.

3. **Once accepted:**
   - Create `stdlib/std/<name>/`
   - Add `<name>.bock` with the public surface
   - Add `tests/` with coverage of every public function
   - Add `README.md`

4. **Wire codegen.** Each codegen backend may need a runtime shim
   for the new package. Codegen lives in a **single** `bock-codegen`
   crate (`compiler/crates/bock-codegen/`), not per-target crates, so
   shims live there, organized by target.

   > Note: the exact shim sub-path under `compiler/crates/bock-codegen/`
   > is not yet settled — no shipping core module has needed a host
   > primitive yet (`core.error` is pure Bock). Settle the layout when
   > the first core module that needs a shim lands, and record it here.

5. **Conformance.** Add fixtures under
   `compiler/tests/conformance/stdlib/<name>/` exercising the
   package on every target.

## Style

- 2-space indent.
- `module std.<name>` declaration at the top of every file.
- `public` keyword required on every exported item.
- Doc comments (`///`) on every public item — these become the
  generated reference docs.
- No target-specific code in `.bock` files. If a function needs
  per-target implementation, it goes in the codegen runtime shim,
  not in stdlib source.
