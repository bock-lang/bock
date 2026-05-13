# Functions

> Stub. The authoritative reference is §6 (Declarations) of
> [`spec/bock-spec.md`](../../../spec/bock-spec.md).

Functions are the unit of abstraction in Bock. Declare with `fn`,
optionally annotate `pure`, and parameters always carry types.

```bock
pure fn square(n: Int) -> Int {
  n * n
}
```

## Pure vs. Effectful

- A function annotated `pure` is checked to perform no I/O, no
  network, no randomness, no time, and no shared-state mutation. The
  compiler rejects code that violates this contract.
- Without `pure`, a function's effect set is **inferred**. The
  inferred effects appear in tooling output and on hover in the
  editor.

## Pipes and First-Class Functions

```bock
let lengths = words |> map((w) => length(w)) |> filter((n) => n > 3)
```

The pipe operator `|>` desugars to function application; see
§7 Expressions of `spec/bock-spec.md`.

## Generics and Constraints

Generic parameters use angle brackets and may be constrained:

```bock
fn show<T: Display>(x: T) -> String { … }
```

See [Types](./types.md) and the spec for the full set of
trait-style constraints.
