# Effects

Bock tracks the side effects a function can perform — logging,
clock access, storage I/O, networking — as part of the
function's type. The compiler propagates effect requirements
through the call graph, and a function that uses an effect must
either declare it or run inside a handler that supplies it.
This page covers how to declare effects, how to use them, and
how the three-layer handler resolution works.

## Declaring an Effect

An `effect` declaration names a set of operations. Each
operation is a `fn` signature — no body, just the types:

```bock
effect Logger {
  fn log(msg: String) -> Void
}

effect Clock {
  fn now() -> Int
  fn sleep(ms: Int) -> Void
}

fn main() { println("declared") }
```

<!-- verify: bock-check -->

The operations are now callable as bare functions inside any
code that has the effect available — `log("hi")`, `now()`, and
so on. The compiler resolves these to the current handler at
the call site.

## Composite Effects

Effects can be composed with `+` into an alias:

```bock
effect Logger {
  fn log(msg: String) -> Void
}

effect Clock {
  fn now() -> Int
  fn sleep(ms: Int) -> Void
}

effect AppEffects = Logger + Clock

fn main() { println("composed") }
```

<!-- verify: bock-check -->

A function that needs `AppEffects` automatically needs both
`Logger` and `Clock`. Composite effects make signatures shorter
in code that always uses the same set together.

## Using an Effect

A function that performs effectful work declares its effects
with a `with` clause:

```bock
effect Logger {
  fn log(msg: String) -> Void
}

fn process(data: String) -> String with Logger {
  log("processing ${data}")
  "done"
}

fn main() { println("declared process") }
```

<!-- verify: bock-check -->

Effects propagate: if `A` calls `B` which requires `Logger`,
then `A` must either declare `Logger` itself or supply a
handler in a `handling` block. In strictness levels
`development` and `production`, the compiler enforces this.

Multiple effects appear comma-separated:

```bock
effect Logger { fn log(msg: String) -> Void }
effect Clock { fn now() -> Int }

fn audited(data: String) -> String with Logger, Clock {
  let t = now()
  log("at ${t}: ${data}")
  data
}

fn main() { println("hi") }
```

<!-- verify: bock-check -->

## Implementing a Handler

A handler is a normal value — typically a `record` — with an
`impl Effect for Type` block that fills in each operation:

```bock
effect Logger {
  fn log(msg: String) -> Void
}

record ConsoleLogger {}

impl Logger for ConsoleLogger {
  fn log(msg: String) -> Void {
    println("[LOG] ${msg}")
  }
}

fn main() { println("handler defined") }
```

<!-- verify: bock-check -->

A handler may carry state — the same record can have fields,
which the operation methods can read:

```bock
effect Logger { fn log(msg: String) -> Void }

record PrefixLogger {
  prefix: String
}

impl Logger for PrefixLogger {
  fn log(msg: String) -> Void {
    println("${self.prefix} ${msg}")
  }
}

fn main() { println("stateful handler") }
```

<!-- verify: bock-check -->

## Installing a Handler

Handlers are installed with `handling (Effect with handler) { ... }`.
Inside the block, calls to the effect operations resolve to
the supplied handler:

```bock
effect Logger { fn log(msg: String) -> Void }

record ConsoleLogger {}

impl Logger for ConsoleLogger {
  fn log(msg: String) -> Void { println("[LOG] ${msg}") }
}

fn audit(action: String) -> Void with Logger {
  log("audit: ${action}")
}

fn main() {
  handling (Logger with ConsoleLogger {}) {
    audit("login")
    audit("logout")
  }
}
```

<!-- verify: bock-check -->

Multiple effects can be installed in one `handling` block:

```bock
effect Logger { fn log(msg: String) -> Void }
effect Clock  { fn now() -> Int }

record ConsoleLogger {}
record SystemClock {}

impl Logger for ConsoleLogger {
  fn log(msg: String) -> Void { println("[LOG] ${msg}") }
}

impl Clock for SystemClock {
  fn now() -> Int { 1000 }
}

fn audited(data: String) -> String with Logger, Clock {
  let t = now()
  log("at ${t}: ${data}")
  data
}

fn main() {
  handling (Logger with ConsoleLogger {}, Clock with SystemClock {}) {
    audited("input")
  }
}
```

