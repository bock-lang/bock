# core.result

Free-function utilities over the built-in `Result[T, E]`.

`Result[T, E]`, `Ok`, and `Err` are prelude types, and a core set of
*methods* already lowers on every target: `is_ok`, `is_err`, `unwrap`,
`unwrap_or`, `map`, and `map_err`. Call those directly on the value.
`core.result` ships only the **complementary** combinators the built-in
method set does not cover — it does not re-expose `is_ok`/`map`/etc.

```bock
use core.result.{and_then, unwrap_or_else, unwrap_err, ok, err}
```

## Functions

### `and_then`

```bock
public fn and_then[T, E, U](r: Result[T, E], f: Fn(T) -> Result[U, E]) -> Result[U, E]
```

Chains a fallible computation onto the `Ok` arm of `r`. Returns `f(x)`
when `r` is `Ok(x)`, and re-wraps the original `Err(e)` unchanged
otherwise. This is the `Result` counterpart to `Optional.flat_map`: use
it to sequence operations that each return a `Result`, short-circuiting
on the first `Err`.

### `unwrap_or_else`

```bock
public fn unwrap_or_else[T, E](r: Result[T, E], f: Fn(E) -> T) -> T
```

Returns the `Ok` payload of `r`, or computes a fallback from the `Err`
payload. The lazy companion to the built-in `r.unwrap_or(default)`:
`f(e)` is invoked only when `r` is `Err`, so an expensive fallback is
built only when needed.

### `unwrap_err`

```bock
public fn unwrap_err[T, E](r: Result[T, E], fallback: E) -> E
```

Returns the `Err` payload of `r`, or `fallback` when `r` is `Ok`. The
`Err`-side mirror of the built-in `r.unwrap()`: use it to read the error
of a `Result` you expect to have failed, supplying a `fallback` for the
`Ok` case rather than aborting.

### `ok`

```bock
public fn ok[T, E](r: Result[T, E]) -> Optional[T]
```

Projects `r` onto its `Ok` arm, discarding any error. Returns `Some(x)`
when `r` is `Ok(x)`, and `None` when `r` is `Err(_)`. The idiomatic way
to turn a `Result` into an `Optional` when the error is not needed.

### `err`

```bock
public fn err[T, E](r: Result[T, E]) -> Optional[E]
```

Projects `r` onto its `Err` arm, discarding any success value. Returns
`Some(e)` when `r` is `Err(e)`, and `None` when `r` is `Ok(_)` — the
complement of [`ok`](#ok).

## Reserved for v1.x

Shapes that would duplicate a built-in method are intentionally **not**
provided — call the method directly: `r.is_ok()`, `r.is_err()`,
`r.unwrap()`, `r.unwrap_or(default)`, `r.map(f)`, `r.map_err(f)`.
