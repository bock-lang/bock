# Patterns

Patterns describe the shape of a value and bind its parts to
names. Bock uses patterns in `match` expressions, `let`
bindings, `if (let pattern = ...)`, and `for pattern in ...`.
This page is the reference for every pattern form.

## Where Patterns Appear

Patterns are valid in four contexts:

```bock
fn main() {
  // 1. match arms
  let n = 3
  let label = match n {
    0 => "zero"
    1 | 2 | 3 => "small"
    _ => "other"
  }
  println(label)

  // 2. let bindings
  let (a, b) = (10, 20)
  println("${a} ${b}")

  // 3. if-let
  let opt = Some(42)
  if (let Some(v) = opt) {
    println("got ${v}")
  }

  // 4. for-loop variable
  let pairs = [(1, "a"), (2, "b")]
  for (i, name) in pairs {
    println("${i}=${name}")
  }
}
```

<!-- verify: bock-check -->

The grammar is the same in every context, but exhaustiveness
checking applies only to `match`.

## Wildcard

`_` matches anything and binds no name. It is the catch-all in
`match`:

```bock
fn classify(n: Int) -> String {
  match n {
    0 => "zero"
    _ => "other"
  }
}

fn main() {
  println(classify(0))
  println(classify(99))
}
```

<!-- verify: bock-check -->

In a destructuring position, `_` ignores a value:

```bock
fn main() {
  let (_, value) = ("ignored", 42)
  println("${value}")
}
```

<!-- verify: bock-check -->

## Identifier Binding

A bare identifier matches any value and binds it. Inside a
`match`, this is how you give a name to the scrutinee in an
arm:

```bock
fn name_or_default(opt: Optional[String]) -> String {
  match opt {
    Some(name) => name
    None => "anonymous"
  }
}

fn main() {
  println(name_or_default(Some("Alice")))
  println(name_or_default(None))
}
```

<!-- verify: bock-check -->

A `mut` binding makes the bound name mutable inside the arm:

```bock
fn bump(n: Int) -> Int {
  match n {
    mut x => {
      x = x + 1
      x
    }
  }
}

fn main() {
  println("${bump(5)}")
}
```

<!-- verify: bock-check -->

## Literal Patterns

Any literal — integer, float, boolean, character, string — can
appear as a pattern. It matches when the scrutinee is equal to
the literal:

```bock
fn classify(n: Int) -> String {
  match n {
    0 => "zero"
    1 => "one"
    42 => "answer"
    _ => "other"
  }
}

fn classify_str(s: String) -> String {
  match s {
    "hello" => "greeting"
    "bye" => "farewell"
    _ => "unknown"
  }
}

fn main() {
  println(classify(42))
  println(classify_str("hello"))
  println(classify_str("xyz"))
}
```

<!-- verify: bock-check -->

## Constructor Patterns

For enum variants, the pattern uses the same constructor syntax
as in expressions:

```bock
enum Shape {
  Circle(Float),
  Rect(Float, Float),
  Dot
}

fn area(s: Shape) -> Float {
  match s {
    Circle(r) => 3.14159 * r * r
    Rect(w, h) => w * h
    Dot => 0.0
  }
}

fn main() {
  println("${area(Circle(2.0))}")
  println("${area(Rect(3.0, 4.0))}")
  println("${area(Dot)}")
}
```

<!-- verify: bock-check -->

`Optional` and `Result` patterns work the same way — `Some(x)`,
`None`, `Ok(v)`, `Err(e)`.

## Record Patterns

A record pattern uses `Type { field: pattern, ... }`. Each
field is matched independently. The field-shorthand applies
in patterns just as it does in construction — `{ x }` binds the
field `x` to a local of the same name.

```bock
record Point { x: Int, y: Int }

fn classify(p: Point) -> String {
  match p {
    Point { x: 0, y: 0 } => "origin"
    Point { x: 0, y } => "on y-axis at ${y}"
    Point { x, y: 0 } => "on x-axis at ${x}"
    Point { x, y } => "(${x}, ${y})"
  }
}

fn main() {
  println(classify(Point { x: 0, y: 0 }))
  println(classify(Point { x: 0, y: 5 }))
  println(classify(Point { x: 3, y: 0 }))
  println(classify(Point { x: 7, y: 9 }))
}
```

<!-- verify: bock-check -->

A trailing `..` ignores the remaining fields:

