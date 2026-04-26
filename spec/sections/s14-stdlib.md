# Spec Excerpt: Standard Library

## Two Tiers
- `core`: Ships with compiler. Always available. All targets.
- `std`: First-party packages. Installed via pkg manager.

## Prelude (auto-imported, no `use` needed)
Types: Int, Float, Bool, String, Char, Void, Never,
  Duration, Instant,
  Optional/Some/None, Result/Ok/Err, List, Map, Set, Fn
Traits: Comparable, Equatable, Hashable, Displayable,
  Serializable, Cloneable, Default, Into, From, TryFrom,
  Iterator, Iterable, Collectable
Functions: print, println, debug, assert, todo, unreachable, sleep

## Core Modules
- core.types: sized ints/floats, BigInt, Decimal
- core.collections: List, Map, Set, Deque, SortedMap, Stack, Queue
- core.string: String ops, StringBuilder, Regex
- core.math: constants (PI, E), functions (abs, sqrt, sin...)
- core.option: Optional[T] utilities
- core.result: Result[T, E] utilities
- core.iter: Iterator trait + combinators
- core.compare: Ordering, Comparable, Equatable
- core.convert: Into, From, TryFrom, Displayable
- core.error: Error base trait
- core.effect: Effect system primitives
- core.concurrency: Channel, Mutex, RwLock, Atomic, WaitGroup
- core.memory: Rc, Arc
- core.time: Duration, Instant, sleep, Clock effect
- core.test: assert/expect, describe/it, mock, Gen, benchmark

## core.time
- Duration: Int64 nanoseconds internally (~292 year range)
- Instant: monotonic point in time (per-process)
- sleep(Duration) -> Void with Clock — prelude function
- Constructors: Duration.zero/nanos/micros/millis/seconds/minutes/hours
- Methods: as_nanos/as_millis/as_seconds, is_zero, is_negative, abs
- Arithmetic: Duration ± Duration, Duration * Int, Duration / Int
- Instant: now() with Clock, elapsed(), duration_since(earlier)
- Instant arithmetic: Instant - Instant = Duration,
  Instant ± Duration = Instant
- Clock effect: now_monotonic() -> Instant, sleep(Duration) -> Void
- MockClock (std.testing) replaces sleep with virtual time
  advancement for fast deterministic tests

## Trait-Language Integration
Comparable → `<`/`>`, Iterable → `for..in`,
Displayable → `${}`, Add/Sub → operators.
