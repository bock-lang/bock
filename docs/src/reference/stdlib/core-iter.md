# core.iter

The iteration abstractions: the `Iterator` and `Iterable` traits, the
concrete `ListIterator[T]`, and a set of eager combinators.

`Iterator` and `Iterable` are in the prelude (§18.2). A user type opts
into `for`-loop iteration by implementing `Iterable`, whose single
`iter` method yields a `ListIterator`. The compiler desugars `for x in
iterable { ... }` over an `Iterable` type into the manual drive
`loop { match it.next() { Some(x) => ...; None => break } }` (§18.5).

The free functions and the iterator type are imported explicitly:

```bock
use core.iter.{list_iter, to_list, map, filter, fold, take, count}
```

> **v1 design.** The v1 surface is intentionally minimal: there is a
> single concrete iterator type (`ListIterator`) rather than
> `impl Trait` / existential iterators, and the combinators are
> **eager** and **`List`-returning** — not lazy or streaming. Each
> combinator drains its iterator and returns a `List`.

## Traits

### `Iterator[T]`

```bock
public trait Iterator[T] {
  fn next(self) -> Optional[T]
}
```

The interface for types that produce a sequence of values one at a time.
An iterator is *driven* by repeatedly calling `next`: each call yields
`Some(value)` for the next element, or `None` once the sequence is
exhausted.

### `Iterable[T]`

```bock
public trait Iterable[T] {
  fn iter(self) -> ListIterator[T]
}
```

The interface for types that can be iterated, producing a
[`ListIterator`](#listiteratort). Implementing `Iterable` is how a user
type opts into `for`-loop support. `iter` returns a fresh iterator over
the value's elements. In the v1 surface the iterator type is fixed to
the concrete `ListIterator`, so an `Iterable[T]` always iterates a
`List[T]`.

## Records

### `ListIterator[T]`

```bock
public record ListIterator[T] { xs: List[T], cursor: Int }
```

The single concrete iterator type in the v1 surface: a cursor over a
`List[T]`. It walks `xs` from `cursor` forward, yielding each element
once via `next`. It is the iterator every `Iterable` produces and the
iterator every combinator in this module consumes. Construct one with
[`list_iter`](#list_iter) rather than by hand.

| Field | Type | Description |
| ----- | ---- | ----------- |
| `xs` | `List[T]` | The backing list being iterated. |
| `cursor` | `Int` | The index of the next element to yield. |

## Functions

### `list_iter`

```bock
public fn list_iter[T](xs: List[T]) -> ListIterator[T]
```

Constructs a `ListIterator` positioned at the start of `xs`. The
ergonomic way to obtain an iterator over a list:
`list_iter([1, 2, 3]).next()`.

### `to_list`

```bock
public fn to_list[T](it: ListIterator[T]) -> List[T]
```

Drains `it` into a `List[T]`, preserving order.

### `count`

```bock
public fn count[T](it: ListIterator[T]) -> Int
```

Counts the elements produced by `it`, draining it.

### `fold`

```bock
public fn fold[T, A](it: ListIterator[T], init: A, f: Fn(A, T) -> A) -> A
```

Folds `it` left-to-right into a single accumulator, starting from
`init`. For each element `x`, the accumulator becomes `f(acc, x)`; the
final accumulator is returned once the iterator is exhausted.

### `map`

```bock
public fn map[T, U](it: ListIterator[T], f: Fn(T) -> U) -> List[U]
```

Eagerly maps `f` over `it`, returning the transformed elements as a
`List[U]` in order.

### `filter`

```bock
public fn filter[T](it: ListIterator[T], pred: Fn(T) -> Bool) -> List[T]
```

Eagerly keeps the elements of `it` for which `pred` returns `true`,
returning them as a `List[T]` in order.

### `take`

```bock
public fn take[T](it: ListIterator[T], n: Int) -> List[T]
```

Eagerly takes up to the first `n` elements of `it`, returning them as a
`List[T]`. Fewer than `n` elements are returned if the iterator is
shorter.

## Reserved for v1.x

Lazy / streaming iterators, `impl Trait` / existential iterator return
types, and in-place mutation are outside the v1 floor. The combinators
are eager and return `List`s in v1.
