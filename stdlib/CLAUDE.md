# Standard Library — Claude Conventions

This subtree holds Bock's standard library, organized as `std.*`
packages.

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
   for the new package. Add shims under
   `compiler/crates/bock-codegen-<target>/runtime/std/<name>/`.

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
