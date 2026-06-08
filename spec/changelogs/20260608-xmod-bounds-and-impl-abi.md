# Cross-module where-clause bounds and trait-impl visibility

**Date:** 2026-06-08
**Affects:** §4.6 (Trait Bounds), §6.5 (Traits), §12.2 (Imports)
**Type:** clarification

## Change

Two cross-module behaviors are clarified as normative (the implementation
previously diverged by silently under-enforcing them; this brings the
compiler in line with the spec's intent):

1. **Imported generic functions' `where`-clause trait bounds are enforced
   at the call site** (Q-xmod-bounds). A bound declared on a function in
   module `A` — e.g. `public fn render[T](x: T) -> Int where (T: Show)` —
   is enforced identically whether `render` is called inside `A` or in a
   module `B` that `use`s it. A call whose concrete type argument does not
   satisfy the bound is an `E4005` where-clause error in either module.
   Previously the export ABI carried no bounds, so an imported generic
   function's `where`-clause was dropped at import and a bound-violating
   call was wrongly accepted.

2. **Trait impls are module-scoped facts that become visible when a module
   is imported** (Q-xmod-impl). An `impl From[A] for B` (and its blanket
   reverse `Into[B] for A`) declared in module `X` participates in
   `From`/`Into` resolution — including `a.into()` — in any module that
   `use`s `X`. Importing a module brings in *all* of its trait impls,
   independent of which value/type names the `use` selected, because trait
   coherence is global, not name-gated. Previously the impl table was built
   per module from local `impl` blocks only, so cross-module `.into()`
   could not resolve an impl declared in the imported module.

The compiler's canonical primitive conversions (`From[Int] for Float`, …)
and sealed primitive conformances (§18.3, §6.6) are re-registered locally
in every module and are NOT part of this cross-module export; only
user-declared impls over user-defined (`Named`) types are exported.

## Rationale

§4.6 specifies `where`-clause bounds and §12.2 specifies imports, but
neither stated whether a bound or an impl survives a module boundary. The
only sound reading is that they do: a bound that is enforced locally but
dropped on import is a soundness hole, and trait coherence (§6.5) is a
whole-program property. This entry pins that reading.

## Migration

Code that relied on the previous under-enforcement may now surface errors
that were always intended:

- A cross-module generic call that violated an imported function's
  `where`-bound now reports `E4005`. Fix it by providing a type argument
  that satisfies the bound (or by implementing the required trait).
- No migration is needed for the `.into()` change — it only makes
  previously-unresolvable cross-module conversions resolve.

## Notes

This is the type-checker (resolution + diagnostics) half. The TS and Go
backends do not yet re-emit the generic-parameter trait constraint
(`<T extends Show>` / `[T Show]`) for an *imported* generic function whose
`where`-clause was reconstructed across the module boundary; that codegen
gap is tracked separately (the conformance fixture
`exec_xmod_where_bound_dispatch` is therefore restricted to js/python/rust).