<!-- verify: bock-check -->

## Three-Layer Resolution

When a function calls an effect operation, the compiler
resolves the call to the innermost handler in scope. There are
three layers, in priority order:

1. **Local handlers** — installed by a `handling` block in the
   current function or one of its callers.
2. **Module-level handlers** — declared at the top of the
   file with `handle Effect with handler`.
3. **Project defaults** — declared in `bock.project`.

The innermost handler wins. A `handling` block can override a
module-level handle; a module-level handle can override a
project default.

### Module-Level Handlers

A `handle Effect with handler` declaration at the top of a
file installs a handler for every function in that module
unless overridden by a local `handling` block:

```bock
module main

effect Logger {
  fn log(msg: String) -> Void
}

record ConsoleLogger {}

impl Logger for ConsoleLogger {
  fn log(msg: String) -> Void { println("[LOG] ${msg}") }
}

handle Logger with ConsoleLogger {}

fn audit(action: String) -> Void with Logger {
  log("audit: ${action}")
}

fn main() {
  audit("login")
}
```

<!-- verify: bock-check -->

The module-level handle is the right place to put the
"default" handler for an effect — the one production code uses
when there's no special context.

### Local Override

A `handling` block inside a function takes precedence over the
module-level handle for the duration of its body. This is the
pattern for tests and for special-case overrides:

```bock
module main

effect Logger { fn log(msg: String) -> Void }

record ConsoleLogger {}
record SilentLogger {}

impl Logger for ConsoleLogger {
  fn log(msg: String) -> Void { println("[LOG] ${msg}") }
}

impl Logger for SilentLogger {
  fn log(msg: String) -> Void {}
}

handle Logger with ConsoleLogger {}

fn audit(action: String) -> Void with Logger {
  log("audit: ${action}")
}

fn main() {
  audit("noisy")   // uses ConsoleLogger
  handling (Logger with SilentLogger {}) {
    audit("quiet") // uses SilentLogger
  }
  audit("noisy again")
}
```

<!-- verify: bock-check -->

## Effect Categories

The spec distinguishes three categories of effects:

- **Pure effects** — computational, no outside interaction.
  The compiler can optimize them aggressively.
- **IO effects** — `Log`, `Clock`, `Storage`, `Network`,
  `Random`. Correlate with `Capability` requirements.
- **Ambient effects** — always available without declaration.
  `Panic` and `Allocate` are the two ambient effects. You do
  not write `with Panic` on every function that might `assert`.

User-defined effects fall into the IO category. The compiler
tracks them through the call graph and enforces declaration in
production strictness.

## Effects and Capabilities

An effect describes *what* the function does ("logs", "reads
storage"). A capability describes *what platform permission* is
needed ("network access"). The two are related but distinct.

```bock
effect Logger { fn log(msg: String) -> Void }

record ConsoleLogger {}

impl Logger for ConsoleLogger {
  fn log(msg: String) -> Void { println("[LOG] ${msg}") }
}

@requires(Capability.Network)
fn fetch(url: String) -> String with Logger {
  log("fetching ${url}")
  "fetched"
}

fn main() {
  handling (Logger with ConsoleLogger {}) {
    let r = fetch("https://example.com")
    println(r)
  }
}
```

<!-- verify: bock-check -->

See [Context](./context.md) for the full capability story.

## Transpilation

Effects compile to **parameter passing** universally: each
handler is threaded as an additional argument through the call
chain. Target-specific shapes — dependency injection in Java,
protocol witnesses in Swift, ordinary closures in JS — are
chosen by the AI transpiler.

When a handler is statically known (only one handler is in
scope for a given call site), the compiler can inline the
handler and erase the indirection. This optimization currently
applies in `production` mode; the source remains the same
either way.

The target mapping for each effect operation is part of the
target profile, not the source — the user writes effect calls
identically across targets.
