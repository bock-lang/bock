# Standard Library

The Bock standard library is two-tiered (§18.1):

- **`core.*`** ships with the compiler. It is small, stable, and
  guaranteed to work on every target. This page documents the v1
  `core` surface.
- **`std.*`** packages are first-party libraries installed through the
  package manager (time/date, JSON, filesystem, HTTP, crypto, …). They
  can evolve independently of the language version and are documented
  separately as they ship.

The standard library is specified in §18 of
[`spec/bock-spec.md`](../../../spec/bock-spec.md). Docs explain; the
spec defines — for normative behavior, follow the section references.

## The v1 `core` surface

Eleven `core` modules ship in v1 (§18.3). Each ships its
**minimum-useful subset** — a curated, cross-target-proven surface,
not necessarily the full feature list §18.3 sketches for the module.
Items deferred to a future release are marked **Reserved for v1.x** on
each page.

A guiding principle runs through `core`: the modules **complement** the
built-in methods on prelude types rather than duplicate them. `String`,
`List`, `Map`, `Set`, `Optional`, and `Result` already carry built-in
*methods* that lower to each target's native operation (`s.to_upper()`,
`xs.contains(x)`, `o.map(f)`, `r.is_ok()`, …). The `core.*` modules add
only the operations those method sets do **not** cover — so you call
the built-in method when one exists, and reach for a `core` free
function when it does not.

| Module | What it provides |
| ------ | ---------------- |
| [`core.option`](./stdlib/core-option.md) | Free-function utilities over the built-in `Optional[T]`. |
| [`core.result`](./stdlib/core-result.md) | Free-function utilities over the built-in `Result[T, E]`. |
| [`core.collections`](./stdlib/core-collections.md) | List/Map utilities and a pure-Bock `SortedSet[T]`. |
| [`core.string`](./stdlib/core-string.md) | String utilities and a value-semantics `StringBuilder`. |
| [`core.iter`](./stdlib/core-iter.md) | The `Iterator`/`Iterable` traits and eager combinators. |
| [`core.compare`](./stdlib/core-compare.md) | `Ordering`, `Comparable`, `Equatable`, and `min`/`max`. |
| [`core.convert`](./stdlib/core-convert.md) | `From`, `Into`, `TryFrom`, `Displayable`. |
| [`core.error`](./stdlib/core-error.md) | The `Error` base trait and `SimpleError`. |
| [`core.effect`](./stdlib/core-effect.md) | Effect-system primitives and the standard `Log` effect. |
| [`core.time`](./stdlib/core-time.md) | `Duration`, `Instant`, the `Clock` effect, and `sleep`. |
| [`core.test`](./stdlib/core-test.md) | Assertions for `@test` functions (free + fluent). |

Four further `core` modules are **Reserved for v1.x** (§18.3) and have
no v1 surface: `core.types` (`BigInt`, `Decimal`), `core.math`
(constants and advanced numerics), `core.memory` (`Rc`/`Arc`), and
`core.concurrency` (`Channel`, `Mutex`, `RwLock`, …).

## The prelude

Some `core` names are **auto-imported** — available without any `use`
(§18.2). These are the prelude-visible re-exports of definitions that
live in the `core.*` modules:

- Primitive types: `Int`, `Float`, `Bool`, `String`, `Char`, `Void`,
  `Never`.
- Container and result types: `Optional`/`Some`/`None`,
  `Result`/`Ok`/`Err`, `List`, `Map`, `Set`, `Fn`.
- Time types: `Duration`, `Instant` (from `core.time`).
- `Ordering`/`Less`/`Equal`/`Greater` (from `core.compare`).
- Core traits: `Comparable`, `Equatable`, `Displayable`, `Into`,
  `From`, `Iterator`, `Iterable` (and others reserved for v1.x).
- Utility functions: `print`, `println`, `debug`, `assert`, `todo`,
  `unreachable`, `sleep`.

Everything else in `core` is imported explicitly, e.g.
`use core.option.{or_else, filter}`.

## Importing a `core` module

`core.*` sources are embedded in the compiler and load automatically
under `bock check`, `bock run`, and `bock build`. Import the items you
need with a `use`:

```bock
use core.option.{or_else, get_or}
use core.collections.{SortedSet, from_list, to_list}
use core.test.{assert_eq, expect}
```

> **`bock test` note.** A `@test` body run under `bock test` cannot yet
> `use core.test.{...}` — the test runner compiles only the single user
> file and does not load embedded `core`. Use the interpreter's built-in
> `assert(cond)` / `expect(x).to_equal(y)` inside `@test` functions for
> now. The `core.test` *module* documented here is reachable from
> `bock check`/`run`/`build`. See [`core.test`](./stdlib/core-test.md).

## Per-module pages

- [`core.option`](./stdlib/core-option.md)
- [`core.result`](./stdlib/core-result.md)
- [`core.collections`](./stdlib/core-collections.md)
- [`core.string`](./stdlib/core-string.md)
- [`core.iter`](./stdlib/core-iter.md)
- [`core.compare`](./stdlib/core-compare.md)
- [`core.convert`](./stdlib/core-convert.md)
- [`core.error`](./stdlib/core-error.md)
- [`core.effect`](./stdlib/core-effect.md)
- [`core.time`](./stdlib/core-time.md)
- [`core.test`](./stdlib/core-test.md)
