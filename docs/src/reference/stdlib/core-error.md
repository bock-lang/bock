# core.error

The foundational error abstraction: the `Error` base trait, a
ready-to-use `SimpleError`, and the `error` constructor.

```bock
use core.error.{Error, SimpleError, error}
```

## Traits

### `Error`

```bock
public trait Error {
  fn message(self) -> String
}
```

The common interface for error values. Any type that can describe itself
as a human-readable message can implement `Error`, so functions that
work with failures can accept `Error` uniformly rather than a concrete
error type. `message` returns this error's human-readable message.

> The v1 surface is `message(self) -> String` **only.**

## Records

### `SimpleError`

```bock
public record SimpleError { message: String }
```

A minimal, ready-to-use `Error` carrying a single message string. Use
`SimpleError` (typically via the [`error`](#error) constructor) when you
need an error value but do not have a domain-specific error type.

## Functions

### `error`

```bock
public fn error(message: String) -> SimpleError
```

Constructs a `SimpleError` from a message string — the ergonomic way to
create an error value: `error("file not found")`.

## Reserved for v1.x

The v1 `Error` surface is deliberately just `message(self) -> String`.
Deferred to the v1.x error-ergonomics bundle (§18.3), because they
depend on trait objects (`Error` used as a type, not as a bound):

- Error chaining (`cause` / `source`).
- An `Error: Displayable` supertrait.
- Context / wrapping helpers.
