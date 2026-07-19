# core.convert

Value-conversion abstractions: the `From`, `Into`, `TryFrom`, and
`Displayable` traits.

`From`, `Into`, and `Displayable` are in the prelude (§18.2);
`TryFrom`, the `ConvertError` type, and the sample conversion records
are imported explicitly:

```bock
use core.convert.{TryFrom, ConvertError, convert_error}
```

`From[T]`, `Into[T]`, and `TryFrom[T]` carry the source type as a trait
type argument, so a single type can implement `From[A]` and `From[B]`
independently. The compiler derives a blanket `Into[U] for T` from each
explicit `From[T] for U`, so **implementing `From` is sufficient to get
`.into()`**.

## Traits

### `From[T]`

```bock
public trait From[T] {
  fn from(value: T) -> Self
}
```

Infallible conversion *from* a source type `T` into the implementing
type. `impl From[Source] for Target` defines how a `Source` becomes a
`Target`; the compiler then derives the blanket `Into[Target] for
Source`, so `source.into()` works wherever a `Target` is expected. This
is the canonical way to add a conversion.

### `Into[T]`

```bock
public trait Into[T] {
  fn into(self) -> T
}
```

Infallible conversion of the implementing type *into* a target type `T`.
You rarely implement `Into` directly — an `impl From[A] for B`
automatically yields `Into[B] for A`. Implement `Into` explicitly only
when the reverse `From` is not desirable; an explicit `Into` always wins
over the derived blanket one.

### `TryFrom[T]`

```bock
public trait TryFrom[T] {
  fn try_from(value: T) -> Result[Self, ConvertError]
}
```

Fallible conversion *from* a source type `T`, returning a
[`ConvertError`](#converterror) when the value cannot be represented in
the implementing type. Use `TryFrom` for narrowing or parsing
conversions that can fail (e.g. parsing a `String` into an `Int`).
Unlike `From`, `TryFrom` is **not** blanket-reversed — there is no
`TryInto` in the v1 surface.

### `Displayable`

```bock
public trait Displayable {
  fn to_string(self) -> String
}
```

The human-readable string projection of a value — the conversion-module
counterpart to a `toString`/`Display` trait. `to_string` returns this
value's `String` representation. Implementing `Displayable` enables
`${expr}` string interpolation for the type (§18.5).

## Canonical primitive conversions

The compiler registers a fixed matrix of **lossless** primitive
conversions (plus the two fallible string-parsing ones), reachable in
**both** directions — the return-type-driven `(x).into()` form and the
associated-function `Prim.from(x)` / `Prim.try_from(s)` form:

| Conversion | `.into()` form | associated-call form |
|------------|----------------|----------------------|
| `Int → Float` | `let f: Float = (5).into()` | `Float.from(5)` |
| sized-int → wider / `Int` | `let n: Int = i8val.into()` | `Int.from(i8val)` |
| `Float32 → Float` | `let f: Float = f32val.into()` | `Float.from(f32val)` |
| `Char → String` | `let s: String = c.into()` | `String.from(c)` |
| `String → Int` (parse) | — | `Int.try_from(s)` |
| `String → Float` (parse) | — | `Float.try_from(s)` |

```bock
let pi: Float = Float.from(3)            // Int -> Float, == 3.0
let label: String = String.from('x')     // Char -> String, == "x"

use core.convert.{ConvertError}
fn parse(s: String) -> Result[Int, ConvertError] {
  Int.try_from(s)                        // Ok(42) for "42", Err(...) otherwise
}
```

`Prim.from(x)` yields the target primitive; `Prim.try_from(s)` yields
`Result[Prim, ConvertError]` (the `Err` payload is a
[`ConvertError`](#converterror) with a `message`). These conversions are
always available — no import is required for the `from`/`try_from`
themselves; only `ConvertError` (used in the `try_from` return type) is
imported.

**Lossy / narrowing conversions are excluded.** A narrowing direction
(e.g. `Int → Int8`) is rejected at compile time (`E4012`); express it
through a `TryFrom` instead. A `Prim.from`/`Prim.try_from` whose source
type has no canonical conversion to the target is likewise rejected with
`E4012`.

> The associated-call callees reachable in v1 are `Int`, `Float`, and
> `String` (the unsized primitive type names the resolver admits as bare
> type names); the sized targets (`Int64`, …) participate as `.into()`
> targets and as `from` *sources*.

## Functions

### `convert_error`

```bock
public fn convert_error(message: String) -> ConvertError
```

The ergonomic way to produce a conversion failure:
`convert_error("value out of range")`.

## Records

### `ConvertError`

```bock
public record ConvertError { message: String }
```

The error produced by a failed `TryFrom` conversion. Carries a
human-readable `message` describing why the conversion could not be
performed (an out-of-range value, an unparseable string, …).

### `Celsius` / `Fahrenheit`

```bock
public record Celsius { degrees: Float }
public record Fahrenheit { degrees: Float }
```

A paired, ready-to-use sample conversion: `impl From[Celsius] for
Fahrenheit` lets both `c.into()` and `Fahrenheit.from(c)` produce a
`Fahrenheit`. They demonstrate the parameterized-trait conversion
pattern you would mirror on your own types.
