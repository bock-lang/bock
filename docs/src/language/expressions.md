# Expressions

Almost everything in Bock is an expression. Function calls,
operators, blocks, control flow, pattern matching, and even
loops produce values. This page is the reference for every
expression form and the precedence rules that bind them.

## Everything Returns a Value

`if`, `match`, and blocks all return values. The "last
expression in the body" rule applies everywhere:

```bock
fn classify(n: Int) -> String {
  if (n == 0) {
    "zero"
  } else if (n > 0) {
    "positive"
  } else {
    "negative"
  }
}

fn main() {
  println(classify(0))
  println(classify(7))
  println(classify(-3))
}
```

<!-- verify: bock-check -->

A block expression introduces a new scope and evaluates to the
value of its last expression:

```bock
fn main() {
  let total = {
    let a = 10
    let b = 20
    a + b
  }
  println("${total}")
}
```

<!-- verify: bock-check -->

## Operator Precedence

Operators bind from lowest precedence to highest. Within a
single row, the associativity column determines how operators
of the same precedence group together.

| Level | Category | Operators | Assoc. |
|-------|----------|-----------|--------|
| 1 | Assignment | `=` `+=` `-=` `*=` `/=` `%=` | Right |
| 2 | Pipe | `\|>` | Left |
| 3 | Compose | `>>` | Left |
| 4 | Range | `..` `..=` | None |
| 5 | Logical OR | `\|\|` | Left |
| 6 | Logical AND | `&&` | Left |
| 7 | Comparison | `==` `!=` `<` `>` `<=` `>=` `is` | None |
| 8 | Bitwise OR | `\|` | Left |
| 9 | Bitwise XOR | `^` | Left |
| 10 | Bitwise AND | `&` | Left |
| 11 | Additive | `+` `-` | Left |
| 12 | Multiplicative | `*` `/` `%` | Left |
| 13 | Power | `**` | Right |
| 14 | Unary | `-` `!` `~` | Prefix |
| 15 | Postfix | `()` `[]` `.field` `.method()` `?` | Left |

A few important notes:

- There are **no infix shift operators**. `>>` is reserved
  for function composition. Use `Int.shift_left(n)` and
  `Int.shift_right(n)` for shift operations.
- The `**` operator is exponentiation. It is right-associative,
  so `2 ** 3 ** 2` is `2 ** 9`.
- The comparison operators are non-associative; `a < b < c` is
  a type error, not a chained comparison.

## Function Calls

A function call is the name (or any callable expression)
followed by parenthesised arguments:

```bock
fn add(a: Int, b: Int) -> Int { a + b }

fn main() {
  println("${add(2, 3)}")
}
```

<!-- verify: bock-check -->

A method call uses dot notation and dispatches to the type's
`impl` block:

```bock
record Point { x: Int, y: Int }

impl Point {
  fn translate(self, dx: Int, dy: Int) -> Point {
    Point { x: self.x + dx, y: self.y + dy }
  }
}

fn main() {
  let p = Point { x: 1, y: 2 }.translate(3, 4)
  println("(${p.x}, ${p.y})")
}
```

<!-- verify: bock-check -->

An associated function — a method declared in an `impl` block
without `self` — is called via the type name:

```bock
record Point { x: Int, y: Int }

impl Point {
  fn origin() -> Point { Point { x: 0, y: 0 } }
}

fn main() {
  let p = Point.origin()
  println("(${p.x}, ${p.y})")
}
```

<!-- verify: bock-check -->

## Pipe Operator

The pipe `|>` chains a value through a sequence of functions.
The piped value becomes the **first argument** of the right-hand
side by default:

```bock
fn double(x: Int) -> Int { x * 2 }
fn add(a: Int, b: Int) -> Int { a + b }

fn main() {
  let r = 5 |> double |> add(3)
  println("${r}")
}
```

<!-- verify: bock-check -->

When the piped value should go somewhere other than the first
position, use `_` as a placeholder at the call site:

```bock
fn add(a: Int, b: Int) -> Int { a + b }

fn main() {
  let r = 5 |> add(_, 10)
  println("${r}")
}
```

<!-- verify: bock-check -->

If the right-hand side returns a closure rather than calling a
function directly, bind it first so the pipe has a concrete
target:

```bock
fn scale_by(factor: Float) -> Fn(Float) -> Float {
  (x) => x * factor
}

fn main() {
  let scale_ten = scale_by(10.0)
  let r = 3.14 |> scale_ten
  println("${r}")
}
```

<!-- verify: bock-check -->

## Function Composition

`>>` composes two functions, producing a new function that
applies the left side first and then the right side:

```bock
fn parse(s: String) -> Int { 42 }
fn validate(n: Int) -> Int { n }
fn transform(n: Int) -> String { "${n}" }

fn main() {
  let process = parse >> validate >> transform
  println(process("input"))
}
```

<!-- verify: bock-check -->

Composition is point-free — there is no value flowing through;
the result is a function ready to be called.

## Partial Application

The `_` placeholder works at any call site to produce a closure
that is curried over the missing arguments. Use it freely
inside pipes (as shown above). Standalone, this is reserved
syntax — the parser accepts it but the type checker treats the
call as immediate. Bind to a lambda for the same effect today:

```bock
fn multiply(a: Float, b: Float) -> Float { a * b }

fn main() {
  let add_tax = (x: Float) => multiply(x, 1.08)
  println("${add_tax(100.0)}")
}
```

<!-- verify: bock-check -->

## Lambda Expressions

A lambda is a parameter list in parentheses followed by `=>`
and an expression or block:

```bock
fn main() {
  let inc = (x: Int) => x + 1
  let add = (a: Int, b: Int) => a + b
  println("${inc(5)}")
  println("${add(3, 4)}")
}
```

