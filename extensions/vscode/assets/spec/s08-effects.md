# Spec Excerpt: Effect System

## Defining Effects
```bock
effect Log {
  fn log(level: Level, message: String) -> Void
}
effect Observable = Log + Trace + Metrics   // composite
```

## Using Effects
```bock
fn process(data: Data) -> Result[Output, Error]
  with Log, Clock
{
  log(Info, "Started at ${now()}")
  transform(data)
}
```
Effects propagate through call graph.

## Handler Resolution (3 Layers)
1. **Local** (`handling` blocks) — innermost wins
2. **Module** (`handle X with Y`) — per-module override
3. **Project** (`bock.project [effects]`) — global defaults

## Implementing Handlers
```bock
record ConsoleLog {}
impl Log for ConsoleLog {
  fn log(level: Level, message: String) -> Void {
    println("[${level}] ${message}")
  }
}
```
Lambda-based: `Log.handler(log: (l, m) => {})`

## Effect Categories
- Pure: computational, no outside interaction
- IO: touches outside world, correlates with capabilities
- Ambient: always available (Panic, Allocate)

## Graduated Strictness
- sketch: inferred, auto propagation
- development: required on public, warn if undeclared
- production: required on all, must be pinned

## Adaptive Effect Handlers
Adaptive handlers select recovery strategies at runtime from a
closed set of developer-defined options, using the AI provider
and `@context`/`@domain`/`@security` annotations for semantic
awareness.
```bock
let resilient = Network.adaptive(
  strategies: [
    retry(max: 3, backoff: exponential(Duration.millis(100))),
    use_cached(ttl: Duration.minutes(5)),
    degrade(fallback: default_response),
  ],
  context_aware: true
)
```
Key constraints:
- Never generates code — only selects from closed strategy set
- Selections logged in runtime decision manifest
- sketch: auto-select; development: logged; production: pinned
- Fallback to first strategy if AI provider unavailable
- Custom strategies via `RecoveryStrategy[E, T]` trait

Pinning granularity: `(error_signature, operation)` pairs.
`error_signature` = error type + hash of structural props
(HTTP status, errno class). Same signature → same strategy.

RecoveryContext contents:
- error, operation name, context annotations snapshot
- elapsed Duration, attempt Int
- history: last 10 errors from this handler
- NOT included: full AIR, call stack, source, concurrent state

## Cancellation Integration
- Strategy `attempt` return type: `Result[T, E] | Cancelled`
- Built-in combinators check cancellation at await points
- `on_cancel(context)` hook on RecoveryStrategy for cleanup
- Cancelled strategy halts handler; no further strategies tried
- Propagates as Cancelled to caller (not as error)

## Decision Manifest Split
Build decisions (`.bock/decisions/build/`): codegen choices,
committed to VCS, stable artifacts, reviewed in code review.
Runtime decisions (`.bock/decisions/runtime/`): adaptive handler
selections, environment-local, not committed, subject to log
rotation. Promotion path: `bock override --promote <id>` moves
stabilized runtime pin into build manifest.

## Transpilation
Effects → parameter passing (universal strategy).
Effect erasure when handler statically known (production).
