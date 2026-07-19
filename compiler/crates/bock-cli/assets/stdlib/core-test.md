# core.test

The assertion surface for Bock's `@test` functions: a free-function
assertion API and a fluent matcher API, fully overlapping. The fluent
layer is powered by the free functions — every `Expectation` matcher
delegates to the matching `assert_*` function.

Every assertion is built over the §18.2 prelude builtin `assert(cond)`,
which lowers on all five targets and, under `bock test`, raises the
assertion-failure path the runner reports as a failing test.

```bock
use core.test.{assert_true, assert_false, assert_eq, assert_ne}
use core.test.{assert_some, assert_none, assert_ok, assert_err, fail}
use core.test.{expect, expect_bool, Expectation, BoolExpectation}
```

> **`bock test` does not yet load this module.** Under `bock test`, a
> `@test` body cannot `use core.test.{...}` — the runner compiles only
> the single test file and does not load embedded `core`. Use the
> interpreter's built-in `assert(cond)` and `expect(x).to_equal(y)`
> inside `@test` functions for now. The `core.test` *module* documented
> here is reachable from `bock check` / `run` / `build` contexts. (See
> the [Standard Library](../stdlib.md) overview.)

## Free-function assertions

### Boolean

```bock
public fn assert_true(cond: Bool) -> Void
public fn assert_false(cond: Bool) -> Void
```

Assert that `cond` is `true` / `false`, failing the test otherwise.

### Equality

```bock
public fn assert_eq[T: Equatable](actual: T, expected: T) -> Void
public fn assert_ne[T: Equatable](actual: T, expected: T) -> Void
```

Assert that `actual` equals / does not equal `expected`, failing the
test otherwise. Generic over any `T: Equatable`, comparing with the `eq`
method (`actual.eq(expected)`) so the comparison dispatches through the
`Equatable` bound.

> **Reach: user `Equatable` types ×5; primitives via `==`.** `assert_eq`
> / `assert_ne` lower on all five targets when `T` is a **user type that
> implements `Equatable`**. For **primitive** equality (`Int`, `String`,
> …), prefer `assert_true(a == b)` with concrete operands — a primitive
> reached through the generic bound does not lower on the static targets.
> (Under `bock test`'s interpreter, `assert_eq` works for primitives
> too.)

### Optional / Result

```bock
public fn assert_some[T](o: Optional[T]) -> Void
public fn assert_none[T](o: Optional[T]) -> Void
public fn assert_ok[T, E](r: Result[T, E]) -> Void
public fn assert_err[T, E](r: Result[T, E]) -> Void
```

Assert that `o` is `Some(_)` / `None`, or that `r` is `Ok(_)` / `Err(_)`,
failing the test otherwise.

### Unconditional failure

```bock
public fn fail(message: String) -> Void
```

Unconditionally fails the current test — the escape hatch for reaching a
branch that should never execute. The `message` documents intent at the
call site.

## Fluent matchers

The fluent API wraps a value in an `Expectation` / `BoolExpectation`,
whose matcher methods forward to the free `assert_*` functions.

### Equality — `expect`

```bock
public fn expect[T: Equatable](value: T) -> Expectation[T]
public record Expectation[T: Equatable] { value: T }
```

`expect(x).to_equal(y)` / `expect(x).to_not_equal(y)`. `T` is bounded
`Equatable` so the matchers delegate to [`assert_eq`](#equality) /
`assert_ne`; the same user-type-vs-primitive reach applies (for
primitive equality use `assert_true(a == b)`).

| Method | Delegates to |
| ------ | ------------ |
| `to_equal(other)` | `assert_eq` |
| `to_not_equal(other)` | `assert_ne` |

### Booleans — `expect_bool`

```bock
public fn expect_bool(value: Bool) -> BoolExpectation
public record BoolExpectation { value: Bool }
```

`expect_bool(b).to_be_true()` / `expect_bool(b).to_be_false()`. Boolean
assertions use a separate, **non-generic** `BoolExpectation` (rather than
living on `expect`) because `Bool` is not `Equatable` behind a generic on
the static targets.

| Method | Delegates to |
| ------ | ------------ |
| `to_be_true()` | `assert_true` |
| `to_be_false()` | `assert_false` |

> **Go note.** On Go, bind the expectation to a `let` before calling a
> matcher (`let e = expect(k); e.to_equal(k2)`) — Go method receivers are
> pointers and cannot be called on the non-addressable result of a
> function call. The other four targets accept the direct
> `expect(x).to_equal(y)` chain.

## Reserved for v1.x

The v1 floor is **assertions only**. Listed by §18.3 but deferred:

- **BDD grouping** (`describe` / `it` / `context` nesting). `@test`
  functions (§15.4) are the v1 grouping unit.
- **Mocking** — use the effect-handler pattern (§10.4: swap a record
  `impl` of an effect for a test double, installed via `handling`) as
  the v1 idiom for test doubles.
- **Property testing** and **snapshot testing** (the latter ships in
  `std.testing`).
- **Ergonomic matcher extras** beyond the floor (`to_throw`,
  `to_contain`, numeric/collection matchers, custom failure messages).

**Benchmarking is not part of `core.test`.** Performance benchmarking is
delegated to target-native tools (`cargo bench`, `pytest-benchmark`,
`go test -bench`, …) per §20.4; `core.test` owns correctness assertions,
not performance measurement.