<!-- verify: bock-check -->

Parentheses around the parameter list are always required —
even for a single parameter. Multi-statement bodies use braces;
the last expression is the value:

```bock
fn main() {
  let format_greeting = (name: String) => {
    let len = name.len()
    "Hello, ${name} (${len} chars)"
  }
  println(format_greeting("Bock"))
}
```

<!-- verify: bock-check -->

## Collection Literals

Each collection type has its own literal syntax:

```bock
fn main() {
  let list = [1, 2, 3]
  let map = {"key": "value", "port": "8080"}
  let set = #{"a", "b", "c"}
  let tuple = ("hello", 42, true)
  println("list len=${list.len()} set size=${set.len()}")
}
```

<!-- verify: bock-check -->

The distinct delimiters disambiguate them from blocks and
record constructions: `{ ... }` is a block unless its first
element is `expr : expr` (map) or it follows a type identifier
(record construction).

## Record Construction

Use the record name followed by `{ field: value, ... }`:

```bock
record User { id: Int, name: String, is_admin: Bool = false }

fn main() {
  let u = User { id: 1, name: "Alice" }
  let admin = User { id: 2, name: "Root", is_admin: true }
  println("${u.name} ${admin.is_admin}")
}
```

<!-- verify: bock-check -->

The spread operator (`..base`) copies remaining fields from
another value:

```bock
record Pt { x: Int, y: Int, z: Int }

fn main() {
  let base = Pt { x: 1, y: 2, z: 3 }
  let updated = Pt { x: 10, ..base }
  println("${updated.x} ${updated.y} ${updated.z}")
}
```

<!-- verify: bock-check -->

## If / Else

`if` is an expression; parentheses around the condition are
required:

```bock
fn main() {
  let label = if (3 > 2) { "yes" } else { "no" }
  println(label)
}
```

<!-- verify: bock-check -->

The `if`/`else if`/`else` chain works as expected. Every branch
must yield a value of the same type when `if` is used as an
expression — except for branches that diverge (`return`,
`break`, etc.), which are excluded from the type merge.

### If-Let

`if (let pattern = expr)` matches a pattern against `expr` and
binds the pattern's identifiers if it succeeds:

```bock
fn lookup(k: String) -> Optional[Int] {
  if (k == "answer") { Some(42) } else { None }
}

fn main() {
  if (let Some(v) = lookup("answer")) {
    println("got ${v}")
  } else {
    println("nothing")
  }
}
```

<!-- verify: bock-check -->

## Match

`match` evaluates a scrutinee and dispatches to the first arm
whose pattern matches:

```bock
fn classify(n: Int) -> String {
  match n {
    0 => "zero"
    1 | 2 | 3 => "small"
    x if (x > 100) => "large: ${x}"
    _ => "other"
  }
}

fn main() {
  println(classify(0))
  println(classify(2))
  println(classify(500))
  println(classify(42))
}
```

<!-- verify: bock-check -->

Each arm has a pattern, an optional guard `if (expr)`, an arrow
`=>`, and a result expression (or block). Arms are separated
by newlines. Patterns may bind identifiers, destructure
records, enums, tuples, and lists, and combine via or-patterns
(`|`). See [Patterns](./patterns.md) for the full grammar.

The compiler checks exhaustiveness — it warns in `development`
strictness and errors in `production` if any case is unhandled.

## Ranges

```bock
fn main() {
  let exclusive = 1..10           // 1, 2, ..., 9
  let inclusive = 1..=10          // 1, 2, ..., 10
  let stepped = (0..100).step(2)  // 0, 2, ..., 98

  let mut total = 0
  for n in exclusive { total = total + n }
  println("excl sum=${total}")

  let mut count = 0
  for _ in stepped { count = count + 1 }
  println("count=${count}")
}
```

<!-- verify: bock-check -->

## String Interpolation

Inside a `"..."` or `"""..."""` string, `${expr}` evaluates a
Bock expression and inserts its `Displayable` form:

```bock
fn main() {
  let n = 42
  let s = "answer = ${n}"
  let computed = "double = ${n * 2}"
  println(s)
  println(computed)
}
```

<!-- verify: bock-check -->

The expression inside `${ ... }` can be arbitrary — function
calls, method calls, arithmetic, anything that produces a
`Displayable` value.

## Error Propagation: `?`

The postfix `?` operator unwraps `Result` and `Optional`. On
`Ok(v)` / `Some(v)` it produces `v`. On `Err(e)` / `None` it
returns from the enclosing function with the same `Err`/`None`.

```bock
fn safe_div(a: Int, b: Int) -> Result[Int, String] {
  if (b == 0) { Err("div by zero") } else { Ok(a / b) }
}

fn chained(a: Int, b: Int, c: Int) -> Result[Int, String] {
  let x = safe_div(a, b)?
  let y = safe_div(x, c)?
  Ok(y + 1)
}

fn main() {
  match chained(100, 5, 2) {
    Ok(v) => println("ok ${v}")
    Err(e) => println("err: ${e}")
  }
}
```

<!-- verify: bock-check -->

`?` works inside any function whose return type is itself a
`Result` (for `Result`-typed sub-expressions) or `Optional`
(for `Optional`-typed sub-expressions). The return type
constrains where `?` can be used.

## Diverging Expressions

Some expressions never produce a value at the join point —
they always exit:

- `return [expr]` — returns from the enclosing function.
- `break [expr]` — exits the enclosing `loop`/`while`/`for`.
- `continue` — skips to the next iteration of the enclosing
  loop.
- `unreachable()` — marks code as logically unreachable. The
  compiler treats it as `Never`-typed.

A diverging expression has type `Never` and unifies with any
expected type. This is what makes `guard ... else { return ... }`
work in any context.
