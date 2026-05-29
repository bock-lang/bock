# Declarations

A Bock source file is a sequence of declarations at the top
level. Function bodies introduce their own local declarations
with `let`. This page covers every kind of declaration the
language admits, the visibility modifiers that govern them, and
the conventions that surround them.

## Local Bindings: `let`

Inside a function body, `let` binds a name to a value. The
type is inferred unless an explicit annotation is provided.

```bock
fn main() {
  let n = 42                 // inferred Int
  let label: String = "ok"   // explicit annotation
  let (x, y) = (3, 4)        // destructuring
  println("${n} ${label} ${x}+${y}")
}
```

<!-- verify: bock-check -->

A binding is immutable by default. To rebind, declare it with
`mut`:

```bock
fn main() {
  let mut counter = 0
  counter = counter + 1
  counter = counter + 1
  println("${counter}")
}
```

<!-- verify: bock-check -->

The `mut` keyword applies to the binding, not the value's type.
A `let mut s = Session { ... }` allows `s = Session { ... }` to
rebind `s`; mutating a field on `s` follows the rules in
[Ownership](./ownership.md).

## Functions: `fn`

A function declaration names parameters with types, declares a
return type, and may declare an effect set with `with` and
generic bounds with `where`:

```bock
fn add(a: Int, b: Int) -> Int {
  a + b
}

fn main() {
  println("${add(2, 3)}")
}
```

<!-- verify: bock-check -->

Functions are expressions: the last expression in the body is
the return value. An explicit `return` is allowed but unusual
in straight-line code; it is more common in `guard`-style early
exits.

### Generics and Where Clauses

Generic parameters live in `[ ]` immediately after the name.
Bounds may appear inline or in a `where` clause:

```bock
fn max_of[T: Comparable](a: T, b: T) -> T {
  if (a > b) { a } else { b }
}

fn min_of[T](a: T, b: T) -> T
  where (T: Comparable)
{
  if (a < b) { a } else { b }
}

fn main() {
  println("${max_of(3, 7)}")
  println("${min_of(3, 7)}")
}
```

<!-- verify: bock-check -->

### Default Parameter Values

The grammar admits default expressions on parameters
(`prefix: String = "Hello"`). The current implementation parses
them but does not yet use them to make arguments optional at
call sites — every parameter must be supplied. This is reserved
syntax; the eventual semantics let callers omit arguments that
have defaults.

### Effect Clauses

A function that uses an effect declares it with `with`:

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
  }
}
```

<!-- verify: bock-check -->

See [Effects](./effects.md) for the full effect machinery.

## Records: `record`

A `record` declares a value type with named fields. Each field
has a type; any field may carry a default expression.

```bock
record Session {
  id: String
  user: String
  is_admin: Bool = false
  expires_at: Int = 0
}

fn main() {
  let s = Session { id: "s1", user: "alice" }
  println("${s.user} admin=${s.is_admin}")
}
```

<!-- verify: bock-check -->

Records support default values, the spread operator (`..base`)
for update-construction, and field-shorthand at construction
sites. See [Types](./types.md#records) for the full record
surface.

## Enums: `enum`

An `enum` declares an algebraic data type — a value of the
enum is exactly one of the listed variants. A variant may
carry positional data, record-shaped data, or no data at all.

```bock
enum Status {
  Active,
  Pending { since: Int },
  Failed(String)
}

fn describe(s: Status) -> String {
  match s {
    Active => "active"
    Pending { since } => "pending since ${since}"
    Failed(reason) => "failed: ${reason}"
  }
}

fn main() {
  println(describe(Active))
  println(describe(Pending { since: 100 }))
  println(describe(Failed("timeout")))
}
```

<!-- verify: bock-check -->

## Effects: `effect`

An `effect` declaration names a set of operations. A function
that performs any of those operations declares the effect with
a `with` clause. A handler provides implementations.

```bock
effect Storage {
  fn read(key: String) -> Optional[String]
  fn write(key: String, value: String) -> Void
}

record MemoryStorage {}

impl Storage for MemoryStorage {
  fn read(key: String) -> Optional[String] { Some("v-for-${key}") }
  fn write(key: String, value: String) -> Void {
    println("stored ${key}=${value}")
  }
}

fn save(k: String, v: String) -> Void with Storage {
  write(k, v)
}

