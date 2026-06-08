# Comparison-operator gating enforced for user types

**Date:** 2026-06-08
**Affects:** §18.5 (Trait-Language Integration)
**Type:** clarification (implementation now enforces an existing normative rule)

## Change

§18.5 already states, normatively, that "A type without `impl Comparable for
MyType` cannot use `<` on `MyType` values; the compiler rejects the code at
type-check time with a 'type does not implement Comparable' diagnostic," and
that wiring this for **user** types is an impl-completeness item
(Q-list-operator-gating-user-types). The type checker now enforces it: the
ordering operators (`<`, `>`, `<=`, `>=`) applied to a **user** (record/class,
i.e. `Type::Named`) operand require an `impl Comparable` for that type. The
operand must conform; when it does not, the checker rejects the comparison with
`E4005` (the trait-bound error code) and a message suggesting
`impl Comparable for <Type>`.

The gate is conservative everywhere it cannot prove non-conformance:

- **Primitives** are gated through `core`'s sealed canonical conformances
  (`Int`, `Float`, `String`, `Char`, and the sized numerics conform; `Bool`
  does **not** — matching the §18.5 matrix, so `true < false` is now rejected).
- **Bounded generic parameters** (`T: Comparable`) are unaffected: the operand
  type is a type variable, gated by its `where`-clause obligation, not by this
  user-type gate.
- **Inference variables, sketch (`Flexible`) types, and the error sentinel**
  are not gated (the operand type is not yet concrete).

`==`/`!=` (`Equatable`) are intentionally **not** gated by this change: records
carry structural equality, and gating equality is a separate item (see the
FOUND in the originating PR).

No normative spec text changed; this changelog records that the implementation
now matches the spec's stated requirement for user types.

## Rationale

Closes the soundness gap where `<` on a user type lacking `impl Comparable`
type-checked clean. The diagnostic gives the user the exact fix (implement
`Comparable`, or wrap a primitive in a newtype per the §18.5 sealed-conformance
escape hatch).

## Migration

User code that compared values of a record/class/enum type with `<`/`>`/`<=`/
`>=` without an `impl Comparable` for that type now fails to type-check. Add
`impl Comparable for <Type> { fn compare(self, other: Self) -> Ordering { … } }`
(or compare a `Comparable` field directly). No conformance fixture, stdlib
module, or example required a change — all already provided the impl where they
compared user values.
