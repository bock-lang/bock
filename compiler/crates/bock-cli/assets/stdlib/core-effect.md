# core.effect

The effect-system foundation: the standard `Log` effect, its
`ConsoleLog` handler, and the `console_log` constructor.

The effect-system *primitives* — the `effect` / `handler` / `handling`
machinery — are a **language feature** (§10), not stdlib surface;
`core.effect` exercises rather than re-declares them, and adds one
canonical, executable standard effect. Import its surface explicitly:

```bock
use core.effect.{Log, ConsoleLog, console_log}
```

## Effects

### `Log`

```bock
public effect Log {
  fn log(message: String) -> Void
}
```

The canonical logging effect: a single operation that records a textual
message somewhere chosen by the installed handler. A function that needs
to log declares `with Log` (§10.2) and invokes the bare `log("...")`
operation; the concrete destination is deferred to whatever handler the
caller installs.

Install a handler with a `handling` block (§10.3 Layer 1) or a
module-level `handle` declaration (§10.3 Layer 2):

```bock
handling (Log with console_log()) {
  log("hello")
}
```

## Records

### `ConsoleLog`

```bock
public record ConsoleLog {}
```

The v1 standard `Log` handler: writes each logged message to standard
output, prefixed with `[log] `. It is the one v1 handler form — a
stateless `record` paired with an `impl Log for ConsoleLog` (§10.4).
Construct it with [`console_log`](#console_log).

## Functions

### `console_log`

```bock
public fn console_log() -> ConsoleLog
```

Constructs a `ConsoleLog` handler for the `Log` effect — the ergonomic
way to obtain the standard handler:
`handling (Log with console_log()) { log("hi") }`.

## Not part of `core.effect`

Some effects are homed elsewhere by design:

- **Ambient effects `Panic` / `Allocate`** (§10.5) are
  compiler-intrinsic — always available without declaration — so they
  need no stdlib surface.
- **`Clock`** (§10.2 / §18.3.1) is owned by [`core.time`](./core-time.md);
  `std.time` ships its default `SystemClock` handler.

## Reserved for v1.x

- **Adaptive effect handlers** (`Effect.adaptive(...)`, §10.8) — runtime
  strategy selection via the AI provider.
- **Lambda-based handler constructors** (`Effect.handler(...)`, §10.4) —
  the stateless-handler shorthand. The v1 handler form is the record +
  `impl` form `ConsoleLog` demonstrates.
- **Layer-3 project-default handlers** (`bock.project [effects]`, §10.3)
  — v1 resolves handlers through Layer 1 (`handling` blocks) and Layer 2
  (module-level `handle`) only.
- **`Cancel`** (§13.5) — the ambient cancellation effect, Reserved with
  the broader cancellation surface.