fn main() {
  handling (Storage with MemoryStorage {}) {
    save("user", "alice")
  }
}
```

<!-- verify: bock-check -->

Effects can be composed: `effect AppEffects = Logger + Clock`
declares an alias for the union. See [Effects](./effects.md).

## Constants: `const`

A `const` declaration introduces a compile-time constant. Its
type must be annotated and its value must be a constant
expression.

```bock
const MAX_RETRIES: Int = 5

fn allowed(n: Int) -> Bool { n <= MAX_RETRIES }

fn main() {
  println("${allowed(3)} ${allowed(10)}")
}
```

<!-- verify: bock-check -->

## Type Aliases: `type`

A `type` declaration introduces a new name for an existing
type. Aliases are transparent — they do not create distinct
types. See [Types](./types.md#type-aliases) for the full
treatment.

```bock
type UserId = String
type Predicate = Fn(Int) -> Bool

fn use_predicate(p: Predicate, x: Int) -> Bool { p(x) }

fn main() {
  let is_even: Predicate = (n) => n % 2 == 0
  println("${use_predicate(is_even, 4)}")
}
```

<!-- verify: bock-check -->

## Traits: `trait`

A `trait` is a set of methods that a type can implement. Traits
may carry default method implementations.

```bock
trait Show {
  fn show(self) -> String
  fn shout(self) -> String { "${self.show()}!" }
}

record Item { name: String }

impl Show for Item {
  fn show(self) -> String { self.name }
}

fn main() {
  let i = Item { name: "apple" }
  println(i.show())
  println(i.shout())
}
```

<!-- verify: bock-check -->

## Classes: `class`

A `class` is a record-with-methods that supports single
inheritance and trait implementation. The class body lists
fields and methods together:

```bock
trait Greet {
  fn greet(self) -> String
}

class Person : Greet {
  name: String

  fn greet(self) -> String { "Hello, ${self.name}" }
}

fn main() {
  let p = Person { name: "World" }
  println(p.greet())
}
```

<!-- verify: bock-check -->

In a project that uses single-mode functional code, records and
traits cover most needs. Classes exist for OO-mode projects and
for interop with target languages where class-shaped output is
idiomatic.

## Impl Blocks: `impl`

An `impl` block attaches methods to a type. Methods that take
`self` are instance methods; methods without `self` are
associated functions, called via `Type.method()`:

```bock
record Point { x: Int, y: Int }

impl Point {
  fn origin() -> Point { Point { x: 0, y: 0 } }
  fn translate(self, dx: Int, dy: Int) -> Point {
    Point { x: self.x + dx, y: self.y + dy }
  }
}

fn main() {
  let p = Point.origin().translate(3, 4)
  println("(${p.x}, ${p.y})")
}
```

<!-- verify: bock-check -->

An `impl Trait for Type` block provides the trait
implementation; see the trait example above.

## Modules: `module`

A `module` declaration at the top of a file gives it a module
path. Files without a `module` declaration cannot be imported.

```bock
module example.utils

public fn shout(s: String) -> String { "${s}!" }
fn private_helper(n: Int) -> Int { n + 1 }
```

<!-- verify: bock-check -->

Full coverage of cross-file imports and the module registry is
in [Modules](./modules.md).

## Visibility

Three visibility modifiers govern which scopes can see a
declaration:

| Modifier | Scope |
|----------|-------|
| (default) | Visible only in the declaring file. |
| `internal` | Visible within the module tree. |
| `public` | Visible everywhere. |

```bock
module example.calc

public fn add(a: Int, b: Int) -> Int { a + b }
internal fn normalize(x: Int) -> Int { if (x < 0) { 0 } else { x } }
fn debug_only(x: Int) -> Int { x * 2 }
```

<!-- verify: bock-check -->

Fields on a `public record` are public by default. Add an
explicit `internal` or no modifier to narrow a single field.
The same rule applies to enum variants — visibility of a
variant is inherited from the enum declaration.

## Annotations

Any declaration may be preceded by annotations. Common
annotations include `@managed`, `@deprecated("reason")`,
`@concurrent`, `@inline`, `@derive(...)`, and the context
annotations (`@context`, `@requires`, `@performance`,
`@security`):

```bock
@derive(Equatable, Hashable)
record User {
  id: Int
  name: String
}

@deprecated("use new_compute instead")
fn old_compute(n: Int) -> Int { n + 1 }

fn main() {
  let u = User { id: 1, name: "Alice" }
  println("${u.name} ${old_compute(2)}")
}
```

<!-- verify: bock-check -->

The full annotation taxonomy lives in
[Context](./context.md) and §15 of `spec/bock-spec.md`.
