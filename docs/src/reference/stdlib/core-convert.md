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
Fahrenheit` lets both `c.into()` and `Fahrenheit::from(c)` produce a
`Fahrenheit`. They demonstrate the parameterized-trait conversion
pattern you would mirror on your own types.
