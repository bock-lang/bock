# core.option

Free-function utilities over the built-in `Optional[T]`.

`Optional[T]`, `Some`, and `None` are prelude types, and a core set of
*methods* already lowers on the tagged value: `is_some`, `is_none`,
`unwrap`, `unwrap_or`, `map`, and `flat_map`. Call those directly on the
value. `core.option` ships only the free-function utilities that
**complement** that method set — it deliberately does not re-expose
`is_some`/`is_none`/`map`/`unwrap_or`, which already exist as methods.

```bock
use core.option.{or_else, filter, to_list, count, get_or}
```

## Functions

### `or_else`

```bock
public fn or_else[T](o: Optional[T], alt: Optional[T]) -> Optional[T]
```

Returns `o` when it is `Some`, otherwise the alternative `alt`. Unlike
the built-in `unwrap_or` method (which collapses to a bare `T`),
`or_else` keeps the result an `Optional[T]`, so fallbacks chain
(`or_else(or_else(a, b), c)`) before a final extraction. Both operands
are evaluated eagerly.

### `filter`

```bock
public fn filter[T](o: Optional[T], pred: Fn(T) -> Bool) -> Optional[T]
```

Keeps the payload of `o` only when `pred` returns `true`. Returns
`Some(x)` when `o` is `Some(x)` and `pred(x)` holds, else `None`. (The
built-in `filter` method is on `List`/`Map`/`Set`, not `Optional`, so
this is a genuine addition.)

### `to_list`

```bock
public fn to_list[T](o: Optional[T]) -> List[T]
```

Views `o` as a list: `[x]` when `Some(x)`, `[]` when `None`. The
single-element / empty-list bridge between the `Optional` and `List`
worlds.

### `count`

```bock
public fn count[T](o: Optional[T]) -> Int
```

Counts the payloads in `o`: `1` when `Some`, `0` when `None`. Useful for
tallying presence across a collection of options by summing their
counts.

### `get_or`

```bock
public fn get_or[T](o: Optional[T], fallback: T) -> T
```

Extracts the payload of `o`, or returns `fallback` when `o` is `None`. A
free-function sibling of the built-in `unwrap_or` method with an eager
default, kept because the free form composes uniformly with the rest of
this module and is reachable in expression positions where the method
form on a complex receiver is awkward.

## Reserved for v1.x

Helpers that would duplicate an existing built-in method are
intentionally **not** provided — use the method directly: `o.is_some()`,
`o.is_none()`, `o.map(f)`, `o.flat_map(f)` (the `and_then` shape), and
`o.unwrap_or(default)`. A lazy-default `unwrap_or_else(o, f: Fn() -> T)`
is also deferred, since zero-argument closures are outside the proven v1
cross-target surface.
