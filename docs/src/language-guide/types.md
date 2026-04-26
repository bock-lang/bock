# Types

> This page is a stub. Full coverage of Bock's type system lives in
> [section 2 of the specification](../../../spec/sections/s02-types.md).

Bock has a static type system with bidirectional inference. Every
expression has a type known at compile time; explicit annotations are
usually optional but always allowed.

## Primitive Types

`Int`, `Float`, `Bool`, `String`, `Char`, and `()` (unit).

## Composite Types

- **Records** — named fields, value semantics.
- **Enums** — sum types with variants and payloads, exhaustively
  matched.
- **Tuples** — fixed-size heterogeneous sequences.
- **Lists, maps, sets** — generic collections from `std.collections`.

## Generics

```bock
fn first<T>(xs: List<T>) -> Option<T> {
  match xs {
    [head, ...] => Some(head)
    []          => None
  }
}
```

See `spec/sections/s02-types.md` for the full grammar of type
expressions, type-class constraints, and variance rules.
