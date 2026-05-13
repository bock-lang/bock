# Types

Bock has a static, structural, inferred type system. Every
expression has a type known at compile time. Annotations are
usually optional and always allowed. Type compatibility is
determined by shape, not by name.

## Primitive Types

The prelude exposes seven primitive types:

| Type | Description |
|------|-------------|
| `Int` | Pointer-width signed integer. Default integer type. |
| `Float` | 64-bit floating-point. Default float type. |
| `Bool` | `true` or `false`. |
| `String` | UTF-8 encoded text. |
| `Char` | A single Unicode scalar value. |
| `Void` | The unit type. Used for functions returning no value. |
| `Never` | Has no values. Used for diverging expressions. |

Sized numeric variants exist for when a specific width is
required: `Int8`, `Int16`, `Int32`, `Int64`, `Int128`, `UInt8`,
`UInt16`, `UInt32`, `UInt64`, `Float32`, `Float64`. Numeric
literals can carry a type suffix to select one of these:
`42_i8`, `1000_i64`, `3.14_f64`.

`Bytes`, `BigInt`, `BigFloat`, and `Decimal` round out the
numeric and binary primitives.

```bock
fn main() {
  let i: Int = 42
  let f: Float = 3.14
  let b: Bool = true
  let s: String = "hello"
  let c: Char = 'A'
  println("${i} ${f} ${b} ${s} ${c}")
}
```

<!-- verify: bock-check -->

### No Implicit Coercion

Bock never coerces between numeric types. Mixing `Int` and
`Float` in arithmetic requires an explicit conversion:

```bock
fn average(total: Int, count: Int) -> Float {
  total.to_float() / count.to_float()
}

fn main() {
  println("${average(10, 4)}")
}
```

<!-- verify: bock-check -->

The `.to_float()`, `.to_int()`, and `.to_string()` conversions
are always available on primitives without an import.

## Type Aliases

A `type` declaration introduces a new name for an existing
type. Aliases are transparent — they do not create distinct
types — but they make signatures self-documenting and let one
edit affect every use site.

```bock
type UserId = String
type Predicate = Fn(Int) -> Bool

fn lookup(id: UserId) -> String { id }

fn main() {
  let id: UserId = "u-42"
  println(lookup(id))
}
```

<!-- verify: bock-check -->

A type alias can name a generic instantiation:

```bock
record Pair[A, B] { first: A, second: B }

type StringPair = Pair[String, String]

fn main() {
  let p: StringPair = Pair { first: "hello", second: "world" }
  println("${p.first} ${p.second}")
}
```

<!-- verify: bock-check -->

## Records

Records are value types with named fields. They are created by
naming the type and listing fields; access is via dot notation.

```bock
record Session {
  id: String
  user: String
  expires_at: Int
}

fn main() {
  let s = Session {
    id: "abc"
    user: "alice"
    expires_at: 1700000000
  }
  println("${s.user} expires at ${s.expires_at}")
}
```

<!-- verify: bock-check -->

### Default Field Values

Fields may carry a default expression. A construction that
omits the field uses the default:

```bock
record Config {
  port: Int = 8080
  debug: Bool = false
  name: String = "default"
}

fn main() {
  let c = Config {}
  println("${c.port} ${c.debug} ${c.name}")
}
```

<!-- verify: bock-check -->

Overriding individual defaults works as expected:

```bock
record Config {
  port: Int = 8080
  debug: Bool = false
}

fn main() {
  let c = Config { port: 9000, debug: true }
  println("${c.port} ${c.debug}")
}
```

<!-- verify: bock-check -->

### Record Update (Spread)

To create a record that copies most fields from another and
overrides a few, use `..base` inside the construction:

```bock
record Pt { x: Int, y: Int, z: Int }

fn main() {
  let base = Pt { x: 1, y: 2, z: 3 }
  let upd = Pt { x: 10, ..base }
  println("${upd.x} ${upd.y} ${upd.z}")
}
```

<!-- verify: bock-check -->

The spread fills in any fields not explicitly named.

### Field Shorthand

When a local variable has the same name as a target field, the
shorthand is just the identifier:

```bock
record User { name: String, email: String }

fn make_user(name: String, email: String) -> User {
  User { name, email }
}

fn main() {
  let u = make_user("Alice", "alice@example.com")
  println("${u.name} ${u.email}")
}
```

<!-- verify: bock-check -->

## Enums (Algebraic Data Types)

Enums are sum types — a value of an enum type is exactly one of
its declared variants. Variants may carry data, either as
positional payload or as a record-shaped payload.

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

Record-shaped payloads use the same field syntax as records:

```bock
enum AuthError {
  InvalidCredentials,
  AccountLocked { until: Int },
  RateLimited { retry_after: Int }
}

fn describe(e: AuthError) -> String {
  match e {
    InvalidCredentials => "bad credentials"
    AccountLocked { until } => "locked until ${until}"
    RateLimited { retry_after } => "retry in ${retry_after}s"
  }
}

fn main() {
  println(describe(InvalidCredentials))
  println(describe(AccountLocked { until: 1800 }))
  println(describe(RateLimited { retry_after: 30 }))
}
```

<!-- verify: bock-check -->

Pattern matching against an enum is exhaustive — the compiler
warns in `development` strictness and errors in `production` if
any variant is unhandled. See [Patterns](./patterns.md) for
the full pattern grammar.

## Compound Types

Bock's prelude ships several compound types backed by
collections in `core`:

| Type | Purpose |
|------|---------|
| `Optional[T]` (`T?`) | A value of `T` or `None`. |
| `Result[T, E]` | `Ok(T)` for success, `Err(E)` for failure. |
| `List[T]` | Ordered, growable sequence of `T`. |
| `Map[K, V]` | Key-value store. |
| `Set[T]` | Unordered collection of unique `T`. |
| `(A, B, ...)` | Anonymous tuple of fixed arity. |

