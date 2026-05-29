# Statements and Control Flow

Bock distinguishes statements from expressions, but the line is
thin: most constructs are expressions, and only a small set —
`let`, `for`, `while`, `loop`, `guard`, `handling` — are pure
statements that do not produce a value. This page covers each
of them and the control-flow they enable.

## Statement vs Expression

A statement is a piece of code that performs an action without
producing a value. An expression evaluates to a value. Many
constructs that look like statements in other languages —
`if`, `match`, blocks, loops with `break value` — are
expressions in Bock and can appear on the right of `=`.

The pure statement forms are:

- `let [mut] pattern [: type] = expr` — local binding.
- `for pattern in expr { ... }` — iterate.
- `while (cond) { ... }` — loop while the condition holds.
- `loop { ... }` — loop forever (until `break`).
- `guard (cond) else { ... }` — early-exit guard.
- `handling (effect with handler, ...) { ... }` — effect
  handler scope.
- Any expression followed by a newline — used for its side
  effect; the value is discarded.
- `;` — separates multiple statements on one line.

A block (`{ ... }`) contains a sequence of statements; if the
last item is an expression with no trailing newline, the block
takes its value.

## Let Bindings

`let` introduces a new binding. Without `mut`, the binding
cannot be reassigned.

```bock
fn main() {
  let n = 42
  let name: String = "Bock"
  let (x, y) = (3, 4)             // destructure tuple
  println("${name} n=${n} (${x},${y})")
}
```

<!-- verify: bock-check -->

The destructuring form accepts any pattern — see
[Patterns](./patterns.md).

`let mut` allows reassignment:

```bock
fn main() {
  let mut counter = 0
  counter = counter + 1
  counter = counter + 1
  println("${counter}")
}
```

<!-- verify: bock-check -->

The compound assignment operators (`+=`, `-=`, `*=`, `/=`,
`%=`) work on `mut` bindings:

```bock
fn main() {
  let mut n = 10
  n += 5
  n -= 2
  n *= 2
  println("${n}")
}
```

<!-- verify: bock-check -->

## If / Else

`if` is an expression — see [Expressions](./expressions.md#if--else)
— but it is most often used as a statement, with the result
discarded:

```bock
fn check(n: Int) {
  if (n < 0) {
    println("negative")
  } else if (n == 0) {
    println("zero")
  } else {
    println("positive")
  }
}

fn main() {
  check(-1)
  check(0)
  check(7)
}
```

<!-- verify: bock-check -->

Parentheses around the condition are required. When an `if`
chain is used as an expression, every branch must yield a
value of the same type; branches that diverge are excluded
from the merge.

## Guard

`guard` is the early-return pattern made first-class. The
`else` block runs when the condition is false and **must**
diverge — typically with a `return`, `break`, `continue`, or a
panic.

```bock
fn process(input: String) -> Result[Int, String] {
  guard (input.len() > 0) else {
    return Err("empty input")
  }
  guard (input.len() < 100) else {
    return Err("input too long")
  }
  Ok(input.len())
}

fn main() {
  match process("hello") {
    Ok(n) => println("len=${n}")
    Err(e) => println("err: ${e}")
  }
  match process("") {
    Ok(n) => println("len=${n}")
    Err(e) => println("err: ${e}")
  }
}
```

<!-- verify: bock-check -->

The point of `guard` over `if (!cond) { return ... }` is
readability: the precondition is what the reader cares about,
and `guard` keeps it on the left where the eye finds it. The
compiler also uses `guard` to reason about ownership across
branches: see [Ownership](./ownership.md#control-flow-and-ownership).

## For Loops

`for` iterates over anything that implements `Iterable`:

```bock
fn main() {
  let xs = [10, 20, 30, 40]
  for x in xs {
    println("${x}")
  }
}
```

<!-- verify: bock-check -->

The loop variable can be a destructuring pattern — useful for
pairs from `enumerate()` or `Map.entries()`:

```bock
fn main() {
  let names = ["alpha", "beta", "gamma"]
  for (i, name) in names.enumerate() {
    println("${i}: ${name}")
  }
}
```

<!-- verify: bock-check -->

Ranges are iterable, so the common counted-loop idiom works
directly:

```bock
fn main() {
  for n in 1..=5 {
    println("${n}")
  }
}
```

<!-- verify: bock-check -->

## While Loops

`while (cond) { ... }` runs the body as long as the condition
evaluates to `true`. Parentheses around the condition are
required, matching `if`:

```bock
fn main() {
  let mut n = 0
  while (n < 5) {
    println("${n}")
    n = n + 1
  }
}
```

<!-- verify: bock-check -->

There is no `do { } while ( )` form. To get the same effect,
use `loop { ... if (cond) { break } }`.

## Loop (Infinite)

`loop` runs forever, exiting only via `break`, `return`, or
panic. It is also an expression — `break value` from inside a
`loop` yields `value` from the surrounding `let`:

```bock
fn main() {
  let mut i = 0
  let result = loop {
    if (i >= 10) { break i }
    i = i + 1
  }
  println("${result}")
}
```

<!-- verify: bock-check -->

## Break and Continue

`break` and `continue` work inside any loop construct
(`for`, `while`, `loop`):

```bock
fn main() {
  let mut total = 0
  for n in 1..=10 {
    if (n == 5) { continue }     // skip 5
    if (n == 8) { break }        // stop before 8
    total = total + n
  }
  println("${total}")
}
```

<!-- verify: bock-check -->

`break expr` returns a value from a `loop` expression (see
above). `break` without a value is the same as `break ()` and
works in `for`/`while` bodies that are used as statements.

## Return

A function's body is an expression — its value is the function's
result. An explicit `return` short-circuits and returns from
the enclosing function:

```bock
fn first_positive(xs: List[Int]) -> Optional[Int] {
  for x in xs {
    if (x > 0) { return Some(x) }
  }
  None
}

fn main() {
  match first_positive([-1, 0, -2, 7, -3]) {
    Some(n) => println("got ${n}")
    None => println("none")
  }
}
```

<!-- verify: bock-check -->

`return` is most useful inside loops or in `guard` clauses;
straight-line code typically relies on the implicit "last
expression is the value" rule.

## Handling Blocks

A `handling` block installs effect handlers for its body. It is
a statement, not an expression — though the block inside it
may produce a value that an outer `let` captures.

```bock
effect Logger {
  fn log(msg: String) -> Void
}

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

Multiple effect/handler pairs may be installed at once:

```bock
handling (Logger with ConsoleLogger {}, Clock with SystemClock {}) {
  audited_work()
}
```

The handlers stay in scope for the body and are uninstalled at
the end of the block. See [Effects](./effects.md) for resolution
rules and how handlers compose.
