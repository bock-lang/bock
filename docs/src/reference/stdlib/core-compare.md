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
makes the same expression check clean. The primitive conformances (`Int`,
`Float`, `String`, `Char`, the sized numerics — **not `Bool`**) are gated the
same way through `core`'s sealed conformances.

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
considered equal. Implementing `Equatable` enables `==` and `!=` on the
type (§18.5).

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
