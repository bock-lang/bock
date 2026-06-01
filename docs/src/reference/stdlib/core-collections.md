# core.collections

The v1 collection surface: free-function utilities that complement the
built-in `List`/`Map`/`Set` methods, plus a pure-Bock `SortedSet[T]`.

Per §18.3, `List`, `Map`, and `Set` are **built-in types** whose methods
lower to each target's native collection op. `core.collections` ships
only what genuinely *complements* that method set — never a duplicate of
it. Import what you need:

```bock
use core.collections.{sum, max_of, min_of, unique, reversed, get_or}
use core.collections.{SortedSet, empty, from_list, add, union, intersection}
use core.collections.{size, is_empty, contains, to_list}
```

## What is built in (and therefore not shipped here)

These lower as built-in **methods** on the value — use them directly:

- **`List`** (read-only): `len`/`length`/`count`, `is_empty`, `get`
  (→ `Optional[T]`), `contains`, `first`/`last`, `concat`, `index_of`,
  `join`.
- **`Set`**: `add`/`remove`, `union`/`intersection`/`difference`,
  `contains`, `is_subset`/`is_superset`, `len`, `is_empty`, `to_list`,
  `filter`/`map`.
- **`Map`**: `get` (→ `Optional[V]`), `set`/`delete`, `merge`,
  `contains_key`, `keys`/`values`/`entries`/`to_list`, `len`,
  `is_empty`, `filter`.

Because `Set` already ships `union`/`intersection`/`difference` and
`Map` already ships `merge`, those set/map-algebra helpers are not
re-exported here.

## List/Map utilities

### `sum`

```bock
public fn sum(xs: List[Int]) -> Int
```

Returns the sum of the integers in `xs` (`0` for an empty list).
`sum([1, 2, 3])` is `6`. (There is no built-in `List.sum`.)

### `max_of`

```bock
public fn max_of[T: Comparable](xs: List[T]) -> Optional[T]
```

Returns the greatest element of `xs`, or `None` when `xs` is empty,
dispatching through the type's `compare` impl. `max_of([key(1),
key(3)])` is `Some(key(3))`. The `Optional` return makes the empty case
total (no panic / sentinel).

### `min_of`

```bock
public fn min_of[T: Comparable](xs: List[T]) -> Optional[T]
```

The mirror of [`max_of`](#max_of): the least element, or `None` for an
empty list.

### `unique`

```bock
public fn unique[T: Comparable](xs: List[T]) -> List[T]
```

Returns `xs` with later duplicates removed, preserving first-seen order,
using `compare` for equality. `unique([key(1), key(1), key(2)])` is
`[key(1), key(2)]`. (There is no built-in `List.unique`.)

### `reversed`

```bock
public fn reversed[T](xs: List[T]) -> List[T]
```

Returns the elements of `xs` in reverse order. `reversed([1, 2, 3])` is
`[3, 2, 1]`. (There is no built-in `List.reversed`.)

### `get_or`

```bock
public fn get_or(m: Map[String, Int], k: String, fallback: Int) -> Int
```

Returns the value `m` maps `k` to, or `fallback` when `k` is absent.
`get_or({"a": 1}, "a", 0)` is `1`; `get_or({"a": 1}, "z", 0)` is `0`.
Names the common value-or-default lookup over the built-in `Map.get`
(which returns an `Optional[V]`).

> Typed concretely as `Map[String, Int]` rather than generic
> `Map[K, V]`: `String`-keyed, `Int`-valued maps are the common case and
> the surface that lowers on all five targets in v1.

## SortedSet

A `T: Comparable` set kept **sorted ascending and deduplicated**, backed
by a single sorted, dedup `List[T]`. Its operations are
**value-semantics**: each returns a *new* `SortedSet` rather than
mutating in place.

The query operations are offered as **free functions**
(`size`/`is_empty`/`contains`/`to_list`) rather than methods, so they do
not collide with the built-in collection-length/membership method
lowering that fires on any receiver.

### `SortedSet[T]`

```bock
public record SortedSet[T] { items: List[T] }
```

Construct one with [`empty`](#empty) or [`from_list`](#from_list), grow
it with [`add`](#add), combine sets with [`union`](#union) /
[`intersection`](#intersection), and query it with the free functions
below. `items` holds the backing elements, kept sorted ascending and
deduplicated.

### Constructors

```bock
public fn empty[T]() -> SortedSet[T]
public fn from_list[T: Comparable](xs: List[T]) -> SortedSet[T]
```

`empty` builds an empty set (`add(empty(), x)`). `from_list` sorts and
deduplicates `xs` — `from_list([key(3), key(1), key(1)])` has items
`[key(1), key(3)]`, independent of input order.

### Builders

```bock
public fn add[T: Comparable](s: SortedSet[T], x: T) -> SortedSet[T]
public fn union[T: Comparable](a: SortedSet[T], b: SortedSet[T]) -> SortedSet[T]
public fn intersection[T: Comparable](a: SortedSet[T], b: SortedSet[T]) -> SortedSet[T]
```

- `add` returns a new set containing every element of `s` plus `x`,
  inserted at its sorted position. If `x` is already present (an `Equal`
  element, per `compare`), `s` is returned unchanged.
- `union` returns every element in either set, sorted and deduplicated.
- `intersection` returns the elements present in **both** sets.

### Queries

```bock
public fn size[T](s: SortedSet[T]) -> Int
public fn is_empty[T](s: SortedSet[T]) -> Bool
public fn contains[T: Comparable](s: SortedSet[T], x: T) -> Bool
public fn to_list[T](s: SortedSet[T]) -> List[T]
```

- `size` is the number of elements; `is_empty` is `true` when there are
  none.
- `contains` is `true` when `s` holds an element `Equal` to `x` (per
  `compare`).
- `to_list` returns the elements as a sorted-ascending `List[T]` — the
  terminal view of a `SortedSet`.

## Reserved for v1.x

The following collection types are listed by §18.3 but are **not** in
the v1 surface: `Deque`, `SortedMap`, `Stack`, `Queue`, `BitSet`, and
fixed-size `Array[T, N]`. Richer data structures (Trie, LRU cache, bloom
filter, priority queue) ship in the separate `std.collection_ext`
package.
