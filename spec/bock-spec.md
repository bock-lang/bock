# The Bock Programming Language Specification

**Version:** 0.1.0-draft
**Date:** March 2026
**Status:** Pre-implementation specification draft

---

## Table of Contents

1. [Introduction](#1-introduction)
2. [Language Overview](#2-language-overview)
3. [Lexical Structure](#3-lexical-structure)
4. [Type System](#4-type-system)
5. [Ownership Model](#5-ownership-model)
6. [Declarations](#6-declarations)
7. [Expressions](#7-expressions)
8. [Statements and Control Flow](#8-statements-and-control-flow)
9. [Pattern Matching](#9-pattern-matching)
10. [Effect System](#10-effect-system)
11. [Context System](#11-context-system)
12. [Module System](#12-module-system)
13. [Concurrency](#13-concurrency)
14. [Interop and FFI](#14-interop-and-ffi)
15. [Annotations](#15-annotations)
16. [Annotated Intermediate Representation (AIR)](#16-annotated-intermediate-representation-air)
17. [Transpilation Pipeline](#17-transpilation-pipeline)
18. [Standard Library](#18-standard-library)
19. [Package Manager](#19-package-manager)
20. [Tooling](#20-tooling)
21. [Formal Grammar](#21-formal-grammar)
22. [Target Profiles](#22-target-profiles)
23. [Appendices](#23-appendices)

---

## 1. Introduction

### 1.1 — What Is Bock

Bock is a feature-declarative, target-agnostic programming language designed for AI-human collaborative development. It occupies a specific niche in the abstraction hierarchy: more precise than natural language prompts, more abstract than any single implementation language.

Bock code describes what a program does and what guarantees it requires — not how those are achieved on any given platform. An AI-driven transpilation pipeline generates idiomatic code for each target language and platform.

### 1.2 — Design Goals

**AI-first, human-friendly.** AI agents are the primary developers. The language treats AI context as a first-class design system — semantic annotations, structured metadata, and decision manifests are core features. Humans remain central as architects, reviewers, and operators.

**Feature-declarative.** Developers declare functionality, constraints, capabilities, and intent. The tooling handles implementation targeting. A function annotated `@concurrent` gets `Promise.all` in JavaScript, `tokio::join!` in Rust, and goroutines in Go — automatically.

**No black boxes.** AI enters the pipeline at discrete, auditable stages. Every AI decision is logged, reproducible, and overridable. A deterministic fallback exists for every AI-assisted stage.

**One language, every platform.** Bock transpiles to JavaScript, TypeScript, Python, Rust, Go, Java, C++, C#, and Swift, and builds directly into deployable artifacts — Android APKs, iOS Xcode projects, web SPAs, Docker containers, serverless packages.

**Graduated rigor.** Projects declare a strictness level (`sketch`, `development`, `production`) that controls type enforcement, context requirements, and AI decision governance. Prototype fast, ship safely.

### 1.3 — Supported Targets

| Target     | Language   | Use Cases                       |
|------------|------------|---------------------------------|
| js         | JavaScript | Web frontends, Node.js servers  |
| ts         | TypeScript | Type-safe web/Node.js           |
| python     | Python     | Data science, scripting, APIs   |
| rust       | Rust       | Systems, performance-critical   |
| go         | Go         | Services, CLI tools, networking |
| java       | Java       | Enterprise, Android             |
| cpp        | C++        | Systems, games, embedded        |
| csharp     | C#         | .NET, Unity, Windows            |
| swift      | Swift      | iOS, macOS                      |

### 1.4 — Strictness Levels

| Level         | Types           | Context Req'd | Mutation       | AI Decisions     |
|---------------|-----------------|---------------|----------------|------------------|
| `sketch`      | Inferred, wide  | Minimal       | Unrestricted   | Auto-resolved    |
| `development` | Inferred, warn  | Module-level  | Warn on broad  | Logged           |
| `production`  | Fully resolved  | Full          | Explicit only  | Must be pinned   |

### 1.5 — Paradigm Configuration

Projects configure a paradigm mode that controls which language features are available:

**FP mode:** All values immutable by default. No `class` keyword. Composition via traits. The `mut` keyword is a compile error outside `@mutable` scopes.

**OOP mode:** `class` keyword with single inheritance. Mutable by default for local variables. Encapsulation enforced (private by default).

**Multi mode (default):** All features available. Immutable by default. Both `class` and functional patterns supported.

---

## 2. Language Overview

A complete Bock module demonstrating core syntax:

```bock
//! User management module.

module app.users

use core.collections.{List, Map}
use core.net.http.{get, post, Response}
use app.models.{User, UserError}

@context("""
  User management module.
  Handles CRUD operations for user accounts.
  All user data must be validated before persistence.
  Email uniqueness is enforced at the application layer.
""")

// ─── Types ───

type Email = String
  where (matches(r"^[^@]+@[^@]+\.[^@]+$"))
type Username = String
  where (len(self) >= 3 && len(self) <= 30)

record CreateUserRequest {
  username: Username
  email: Email
  display_name: Optional[String] = None
}

enum UserError {
  NotFound { id: UserId }
  DuplicateEmail { email: Email }
  ValidationFailed { reasons: List[String] }
  StorageError { cause: String }
}

// ─── Functions ───

@requires(Capability.Storage)
public fn create_user(
  req: CreateUserRequest,
) -> Result[User, UserError]
  with Log, Trace
{
  log(Info, "Creating user: ${req.username}")

  guard (!email_exists(req.email)) else {
    return Err(UserError.DuplicateEmail {
      email: req.email,
    })
  }

  let user = User {
    id: generate_id()
    username: req.username
    email: req.email
    display_name: req.display_name
      .unwrap_or(req.username)
    created_at: now()
  }

  save_user(user)?
  Ok(user)
}

public fn find_active(users: List[User]) -> List[User] {
  users
    |> filter((u) => u.is_active && !u.is_suspended)
    |> sort_by((u) => u.created_at)
}

public fn format_summary(user: User) -> String {
  let status = match user.status {
    Active => "active"
    Suspended { reason } => "suspended: ${reason}"
    Deactivated => "deactivated"
  }
  "${user.display_name} (${user.email}) — ${status}"
}

// ─── Generics with constraints ───

public fn merge_records[A, B, C](
  left: A,
  right: B,
) -> Result[C, MergeError]
  where (
    A: Into[C],
    B: Into[C],
    C: Combinable + Validatable,
  )
{
  let merged = left.into().combine(right.into())
  guard (merged.is_valid()) else {
    return Err(MergeError.InvalidResult)
  }
  Ok(merged)
}

// ─── Tests ───

@test
fn test_create_user_success() {
  let req = CreateUserRequest {
    username: "testuser"
    email: "test@example.com"
  }
  let result = create_user(req)
  expect(result).to_be_ok()
  expect(result.unwrap().username).to_equal("testuser")
}

property("all created users have valid emails") {
  forall (req: Gen[CreateUserRequest]) {
    match create_user(req) {
      Ok(user) => expect(user.email).to_match_pattern(Email)
      Err(_) => Ok(())
    }
  }
}
```

---

## 3. Lexical Structure

### 3.1 — Encoding

Bock source files are UTF-8 encoded.

### 3.2 — Whitespace and Line Handling

Whitespace is not significant for block structure. Newlines terminate statements unless a continuation context is active:

A statement continues to the next line when:
1. The current line ends with a binary operator
2. The current line ends with a comma
3. The current line ends with an opening delimiter (`(`, `[`, `{`)
4. The next line starts with a dot `.`
5. The next line starts with a pipe `|>`
6. The next line starts with a closing delimiter (`)`, `]`, `}`)
7. An explicit line continuation `\` is present
8. The next line starts with `else`

Multiple statements on one line are separated by `;`.

### 3.3 — Comments

```bock
// Line comment
/* Block comment (nestable) */
/// Documentation comment (parsed by tooling)
//! Module-level documentation comment
```

### 3.4 — Identifiers

Identifiers start with a letter or underscore and continue with letters, digits, or underscores. Type identifiers must start with an uppercase letter.

**Reserved keywords:** `fn`, `let`, `mut`, `const`, `if`, `else`, `match`, `for`, `in`, `while`, `loop`, `break`, `continue`, `return`, `guard`, `with`, `handling`, `handle`, `record`, `enum`, `class`, `trait`, `impl`, `self`, `Self`, `module`, `use`, `public`, `internal`, `native`, `async`, `await`, `effect`, `platform`, `where`, `type`, `true`, `false`, `Ok`, `Err`, `Some`, `None`, `property`, `forall`, `unreachable`.

### 3.5 — Literals

**Integers:** Decimal (`42`, `1_000_000`), hexadecimal (`0xFF`), octal (`0o77`), binary (`0b1010`). Optional type suffix: `42_u8`, `1000_i64`.

**Floats:** `3.14`, `1.0e10`, `2.5E-3`. Optional type suffix: `3.14_f64`.

**Booleans:** `true`, `false`.

**Characters:** `'a'`, `'\n'`, `'\u{1F600}'`.

**Strings:** Double-quoted with `${expr}` interpolation:
```bock
"Hello, ${name}!"
```

**Raw strings:** `r"no ${interpolation}"` — no escape processing.

**Multi-line strings:** Triple-quoted:
```bock
let html = """
  <div>${content}</div>
"""
```

**Raw multi-line:** `r"""..."""`.

### 3.6 — Operators

**Arithmetic:** `+`, `-`, `*`, `/`, `%`, `**` (power).

**Comparison:** `==`, `!=`, `<`, `>`, `<=`, `>=`, `is` (type check).

**Logical:** `&&`, `||`, `!`.

**Bitwise:** `&`, `|`, `^`, `~`.

**Assignment:** `=`, `+=`, `-=`, `*=`, `/=`, `%=`.

**Special:** `|>` (pipe), `>>` (compose), `=>` (arrow), `->` (return type), `?` (propagate/optional), `..` (range exclusive), `..=` (range inclusive), `_` (wildcard/placeholder).

### 3.7 — Operator Precedence

From lowest to highest binding:

| Level | Category       | Operators                  | Assoc. |
|-------|----------------|----------------------------|--------|
| 1     | Assignment     | `= += -= *= /= %=`        | Right  |
| 2     | Pipe           | `\|>`                      | Left   |
| 3     | Compose        | `>>`                       | Left   |
| 4     | Range          | `.. ..=`                   | None   |
| 5     | Logical OR     | `\|\|`                     | Left   |
| 6     | Logical AND    | `&&`                       | Left   |
| 7     | Comparison     | `== != < > <= >= is`       | None   |
| 8     | Bitwise OR     | `\|`                       | Left   |
| 9     | Bitwise XOR    | `^`                        | Left   |
| 10    | Bitwise AND    | `&`                        | Left   |
| 11    | Additive       | `+ -`                      | Left   |
| 12    | Multiplicative | `* / %`                    | Left   |
| 13    | Power          | `**`                       | Right  |
| 14    | Unary          | `- ! ~`                    | Prefix |
| 15    | Postfix        | `() [] . .method() ?`      | Left   |

> **Bit shifts:** There are no infix shift operators. Use `Int.shift_left(n)` and `Int.shift_right(n)` methods. `>>` is reserved exclusively for function composition (level 3).

### 3.8 — Delimiters

| Delimiter | Purpose                                              |
|-----------|------------------------------------------------------|
| `( )`     | Function args, conditions, constraints, tuples       |
| `[ ]`     | Generic params, list literals, type parameters       |
| `{ }`     | Blocks, map literals, record construction            |
| `#{ }`    | Set literals                                         |
| `" "`     | Strings                                              |
| `""" """` | Multi-line strings                                   |
| `` ` ``   | FFI inline code                                      |

---

## 4. Type System

### 4.1 — Design: Structural, Inferred, Enforced

Bock uses structural typing — compatibility is determined by shape, not name. Types are inferred wherever possible but must be fully resolved at compile time. The compiler rejects ambiguous types.

### 4.2 — Primitive Types

`Int`, `Float`, `Bool`, `String`, `Char`, `Void`, `Never`.

Sized variants: `Int8`, `Int16`, `Int32`, `Int64`, `Int128`, `UInt8`, `UInt16`, `UInt32`, `UInt64`, `Float32`, `Float64`. Also `Byte`, `Bytes`, `BigInt`, `BigFloat`, `Decimal`.

Primitive conversion methods (always available, no import needed):

```bock
let average = total / count.to_float()  // Int → Float
let index = offset.to_int()             // Float → Int (truncates toward zero)
let label = count.to_string()           // any primitive → String
```

There is no implicit numeric coercion. Mixed `Int`/`Float` arithmetic requires an explicit conversion call.

### 4.3 — Compound Types

`Optional[T]` (with `T?` shorthand), `Result[T, E]`, `List[T]`, `Map[K, V]`, `Set[T]`, tuples `(A, B, C)`.

### 4.4 — Function Types

```bock
Fn(Int, Int) -> Int
Fn(String) -> Void with Log    // function type with effects
```

### 4.5 — Generic Types

Generics use square brackets:

```bock
fn first[T](list: List[T]) -> Optional[T] {
  list.get(0)
}

record Pair[A, B] { first: A, second: B }
```

### 4.6 — Trait Bounds

```bock
fn serialize[T: Serializable](value: T) -> String

fn process[T: Serializable + Comparable](
  items: List[T],
) -> List[T]

fn merge[A, B, C](left: A, right: B) -> C
  where (A: Into[C], B: Into[C], C: Combinable)
```

### 4.7 — Refined Types

Types with compile-time or runtime-checked constraints:

```bock
type Email = String
  where (matches(r"^[^@]+@[^@]+\.[^@]+$"))
type Port = Int where (1 <= self <= 65535)
type NonEmpty[T] = List[T] where (len(self) > 0)
```

Refinements are checked at construction: statically for literals, at runtime for dynamic values.

### 4.8 — Capability Types

```bock
@requires(Capability.Network, Capability.Storage)
fn fetch_and_cache(url: Url) -> Result[Data, Error]
```

The capability taxonomy: `Network`, `Storage`, `Crypto`, `GPU`, `Camera`, `Microphone`, `Location`, `Notifications`, `Bluetooth`, `Biometrics`, `Clipboard`, `SystemProcess`, `FFI`, `Environment`, `Clock`, `Random`.

Capabilities propagate through the call graph. The compiler verifies declarations match usage.

### 4.9 — Flexible Types (Sketch Mode)

In `sketch` mode, the compiler allows `Flexible` types — tracked structurally and narrowed aggressively based on usage. The `bock promote` tool converts these to concrete types.

---

## 5. Ownership Model

Bock has a lightweight ownership system: enough to generate correct code for both GC and manual-memory targets without requiring lifetime annotations.

### 5.1 — Core Rules

1. **Values are owned.** Every value has one owner.
2. **Ownership transfers on assignment** (move semantics by default).
3. **Borrowing is implicit for reads.**
4. **Explicit `mut` for mutable borrows.**
5. **No lifetime annotations.** Inferred by the compiler.

```bock
let data = load_records()       // data owns the records
let summary = summarize(data)   // implicit borrow
transform(mut data)             // explicit mutable borrow
let archive = data              // ownership moves
// use(data)                    // ✗ Compile error: moved
```

**Control flow and ownership.** At branch join points (`if`/`else`, `match` arms), the compiler merges ownership states. Branches that diverge (`return`, `break`, `continue`, `Never`-typed expressions) are excluded from the merge — their ownership effects don't propagate past the join point. This means `guard` blocks and early-return patterns work naturally without false move warnings. For non-diverging branches, merging is conservative: if any branch moves a variable, it is considered moved after the join. Moving a variable inside a loop body is an error (second iteration would use-after-move).

### 5.2 — Target Mapping

| Bock Concept     | Rust            | GC Targets       | C++              |
|------------------|-----------------|------------------|------------------|
| Ownership        | Direct          | Ignored (GC)     | `std::move`      |
| Immutable borrow | `&T`            | Pass by value/ref| `const T&`       |
| Mutable borrow   | `&mut T`        | Pointer/reference| `T&`             |
| Move             | Move semantics  | Reassignment     | `std::move`      |

### 5.3 — `@managed` Escape Hatch

For code that doesn't need ownership tracking:

```bock
@managed
fn build_ui() -> View {
  // GC semantics regardless of target
}
```

---

## 6. Declarations

### 6.1 — Functions

```bock
fn name[T](param: Type) -> ReturnType
  with Effect1, Effect2
  where (T: Bound)
{
  body
}
```

Functions are private by default. `public` makes them visible everywhere; `internal` makes them visible within the module tree.

### 6.2 — Records

Value types with named fields:

```bock
record Session {
  id: SessionId
  user: User
  expires_at: Timestamp
  is_valid: Bool = true          // default value
}
```

### 6.3 — Enums (Algebraic Data Types)

```bock
enum AuthError {
  InvalidCredentials
  AccountLocked { until: Timestamp }
  SessionExpired { session_id: SessionId }
  RateLimited { retry_after: Duration }
}
```

### 6.4 — Classes (OOP and Multi mode)

```bock
class Button : Component, Renderable {
  label: String
  on_click: Fn() -> Void

  fn render(self) -> View {
    View.button(self.label, self.on_click)
  }
}
```

Single inheritance, multiple trait implementation.

### 6.5 — Traits

```bock
trait Renderable {
  fn render(self) -> View

  fn is_visible(self) -> Bool { true }  // default impl
}

trait Collection {
  type Item
  fn iter(self) -> Iterator[Item = Self.Item]
  fn len(self) -> Int
  fn is_empty(self) -> Bool { self.len() == 0 }
}
```

### 6.6 — Platform Traits

Traits that require per-target implementations:

```bock
platform trait Storage {
  fn read(key: String) -> Result[Optional[String], StorageError]
  fn write(key: String, value: String) -> Result[Void, StorageError]
  fn delete(key: String) -> Result[Void, StorageError]
}
```

### 6.7 — Impl Blocks

```bock
impl Log for ConsoleLog {
  fn log(level: Level, message: String) -> Void {
    println("${level}: ${message}")
  }
}
```

Associated functions (methods without `self`) are called via `Type.method()` syntax:

```bock
impl Point {
  fn origin() -> Point { Point { x: 0, y: 0 } }
  fn from_coords(x: Int, y: Int) -> Point { Point { x, y } }
}

let p = Point.origin()
let q = Point.from_coords(3, 4)
```

A bare type name in expression position is only valid when followed by `.method()` (associated function call) or `{ ... }` (record construction). Type names are not values on their own.

### 6.8 — Type Aliases

```bock
type UserId = String where (len(self) == 26)
```

### 6.9 — Constants

```bock
const MAX_RETRIES: Int = 5
const SESSION_TTL: Duration = 24.hours
```

### 6.10 — Derive Macros

```bock
@derive(Equatable, Hashable, ToJson, FromJson)
record User {
  id: UserId
  name: String
  email: Email
}
```

---

## 7. Expressions

### 7.1 — All Control Flow Is Expression-Valued

`if`, `match`, and blocks return values:

```bock
let access = if (user.is_admin) {
  Access.Full
} else {
  Access.Guest
}

let label = match status {
  Active => "active"
  Inactive => "inactive"
}
```

### 7.2 — Pipe Operator

```bock
let result = raw_data
  |> parse
  |> validate
  |> transform

// Piped value is first argument by default
data |> filter(is_valid) |> map(serialize)

// Placeholder for non-first position
headers |> add(request, _, "Content-Type")
```

The pipe operator always prepends the piped value as the first argument to the right-hand side. It does not evaluate the RHS independently. When piping into a function that returns a closure, bind the result first:

```bock
let scaler = scale_by(10.0)   // returns Fn(List[Float]) -> List[Float]
data |> scaler                 // pipes into the closure
```

### 7.3 — Function Composition

```bock
let process = parse >> validate >> transform >> serialize
```

### 7.4 — Partial Application

```bock
let add_tax = multiply(_, 1.08)
let prices = items.map(add_tax)
```

### 7.5 — Lambda Expressions

```bock
let double = (x) => x * 2

users.map((u) => {
  let name = u.full_name()
  format_greeting(name)
})
```

Parentheses around parameters are always required. Multi-statement lambdas use braces; the last expression is the return value.

### 7.6 — Collection Literals

```bock
let list = [1, 2, 3]
let map = {"key": "value", "port": 8080}
let set = #{"a", "b", "c"}
let tuple = ("hello", 42, true)
```

### 7.7 — Record Construction

```bock
let user = User {
  id: generate_id()
  name: req.name
  ..defaults                      // spread
}
```

### 7.8 — Ranges

```bock
let exclusive = 1..10             // 1 through 9
let inclusive = 1..=10            // 1 through 10
let stepped = (0..100).step(2)   // 0, 2, 4, ..., 98
```

### 7.9 — String Interpolation

```bock
"Hello, ${user.name}! You have ${items.len()} items."
```

### 7.10 — Error Propagation

```bock
let user = find_user(id)?         // returns Err early if Err
```

---

## 8. Statements and Control Flow

### 8.1 — Let Bindings

```bock
let name = "Bock"
let mut counter = 0
let (x, y) = get_point()         // destructuring
```

### 8.2 — If / Else

Parentheses around conditions are required:

```bock
if (user.is_admin) {
  grant_access()
} else if (user.is_member) {
  grant_limited()
} else {
  deny()
}
```

### 8.3 — If-Let

```bock
if (let Some(user) = find_user(id)) {
  greet(user)
}
```

### 8.4 — Guard

```bock
guard (input.is_valid()) else {
  return Err(Error.InvalidInput)
}
```

The `else` block must diverge (`return`, `break`, `continue`, or `Never`).

### 8.5 — For Loops

```bock
for item in collection { process(item) }
for (i, item) in collection.enumerate() { ... }
```

### 8.6 — While Loops

```bock
while (queue.has_items()) {
  process(queue.pop())
}
```

### 8.7 — Loop (Infinite)

```bock
loop {
  let event = poll()
  if (event.is_quit()) { break }
  handle(event)
}
```

---

## 9. Pattern Matching

```bock
match value {
  0 => "zero"
  1 | 2 => "small"
  n if (n > 100) => "large: ${n}"
  Point { x: 0, y } => "on y-axis"
  Some(Ok(v)) => "got ${v}"
  _ => "other"
}
```

Patterns support: wildcards (`_`), literals, bindings, or-patterns (`|`), guards (`if (expr)`), destructuring (records, enums, tuples, lists), nested patterns, and rest patterns (`..`). Exhaustiveness is warned in `development`, enforced in `production`.

---

## 10. Effect System

### 10.1 — Defining Effects

```bock
effect Log {
  fn log(level: Level, message: String) -> Void
}

effect Clock {
  fn now() -> Timestamp
  fn sleep(duration: Duration) -> Void
}

// Composite effects
effect Observable = Log + Trace + Metrics
```

### 10.2 — Using Effects

```bock
fn process(data: Data) -> Result[Output, Error]
  with Log, Clock
{
  log(Info, "Started at ${now()}")
  transform(data)
}
```

Effects propagate: if `A` calls `B` which requires `Log`, then `A` must also declare `Log` or provide a handler.

### 10.3 — Handler Resolution (Three Layers)

**Layer 3 — Project defaults:** Configured in `bock.project`. Applied when no closer handler exists.

```toml
[effects]
Log = "std.logging.ConsoleLog"
Clock = "std.time.SystemClock"

[effects.overrides.test]
Log = "std.testing.NullLog"
Clock = "std.testing.MockClock"
```

**Layer 2 — Module-level:** Override project defaults for a module.

```bock
handle Log with AuditLogger
```

**Layer 1 — Local handlers:** Fine-grained control via `handling` blocks.

```bock
handling (Log with test_log, Clock with mock_clock) {
  process(data)
}
```

Resolution order: Local > Module > Project. Innermost handler wins (dynamic scoping).

### 10.4 — Implementing Handlers

```bock
record ConsoleLog {}

impl Log for ConsoleLog {
  fn log(level: Level, message: String) -> Void {
    println("[${level}] ${message}")
  }
}
```

Lambda-based handlers for quick overrides:

```bock
let silent = Log.handler(
  log: (level, message) => {}
)
```

### 10.5 — Effect Categories

**Pure effects:** Computational, no outside interaction. Compiler optimizes aggressively.

**IO effects:** Touch the outside world. Correlate with capabilities.

**Ambient effects:** Always available without declaration (`Panic`, `Allocate`).

### 10.6 — Transpilation

Effects compile to parameter passing universally. Target-optimized strategies (dependency injection in Java, protocol witnesses in Swift) are applied by the AI transpiler.

When a handler is statically known, the compiler can inline it and erase the indirection entirely (effect erasure optimization, applied in `production` mode).

### 10.7 — Graduated Strictness

| Aspect              | `sketch`          | `development`      | `production`       |
|---------------------|-------------------|--------------------|--------------------|
| Declaration         | Inferred          | Required on public | Required on all    |
| Propagation         | Automatic         | Warn if undeclared | Error if undeclared|
| Handler resolution  | Project defaults  | Project + module   | Must be pinned     |

### 10.8 — Adaptive Effect Handlers

Standard effect handlers apply a fixed recovery strategy chosen at write time. Adaptive handlers extend this by selecting a strategy at runtime from a closed set of developer-defined options, using the AI provider and the semantic context carried by annotations.

**Defining an adaptive handler.**

```bock
let resilient_network = Network.adaptive(
  strategies: [
    retry(max: 3, backoff: exponential(Duration.millis(100))),
    use_cached(ttl: Duration.minutes(5)),
    degrade(fallback: default_response),
  ],
  context_aware: true   // pass @context annotations to the selector
)

handling (Network with resilient_network) {
  let status = fetch_payment_status(order_id)
}
```

When an effect operation fails, the adaptive handler receives the error, the effect operation that failed, and — when `context_aware` is enabled — the `@context`, `@performance`, `@domain`, and `@security` annotations from the call site and its enclosing module. It selects the most appropriate strategy from the closed set and executes it.

**The closed-set constraint.** An adaptive handler never generates code, synthesizes new behavior, or executes arbitrary LLM output. It selects from the strategies the developer explicitly provided. This is a classification problem, not a generation problem. The LLM's role is to understand *which* predetermined strategy best fits the runtime situation, informed by the semantic context that annotations carry.

For example, given a `ConnectionTimeout` error in a function annotated `@context("PCI-DSS compliance required")`, the selector understands that `use_cached` is inappropriate for payment data (stale financial data is dangerous) and selects `retry` instead. Without the context annotation, it might have selected `use_cached` as the faster recovery. This semantic awareness is the differentiator — no other error handling system has access to structured intent metadata at the point of failure.

**Strategy selection logging.** Every selection is recorded in the decision manifest (§17.4):

```json
{
  "type": "adaptive_recovery",
  "effect": "Network",
  "operation": "fetch_payment_status",
  "error": "ConnectionTimeout",
  "selected": "retry(max: 3, backoff: exponential)",
  "considered": ["retry", "use_cached", "degrade"],
  "reasoning": "PCI-DSS context prohibits cached financial data",
  "context": ["@context(PCI-DSS)", "@performance(latency: 200ms)"],
  "confidence": 0.92,
  "pinned": false
}
```

This integrates with `bock inspect` and `bock override` — developers can review selections, override individual decisions, and pin strategies for production reproducibility.

**Strictness interaction.**

| Level | Adaptive behavior |
|---|---|
| `sketch` | Auto-select, auto-apply. Failures logged but not reviewed. |
| `development` | Auto-select, auto-apply. All selections logged with reasoning. Developer review via `bock inspect`. |
| `production` | Pinned selections only. Adaptive handler degrades to the pinned strategy for each known error pattern. Unknown errors fall through to the deterministic fallback or propagate. |

In `production` mode, an adaptive handler with all strategies pinned is functionally identical to a deterministic handler — the AI is not consulted at runtime. This means adaptive handlers can be developed and tuned in `development` and then frozen for production without changing code.

**Pinning granularity.** Pinned selections are keyed on `(error_signature, operation)` pairs. The `error_signature` is the error type combined with a hash of its structural properties (e.g., HTTP status code, `errno` class) — not the full error instance. This means `ConnectionTimeout{after: 30s}` and `ConnectionTimeout{after: 45s}` pin to the same strategy, while `ConnectionTimeout` and `ConnectionRefused` pin independently. Same error signature at the same operation replays the same strategy; a new error signature at a pinned operation falls through to the next layer (deterministic fallback in production, AI selection in development).

**Fallback behavior.** If the AI provider is unavailable (no network, no API key, timeout), the adaptive handler falls back to the first strategy in the list. Strategies are ordered by developer preference — the first is the default. This ensures the program never depends on AI availability for correctness.

**Recovery context.** The `RecoveryContext` passed to strategies and to the AI selector contains:

```bock
record RecoveryContext {
  error: ErrorValue                    // the error that triggered recovery
  operation: String                    // effect operation name (e.g., "Network.fetch")
  annotations: ContextSnapshot         // @context, @performance, @domain, @security
                                        // from call site + enclosing module
  elapsed: Duration                    // time since operation started
  attempt: Int                         // retry attempt number (0 for first)
  history: List[ErrorRecord]           // bounded to 10 most recent errors
                                        // from this handler's lifetime
}
```

Deliberately excluded: full AIR nodes, call stack, other concurrent operations' states, source code. These would inflate prompt token cost without meaningfully improving selection quality, and could leak source structure to the AI provider in violation of `@security` classifications. The `ContextSnapshot` type exposes only the annotation values the strategy needs — it does not contain AIR references.

**Built-in strategy combinators.**

`retry(max, backoff)` — retry the failed operation with configurable backoff (linear, exponential, jittered).

`use_cached(ttl)` — return a cached result if available and within TTL. Requires a cache handler in scope.

`degrade(fallback)` — return a fallback value and continue. The fallback must type-check against the operation's return type.

`circuit_break(threshold, reset_after)` — after `threshold` consecutive failures, stop attempting the operation for `reset_after` duration and return the fallback immediately.

`escalate()` — propagate the error without recovery. Equivalent to no handler.

Developers can define custom strategies by implementing the `RecoveryStrategy` trait:

```bock
trait RecoveryStrategy[E, T] {
  fn name(self) -> String
  fn attempt(self, error: E, context: RecoveryContext)
    -> Result[T, E] | Cancelled
  fn on_cancel(self, context: RecoveryContext) -> Void = {}  // default no-op
}
```

**Cancellation.** Adaptive handlers respect task cancellation (§13.5). When the enclosing task is cancelled while a strategy is executing:

- Built-in combinators check cancellation at their internal await points — `retry` between attempts, `circuit_break` during its reset wait, `use_cached` during cache lookup
- A strategy returning `Cancelled` halts the adaptive handler immediately — no further strategies are attempted
- The `on_cancel` hook fires for the currently executing strategy, allowing cleanup of external state (failure counters, held locks, partial writes)
- The adaptive handler propagates `Cancelled` to its caller, consistent with §13.5

Custom strategies that perform blocking work or hold resources across checkpoints should implement `on_cancel` and check cancellation within `attempt` at operation boundaries.

**Runtime decisions in the manifest.** Adaptive handler selections are runtime decisions, stored separately from build-time codegen decisions. Build decisions live in `.bock/decisions/build/` and are committed to version control — they are artifacts of the compilation process, stable across runs, and reviewable as part of code review. Runtime decisions live in `.bock/decisions/runtime/` and are environment-local — they accumulate with every production event and are not committed. `bock inspect` shows build decisions by default; `bock inspect --runtime` surfaces runtime decisions with filtering by operation, error type, or time window. `bock override` operates on build decisions by default and requires explicit `--runtime` to pin runtime selections.

**Promotion path.** A runtime selection that has stabilized (same strategy chosen for the same error signature across many occurrences) can be promoted to a build-time pin. `bock override --promote <selection-id>` copies the pin from `runtime/` to `build/` and commits it to the codebase, freezing that recovery decision into the deployed configuration. This is the path by which adaptive handlers transition from "AI decides at runtime" to "code decides at build time" — the adaptive tuning phase yields deterministic production behavior.

---

## 11. Context System

### 11.1 — Purpose

Context is structured semantic metadata that serves the AI transpiler (informing code generation), the compiler (enabling verification), and human developers (self-documenting code).

### 11.2 — `@context` — Free-Form Intent

```bock
@context("""
  Payment processing module.
  PCI-DSS compliance required.
  All card data must be tokenized before storage.
""")
module app.payments
```

Supports optional structured markers within the free-form text:

```
@intent: Validate credentials and establish sessions.
@assumption: Database connection is always available.
@constraint: Must complete within 500ms p99.
@security: Input validation on all string parameters.
@related: app.sessions, app.crypto
```

### 11.3 — `@requires` — Capabilities

```bock
@requires(Capability.Network, Capability.Storage)
fn fetch_and_cache(url: Url) -> Result[Data, Error]
```

Compiler-verified: propagates through call graph, generates platform permission requests.

### 11.4 — `@performance` — Performance Budgets

```bock
@performance(max_latency: 100.ms, max_memory: 50.mb)
fn sort_results(items: List[SearchResult]) -> List[SearchResult]
```

Informs AI optimization decisions. Can generate runtime monitoring.

### 11.5 — `@invariant` — Verified Constraints

```bock
@invariant(result.len() <= input.len())
fn filter_valid(input: List[Record]) -> List[Record]
```

Static verification attempted; runtime assertion as fallback.

### 11.6 — `@security` — Security Classification

```bock
@security(level: "confidential", pii: true)
record UserProfile { ... }
```

Prevents accidental logging, generates audit trails, triggers secure coding patterns in transpilation.

### 11.7 — `@domain` — Domain Tags

```bock
@domain("e-commerce", "checkout")
module app.checkout
```

Helps the AI manage its context window across large codebases.

### 11.8 — Context Composition

Module context is inherited by declarations. Declaration-level annotations override module-level annotations of the same kind, except for capability `@requires` which is additive (declaration capabilities union with module capabilities).

**Security classification propagation.** Security classifications propagate across module boundaries at the type level. A type is PII-tainted if it is directly annotated `@security(pii: true)`, or contains a field whose type is PII-tainted, or is a generic instantiation where any type parameter is PII-tainted (e.g., `List[UserProfile]` is PII-tainted if `UserProfile` is). Any function whose signature references a PII-tainted type must exist in a module with a security context that acknowledges PII — otherwise the compiler emits a warning. When module A exports a PII-tainted function and module B imports it, module B must also carry appropriate security context. Passing PII-tainted types to logging or output functions (`print`, `println`, `log`, or any `Log`-effect function) generates a warning regardless of module context.

This is type-level analysis, not value-level taint tracking. The compiler checks what types cross function signature boundaries, not what happens to data inside function bodies.

The compiler enforces context completeness in `production` mode.

---

## 12. Module System

### 12.1 — File-Based Modules

Each `.bock` file declares its module path, which must match the filesystem path:

```
src/app/auth.bock → module app.auth
```

### 12.2 — Imports

```bock
use core.collections.{List, Map}
use app.models.User
use app.services.*                 // wildcard (discouraged)
```

### 12.3 — Visibility

```bock
public fn api_endpoint() { ... }   // visible everywhere
internal fn helper() { ... }       // visible in module tree
fn private_impl() { ... }          // visible in file only
```

Default visibility varies by strictness level.

### 12.4 — Re-exports

```bock
// In mod.bock — defines module's public API
public use app.models.user.User
public use app.models.session.Session
```

---

## 13. Concurrency

### 13.1 — Concurrent Execution

```bock
@concurrent
fn fetch_all(urls: List[Url]) -> List[Result[Response, Error]] {
  urls.map(fetch)
}
```

The transpiler selects the mechanism: `Promise.all` for JS, `tokio::join!` for Rust, goroutines for Go.

### 13.2 — Async/Await

```bock
async fn pipeline(input: Data) -> Result[Output, Error] {
  let enriched = await enrich(input)?
  let validated = await validate(enriched)?
  await store(validated)
}
```

### 13.3 — Channels

```bock
let ch = Channel[Message].new(buffer: 10)

@concurrent {
  for item in source() { ch.send(item) }
  ch.close()
}

for msg in ch { process(msg) }
```

### 13.4 — Synchronization Primitives

`Mutex[T]`, `RwLock[T]`, `Atomic[T]`, `WaitGroup`, `OnceCell[T]` — available from `core.concurrency`.

### 13.5 — Cancellation

Cancellation is modeled as an ambient effect (`Cancel`) available in every async context. It delivers a signal that a task should stop execution at the next cooperative checkpoint. Cancellation is cooperative — a task that never reaches a checkpoint cannot be cancelled.

**Checkpoints.** The compiler inserts cancellation checks at well-defined points:

- Every `await` expression
- Every effect operation invocation (`with Clock`, `with Network`, etc.)
- Explicit `check_cancel()` calls for tight loops that don't otherwise reach a checkpoint
- Loop iteration boundaries in `@concurrent` blocks

At each checkpoint, if cancellation has been signaled, the task propagates a `Cancelled` value through the call stack. This is not an exception — it's a typed return value that the type system tracks like any other `Result`-like outcome.

**Requesting cancellation.** A task handle exposes `cancel()`:

```bock
let task = @concurrent { long_running_operation() }
// ... later
task.cancel()
let result = await task   // returns Cancelled
```

Structured concurrency: cancelling a task cancels all of its child tasks transitively. `@concurrent { ... }` blocks propagate cancellation to every operation started within them.

**Checking cancellation manually.**

```bock
fn compute_intensive(data: List[Int]) -> Result[Summary, Cancelled] with Cancel {
  let mut acc = 0
  for (i, x) in data.enumerate() {
    if (i % 1000 == 0) { check_cancel()? }
    acc = acc + expensive(x)
  }
  Ok(summarize(acc))
}
```

The `?` propagates `Cancelled` the same way it propagates `Err`. Functions that observe cancellation declare the `Cancel` effect; functions that only pass through cancellation do not need to declare it (the ambient effect is always available in async contexts).

**Target mapping.** The transpiler maps the `Cancel` effect to each target's native mechanism:

| Target | Mechanism                          |
|--------|------------------------------------|
| Rust   | `tokio::sync::CancellationToken`   |
| JS/TS  | `AbortSignal`                      |
| Go     | `context.Context` with `Done()`    |
| Python | `asyncio.Task.cancel()` + check    |

**Cancellation and cleanup.** Code that holds resources across a checkpoint must handle cancellation explicitly. The `with` handler mechanism provides the standard cleanup pattern — handlers can register cleanup on the `Cancel` effect to release resources when the enclosing task is cancelled.

**Strictness interaction.**

| Level | Cancellation behavior |
|---|---|
| `sketch` | Checkpoints auto-inserted; no annotations required |
| `development` | Long-running operations (loops, recursion) warned if no `check_cancel()` reachable |
| `production` | Error if a `@concurrent` or `async` function has no reachable checkpoint within a configurable depth bound |

---

## 14. Interop and FFI

### 14.1 — Native Blocks

```bock
@target(js)
native fn query_selector(sel: String) -> Optional[Element] {
  `document.querySelector(${sel})`
}
```

### 14.2 — Platform Abstraction Layer

For structured multi-target APIs, `platform trait` defines an interface with per-target implementations.

FFI usage in multi-target projects triggers a linter warning suggesting migration to a platform trait.

---

## 15. Annotations

Annotations use the `@` prefix and form a unified metadata system:

**Compiler directives:** `@concurrent`, `@managed`, `@deterministic`, `@inline`, `@deprecated("use X")`, `@cold`, `@hot`.

**Capabilities:** `@requires(Capability.Network)`.

**Target:** `@target(js, rust)`, `@platform(ios: min_version("16.0"))`.

**Context:** `@context("...")`, `@performance(...)`, `@invariant(...)`, `@security(...)`, `@domain(...)`.

**Testing:** `@test`, `@test(skip: "reason")`, `@benchmark`, `@property`.

**Code generation:** `@derive(Trait1, Trait2)`.

---

## 16. Annotated Intermediate Representation (AIR)

### 16.1 — Layer Model

AIR is structured in four layers, each adding information:

**Layer 0 — Structural AIR (S-AIR):** Syntax tree with resolved names and scopes. Produced by the parser and name resolver. Deterministic.

**Layer 1 — Typed AIR (T-AIR):** All types resolved, ownership annotations attached, effects tracked, capabilities computed. Produced by the type checker and ownership analyzer. Deterministic. This is the layer used for binary package distribution.

**Layer 2 — Contextual AIR (C-AIR):** Context annotations parsed, validated, and attached. Cross-module context composed. Produced by the context resolver. Deterministic.

**Layer 3 — Target-Ready AIR (TR-AIR):** Target capability gaps identified, platform abstractions resolved, FFI blocks filtered. Ready for code generation. Produced by the target analyzer. Deterministic.

All layers are deterministic. AI enters only after AIR production.

### 16.2 — Node Structure

Every AIR node carries:

```
AIRNode {
  id: NodeId                     // stable across incremental builds
  kind: NodeKind                 // semantic category (typed children within)
  span: SourceSpan               // source location
  type: TypeInfo                 // resolved type (Layer 1+)
  ownership: OwnershipInfo       // ownership state (Layer 1+)
  effects: Set[Effect]           // declared/inferred effects (Layer 1+)
  capabilities: Set[Capability]  // required capabilities (Layer 1+)
  context: ContextBlock          // attached context (Layer 2+)
  target: TargetInfo             // target analysis (Layer 3+)
  metadata: Map[String, Value]   // extensible
}
```

Children are structurally typed within each `NodeKind` variant rather than stored in a flat list. For example, `NodeKind::FnDecl` carries `params: Vec<AIRNode>`, `body: AIRNode`, and `return_type: Option<AIRNode>` as typed fields. This mirrors the AST structure and gives the compiler structural guarantees at each layer.

### 16.3 — Serialization

**AIR-T (text format):** Human-readable, designed for AI consumption. This is what the AI transpiler receives.

**AIR-B (binary format):** Compact, content-addressed, module-level granularity. Used for build caches and binary package distribution.

### 16.4 — Binary Package Compatibility

Packages distribute pre-compiled T-AIR alongside source. Compatibility rules:

- Patch releases (1.2.x): Always compatible.
- Minor releases (1.x.0): Backward compatible (new features not pre-compiled).
- Major releases (x.0.0): Recompile from source (automatic fallback).

The compiler checks AIR format version and falls back to source compilation transparently when incompatible.

---

## 17. Transpilation Pipeline

### 17.1 — Pipeline Stages

```
Source → Parse → Type Check → Context Resolve → Target Analyze
                                                      │
                                              Code Generation
                                            (AI or Rule-Based)
                                                      │
                                                 Verification
                                               (deterministic)
                                                      │
                                             Target Compilation
                                            (target toolchain)
                                                      │
                                           Deliverable Assembly
```

### 17.2 — AI-First with Deterministic Fallback

**Tier 1 — AI Generation (default):** The AI model receives TR-AIR + target profile + project context and generates idiomatic target code. AI generation is invoked selectively at capability gap points identified by §17.6 and at AIR constructs the target profile flags as requiring idiomatic synthesis (e.g., complex pattern translations, effect handler composition). Trivial constructs with stable rule-based translations (literals, arithmetic, direct function calls) are handled by Tier 2 rules without AI involvement, even when Tier 1 is enabled. This keeps compilation cost bounded and ensures most AIR nodes compile deterministically.

**Tier 2 — Rule-Based Generation (fallback):** Traditional deterministic transpilation via syntax rules and templates. Handles the common case by default and serves as the fallback when Tier 1 fails or is unavailable. Activated exclusively via `bock build --deterministic` or `@deterministic`.

**Tier 3 — AI Optimization (optional):** A second AI pass reviewing generated code for performance and idiomaticness. Activated via `bock build --optimize`.

### 17.3 — Verification (Always Deterministic)

Generated code is checked for: valid syntax in the target language, preserved semantic contracts, passing test translations, and correct capability profile.

### 17.4 — Decision Manifest

Every AI decision is recorded:

```json
{
  "module": "src/net/http_client.bock",
  "target": "rust",
  "decision": "async_runtime",
  "choice": "tokio",
  "alternatives": ["async-std", "smol"],
  "reasoning": "Project depends on axum which requires tokio",
  "model": "bock-codegen-v3.1",
  "confidence": 0.92,
  "pinned": false
}
```

Confidence is a float in the range `0.0`–`1.0`. The compiler accepts AI output with confidence at or above the configured threshold (default `0.75`) and falls back to Tier 2 rule-based generation when confidence is below the threshold or when the provider is unavailable. The threshold is configurable via the `[ai]` section in `bock.project`.

**Build-time vs runtime decisions.** The manifest tracks two populations of decisions with different lifecycles:

*Build decisions* are made during compilation — codegen choices, capability gap synthesis, optimization selections. They are stored in `.bock/decisions/build/`, committed to version control, and reviewed as part of code review. They are stable artifacts of the build.

*Runtime decisions* are made during program execution — adaptive effect handler selections (§10.8) are the primary source. They are stored in `.bock/decisions/runtime/`, environment-local, not committed. They accumulate with every production event and are subject to log rotation or size caps.

`bock inspect` shows build decisions by default. `bock inspect --runtime` surfaces runtime decisions with filtering by operation, error type, or time window. `bock inspect --all` presents both with clear separation. `bock override` operates on build decisions by default; `--runtime` scopes it to runtime pins. `bock override --promote <selection-id>` moves a stabilized runtime pin into the build manifest, committing it to the codebase.

In `production` strictness, all decisions must be pinned. Pinned decisions bypass the confidence check — the stored choice is replayed identically regardless of any new AI response. `bock inspect` browses decisions; `bock override` changes them.

### 17.5 — Deliverables

```bash
bock build --target android    → APK/AAB + Gradle project
bock build --target ios        → Xcode project + IPA
bock build --target web        → Bundled SPA/SSR application
bock build --target linux-svc  → systemd-ready binary
bock build --target docker     → Container image
bock build --target lambda     → Deployment package
```

Deliverables are configured via target manifests (`.target` files) specifying platform resources, signing, bundling, and optimization.

### 17.6 — Capability Gap Resolution

When AIR uses a feature a target lacks, the transpiler synthesizes it:

| AIR Construct    | Gap Example          | Synthesis                    |
|------------------|----------------------|------------------------------|
| Algebraic types  | JS (no ADTs)         | Tagged objects + switch       |
| Pattern matching | Go (no match)        | if/else chains               |
| Ownership/Move   | JS, Python (GC)      | Erase annotations            |
| Channels         | JS (no native)       | AsyncIterator + Queue class  |
| Refinement types | All targets          | Validation at boundary       |
| Effects          | All targets          | Parameter passing            |

### 17.7 — Codegen Rule Learning (Post-v1)

The rule-based generator (Tier 2) ships with a fixed set of translation rules per target. In practice, every target language has an unbounded number of idiomatic patterns that fixed rules cannot anticipate — prelude function mappings, syntactic idiom translations, formatting conventions, and API surface differences across language versions. Rather than enumerating these exhaustively, the compiler supports a feedback loop that grows the rule base from real compilation experience.

**The loop.** When Tier 2 generates code that fails target compilation or verification, an LLM agent receives the AIR, the failing generated code, and the compiler error. It produces both a fix and a candidate rule — a pattern-template pair that would have generated the correct code deterministically. The candidate rule is validated (the target compiler must accept its output for all existing test cases), then merged into the local rule cache.

**Rule scope.** The feedback loop handles two categories:

*Prelude function mapping* — bounded, deterministic translations of core library functions to target equivalents (e.g., `println` → `console.log` in JS, `println!("{}", x)` in Rust, `fmt.Println(x)` in Go). These are pure lookup tables.

*Syntactic idiom translation* — deterministic rewrites of known AIR patterns to target-idiomatic forms (e.g., `match` with simple string arms → `switch` in JS/Go, multi-line strings → target-specific quoting). These are pattern-template pairs.

The loop does not attempt semantic gap bridging (ADTs → tagged objects, channels → async queues) or runtime synthesis. These require contextual reasoning and remain in the AI tier (Tier 1). However, when an AI-generated solution for a semantic gap recurs across multiple modules with a stable pattern, the compiler may propose promoting it to a rule — subject to validation and, in production strictness, human approval.

**Rule format.** Rules are declarative pattern-template mappings stored alongside the project or distributed as packages. Each rule specifies an AIR pattern to match, a target language, a code template with interpolation slots, and a priority for conflict resolution. The exact format is TBD pending implementation experience with the Tier 2 generators.

**Strictness interaction.** Rule learning follows the graduated strictness model:

| Level | Rule application | Rule extraction | New rules |
|---|---|---|---|
| `sketch` | Auto-apply | Auto-extract | Applied immediately |
| `development` | Auto-apply, logged | Extract + propose | Require review |
| `production` | Pinned rules only | Disabled | Require approval + pin |

**Decision manifest integration.** Applied rules are logged as decision manifest entries (§17.4) with type `codegen_rule_applied`, referencing the rule ID, its provenance (builtin, extracted, or manual), and pin status. This integrates with `bock inspect` and `bock override` — a developer can browse which rules were applied, override specific rules, or pin rules for production reproducibility.

**Distribution.** Rule packages are distributed through the existing package registry (§19). A project can depend on curated rule packages for specific targets or language versions (e.g., `codegen-rules-node22`, `codegen-rules-python312`). Local rules take priority over distributed rules. There is no centralized rule aggregation service — curation happens through the normal package publishing process.

### 17.8 — AI Provider Interface

The compiler communicates with AI models through a provider-agnostic interface that abstracts over API formats and hosting. The codegen pipeline calls the provider; the provider handles prompt construction, HTTP transport, and response parsing.

**Interaction modes.** The provider interface supports four modes, corresponding to the three tiers of transpilation plus runtime strategy selection:

*Generate* — receives AIR + target profile + project context, returns target code with confidence metadata and alternatives considered. This is the Tier 1 (AI generation) path. The response includes enough information to populate a decision manifest entry (§17.4).

*Repair* — receives failing generated code + target compiler error + original AIR, returns fixed code and optionally a candidate rule (pattern-template pair) that would have generated the correct code deterministically. This is the feedback loop path (§17.7).

*Optimize* — receives working target code + original AIR + target profile, returns improved code with an explanation of changes. This is the Tier 3 (AI optimization) path.

*Select* — receives a closed set of options (strategy identifiers), a selection context, and semantic annotations, and returns a choice identifier from the provided set plus confidence and reasoning. This is the adaptive effect handler path (§10.8). Unlike the other three modes, Select returns a classification decision, never generated code. The provider must not return identifiers outside the provided set; the trait's return type enforces this.

All four modes go through the same provider configuration. The compiler constructs mode-specific prompts; the provider handles transport.

**Verification is separate.** Code verification (§17.3) is always deterministic and does not involve the AI provider. The codegen pipeline calls the provider for generation, then calls the verifier (owned by the target profile) independently. The provider never self-validates its own output.

**Built-in providers.** Two provider types ship with the compiler:

An *OpenAI-compatible* provider that speaks the Chat Completions API format (`/v1/chat/completions`). This covers the majority of cloud and local model providers, including local inference servers. This is the default for local model usage.

An *Anthropic Messages API* provider that speaks the native Anthropic format (`/v1/messages`). This enables access to features not expressible in the Chat Completions format — extended thinking (which provides reasoning traces for decision manifests), structured content blocks (which separate code from explanation), and system prompt separation (which improves prompt engineering for code generation).

Additional providers can be added as trait implementations without changes to the codegen pipeline. The interface is designed so that two implementations cover the current provider landscape; a plugin system is not warranted at this time.

**Configuration.** Provider selection, endpoint, model, and credentials are configured in the project file's `[ai]` section (Appendix A). API keys are referenced by environment variable name — keys never appear in project files. The specific provider type identifiers, supported model names, and endpoint URLs are documented in the tooling guide, not in this specification, as they evolve independently of the language.

**Caching.** AI responses may be cached for reproducibility and cost reduction. The cache is content-addressed, keyed on the hash of the full request (AIR + context + model identifier). Cached responses are treated as pinned decisions — replaying a cached response produces identical output regardless of model non-determinism. Cache behavior is configured via the `[ai]` section.

---

## 18. Standard Library

### 18.1 — Two-Tier Architecture

**`core` — Ships with the compiler.** Small, stable, works on every target. Contains primitives, fundamental traits, collections, error handling, concurrency primitives, and the test framework.

**`std` — First-party packages.** Rich functionality installed via the package manager. Can evolve independently of the language version. Includes time/date, JSON, filesystem, HTTP, crypto, logging, tracing, storage, process management, regex, validation, configuration, and advanced data structures.

### 18.2 — Prelude (Auto-Imported)

Always available without import: `Int`, `Float`, `Bool`, `String`, `Char`, `Void`, `Never`, `Duration`, `Instant`, `Optional`/`Some`/`None`, `Result`/`Ok`/`Err`, `List`, `Map`, `Set`, `Fn`, core traits (`Comparable`, `Equatable`, `Hashable`, `Displayable`, `Serializable`, `Cloneable`, `Default`, `Into`, `From`, `Iterator`, `Iterable`), utility functions (`print`, `println`, `debug`, `assert`, `todo`, `unreachable`, `sleep`).

### 18.3 — Core Modules

`core.types` — Sized integers and floats, `BigInt`, `Decimal`.
`core.collections` — `List`, `Map`, `Set`, `Deque`, `SortedMap`, `SortedSet`, `Stack`, `Queue`, `BitSet`, `Array[T, N]`.
`core.string` — String manipulation, `StringBuilder`, `Regex`. `String.len()` returns the count of Unicode scalar values (characters), not bytes; use `byte_len()` for byte count.
`core.math` — Constants, functions, numeric traits.
`core.option` — `Optional[T]` utilities.
`core.result` — `Result[T, E]` utilities.
`core.iter` — `Iterator` trait and combinators.
`core.compare` — `Ordering`, `Comparable`, `Equatable`.
`core.convert` — `Into`, `From`, `TryFrom`, `Displayable`.
`core.error` — `Error` base trait.
`core.effect` — Effect system primitives.
`core.concurrency` — `Channel`, `Mutex`, `RwLock`, `Atomic`, `WaitGroup`.
`core.memory` — `Rc`, `Arc` for `@managed` contexts.
`core.time` — `Duration`, `Instant`, `sleep`, monotonic time primitives, `Clock` effect.
`core.test` — Assertions, BDD grouping, mocking, property testing, benchmarking.

### 18.3.1 — core.time

Monotonic time primitives, available on all targets. `core.time` owns the `Clock` effect; `std.time` provides the default handler and extends `core.time` with wall-clock operations, dates, and timezones.

**Types.**

`Duration` — a span of time. Internally stored as `Int64` nanoseconds, giving a range of approximately ±292 years. Sufficient for all realistic use cases.

`Instant` — a monotonic point in time. Comparable within a single process run; not comparable across processes or across reboots.

**Prelude function.**

```bock
sleep(duration: Duration) -> Void with Clock
```

Suspends execution for the given duration. In an `async` context, yields to the runtime so other tasks may proceed. In synchronous code, blocks the current thread. Requires the `Clock` effect.

**Duration constructors.**

```bock
Duration.zero() -> Duration
Duration.nanos(n: Int) -> Duration
Duration.micros(n: Int) -> Duration
Duration.millis(n: Int) -> Duration
Duration.seconds(n: Int) -> Duration
Duration.minutes(n: Int) -> Duration
Duration.hours(n: Int) -> Duration
```

**Duration methods.**

```bock
as_nanos() -> Int
as_millis() -> Int
as_seconds() -> Int
is_zero() -> Bool
is_negative() -> Bool
abs() -> Duration
```

**Duration arithmetic.**

```bock
Duration + Duration -> Duration
Duration - Duration -> Duration
Duration * Int      -> Duration        // scalar multiplication
Duration / Int      -> Duration        // scalar division
```

`Duration` implements `Comparable` and `Equatable`.

**Instant operations.**

```bock
Instant.now()                             -> Instant with Clock
instant.elapsed()                         -> Duration with Clock
instant.duration_since(earlier: Instant)  -> Duration
```

**Instant arithmetic.**

```bock
Instant - Instant   -> Duration         // sugar for duration_since
Instant + Duration  -> Instant
Instant - Duration  -> Instant
```

**Clock effect.**

```bock
effect Clock {
  fn now_monotonic() -> Instant
  fn sleep(duration: Duration) -> Void
}
```

The default handler (`std.time.SystemClock`) uses the target's native monotonic clock and sleep primitives. Test environments typically override with `std.testing.MockClock`, which replaces `sleep` with virtual time advancement — a test containing `sleep(Duration.seconds(60))` advances mock time by 60 seconds without actually blocking. This enables fast, deterministic tests of time-dependent code.

### 18.4 — Standard Modules

`std.time` — Wall-clock time, dates, timezones, calendar arithmetic. Extends `core.time` with the default `Clock` handler (`SystemClock`).
`std.json` — Parse, stringify, derive-based serialization.
`std.io` — Read/Write traits, buffered I/O, streams.
`std.fs` — Path, File, `FileIO` effect.
`std.net.http` — Client, Request, Response, middleware.
`std.net.tcp` — TCP listeners and streams.
`std.net.websocket` — WebSocket connections.
`std.crypto` — Hashing, HMAC, encryption, password hashing, `Random` effect.
`std.encoding` — Base64, hex, UTF-8, URL encoding.
`std.logging` — `Log` effect, handlers (Console, File, Structured, Null, Capture).
`std.tracing` — `Trace` effect, OpenTelemetry support.
`std.storage` — `Storage` effect, filesystem and in-memory handlers.
`std.process` — `Process` effect, command execution.
`std.regex` — Extended regex support.
`std.validation` — Composable validators, built-in rules.
`std.config` — Multi-format configuration loading.
`std.collection_ext` — Trie, LRU cache, bloom filter, priority queue.
`std.math_ext` — Complex numbers, matrices, statistics.
`std.testing` — Extended test framework, fixtures, snapshot testing.

### 18.5 — Trait-Language Integration

Core traits opt types into language features: `Comparable` enables `<`/`>`, `Iterable` enables `for..in`, `Displayable` enables `${}` interpolation, `Add`/`Sub`/etc. enable operator overloading.

---

## 19. Package Manager

### 19.1 — Design Principles

Deterministic builds (same `bock.lock` = same output). Target-aware dependencies. AI model versioning as a dependency axis. Security-first (signatures, checksums, auditing).

### 19.2 — Package Manifest (`bock.package`)

```toml
[package]
name = "http-framework"
version = "2.1.0"
license = "MIT"

[package.targets]
supported = ["js", "rust", "go", "python"]

[dependencies]
core-http = "^1.0"

[dependencies.target.js]
node-adapter = "^1.0"

[dev-dependencies]
test-client = "^1.0"

[features]
default = ["json", "logging"]
full = ["json", "xml", "websocket", "logging"]
```

### 19.3 — Dependency Resolution

Semver constraints, target filtering, feature unification, conflict detection. Uses the PubGrub algorithm. Transitive dependencies are private by default.

### 19.4 — AI Model as Dependency

Model version tracked in `bock.lock`. Floats in `sketch`, logged in `development`, pinned in `production`. Packages declare minimum model requirements.

### 19.5 — Private Registries

The registry protocol is an open HTTPS REST API. Anyone can host a compatible registry. Configuration:

```toml
[registries]
internal = "https://bock.company.internal"

[dependencies]
"@company/auth" = { version = "^3.0", registry = "internal" }
```

Supports mirroring for air-gapped environments.

### 19.6 — Workspaces

```toml
# bock.workspace
[workspace]
members = ["packages/core", "packages/web", "shared"]

[workspace.dependencies]
core-http = "^1.2"
```

### 19.7 — Versioning and Stability

Strict semver. Stability tiers: `stable`, `beta`, `experimental`. Production strictness can reject dependencies below a stability threshold.

---

## 20. Tooling

### 20.1 — CLI (`bock`)

Single binary containing all tooling. The CLI surface is designed for ergonomic discoverability — verbs that describe complete operations are top-level commands rather than flags on broader commands. The shape may evolve through implementation experience; when it does, this section is amended to match. The spec is normative for capabilities, not for the precise shape of the command surface.

**Build and execute:**

`bock new` — Project scaffolding with interactive or flag-based configuration. Generates `bock.project` with a commented-out `[ai]` block for opt-in AI configuration; see §20.7.
`bock build` — Transpile and compile. Produces a scaffolded project (project mode) by default; see §20.6.2 for output modes. Flags: `--target`, `--all-targets`, `--source-only`, `--deliverable`, `--deterministic`, `--optimize`, `--release`.
`bock run` — Build and execute. Default uses interpreter. `--target` for specific language. `--watch` for hot reload.
`bock check` — Type check, lint, context validation. `--types`, `--lint`, `--context` for selective checking.
`bock test` — Run tests. Default uses interpreter (fast). `--target` for transpilation tests. `--all-targets`, `--smart` for cross-target. `--coverage`, `--snapshot`.
`bock fmt` — Format (one style, zero configuration). `--check` for CI.
`bock fix` — Auto-fix lint warnings.
`bock repl` — Interactive REPL with `:type`, `:air`, `:target` commands.

**Decision and rule manifest management:**

`bock inspect` — Read-only browsing of AI decisions, rule cache, and AI response cache. Defaults to build decisions; `--runtime` for runtime decisions, `--all` for both. `--diff` for changes since last build.
`bock pin <decision-id>` — Pin a decision so it replays deterministically. `--all` to pin every unpinned decision (production readiness).
`bock unpin <decision-id>` — Remove pin metadata from a decision.
`bock override <decision-id> --choice=<alternative>` — Change which alternative is selected for an existing decision. `--promote <runtime-id>` migrates a stabilized runtime decision into the build manifest (§10.8).
`bock cache` — Manage on-disk caches (AI response cache, rule cache, decision manifests). Subcommands: `list`, `clear`, `prune`, `stats`.

**Project lifecycle:**

`bock promote` — Migrate code to higher strictness level with auto-fixes.
`bock migrate` — AI-assisted import from other languages.
`bock doc` — Generate, serve, and publish documentation.
`bock pkg` — Package management (add, remove, update, audit, publish, search).
`bock model` — AI model management (list, install, use).
`bock target` — Target management (list, add, info).
`bock ci` — Run all CI checks in one command.

### 20.2 — Formatter

Zero configuration. One canonical style.

- 2-space indentation
- 80-character soft limit, 100 hard limit
- Opening brace on same line
- Newline-terminated statements (semicolons optional)
- Trailing commas in multi-line constructs
- Sorted imports (core → std → external → local)
- Consistent wrapping for long signatures

### 20.3 — Language Server (LSP)

Full LSP implementation plus Bock-specific extensions:

**AI Context Panel:** Real-time view of what the AI transpiler sees at cursor position — context annotations, capabilities, effects, ownership state, active handlers.

**Target Preview:** Live transpiled output for any function, switchable between targets.

**Capability Graph:** Visual call-graph with capability and effect propagation.

**Smart Completions:** Ownership-aware (marks consuming methods), effect-aware (suggests effect operations), pipe-aware (suggests type-compatible functions).

**Inline Diagnostics:** Ownership transfer warnings, capability narrowing hints, AI decision previews.

### 20.4 — Testing Tiers

**Tier 1 — Semantic tests:** Run on the Bock interpreter. Fast. Target-independent. The canonical semantics reference.

**Tier 2 — Transpilation tests:** Same tests compiled to target languages. Per-target execution.

**Tier 3 — Integration tests:** Platform-specific tests (`@target`, `@platform` annotated) requiring actual runtimes.

**Smart target selection:** Analyzes which AIR constructs changed and which targets are affected. Tests targets where changed constructs must be emulated (high risk), skips targets with native support (low risk).

Principle: semantic pass + target fail = transpiler bug, not user code bug.

### 20.5 — Debugger

Built-in interpreter debugger with breakpoints, stepping, expression evaluation, ownership state inspection, effect handler display, and context viewing. Source maps enable debugging transpiled code in target-language debuggers.

### 20.6 — Build System

Incremental builds at module granularity via content hashing. Parallel compilation. Remote build cache. Build hooks (Bock scripts). Distributed builds for CI.

Build pipeline: Parse → Type Check → Context Resolve → Target Analyze → Code Generate → Verify → Target Compile → Assemble Deliverable.

#### 20.6.1 — Output Layout

Build output preserves the source filesystem structure. A source file at `src/<path>.bock` produces output at `build/<target>/<path>.<ext>`, where `<ext>` is the target language's idiomatic extension. Module nesting in `src/` is preserved in the target output — `src/foo/bar.bock` becomes `build/js/foo/bar.js`, `build/py/foo/bar.py`, and so on.

Target-specific scaffolding files (manifests, package descriptors, ecosystem-required entry points) are generated alongside the mirrored source structure at `build/<target>/` root or per the target ecosystem's conventions. These are part of producing usable output, not in place of it. Per-target scaffolding details are documented in each target's codegen package.

Entry-point selection — which output file is invoked when running the build artifact — is a project-level concern documented in `bock.project`, not derived from the filename convention. By default, `src/main.bock` is the entry point if present.

#### 20.6.2 — Output Modes

`bock build --target T` produces output in one of three modes, selected by flag. (These are distinct from the AI involvement tiers in §17.2; "tier" is reserved for those, "mode" describes output completeness.)

**Source mode.** Bare transpilation: target source files mirroring the project's source structure, with no manifests, scaffolding, or entry-point wiring. The output is suitable for integration into an existing target-language project the user already manages.
- Flag: `--source-only`
- Output: source files only

**Project mode.** Source files plus target-ecosystem scaffolding — the manifests, configuration, and entry-point wiring needed for the output to be a working project in the target language's normal toolchain. After `bock build --target rust`, the user can `cd build/rust && cargo build`. After `bock build --target js`, the user can `cd build/js && npm install && node main.js`.
- Flag: default for `bock build --target T`
- Output: source files + target-ecosystem scaffolding (`package.json` / `Cargo.toml` / `pyproject.toml` / `go.mod` / `tsconfig.json` etc.)

**Deliverable mode.** Final runnable artifact: bundled JS, compiled binary, container image, mobile package, deployment archive. Deliverable mode may invoke external tooling (target compilers, bundlers, containerizers) beyond Bock's own transpiler. See §17.5 for deliverable types and configuration via target manifests.
- Flag: `--deliverable`
- Output: target-specific runnable artifact

The default is project mode because `bock build` implies producing something the user can run. Source mode exists for the integration-into-existing-project case; deliverable mode exists for production deployment. The mode flags are mutually exclusive — a single `bock build` invocation produces output in exactly one mode.

Per-target scaffolding details (the contents of `package.json`, the structure of a generated `Cargo.toml`, the layout of a Python project) are documented in each target's codegen package. The spec commits to the mode model and the structural distinction; per-target manifest contents evolve with the target ecosystems and are not enumerated here.

### 20.7 — Project Scaffolding

`bock new <name>` generates a minimal project structure: `bock.project`, `.gitignore`, `src/main.bock`, and `tests/`.

The generated `bock.project` includes a commented-out `[ai]` block that documents AI provider configuration without activating it. Bock uses rule-based code generation by default; AI configuration is opt-in. The commented block makes the configuration surface discoverable without prescribing provider choices, requiring API keys, or assuming network availability at project creation.

```toml
# AI provider configuration (optional)
# Bock uses rule-based code generation by default. Configure an AI
# provider below to enable AI-assisted generation for capability gaps.
# See documentation for setup guides.
#
# [ai]
# provider = "openai-compatible"  # or "anthropic"
# endpoint = "..."
# model = "..."
# api_key_env = "..."
```

The scaffolder does not prompt interactively for provider configuration during `bock new`. Interactive flows fail awkwardly in CI and scripted contexts and demand provider knowledge from users whose first interaction with Bock is project creation. Users who want AI-assisted codegen uncomment and complete the block; users who do not delete it.

---

## 21. Formal Grammar

The complete EBNF grammar follows. Notation: `UPPER_CASE` = terminal tokens, `lower_case` = non-terminal productions, `'literal'` = keyword/symbol, `[ ]` = optional, `{ }` = repetition, `|` = alternation.

### 21.1 — Module Structure

```ebnf
source_file = { module_doc_comment } [ module_decl ]
              { import_decl } { top_level_item } ;
module_decl = 'module' module_path NEWLINE ;
module_path = IDENT { '.' IDENT } ;
import_decl = 'use' module_path [ import_list ] NEWLINE ;
import_list = '.' '{' IDENT { ',' IDENT } [ ',' ] '}'
            | '.' IDENT | '.' '*' ;
```

### 21.2 — Top-Level Items

```ebnf
top_level_item = { annotation }
    ( fn_decl | record_decl | enum_decl | class_decl
    | trait_decl | platform_trait_decl | impl_block
    | effect_decl | type_alias | const_decl
    | module_handle_decl | property_test_decl ) ;
```

### 21.3 — Annotations

```ebnf
annotation = '@' annotation_name [ '(' annotation_arg_list ')' ] ;
annotation_name = IDENT { '.' IDENT } ;
annotation_arg_list = annotation_arg { ',' annotation_arg } [ ',' ] ;
annotation_arg = expression | IDENT ':' expression
               | STRING_LITERAL | MULTILINE_STRING ;
```

### 21.4 — Functions

```ebnf
fn_decl = [ visibility ] [ 'async' ] 'fn' IDENT
          [ generic_params ] '(' [ param_list ] ')'
          [ '->' type_expr ] [ effect_clause ]
          [ where_clause ] block ;
visibility = 'public' | 'internal' ;
param_list = param { ',' param } [ ',' ] ;
param = [ 'mut' ] ( 'self' | IDENT ':' type_expr [ '=' expression ] ) ;
effect_clause = 'with' type_path { ',' type_path } ;
where_clause = 'where' '(' type_constraint { ',' type_constraint } [ ',' ] ')' ;
type_constraint = TYPE_IDENT ':' type_bound { '+' type_bound } ;
```

### 21.5 — Type Declarations

```ebnf
record_decl = [ visibility ] 'record' TYPE_IDENT [ generic_params ]
              [ where_clause ] '{' { record_field } '}' ;
record_field = { annotation } [ visibility ] IDENT ':' type_expr
               [ '=' expression ] NEWLINE ;
enum_decl = [ visibility ] 'enum' TYPE_IDENT [ generic_params ]
            [ where_clause ] '{' enum_variant { NEWLINE enum_variant } '}' ;
enum_variant = { annotation } TYPE_IDENT [ enum_variant_body ] ;
enum_variant_body = '{' record_field { record_field } '}'
                  | '(' type_expr { ',' type_expr } ')' ;
class_decl = [ visibility ] 'class' TYPE_IDENT [ generic_params ]
             [ ':' type_expr { ',' type_expr } ]
             [ where_clause ] '{' { class_member } '}' ;
trait_decl = [ visibility ] 'trait' TYPE_IDENT [ generic_params ]
             [ ':' type_bound { '+' type_bound } ]
             [ where_clause ] '{' { trait_member } '}' ;
platform_trait_decl = [ visibility ] 'platform' 'trait' TYPE_IDENT
                      [ generic_params ] [ where_clause ]
                      '{' { trait_member } '}' ;
impl_block = 'impl' [ generic_params ]
             [ type_path [ generic_args ] 'for' ]
             type_path [ generic_args ]
             [ where_clause ] '{' { fn_decl } '}' ;
```

### 21.6 — Effects

```ebnf
effect_decl = [ visibility ] 'effect' TYPE_IDENT
              [ '=' type_path { '+' type_path } ]
              '{' { fn_signature } '}' ;
            | [ visibility ] 'effect' TYPE_IDENT
              '=' type_path { '+' type_path } NEWLINE ;
fn_signature = [ visibility ] [ 'async' ] 'fn' IDENT
               [ generic_params ] '(' [ param_list ] ')'
               [ '->' type_expr ] [ effect_clause ]
               [ where_clause ] NEWLINE ;
```

### 21.7 — Other Declarations

```ebnf
type_alias = [ visibility ] 'type' TYPE_IDENT [ generic_params ]
             '=' type_expr [ 'where' '(' refinement_predicate ')' ] NEWLINE ;
const_decl = [ visibility ] 'const' IDENT ':' type_expr '=' expression NEWLINE ;
module_handle_decl = 'handle' type_path 'with' expression NEWLINE ;
```

### 21.8 — Type Expressions

```ebnf
type_expr = type_primary { type_postfix } ;
type_primary = type_path [ generic_args ]
             | '(' type_expr { ',' type_expr } ')'
             | '(' ')' | fn_type | 'Self' ;
type_postfix = '?' ;
type_path = TYPE_IDENT { '.' TYPE_IDENT }
          | module_path '.' TYPE_IDENT ;
generic_args = '[' type_expr { ',' type_expr } [ ',' ] ']' ;
generic_params = '[' generic_param { ',' generic_param } [ ',' ] ']' ;
generic_param = TYPE_IDENT [ ':' type_bound { '+' type_bound } ] ;
fn_type = 'Fn' '(' [ type_expr { ',' type_expr } ] ')'
          '->' type_expr [ effect_clause ] ;
```

### 21.9 — Expressions (Precedence-Ordered)

```ebnf
expression = assignment_expr ;
assignment_expr = pipe_expr [ assignment_op pipe_expr ] ;
pipe_expr = compose_expr { '|>' compose_expr } ;
compose_expr = range_expr { '>>' range_expr } ;
range_expr = or_expr [ ( '..' | '..=' ) or_expr ]
           | '..' [ or_expr ] | '..=' or_expr ;
or_expr = and_expr { '||' and_expr } ;
and_expr = comparison_expr { '&&' comparison_expr } ;
comparison_expr = bitwise_or_expr
                  { ( '==' | '!=' | '<' | '>' | '<=' | '>=' | 'is' )
                    bitwise_or_expr } ;
bitwise_or_expr = bitwise_xor_expr { '|' bitwise_xor_expr } ;
bitwise_xor_expr = bitwise_and_expr { '^' bitwise_and_expr } ;
bitwise_and_expr = additive_expr { '&' additive_expr } ;
additive_expr = multiplicative_expr { ( '+' | '-' ) multiplicative_expr } ;
multiplicative_expr = power_expr { ( '*' | '/' | '%' ) power_expr } ;
power_expr = unary_expr [ '**' power_expr ] ;
unary_expr = ( '-' | '!' | '~' ) unary_expr | postfix_expr ;
postfix_expr = primary_expr { postfix_op } ;
postfix_op = '(' [ arg_list ] ')' | '[' expression ']'
           | '.' IDENT | '.' IDENT '(' [ arg_list ] ')' | '?' ;
```

### 21.10 — Primary Expressions

```ebnf
primary_expr = IDENT | TYPE_IDENT | literal | '(' expression ')'
             | '(' expression ',' expression { ',' expression } [ ',' ] ')'
             | if_expr | match_expr | block | lambda_expr
             | collection_literal | record_construction
             | 'await' expression | 'return' [ expression ]
             | 'break' [ expression ] | 'continue'
             | 'unreachable' '(' ')' ;
if_expr = 'if' '(' condition ')' block
          { 'else' 'if' '(' condition ')' block }
          [ 'else' block ] ;
condition = expression | 'let' pattern '=' expression ;
match_expr = 'match' expression '{' match_arm { NEWLINE match_arm } '}' ;
match_arm = pattern [ 'if' '(' expression ')' ] '=>' ( expression | block ) ;
lambda_expr = '(' [ lambda_param { ',' lambda_param } ] ')'
              '=>' ( expression | block ) ;
lambda_param = IDENT [ ':' type_expr ] ;
```

### 21.11 — Patterns

```ebnf
pattern = pattern_alt { '|' pattern_alt } ;
pattern_alt = '_' | IDENT | 'mut' IDENT | literal
            | type_path [ pattern_fields ] | '(' pattern { ',' pattern } ')'
            | '[' [ pattern { ',' pattern } ] ']'
            | pattern '..' pattern | '..' ;
pattern_fields = '{' pattern_field { ',' pattern_field } [ ',' '..' ] [ ',' ] '}'
               | '(' pattern { ',' pattern } ')' ;
pattern_field = IDENT | IDENT ':' pattern ;
```

### 21.12 — Statements

```ebnf
statement = let_statement | for_loop | while_loop | loop_statement
          | guard_statement | handling_block | expression NEWLINE | ';' ;
block = '{' { statement } '}' ;
let_statement = 'let' [ 'mut' ] pattern [ ':' type_expr ] '=' expression NEWLINE ;
for_loop = 'for' pattern 'in' expression block ;
while_loop = 'while' '(' condition ')' block ;
loop_statement = 'loop' block ;
guard_statement = 'guard' '(' condition ')' 'else' block ;
handling_block = 'handling' '(' handler_binding
                 { ',' handler_binding } [ ',' ] ')' block ;
handler_binding = type_path 'with' expression ;
```

### 21.13 — Collection Literals

```ebnf
list_literal = '[' [ expression { ',' expression } [ ',' ] ] ']' ;
map_literal = '{' map_entry { ',' map_entry } [ ',' ] '}' ;
map_entry = expression ':' expression ;
set_literal = '#' '{' [ expression { ',' expression } [ ',' ] ] '}' ;
record_construction = type_path '{'
                      [ field_init { field_init } ] '}' ;
field_init = IDENT ':' expression NEWLINE | IDENT NEWLINE
           | '..' expression NEWLINE ;
```

### 21.14 — Native/FFI

```ebnf
native_fn_decl = { annotation } [ visibility ] 'native' 'fn' IDENT
                 '(' [ param_list ] ')' [ '->' type_expr ]
                 '{' '`' native_code '`' '}' ;
```

### 21.15 — Tests

```ebnf
property_test_decl = 'property' '(' STRING_LITERAL ')' '{'
                     'forall' '(' property_bindings ')' block '}' ;
property_bindings = property_binding { ',' property_binding } [ ',' ] ;
property_binding = IDENT ':' type_expr
                   [ '.' IDENT '(' [ arg_list ] ')' ] ;
```

### 21.16 — Disambiguation Rules

**Map vs Block:** `{` after a `TYPE_IDENT` → record construction. First element matches `expression ':'` → map literal. Otherwise → block.

**Tuple vs Grouping:** `(expr)` → grouping. `(expr, ...)` → tuple. Trailing comma forces tuple: `(x,)`.

**Generics:** Not ambiguous — Bock uses `[]` for generics, so `<` is always comparison.

**Bitwise OR vs Pattern Alternative:** `|` is bitwise OR in expressions, pattern alternative in patterns. Context-determined.

**Pipe vs Bitwise OR:** `|>` (two chars) vs `|` (one char). Lexically distinct.

**Type.ident vs instance.ident:** `TYPE_IDENT.ident(...)` is an associated function call. `TYPE_IDENT.ident` without `(` is not valid in expression position (type names are not values). `value.ident` where `value` is a local/expression is instance field access or method call as usual.

---

## 22. Target Profiles

Each target is described by a capability profile used by the transpiler:

```
TargetProfile {
  id: TargetId
  capabilities: {
    memory_model: GC | ARC | Manual
    null_safety: Bool
    algebraic_types: Native | Emulated | None
    async_model: EventLoop | GreenThread | OSThread | None
    generics: Reified | Erased | Monomorphized
    first_class_functions: Bool
    pattern_matching: Native | SwitchBased | Emulated
    traits: Native | InterfaceBased | Emulated
    string_interpolation: Native | Concatenation
  }
  conventions: {
    naming: CamelCase | SnakeCase
    error_handling: Exceptions | ResultTypes | Mixed
    visibility_default: Public | Private
    indent: Spaces(2) | Spaces(4)
  }
}
```

---

## 23. Appendices

### Appendix A: Project Configuration Reference

```toml
# bock.project

[project]
name = "my-app"
version = "0.1.0"
authors = ["team@example.com"]

[strictness]
default = "development"
[strictness.overrides]
"src/experiments/**" = "sketch"
"src/core/**" = "production"

[paradigm]
default = "multi"

[targets]
primary = "web"
additional = ["ios", "android"]

[ai]
provider = "openai-compatible"    # built-in: "openai-compatible" | "anthropic"
endpoint = "https://api.example.com/v1"
model = "model-name"
api_key_env = "AI_API_KEY"        # env var name containing the key (not the key itself)
confidence_threshold = 0.75       # accept AI output at or above this (0.0–1.0)
deterministic_fallback = true     # fall back to Tier 2 rules on AI failure
auto_pin = false                  # auto-pin AI decisions in development mode
cache = true                      # cache AI responses (content-addressed)
max_retries = 3
timeout_seconds = 30

[effects]
Log = "std.logging.ConsoleLog"
Trace = "std.tracing.NoOpTrace"
Clock = "std.time.SystemClock"
Random = "std.crypto.random.SecureRandom"

[effects.overrides.test]
Log = "std.testing.NullLog"
Clock = "std.testing.MockClock"
Random = "std.testing.DeterministicRandom"

[dependencies]
core-http = "^1.0"

[plugins]
derive-protobuf = { version = "^1.0", allow = ["derive"] }
lint-security = { version = "^2.0", allow = ["lint"] }

[testing]
smart_target_threshold = 0.3
always_test = ["js"]

[build]
min_aura = "1.2.0"
[build.hooks]
pre_build = "scripts/generate-version.bock"
[build.cache]
remote = "s3://build-cache/my-app"

[registries]
internal = "https://bock.company.internal"
```

### Appendix B: Project Structure

```
my-app/
├── bock.project
├── bock.lock
├── src/
│   ├── main.bock
│   ├── app/
│   │   ├── auth.bock
│   │   ├── payments.bock
│   │   └── models.bock
│   └── platform/
│       ├── web/
│       ├── ios/
│       └── android/
├── test/
│   ├── auth_test.bock
│   └── payments_test.bock
├── resources/
│   ├── web/
│   ├── ios/
│   └── android/
├── targets/
│   ├── web.target
│   ├── ios.target
│   └── android.target
└── .bock/
    ├── decisions/
    │   ├── build/      # codegen decisions; committed to VCS
    │   └── runtime/    # adaptive handler selections (§10.8); local
    ├── rules/           # extracted codegen rules (§17.7); per-target subdirs
    └── cache/
```

### Appendix C: Plugin System

Plugins are WASM-sandboxed modules distributed through the package registry. Four categories: **Derive** (generate `impl` blocks), **Lint** (custom diagnostics), **Annotation** (process custom annotations), **Transpilation** (custom code generation passes).

Security model: explicit capability grants in project manifest, WASM sandboxing (no filesystem/network access), compiler validation of all plugin output, content-addressed WASM in lockfile for reproducibility.

### Appendix D: Grammar Summary Table

| Construct            | Syntax                                     |
|----------------------|--------------------------------------------|
| Function             | `fn name(params) -> Return { body }`       |
| Variable (immut)     | `let x = value`                            |
| Variable (mutable)   | `let mut x = value`                        |
| Constant             | `const X: Type = value`                    |
| Record               | `record Name { field: Type }`              |
| Enum (ADT)           | `enum Name { Variant { field: Type } }`    |
| Class                | `class Name : Parent, Trait { ... }`       |
| Trait                | `trait Name { fn method(self) -> T }`      |
| Platform Trait       | `platform trait Name { ... }`              |
| Effect               | `effect Name { fn op() -> T }`             |
| Composite Effect     | `effect Name = A + B + C`                  |
| Generic              | `fn name[T: Bound](x: T) -> T`            |
| Where Clause         | `where (T: Bound, U: Bound)`              |
| Lambda               | `(params) => expr`                         |
| Pipe                 | `x \|> f \|> g`                            |
| Compose              | `f >> g >> h`                              |
| Partial Application  | `f(_, arg2)`                               |
| Match                | `match val { Pattern => expr }`            |
| If (expression)      | `let x = if (cond) { a } else { b }`      |
| If-Let               | `if (let Some(x) = expr) { ... }`         |
| Guard                | `guard (cond) else { return }`             |
| For Loop             | `for item in collection { ... }`           |
| While Loop           | `while (cond) { ... }`                     |
| Infinite Loop        | `loop { ... break }`                       |
| Range (exclusive)    | `0..10`                                    |
| Range (inclusive)     | `0..=10`                                   |
| String Interpolation | `"hello ${name}"`                          |
| Raw String           | `r"no ${interpolation}"`                   |
| Multi-line String    | `"""..."""`                                |
| List Literal         | `[1, 2, 3]`                               |
| Map Literal          | `{"key": value}`                           |
| Set Literal          | `#{"a", "b"}`                              |
| Tuple                | `(a, b, c)`                                |
| Optional Shorthand   | `T?`                                       |
| Error Propagation    | `expr?`                                    |
| Module               | `module path.name`                         |
| Import               | `use path.{A, B}`                          |
| Visibility           | `public`, `internal`, (default: private)   |
| Annotation           | `@name(args)`                              |
| Effect Clause        | `fn f() with Log, Trace`                   |
| Handling Block       | `handling (Log with handler) { ... }`      |
| Module Handler       | `handle Log with AuditLogger`              |
| Native FFI           | `` native fn f() { `target code` } ``     |
| Doc Comment          | `/// text`                                 |
| Module Doc           | `//! text`                                 |
| Async                | `async fn f() { await expr }`              |
| Concurrent           | `@concurrent fn f()` / `@concurrent { }`  |
| Derive               | `@derive(Trait1, Trait2)`                  |
| Type Alias           | `type Name = Type where (pred)`            |
| Spread               | `Record { field: v, ..other }`             |
| Type Check           | `value is Type`                            |
| Numeric Suffix       | `42_u8`, `3.14_f64`                        |

---

*End of specification.*