```bock
record User { id: Int, name: String, age: Int }

fn label(u: User) -> String {
  match u {
    User { name, .. } => name
  }
}

fn main() {
  println(label(User { id: 1, name: "Alice", age: 30 }))
}
```

<!-- verify: bock-check -->

## Tuple Patterns

A tuple pattern uses parentheses around the component patterns:

```bock
fn add_pair(p: (Int, Int)) -> Int {
  let (a, b) = p
  a + b
}

fn describe(point: (Int, Int)) -> String {
  match point {
    (0, 0) => "origin"
    (x, 0) => "on x-axis at ${x}"
    (0, y) => "on y-axis at ${y}"
    (x, y) => "(${x}, ${y})"
  }
}

fn main() {
  println("${add_pair((3, 4))}")
  println(describe((0, 0)))
  println(describe((5, 0)))
}
```

<!-- verify: bock-check -->

## List Patterns

A list pattern uses `[]` with element patterns. Use `..` to
match the rest of the list:

```bock
fn describe(xs: List[Int]) -> String {
  match xs {
    [] => "empty"
    [only] => "one: ${only}"
    [a, b] => "two: ${a}, ${b}"
    _ => "many"
  }
}

fn main() {
  println(describe([]))
  println(describe([42]))
  println(describe([1, 2]))
  println(describe([1, 2, 3, 4]))
}
```

<!-- verify: bock-check -->

## Or-Patterns

A vertical bar `|` between patterns matches if any alternative
matches:

```bock
fn classify(n: Int) -> String {
  match n {
    0 => "zero"
    1 | 2 | 3 => "small"
    _ => "other"
  }
}

fn main() {
  println(classify(0))
  println(classify(2))
  println(classify(7))
}
```

<!-- verify: bock-check -->

Every alternative in an or-pattern must bind the same set of
names with the same types, since the arm body can refer to
those bindings regardless of which alternative matched.

## Range Patterns

Numeric ranges appear as patterns using `..` (exclusive) and
`..=` (inclusive):

```bock
fn classify(n: Int) -> String {
  match n {
    1..=9 => "single digit"
    10..=99 => "two digits"
    _ => "many"
  }
}

fn main() {
  println(classify(5))
  println(classify(50))
  println(classify(500))
}
```

<!-- verify: bock-check -->

## Nested Patterns

Patterns compose. A constructor pattern can carry a
destructuring pattern; a record pattern can carry another
record pattern in a field; everything nests.

```bock
fn double_inner(opt: Optional[Result[Int, String]]) -> String {
  match opt {
    Some(Ok(n)) => "ok inner ${n}"
    Some(Err(e)) => "err inner ${e}"
    None => "none"
  }
}

fn main() {
  println(double_inner(Some(Ok(42))))
  println(double_inner(Some(Err("nope"))))
  println(double_inner(None))
}
```

<!-- verify: bock-check -->

## Guard Clauses

An arm can carry a guard — an additional boolean predicate —
written with `if (expr)` after the pattern. The arm only
matches if both the pattern matches and the guard evaluates to
`true`:

```bock
fn classify(n: Int) -> String {
  match n {
    0 => "zero"
    x if (x < 0) => "negative ${x}"
    x if (x > 100) => "large ${x}"
    _ => "other"
  }
}

fn main() {
  println(classify(0))
  println(classify(-5))
  println(classify(500))
  println(classify(42))
}
```

<!-- verify: bock-check -->

A guard can reference any of the names bound by the pattern.
Guards do not affect exhaustiveness analysis — the compiler
sees the pattern, not the guard, when reasoning about
coverage.

## Exhaustiveness

In a `match`, every possible value of the scrutinee's type
must be covered. The compiler warns in `development`
strictness and errors in `production` if any case is unhandled.
The remedy is usually an `_` arm that catches the rest, or
filling in the missing constructor:

```bock
enum Color { Red, Green, Blue }

fn name(c: Color) -> String {
  match c {
    Red => "red"
    Green => "green"
    Blue => "blue"
  }
}

fn main() {
  println(name(Red))
  println(name(Green))
  println(name(Blue))
}
```

<!-- verify: bock-check -->

The other contexts where patterns appear — `let`, `if-let`,
`for`-loop — do not require exhaustiveness. A `let` with a
refutable pattern (one that might not match) is currently
restricted to irrefutable patterns; use `match` or `if-let` for
anything that can fail to match.
