# core.compare

Ordering and equality abstractions: the `Ordering` enum, the
`Comparable` and `Equatable` traits, and generic `min`/`max` helpers.

`Ordering`, `Less`, `Equal`, `Greater`, `Comparable`, and `Equatable`
are in the prelude (§18.2) — user code can name them to write a
`compare`/`eq` impl without an import. The free functions are imported
explicitly:

```bock
use core.compare.{max, min, key}
```

Implementing these traits opts a type into operator syntax: `Equatable`
enables `==`/`!=`, and `Comparable` enables `<`/`>`/`<=`/`>=` (§18.5).

This gating is enforced at type-check time. Using an ordering operator
(`<`, `>`, `<=`, `>=`) on a user type that does **not** `impl Comparable`
is rejected with an `E4005` diagnostic suggesting the missing impl:

```bock
record Point { x: Int, y: Int }

let a: Point = Point { x: 1, y: 2 }
let b: Point = Point { x: 3, y: 4 }
let _ = a < b
//      ^ error[E4005]: type `Point` does not implement `Comparable`; the
//        `<`/`>`/`<=`/`>=` operators require it — implement `Comparable` for `Point`
```

Adding `impl Comparable for Point` (a `compare(self, other: Point) -> Ordering`)
makes the same expression check clean and run correctly on every target: each
ordering operator lowers through the type's `compare`, mapping `a < b` to
`compare == Less`, `a > b` to `== Greater`, `a <= b` to `!= Greater`, and
`a >= b` to `!= Less`. The primitive conformances (`Int`, `Float`, `String`,
`Char`, the sized numerics — **not `Bool`**) are gated the same way through
`core`'s sealed conformances (and use the target's native ordering operator).

`==`/`!=` gate behind `Equatable` the same way, but `Equatable` has a
**structural default** (§18.5, DQ29): a record is `Equatable` iff every field
type is, an enum iff every payload type of every variant is (both recursive),
and the compound built-ins compose conditionally — `List[T]`/`Set[T]`/
`Optional[T]` iff `T`, `Map[K, V]` iff `K` and `V`, `Result[T, E]` iff `T` and
`E`, tuples iff all components (`Map`/`Set` equality is order-independent). A
generic type decides per instantiation (`Pair[Int, String]` is `Equatable`;
`Pair[Int, Fn(Int) -> Int]` is not). So `Point` above supports `==` with **no
impl at all** — field-wise equality — and a record of `Equatable` fields also
satisfies an `Equatable` *bound* (`fn dedupe[T: Equatable](items: List[T])`).
A non-`Equatable` leaf poisons the type: `==` on a record holding an `Fn`
field is rejected with an `E4015` diagnostic naming the offending field path
and type:

```bock
record Callback { name: String, handler: Fn(Int) -> Int }

let _ = a == b
//      ^ error[E4015]: type `Callback` does not implement `Equatable`; the
//        `==`/`!=` operators require it — field `handler` of type
//        `Fn(Int) -> Int` is not Equatable
```

Two boundaries: **classes never conform structurally** (a class gets `==` only
via an explicit `impl Equatable`), and an **explicit impl suppresses the
structural default** — its `eq` then defines the type's equality everywhere
`==`/`!=` apply, on every target. The structural default does **not** extend
to `Comparable` (ordering has no canonical structural meaning) or `Hashable`
(deferred to the v1.x derive design pass). `Float` fields keep IEEE semantics:
a record holding `NaN` is `!=` itself.

## Enums

### `Ordering`

```bock
public enum Ordering { Less, Equal, Greater }
```

The result of comparing two values: whether the first is less than,
equal to, or greater than the second. `Ordering` is the return type of
`Comparable.compare`; matching on its three variants is the canonical
way to branch on a comparison.

| Variant | Meaning |
| ------- | ------- |
| `Less` | The first value is less than the second. |
| `Equal` | The two values are equal. |
| `Greater` | The first value is greater than the second. |

## Traits

### `Equatable`

```bock
public trait Equatable {
  fn eq(self, other: Self) -> Bool
}
```

The interface for types whose values can be compared for equality. The
`other` operand has the implementing type, so an impl for `T` compares a
`T` against a `T`. `eq` returns `true` when `self` and `other` are
considered equal.

Records and enums whose fields/payloads are all `Equatable` conform
**structurally** (§18.5, DQ29) — they support `==`/`!=` with field-wise
equality and satisfy `Equatable` bounds without an impl. Write an explicit
`impl Equatable` to **override** that default with custom equality (the impl
wins; `==`/`!=` dispatch through its `eq`), to opt a **class** into equality
(classes never conform structurally), or to define equality for a type the
structural rule rejects.

### `Comparable`

```bock
public trait Comparable {
  fn compare(self, other: Self) -> Ordering
}
```

The interface for types whose values have a total ordering. A type
implements `Comparable` by defining how one of its values orders
relative to another, returning an `Ordering`. Implementing `Comparable`
enables `<`, `>`, `<=`, and `>=` on the type (§18.5).

## Functions

### `max`

```bock
public fn max[T: Comparable](a: T, b: T) -> T
```

Returns the greater of two `Comparable` values, preferring `b` on a tie.
Dispatches through the type's `compare` impl.

### `min`

```bock
public fn min[T: Comparable](a: T, b: T) -> T
```

Returns the lesser of two `Comparable` values, preferring `b` on a tie.
Dispatches through the type's `compare` impl.

### `key`

```bock
public fn key(value: Int) -> Key
```

Constructs a [`Key`](#key) from an integer value: `key(3)`.

## Records

### `Key`

```bock
public record Key { value: Int }
```

A minimal, ready-to-use `Comparable`/`Equatable` value wrapping a single
integer key (`value` — the integer it orders and compares by). Use `Key`
(or mirror its impls on your own type) when you need a concrete
comparable value. Construct one with [`key`](#key).
