# Stdlib-surface Design ratifications + DQ31 container equality

**Date:** 2026-06-15
**Affects:** §18.2 (prelude), §18.3 (`core.convert`, `core.iter`), §18.5 (trait-language integration), §6.5 (traits)
**Type:** clarification (Bucket A ratifications already carried in spec) + addition (DQ31)

## Change

The operator/Design ratification pass of 2026-06-15 resolved the standing
non-blocking stdlib-surface design questions. Most were **already reconciled in
the spec** (the spec was kept ahead of the escalation register); this entry
records the Design ratification that closes the escalations, plus the two
genuinely new normative touches.

**Ratified as shipped (no spec text change — the spec already carried these):**

- **DQ10** — the normative primitive×core-trait conformance matrix in §18.5
  (`Equatable`: Int/Float/String/Bool/Char + sized numerics; `Comparable`:
  same minus `Bool`; `Displayable`: all; `Hashable`: all minus `Float`) with the
  three IEEE/Bool caveats — ratified.
- **DQ12 / DQ14 / DQ15 / DQ24** — the `core.iter` v1 floor in §18.3
  (generic `Iterator[T]`/`Iterable[T]`, eager `List`-returning combinators, the
  normative six `to_list`/`count`/`fold`/`map`/`filter`/`take`, concrete dispatch
  on `ListIterator[T]`, inherent-`next` satisfaction, the dual native/desugar
  iteration model, and the v1.x-reserved list incl. associated-type iterators,
  lazy/generic-bound combinators, and `iter() -> Self`) — ratified. The §6.5
  associated-type `Collection` example is already annotated as illustration only.
- **DQ13** — `TryFrom` and `Error` are prelude members (§18.2). The prelude list
  already includes both; this ratifies them as auto-imported (both are
  `core.*`-defined and fundamental).

**New normative text:**

- **DQ11** — §18.3 `core.convert` now pins the v1 conversion floor: the canonical
  `From` matrix (`Int→Float`, signed widening, `Float32→Float`, `Char→String`),
  `TryFrom[String]` for `Int`/`Float`, a fixed `ConvertError`, **no** `TryInto`,
  and **unsealed** primitive→primitive conversions. Additive/refineable in v1.x.
- **DQ31** — §18.5 gains a normative rule for container `==` when an element type
  carries an explicit `impl Equatable`: **container equality defers to the
  element type's `Equatable` conformance** (the structural default for
  structural-default elements; the element's own `eq` for custom-impl elements),
  with a **target-independent observable result**. The codegen specialization
  (native deep-equality for structural-default elements, element-`eq` loop only
  where an element carries a custom impl) is an **optimization note, not
  normative**; provenance composes recursively; `Map`/`Set` key-matching and
  membership run on the element's `eq`; the §18.5 poison rule still rejects `==`
  on a container of a non-`Equatable` element on either path.

## Rationale

DQ10–DQ15/DQ24 were filed as "ratify the shipped minimum-useful floor" and the
impl/spec had already converged; the ratification closes the escalations and the
standing non-blocking Design queue. DQ31 was the one open semantic fork: option
(a) read consistently with the DQ29 ruling (explicit impl wins) across the
container boundary — a type has one equality. The three-way escalation framing
conflated semantics with codegen; separating them makes the apparent cost of (a)
fall only on containers whose elements actually carry a custom impl, while the
common structural-default case keeps each backend's native equality. (b) was
rejected as a correctness bug (silently ignores element impls; re-opens
cross-target divergence) and (c) as amputating the feature. Full ruling:
`tracking/design-questions.md` DQ31.

## Migration

None. DQ10/DQ12/DQ13/DQ14/DQ15/DQ24 describe already-shipped behavior. DQ11
pins the existing `core.convert` floor. DQ31 pins behavior that was previously
divergent across targets in one corner (a custom-`eq` element inside a container);
the implementation follow-up (`tracking/queue.md`, DQ31 item) makes all five
targets agree on the normative result and adds the ×5 fixtures.
