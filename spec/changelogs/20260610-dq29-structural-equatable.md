# Structural Equatable conformance + `==`/`!=` operator gating (DQ29)

**Date:** 2026-06-10
**Affects:** §18.5 (Trait-Language Integration)
**Type:** amendment (new normative paragraph) + implementation enforcement

## Change

§18.5 gains a normative **"Structural `Equatable` conformance (DQ29)"**
paragraph, and the implementation enforces it. The rule:

1. **Records** (§6.2) conform to `Equatable` iff every field type conforms
   (recursively). Equality is field-wise.
2. **Enums** (§6.3) conform iff every payload type of every variant conforms.
   Equality is tag-then-payload; unit variants compare by tag.
3. **Compound built-ins compose conditionally:** `List[T]`/`Set[T]`/
   `Optional[T]` iff `T`; `Map[K, V]` iff `K` and `V`; `Result[T, E]` iff `T`
   and `E`; tuples iff all components. `Map`/`Set` equality is
   order-independent.
4. **Generic user types instantiate conditionally:** `Pair[A, B]` is
   `Equatable` iff `A` and `B` are, decided at the use site on the
   instantiated type.
5. **Non-`Equatable` leaves poison:** a record with an `Fn` field is not
   `Equatable`; `==` on it is a compile error naming the offending field path
   and type.
6. **Explicit impl wins:** `impl Equatable for R` suppresses the structural
   default (skip-if-occupied, the `From` ⇒ `Into` blanket's precedence) and
   its `eq` defines the type's equality everywhere `==`/`!=` apply.
7. **Classes are excluded** (data/identity line): explicit impl only.
8. **`Float` fields compose with the DQ10 IEEE caveat:** a record holding
   `NaN` is `!=` itself. No special case.

With the rule in place, `==`/`!=` **gate** behind `Equatable` exactly as
`<`/`>`/`<=`/`>=` gate behind `Comparable` (the 2026-06-08 operator-gating
changelog), and an `Equatable` **bound** (`where (T: Equatable)`) is satisfied
by structural conformance — a record of `Equatable` fields now passes
`fn dedupe[T: Equatable](items: List[T])`, which previously rejected it.

**Asymmetry (also normative):** structural conformance does NOT extend to
`Comparable` (ordering has no canonical structural meaning; explicit-impl-only
stays) nor to `Hashable` (deferred to the v1.x derive-era design pass, paired
with `@derive`).

## Implementation

- The checker answers "is this concrete type `Equatable`" with an on-demand
  recursive structural predicate at the use site (co-inductive on recursive
  types), for BOTH the `==`/`!=` operator gate and `Equatable`-bound
  satisfaction — not with conditional trait-table entries.
- Rejections emit the new **`E4015`** (sibling of the Comparable gate's
  `E4005`), naming the field path and leaf type and suggesting the fix.
- Codegen pins cross-target structural equality (11 new exec fixtures × 5
  targets): JS/TS lower stamped equalities through a `__bockEq` deep-equality
  runtime (native `===` is reference identity on objects — this also fixes
  Q-js-user-equality-reference/#339, user-type `==` with an explicit impl
  lowering to reference equality); Rust derives `PartialEq` on structurally
  conforming records/enums and routes explicit-impl `==` through `eq`; Go
  routes collection-involving equality through a `reflect.DeepEqual` helper
  (slices/maps have no native `==`) and explicit-impl `==` through `Eq`;
  Python routes explicit-impl `==` through `eq` (dataclass `==` is already
  structural); the reference interpreter dispatches stamped explicit-impl
  `==` through the impl and bridges `a.eq(b)` on structural instantiations of
  `Equatable`-bounded generics.

## Rationale

Records/enums had free structural equality on most targets but reference
equality on JS/TS, compile errors on Rust/Go for collections, and silently
ignored explicit impls — and `T: Equatable` bounds rejected exactly the types
the ruling's conditional rule admits. DQ29 (Design) ruled structural
conformance with explicit-impl override as the v1 semantics; this change
implements that ruling end-to-end (checker gate + bounds, diagnostics,
codegen, spec).
