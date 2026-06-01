# core.time

Monotonic time primitives, available on every target: the `Duration`
and `Instant` types, the `Clock` effect, and the prelude `sleep`
function.

Unlike the other v1 `core` modules, `core.time` is a **compiler
builtin** — `Duration` and `Instant` are registered in the compiler's
primitive registry and lowered inline by every backend; there is no
`stdlib/core/time/` Bock source. `Duration` and `Instant` are in the
prelude (§18.2), so they are available without any `use`, and so is the
`sleep` function. This module is specified in §18.3.1 of
[`spec/bock-spec.md`](../../../../spec/bock-spec.md).

`core.time` owns the `Clock` effect; the first-party `std.time` package
provides the default handler and extends `core.time` with wall-clock
operations, dates, and timezones.

## Types

### `Duration`

A span of time. Internally stored as `Int64` nanoseconds, giving a range
of approximately ±292 years. `Duration` implements `Comparable` and
`Equatable`.

**Constructors:**

```bock
Duration.zero()        -> Duration
Duration.nanos(n: Int)   -> Duration
Duration.micros(n: Int)  -> Duration
Duration.millis(n: Int)  -> Duration
Duration.seconds(n: Int) -> Duration
Duration.minutes(n: Int) -> Duration
Duration.hours(n: Int)   -> Duration
```

**Methods:**

```bock
as_nanos()   -> Int
as_millis()  -> Int
as_seconds() -> Int
is_zero()    -> Bool
is_negative() -> Bool
abs()        -> Duration
```

**Arithmetic** (operator overloads):

```bock
Duration + Duration -> Duration
Duration - Duration -> Duration
Duration * Int      -> Duration    // scalar multiplication
Duration / Int      -> Duration    // scalar division
```

```bock
let total = Duration.seconds(2) + Duration.millis(500)
println("${total.as_millis()}")     // 2500

let scaled = Duration.millis(500) * 3
println("${scaled.as_millis()}")    // 1500

let back = Duration.zero() - Duration.minutes(1)
println("${back.abs().as_seconds()}")  // 60
```

### `Instant`

A monotonic point in time. Comparable within a single process run; not
comparable across processes or across reboots.

**Operations** (these require the `Clock` effect — see below):

```bock
Instant.now()                            -> Instant with Clock
instant.elapsed()                        -> Duration with Clock
instant.duration_since(earlier: Instant) -> Duration
```

**Arithmetic** (operator overloads):

```bock
Instant - Instant  -> Duration    // sugar for duration_since
Instant + Duration -> Instant
Instant - Duration -> Instant
```

## The `sleep` function

```bock
sleep(duration: Duration) -> Void with Clock
```

`sleep` is in the prelude (§18.2) — available without import. It
suspends execution for the given duration: in an `async` context it
yields to the runtime so other tasks may proceed; in synchronous code it
blocks the current thread. It requires the `Clock` effect.

## The `Clock` effect

`core.time` owns the `Clock` effect, which abstracts the host's
monotonic clock and sleep primitive:

```bock
effect Clock {
  fn now_monotonic() -> Instant
  fn sleep(duration: Duration) -> Void
}
```

The default handler — `std.time.SystemClock` — uses the target's native
monotonic clock and sleep primitives. Test environments typically
override the handler with a mock clock (`std.testing.MockClock`) that
replaces `sleep` with **virtual time advancement**: a test containing
`sleep(Duration.seconds(60))` advances mock time by 60 seconds without
actually blocking, enabling fast, deterministic tests of time-dependent
code.

> **Note.** `Instant.now()`, `instant.elapsed()`, and `sleep` read the
> host monotonic clock and are non-deterministic, so they are gated
> behind the `Clock` effect and its installed handler. The deterministic
> `Duration` surface (constructors, methods, and `+`/`-`/`*` operators)
> needs no effect.
