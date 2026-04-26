# Effects

> Stub. Full coverage in
> [section 8 of the specification](../../../spec/sections/s08-effects.md).

Bock tracks effects on every function. The compiler infers an effect
set from the body of each function and propagates it to every caller.

## The Effect Set

```
{ io, net, rand, time, mut, panic, async, log }
```

Each effect represents a class of observable behavior. A function's
effect set is its inferred upper bound — the union of all the
effects its body may perform.

## Annotations

- `pure fn …` — must be empty effect set; compiler enforces.
- `fn @[io, net] …` — explicit annotation; checked against inferred
  set.

## Why It Matters

- Test harnesses can demand `pure` for property-based tests.
- The CLI reports effects per function in `bock check --explain`.
- Codegen uses the effect set to choose between sync and async
  emission per target.

## Capturing and Propagation

A function calling an effectful function inherits those effects
unless it consumes them in a controlled way (effect handlers; see
`spec/sections/s09-context.md`).