### Optional

`Optional[T]` represents a value that may be missing. The `T?`
shorthand is equivalent. Both forms accept `Some(value)` and
`None`:

```bock
fn lookup(k: String) -> Optional[Int] {
  if (k == "answer") { Some(42) } else { None }
}

fn main() {
  match lookup("answer") {
    Some(v) => println("got ${v}")
    None => println("missing")
  }
}
```

<!-- verify: bock-check -->

### Result

`Result[T, E]` represents an outcome that either succeeded with
a `T` or failed with an `E`:

```bock
fn divide(a: Int, b: Int) -> Result[Int, String] {
  if (b == 0) { Err("division by zero") } else { Ok(a / b) }
}

fn main() {
  match divide(10, 2) {
    Ok(v) => println("ok ${v}")
    Err(e) => println("err ${e}")
  }
}
```

<!-- verify: bock-check -->

The `?` operator on a `Result` short-circuits — if the value is
`Err(e)`, `?` propagates the error to the enclosing function.
See [Expressions](./expressions.md#error-propagation).

### Tuples

Tuples group a fixed number of heterogeneous values:

```bock
fn split(s: String) -> (String, Int) {
  (s, s.len())
}

fn main() {
  let (text, n) = split("hello")
  println("text=${text} len=${n}")
}
```

<!-- verify: bock-check -->

Tuple fields are accessed by destructuring with `let (a, b) =
tup` or with a tuple pattern in `match`. A single-element tuple
is written with a trailing comma: `(x,)` — without the comma,
`(x)` is a parenthesised expression.

### Lists, Maps, and Sets

Collection literals use distinct delimiters so the syntax
matches the runtime shape:

```bock
fn main() {
  let xs = [1, 2, 3, 4, 5]
  let prices = {"apple": 100, "pear": 150}
  let tags = #{"red", "ripe"}

  println("first xs = ${xs.get(0)}")
  println("apple price = ${prices.get("apple")}")
  println("has red = ${tags.contains("red")}")
}
```

<!-- verify: bock-check -->

Operations on these types — `map`, `filter`, `len`, indexing —
are method calls on the value; see the `core.collections` API
in the standard library reference.

## Generics

Generic parameters appear in square brackets after the name of
the function, record, enum, or trait:

```bock
fn first[T](list: List[T]) -> Optional[T] {
  list.get(0)
}

record Pair[A, B] {
  first: A
  second: B
}

fn swap[A, B](p: Pair[A, B]) -> Pair[B, A] {
  Pair { first: p.second, second: p.first }
}

fn main() {
  let p = Pair { first: 1, second: "two" }
  let q = swap(p)
  println("${q.first} ${q.second}")
}
```

<!-- verify: bock-check -->

Generic arguments are also written in square brackets. The
distinct delimiter (`[T]` vs Rust's `<T>`) avoids the ambiguity
between generics and comparison — `<` is always less-than.

### Trait Bounds

A generic parameter can be bounded by one or more traits using
`:` and `+`:

```bock
fn max_of[T: Comparable](a: T, b: T) -> T {
  if (a > b) { a } else { b }
}

fn main() {
  println("${max_of(3, 7)}")
  println("${max_of("apple", "banana")}")
}
```

<!-- verify: bock-check -->

For longer bound lists, a `where` clause keeps the signature
readable:

```bock
fn merge_pair[T](a: T, b: T) -> T
  where (T: Comparable)
{
  if (a < b) { a } else { b }
}

fn main() {
  println("${merge_pair(1, 2)}")
}
```

<!-- verify: bock-check -->

## Function Types

A first-class function is described by `Fn(ArgTypes) -> Ret`,
optionally annotated with the effects it requires:

```bock
type IntFn = Fn(Int) -> Int

fn apply(f: IntFn, x: Int) -> Int { f(x) }

fn main() {
  let inc: IntFn = (x) => x + 1
  println("${apply(inc, 41)}")
}
```

<!-- verify: bock-check -->

The effect clause on a function type works exactly like the one
on a function declaration:

```bock
type LogFn = Fn(String) -> Void
```

See [Effects](./effects.md) for the full effect type
machinery.

## Refinement Types (Spec)

The spec describes refinement types — type aliases with a
predicate that constrains valid values:

```bock
type Port = Int where (1 <= self && self <= 65535)
type Email = String where (matches(r"^[^@]+@[^@]+\.[^@]+$"))
```

The current compiler does not parse predicates on type
aliases; only the unconstrained form (`type Port = Int`) is
accepted today. The refinement clause is reserved syntax; once
implemented it will be checked statically when the value is a
literal and at runtime otherwise. See §4.7 of
`spec/bock-spec.md` for the eventual semantics.

## Capability Types

Functions can declare the platform capabilities they need with
`@requires`:

```bock
@requires(Capability.Network, Capability.Storage)
fn sync_user_data(user_id: Int) -> Result[String, String] {
  if (user_id > 0) { Ok("synced") } else { Err("invalid user") }
}

fn main() {
  match sync_user_data(1) {
    Ok(_) => println("ok")
    Err(e) => println(e)
  }
}
```

<!-- verify: bock-check -->

The capability taxonomy ships in the prelude: `Network`,
`Storage`, `Crypto`, `GPU`, `Camera`, `Microphone`, `Location`,
`Notifications`, `Bluetooth`, `Biometrics`, `Clipboard`,
`SystemProcess`, `FFI`, `Environment`, `Clock`, `Random`.

Capabilities propagate through the call graph. The full
treatment is in [Context](./context.md).
