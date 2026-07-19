# Bock Context Pack

**Pack version:** 0.1.1
**Derived from:** `spec/bock-spec.md` — 23 sections, spec version 0.1.0-draft (March 2026)
**Repo commit:** `397161f` (the `main` commit this pack was authored and verified against)
**Verified:** every ```bock code block in this file passes `bock check`
(enforced by `tools/scripts/verify-context-pack.sh`); the worked examples in
§6 were additionally executed (interpreter and/or transpiled targets — each
example states where).

This file is a self-contained primer that makes a frontier model a competent
Bock author at session start. Drop it into the model's context before asking
it to read, write, or review Bock code. It is curated, not complete: the
authoritative reference is `spec/bock-spec.md`; the error codes come from the
compiler's diagnostic registry and emission sites. When this pack and the spec
disagree, the spec wins — and please report the discrepancy.

---

## 1. What Bock is — the mental model

Bock is a **feature-declarative, target-agnostic** language for AI–human
collaborative development. One Bock codebase transpiles to **five targets in
v1**: JavaScript (`js`), TypeScript (`ts`), Python (`python`), Rust (`rust`),
and Go (`go`). Java/C++/C#/Swift are planned for v1.x.

Core ideas you must internalize before writing a line:

1. **Declare what, not how.** Bock code states functionality, constraints,
   capabilities, and intent. The compiler (deterministic rules by default,
   an optional AI provider at capability gaps) produces idiomatic code per
   target. Codegen is auditable: AI decisions land in a decision manifest
   (`.bock/decisions/`), inspectable with `bock inspect`, pinnable with
   `bock pin` / `bock override`.
2. **Effects are part of function types.** A function that logs declares
   `with Log`; a function that reads the clock declares `with Clock`. Effects
   propagate: callers must declare or handle their callees' effects. Handlers
   are installed with `handling (Effect with handler) { ... }` blocks
   (dynamic scoping, innermost wins) or module-level `handle Effect with h`.
3. **Capabilities are compiler-verified.** `@requires(Capability.Network)`
   declares what a function touches; the compiler checks the call graph.
4. **Graduated strictness** per package: `sketch` → `development` (default)
   → `production`. Strictness controls whether completeness gaps (missing
   `@context` on public items, undeclared effects) are warnings or errors.
5. **Semantics are cross-target.** The same program must behave identically
   on every target; the interpreter (`bock run`) is the Tier-1 semantic
   reference. "Semantic pass + target fail = transpiler bug, not user code
   bug."
6. **Annotations are semantic metadata**, not decoration: `@context` (intent,
   flows to the AI transpiler), `@requires` (capabilities), `@test`,
   `@performance`, `@security`, `@invariant`, `@domain`, `@concurrent`,
   `@managed`, `@deterministic`, `@inline`, `@hot`, `@cold`, `@deprecated`.
   **Unknown annotations are a compile error** — there is no silent tier.

Bindings are immutable by default. Types are structural and inferred but must
fully resolve at compile time. There is no `null` — use `Optional[T]`. Errors
are values — `Result[T, E]` and the `?` operator, no exceptions.

---

## 2. The toolchain — commands you will actually run

```text
bock check [files...]      # type check + lint + context validation
                           #   no args = check all .bock files under cwd
                           #   --brief    compact one-line diagnostics
                           #   --strict   production strictness (warnings -> errors)
                           #   --only=types,context   restrict aspects
                           # exit 0 unless there is at least one ERROR
                           # (warnings never fail the check)

bock run [file.bock]       # execute on the interpreter (fast, no toolchain)
bock test [files...]       # run @test functions on the interpreter
                           #   no args = discover .bock files recursively
                           #   --filter <pattern> selects tests by name

bock build -t js --source-only   # transpile only; output under build/<target>/
bock build -t python             # project mode (default): scaffolded, runnable
                                 # target project incl. transpiled tests
bock build --all-targets         # all five targets
bock fmt [--check]               # one canonical style, zero config
bock new <name>                  # scaffold: bock.project, src/main.bock, tests/
```

Project facts: the project root marker is `bock.project` (TOML). `bock build`
operates on the project in the cwd (it takes no file arguments). Entry point
is `src/main.bock` by default. Build cache lives in `.bock/` (gitignored).
Output goes to `build/<target>/`, mirroring the source tree.

A first program:

```bock
module main

fn main() {
  println("Hello from Bock!")
}
```

Check it with `bock check hello.bock`, run it with `bock run hello.bock`.

---

## 3. Language primer

### 3.1 Modules and imports

Every file intended for cross-file `use` starts with `module <dotted.path>`.
Imports use the **braced form only** — there is no single-name shorthand, no
`as` aliasing (v1.x), and a bare module import is rejected (`E4014`):

```bock
module app.geometry

use core.collections.{max_of}

@context("Largest absolute coordinate in a path.")
public fn max_extent(path: List[Int]) -> Optional[Int] {
  max_of(path.map((p) => p.abs()))
}
```

- `use core.collections.{List, Map}` — correct.
- `use core.error` — **error E4014** (bare module import is not a v1 form).
- `use app.services.*` — legal but discouraged.

Visibility: `public` (everywhere), `internal` (module tree), default =
private to the file. Exported items **must** be marked `public`.

### 3.2 Bindings, primitives, conversions

```bock
module main

fn demo() -> String {
  let name = "Bock"              // immutable binding (default)
  let mut total = 0              // mutable binding — required for assignment
  total = total + 1

  let price: Float = 9.5         // explicit type annotation
  let count = 3
  let average = price / count.to_float()  // no implicit Int/Float coercion
  let label = count.to_string()  // any primitive -> String

  let (x, y) = (10, 20)          // tuple destructuring

  "${name} ${total} ${average} ${label} ${x + y}"
}
```

Primitives: `Int`, `Float`, `Bool`, `String`, `Char`, `Void`, `Never`, plus
sized variants (`Int8`…`Int128`, `UInt8`…`UInt64`, `Float32/64`). Compound:
`Optional[T]` (shorthand `T?`), `Result[T, E]`, `List[T]`, `Map[K, V]`,
`Set[T]`, tuples `(A, B)`. Function types: `Fn(Int) -> Int`, optionally with
effects: `Fn(String) -> Void with Log`.

Numeric rules (normative, §3.6):

- **No implicit coercion.** `Int + Float` is a type error; convert with
  `.to_float()` / `.to_int()` (truncates toward zero).
- **Integer `/` truncates toward zero**: `17 / 5 == 3`, `-17 / 5 == -3`.
  `%` takes the sign of the dividend: `-17 % 5 == -2`. Division or modulo by
  zero aborts at runtime on every target.
- Booleans are lowercase `true` / `false` and stringify exactly so.

Strings: `"${expr}"` interpolation, `r"raw"`, `"""multi-line"""`.
`String.len()` counts **Unicode scalar values**, not bytes (`byte_len()` for
bytes). String concatenation with `+` works.

Statement terminators: **newlines**, not semicolons. A statement continues if
the line ends with an operator/comma/open delimiter, or the next line starts
with `.`, `|>`, a closing delimiter, or `else`. `;` only separates multiple
statements on one line — never use it inside record/enum bodies.

### 3.3 Records, enums, pattern matching

Fields, variants, and match arms are **newline-separated — no commas or
semicolons between them** (trailing commas inside a single field line are
tolerated; separating semicolons are parse errors).

```bock
module main

enum Status {
  Active
  Suspended { reason: String }
}

fn show(s: Status) -> String {
  match s {
    Active => "active"
    Suspended { reason } => "suspended: ${reason}"
  }
}

fn main() {
  println(show(Status.Suspended { reason: "testing" }))
}
```

Construction vs patterns — read carefully, models get this wrong:

- **Construction with a payload**: qualified `Status.Suspended { reason: "x" }`
  or bare `Suspended { reason: "x" }` both work.
- **Construction without a payload**: use the **bare** variant name
  (`Active`). The qualified form `Status.Active` in expression position
  passes `bock check` today but fails at runtime — avoid it (see §8).
- **Patterns must use bare variant names**: `Active =>`,
  `Suspended { reason } =>`. Qualified patterns (`Status.Active =>`) are
  rejected by the v1 parser.

Patterns support wildcards `_`, literals, bindings, or-patterns `1 | 2`,
guards `n if (n > 100) =>`, nested destructuring (`Some(Ok(v))`), and rest
`..`. Exhaustiveness is a warning in `development`, an error in `production`.

Records get default field values (`is_valid: Bool = true`) and spread
construction (`User { name: n, ..defaults }`).

### 3.4 Control flow — everything is an expression, parens are required

```bock
module main

fn classify(n: Int) -> String {
  // if is an expression; parentheses around the condition are REQUIRED
  let sign = if (n < 0) { "negative" } else { "non-negative" }

  // match is an expression; arms are newline-separated, no commas
  let size = match n {
    0 => "zero"
    1 | 2 => "small"
    v if (v > 100) => "large"
    _ => "medium"
  }

  "${sign} ${size}"
}

fn first_even(xs: List[Int]) -> Optional[Int] {
  for x in xs {
    if (x % 2 == 0) { return Some(x) }
  }
  None
}

fn countdown(start: Int) -> Int {
  let mut n = start
  while (n > 0) {
    n = n - 1
  }
  n
}

fn validated(input: String) -> Result[String, String] {
  // guard: else block must diverge (return / break / continue)
  guard (input.len() > 0) else {
    return Err("empty input")
  }
  Ok(input)
}
```

Also available: `loop { ... break }`, `if (let Some(x) = expr) { ... }`,
ranges `1..10` (exclusive) / `1..=10` (inclusive), and `for (i, x) in
xs.enumerate()`. The last expression of a block is its value; `return` is for
early exit.

### 3.5 Lambdas, collections, pipes

```bock
module main

fn demo() -> Int {
  let xs = [1, 2, 3, 4, 5]

  // Lambdas: parentheses around parameters are ALWAYS required
  let doubled = xs.map((x) => x * 2)
  let evens = xs.filter((x) => x % 2 == 0)
  let total = xs.fold(0, (acc: Int, x: Int) => acc + x)

  // Map and Set literals
  let ages = {"ada": 36, "alan": 41}
  let tags = #{"core", "v1"}

  // Map membership is contains_key (NOT contains; that is Set-only)
  let known = ages.contains_key("ada") && tags.contains("v1")

  if (known) { doubled.len() + evens.len() + total } else { 0 }
}
```

`x => x * 2` is a parse error (`E2030`) — always `(x) => x * 2`. Multi-
statement lambdas use braces; the last expression is the return value.

The pipe `|>` prepends the piped value as the **first argument** of the
right-hand call: `data |> parse |> validate`. `_` is the placeholder for
non-first positions: `headers |> add(request, _, "Content-Type")`. Function
composition is `>>`; partial application is `f(_, arg2)`.

Built-in collection methods you can rely on (closed set — anything else is
`E4013 no such method`): `len`, `is_empty`, `get` (returns `Optional`),
`first`, `last`, `find`, `index_of`, `contains` (List/Set), `contains_key` /
`contains_value` (Map), `push`/`append` (mut, in place), `pop`, `insert`,
`remove`, `concat`, `reverse`, `sort`, `dedup`, `flatten`, `take`, `skip`,
`slice`, `filter`, `map`, `flat_map`, `fold`, `reduce`, `for_each`, `any`,
`all`, `enumerate`, `zip`, `join`, `keys`, `values`, `entries`, `set`,
`merge`, `add`, `union`, `intersection`, `difference`; strings add
`starts_with`, `ends_with`, `to_upper`, `to_lower`, `trim`, `substring`,
`replace`, `split`, `chars`, `repeat`, `char_at`; scalars add `abs`, `min`,
`max`, `clamp`, `floor`, `ceil`, `round`, `sqrt`, `to_float`, `to_int`,
`to_string`; Optional/Result add `is_some`, `is_none`, `unwrap`, `unwrap_or`,
`is_ok`, `is_err`, `map_err`.

### 3.6 Mutation and ownership

```bock
module main

fn demo() -> List[Int] {
  // In-place mutation: needs `let mut`; push returns Void
  let mut log = [1]
  log.push(2)

  // Functional building: works on plain `let`, returns a NEW list
  let base = [1]
  let grown = base + [2]
  let joined = base.concat(grown)

  log + joined
}
```

`push`/`append` **require a `mut` receiver and return `Void`** (`E5004`
otherwise). Functional style (`+`, `concat`) needs no `mut` and is generally
preferred. Re-binding with `let` (shadowing) is idiomatic:
`let list = list.add(item)`.

Ownership (lightweight, no lifetimes): values are owned; **assignment
moves**; reads borrow implicitly; mutable borrows are explicit
(`transform(mut data)`). Using a moved value is `E5001`; moving a captured
value inside a loop body is `E5003`. Diverging branches (`return`, `break`)
are excluded from ownership merges, so guard-and-return patterns don't
produce false move errors. `@managed` opts a function out of ownership
tracking (GC semantics everywhere).

### 3.7 Traits and operator gating

Operators on **user types** are trait-gated (§18.5): implementing the trait
enables the operator.

| Trait | Enables |
|---|---|
| `Equatable` (`fn eq(self, other: Self) -> Bool`) | `==`, `!=` |
| `Comparable` (`fn compare(self, other: Self) -> Ordering`) | `<`, `>`, `<=`, `>=` |
| `Iterable` | `for x in value` |
| `Displayable` (`fn to_string(self) -> String`) | `${value}` interpolation |
| `Add`/`Sub`/`Mul`/`Div`/`Mod` | `+` `-` `*` `/` `%` |

Using `<` on a type without `impl Comparable` is a type error (`E4005`).
Primitive conformances are **sealed**: `impl Comparable for Int` in user code
is `E4011` — wrap the primitive in a newtype record instead.
`Comparable.compare` returns `Ordering` (`Less` / `Equal` / `Greater`, all in
the prelude). See the worked example in §6.4.

One method namespace per type (§6.7): a method name may be defined **once**
across all of a type's `impl` blocks and class body. A matching inherent
method satisfies a trait requirement (the trait impl block may be empty);
defining the same name twice is the `E4012` coherence error.

Traits support default method bodies. `trait`, `impl Trait for Type`,
`impl Type { ... }` (inherent/associated fns, called `Type.method()`),
single-inheritance `class Button : Component, Renderable { ... }` all exist
in v1. Associated types (`type Item` in a trait) parse but have **no v1
semantics**.

### 3.8 Generics

Square brackets — `<` is *always* comparison in Bock:

```bock
module main

/// Generics use SQUARE brackets; `<` is always comparison.
fn first[T](list: List[T]) -> Optional[T] {
  list.get(0)
}

/// Generic over the item type; trait bounds go in the brackets or a
/// `where (...)` clause.
fn max_by_score[T](items: List[T], score: Fn(T) -> Int) -> Optional[T] {
  items.fold(None, (best: Optional[T], item: T) => {
    match best {
      None => Some(item)
      Some(b) => {
        if (score(b) < score(item)) { Some(item) } else { Some(b) }
      }
    }
  })
}

record Pair[A, B] {
  first: A
  second: B
}

fn demo() -> Optional[String] {
  let names = ["ada", "grace", "alan"]
  max_by_score(names, (n) => n.len())
}
```

Bounds: `fn serialize[T: Serializable](v: T)`, multiple bounds with `+`, or
a `where (A: Into[C], B: Into[C])` clause (parens required).

### 3.9 Effects

```bock
module app.metrics

/// A user-defined effect: the operation signature, no implementation.
public effect Metrics {
  fn incr(name: String) -> Void
}

@context("A handler is a record implementing the effect's trait.")
public record NullMetrics {
}

impl Metrics for NullMetrics {
  fn incr(name: String) -> Void {
  }
}

@context("Tracks one event, then echoes its name back.")
public fn track(name: String) -> String with Metrics {
  incr(name)        // bare op call: legal because of `with Metrics`
  name
}

fn demo() -> String {
  // `handling` is a STATEMENT, not an expression — capture results via a
  // mutable binding declared outside the block.
  let mut result = ""
  handling (Metrics with NullMetrics {}) {
    result = track("page_view")
  }
  result
}
```

Rules:

- A function that invokes an effect op declares it: `with Log, Clock` after
  the return type. Effects propagate to callers.
- Handlers resolve Layer 1 (`handling` block) > Layer 2 (module-level
  `handle Log with h`). Project-level defaults are v1.x.
- The v1 handler form is a record + `impl Effect for Record` (§10.4).
  Lambda handlers (`Log.handler(...)`) are v1.x.
- The stdlib ships one canonical effect: `core.effect.Log` with op
  `log(message: String) -> Void` (note: **one argument**, no level), handler
  `ConsoleLog` (prints `[log] <message>`), constructor `console_log()`.
- `core.time` owns the `Clock` effect (`Instant.now()`, `sleep(duration)`).
- Calling an op like `log("x")` in a function that neither declares
  `with Log` nor sits inside a `handling` block is an effect-system
  violation: you get `E6005` — "`log` is an operation of effect `Log`,
  but the effect is neither declared by the enclosing function nor handled
  here" — with a note stating the fix (add `with Log`, or wrap the call in
  `handling (Log with <handler>) { ... }`). The lambda-handler form
  `Log.handler(...)` is reserved until v1.x and reports `E6006`.

### 3.10 Capabilities and annotations

```bock
module app.net

@requires(Capability.Network)
@context("Capability demo: anything touching the network declares it.")
public fn ping(host: String) -> Bool {
  host.len() > 0
}
```

Capability taxonomy: `Network`, `Storage`, `Crypto`, `GPU`, `Camera`,
`Microphone`, `Location`, `Notifications`, `Bluetooth`, `Biometrics`,
`Clipboard`, `SystemProcess`, `FFI`, `Environment`, `Clock`, `Random`.
Capabilities propagate through the call graph; a caller that hasn't declared
a callee's capability gets `E8021`/`E7003`.

`@context("...")` carries free-form intent (multi-line `"""..."""` allowed) —
it flows to the AI transpiler and is the completeness unit: at development
strictness every **public** declaration without `@context` warns (`W8013`);
under `--strict` / production it errors. Annotate every public item.
Annotations attach to individual declarations only — module-level annotations
are v1.x. `@performance` budgets need unit-suffixed literals (`100.ms`,
`50.mb` — bare integers are `E8003`).

### 3.11 Tests

`@test` functions live next to the code. The fluent `expect(...)` API is
available without import; the `assert_*` family needs
`use core.test.{...}`.

See the worked example in §6.5 — run with `bock test file.bock`.

### 3.12 Style (enforced by `bock fmt`)

- 2-space indent; opening brace on the same line; 80-char soft limit.
- Records/enums/match arms newline-separated.
- `if (cond)`, `(x) => expr` — parens required.
- Imports sorted core → std → external → local.
- Doc comments `///` on declarations, `//!` for module headers.
- No semicolons at line ends (newline terminates).

---

## 4. The v1 boundary — what does NOT exist

The single most common model failure is inventing surface that isn't there.
**v1 ships `core.*` only — 11 modules.** The `std.*` packages (`std.fs`,
`std.json`, `std.net.http`, `std.crypto`, `std.logging`, …) are specced
(§18.4) but **not shipped in v1**. Consequences:

- **No file I/O.** There is no way to read or write a file in v1 Bock.
- **No HTTP, no sockets, no JSON parsing, no regex, no environment
  variables, no random numbers, no subprocess.**
- I/O surface that does exist: `print`/`println` (stdout), and that's it.

The 11 v1 core modules: `core.option`, `core.result`, `core.collections`
(`List`/`Map`/`Set`/`SortedSet`), `core.string` (+ `StringBuilder`),
`core.iter`, `core.compare`, `core.convert`, `core.error` (v1 surface:
`message(self) -> String` only), `core.effect`, `core.time`
(`Duration`, `Instant`, `Clock`), `core.test`.

Prelude (no import needed): the primitive types, `Duration`, `Instant`,
`Optional`/`Some`/`None`, `Result`/`Ok`/`Err`, `Ordering`/`Less`/`Equal`/
`Greater`, `List`/`Map`/`Set`, `Fn`, the core traits (`Comparable`,
`Equatable`, `Hashable`, `Displayable`, `Serializable`, `Cloneable`,
`Default`, `Into`, `From`, `TryFrom`, `Iterator`, `Iterable`, `Error`), and
`print`, `println`, `debug`, `assert`, `todo`, `unreachable`, `sleep`.
`todo()` and `unreachable()` are `Never`-typed (they satisfy any return type)
and abort at runtime.

**Reserved for v1.x — using any of these is a compile error today:**

- `core.concurrency`: channels, `Mutex`, `RwLock`, `Atomic`, `WaitGroup`.
- `native` blocks / FFI, `@target(...)`, `@platform(...)`.
- `@derive(...)` — write `impl Trait for Type` by hand.
- `@property` / property-based testing (`property`/`forall`).
- Refinement predicates on type aliases (`type Port = Int where (...)`).
  Plain aliases (`type Email = String`) are fine.
- Associated-type semantics; lazy iterators. The v1 `core.iter` floor is
  eager and exactly six combinators on iterators: `to_list`, `count`,
  `fold`, `map`, `filter`, `take` (List methods in §3.5 are richer).
- Module-level annotations (`@context module foo`), aliased imports
  (`use x as y`), module-qualified access (bare `use core.error`).
- `bock test --target` (cross-target test runs), `bock build --optimize`,
  `--deliverable`, `--no-tests`, `bock fix`, `[paradigm]`/`[effects]`
  config tables.
- Tuple positional indexing `t.0` (`E2092`) — destructure with
  `let (a, b) = t` instead.
- `async fn` / `await` / `@concurrent` parse, but `core.concurrency` is
  v1.x — stay synchronous in v1 unless you know the target story.

Sized-integer detail: `Int` is `i64`-backed; `BigInt`/`Decimal` live in
`core.types` (v1.x).

---

## 5. Error codes — what the compiler will tell you

Sourced from `compiler/crates/bock-errors/src/catalog.rs` plus the emission
sites in `bock-lexer`, `bock-parser`, `bock-air` (resolver + context), and
`bock-types` (checker/ownership/effects/capabilities). `E` = error,
`W` = warning. Warnings never fail `bock check`; `--strict` promotes
strictness-gated ones.

**1xxx — lexing and name resolution.** The lexer owns `E1001`–`E1006`; the
name-resolution pass owns the rest. Each code means exactly one thing.

| Code | Meaning | Typical fix |
|---|---|---|
| E1001 | Lexer: unexpected character | Remove or fix the stray character |
| E1002/E1003/E1004 | Unterminated string / bad escape / bad char literal | Fix the literal |
| E1005 | Lexer: invalid digit for numeric base | e.g. `8` in an octal literal, or a non-hex digit after `0x` |
| E1006 | Lexer: unterminated `/* */` block comment | Close the block comment |
| E1007 | Resolver: symbol exists but is private | Add `public` at the definition |
| E1008 | Resolver: circular module dependency | Break the `use` cycle (see §10) |
| E1009 | Resolver: **undefined name** | Most often: a name not in scope. Check imports. (A bare effect op like `log` outside `with`/`handling` is **E6005**, not this — see the 6xxx table) |
| E1010 | Resolver: **module not found** | Check the `use` path and that the file declares `module <path>` |
| E1011 | Resolver: **symbol not found in module** | The module exists but doesn't export that name |
| W1001 | Resolver: unused import | Remove it (see §8 for a false-positive case) |

**2xxx — parser.**

| Code | Meaning | Typical fix |
|---|---|---|
| E2000 | Parse error at top level / expected specific token | The classic trigger: missing parens — `if x > 0 {` → `if (x > 0) {` |
| E2001/E2002 | Unexpected token / missing expected token | Read the caret; often a stray `;` or comma in a newline-separated body |
| E2010 | Invalid declaration | Malformed top-level item |
| E2020/E2021/E2022 | Invalid expression / pattern / type expression | |
| E2030/E2031 | Parens required (lambda params / `if` condition) / invalid parameter | Lambda params must be parenthesized: `(x) => ...`; `if (cond) { ... }` |
| E2073 | Expected function name after `fn` | Give the function a name: `fn name(...) { ... }` |
| E2040 | Invalid generic parameter list | Generics use `[T]`, not `<T>` |
| E2050 | Invalid `use` declaration | Use the braced form |
| E2060/E2061 | Invalid annotation / const declaration | |
| E2070/E2071/E2072 | Invalid match arm / associated type name / method name | Arms are `pattern => expression`, newline-separated |
| E2090/E2091 | Invalid effect declaration / operation name | |
| E2092 | Tuple positional indexing `t.0` is not v1 | `let (a, b) = t` |

**4xxx — type checker.**

| Code | Meaning | Typical fix |
|---|---|---|
| E4001 | Type mismatch — message reads `expected \`T\`, found \`U\`` (plain type names, no `Primitive(Int)` Debug leak) | Includes method bodies whose last expression disagrees with the declared return type. Direction-aware conversion hints fire when applicable: `Int`→`String` suggests `.to_string()`, `Float`→`Int` suggests `.to_int()` (truncates toward zero) |
| E4002 | Undefined variable | |
| E4003 | Arity mismatch in call | |
| E4004 | Value is not callable | Type names are not values; `Type.method()` only |
| E4005 | Trait bound / `where` predicate failed | Includes operator gating: `<` on a type without `impl Comparable` |
| E4010 | Overlapping trait impls (coherence) | |
| E4011 | Core trait impl on a primitive (sealed) | Wrap in a newtype record, implement on the wrapper |
| E4012 | Two uses: (a) same method name defined twice for one type (single method namespace, §6.7); (b) `.into()`/`from`/`try_from` conversion unresolved | (a) delete the duplicate; (b) add the `From`/`Into` impl, or give `.into()` a typed destination |
| E4013 | No such method on a concrete type (with "did you mean...?") | Don't invent APIs — the built-in method set is closed. `map.contains(...)` → `contains_key` |
| E4014 | Bare module import (`use core.error`) | `use core.error.{Error}` |
| E4015 | `==`/`!=` operand (or an `Equatable`-bound instantiation) is not `Equatable` (DQ29) — records/enums conform **structurally** iff every field/payload type conforms (recursively); containers iff their element types do; classes need an explicit `impl` | Implement `Equatable` for the type, or remove the comparison. The message names the offending field path and type (e.g. an `Fn` field poisons the type) |

**5xxx — ownership.**

| Code | Meaning | Typical fix |
|---|---|---|
| E5001 | Use after move | Restructure, or take a borrow (reads borrow implicitly) |
| E5002 | `mut` borrow of a non-`mut` binding | Declare `let mut` |
| E5003 | Value moved inside a loop body | Move it outside the loop or clone per-iteration |
| E5004 | In-place `List` mutator on a non-`mut` receiver — `push`/`append` (DQ18) plus `pop`/`remove_at`/`insert`/`reverse` and indexed `set` (DQ30) | `let mut` (or a `mut` parameter), or build functionally with `+`/`concat` |

**6xxx — effects / 7xxx — capabilities.** Symmetric pairs; the W-variants
are development-mode warnings that promote to errors in production.

| Code | Meaning | Typical fix |
|---|---|---|
| E6001 / W6002 | Function uses an effect not in its `with` clause | Add `with <Effect>` |
| E6003 / W6004 | Callee's effect escapes the caller's declaration | Declare it on the caller or wrap the call in `handling` |
| E6005 | Effect op called with its effect neither declared nor handled — "`log` is an operation of effect `Log`, but the effect is neither declared … nor handled here" (resolver pass; **not** the generic undefined-name `E1009`) | Add `with <Effect>` to the enclosing function, or wrap the call in `handling (<Effect> with <handler>) { ... }` |
| E6006 | Lambda-handler form `Effect.handler(...)` is reserved until v1.x (checker pass) | Use the v1 handler form: a record + `impl <Effect> for <Record>`, installed via `handle`/`handling` |
| E7001 / W7002 | Function requires an ungranted capability | Add `@requires(Capability.X)` |
| E7003 / W7004 | Callee capability not declared by caller | Same — capabilities propagate |

**8xxx — context system (annotation validation, handler/capability
verification).**

| Code | Meaning | Typical fix |
|---|---|---|
| E8001/E8002 | Unknown capability name / malformed `@requires` argument | Use the §3.10 taxonomy, `Capability.Network` or bare `Network` |
| E8003 | `@performance` literal lacks a unit suffix | `100.ms`, `50.mb` |
| E8004/E8010 | `@invariant` needs an expression / a Bool-typed expression | |
| E8005/E8006 | Malformed `@security` / `@domain` arguments | `@security(level: "confidential", pii: true)`, `@domain("area")` |
| E8011/W8015/E8016 | Security level less restrictive than parent / unknown level / non-positive budget | |
| W8013 / E8013 | Public item missing context annotations (warn at development, error at production/`--strict`) | Add `@context("...")` to every public declaration |
| E8020 / W8020 | Effect op has no handler in scope / effect declared in `with` but never used | Install a `handling` block or declare `with`; drop unused effects |
| E8021 | Callee requires a capability not declared in the current scope | Add `@requires(Capability.X)` to the enclosing function |
| W8022 | PII-tainted type passed to a logging/output function | Don't log PII; restructure |
| E8023 | Public fn/class/trait/type missing `@context` (production) | Annotate |
| W8023 | PII-tainted signature without a security context | Add `@security(level: "confidential")` or `@security(pii: true)` to the module |

---

## 6. Worked examples

All five pass `bock check`. Execution verification per example is stated in
its header comment here in the pack (interpreter = `bock run`; js = `bock
build -t js --source-only` + `node`).

### 6.1 Records, impl blocks, Optional, match

Verified: `bock check` ✓ · interpreter ✓ · js ✓ · python ✓

```bock
//! Library catalog — records, impl blocks, Optional, match, and list ops.

module main

@context("A book in the catalog.")
public record Book {
  id: Int
  title: String
  available: Bool
}

impl Book {
  @context("One-line display label: + available, - checked out.")
  public fn label(self) -> String {
    let mark = if (self.available) { "+" } else { "-" }
    "${mark} #${self.id} ${self.title}"
  }
}

/// Finds a book by id, if present.
fn find_book(books: List[Book], id: Int) -> Optional[Book] {
  books.find((b) => b.id == id)
}

/// Titles of all available books, functional style (no mutation).
fn available_titles(books: List[Book]) -> List[String] {
  books
    .filter((b) => b.available)
    .map((b) => b.title)
}

fn main() {
  let books = [
    Book { id: 1, title: "AIR and Everything", available: true },
    Book { id: 2, title: "Effects in Practice", available: false },
    Book { id: 3, title: "Five Targets, One Source", available: true },
  ]

  for book in books {
    println(book.label())
  }

  match find_book(books, 2) {
    Some(b) => println("found: ${b.title}")
    None => println("no such book")
  }

  let titles = available_titles(books)
  println("available: ${titles.len()}")
}
```

```text
$ bock check catalog.bock     # 1 file checked, no errors
$ bock run catalog.bock
+ #1 AIR and Everything
- #2 Effects in Practice
+ #3 Five Targets, One Source
found: Effects in Practice
available: 2
```

### 6.2 Enums with payloads, Result, `?`, guard

Verified: `bock check` ✓ · interpreter ✓ (all three paths) · js ✓ (full
output below). The interpreter now propagates `Err` through `?` correctly
(fixed since 0.1.0, [#342]); the code is correct per spec §7.10.

```bock
//! Port configuration — enums with payloads, Result, `?`, guard, match.

module main

@context("Why a port number was rejected.")
public enum PortError {
  Reserved { port: Int }
  OutOfRange { port: Int }
}

/// Validates a TCP port: 1..=65535, with 1..=1023 reserved.
fn checked_port(port: Int) -> Result[Int, PortError] {
  guard (port >= 1 && port <= 65535) else {
    return Err(PortError.OutOfRange { port: port })
  }
  guard (port > 1023) else {
    return Err(PortError.Reserved { port: port })
  }
  Ok(port)
}

/// Builds a bind address. `?` propagates any Err from checked_port.
fn bind_address(host: String, port: Int) -> Result[String, PortError] {
  let p = checked_port(port)?
  Ok("${host}:${p}")
}

fn describe(result: Result[String, PortError]) -> String {
  match result {
    Ok(addr) => "listening on ${addr}"
    Err(Reserved { port }) => "port ${port} is reserved"
    Err(OutOfRange { port }) => "port ${port} is out of range"
  }
}

fn main() {
  println(describe(bind_address("0.0.0.0", 8080)))
  println(describe(bind_address("0.0.0.0", 443)))
  println(describe(bind_address("0.0.0.0", 70000)))
}
```

```text
$ bock check ports.bock       # 1 file checked, no errors
$ bock build -t js --source-only && node build/js/main.js
listening on 0.0.0.0:8080
port 443 is reserved
port 70000 is out of range
```

### 6.3 Effects: `with`, `handling`, swapping handlers

Verified: `bock check` ✓ · interpreter ✓ · js ✓

```bock
//! Effects — declaring, requiring (`with`), and handling (`handling`).

module main

use core.effect.{Log, console_log}

@context("Processes items, logging each one through the ambient Log handler.")
public fn process(items: List[String]) -> Int with Log {
  let mut count = 0
  for item in items {
    log("processing ${item}")
    count = count + 1
  }
  count
}

/// An alternative handler: drops every message.
record QuietLog {
}

impl Log for QuietLog {
  fn log(message: String) -> Void {
  }
}

fn main() {
  let items = ["alpha", "beta"]

  // Layer 1: install a handler for the duration of a block.
  handling (Log with console_log()) {
    let n = process(items)
    println("processed ${n}")
  }

  // Same code, different handler: no log output. (Distinct binding name:
  // re-declaring `n` in a sibling handling block currently mis-lowers on js —
  // see Known divergences.)
  handling (Log with QuietLog {}) {
    let m = process(items)
    println("quietly processed ${m}")
  }
}
```

```text
$ bock run effects.bock
[log] processing alpha
[log] processing beta
processed 2
quietly processed 2
```

### 6.4 Traits: Comparable / Equatable / Displayable on a user type

Verified: `bock check` ✓ · interpreter ✓ · js ✓

```bock
//! Traits — implementing Comparable/Equatable/Displayable on a user type.
//!
//! Operators are trait-gated (§18.5): `<` needs Comparable, `==` needs
//! Equatable, and implementing the trait enables the operator for the type.

module main

@context("Money stored as integer cents to avoid Float rounding.")
public record Money {
  cents: Int
}

impl Comparable for Money {
  fn compare(self, other: Money) -> Ordering {
    if (self.cents < other.cents) {
      Less
    } else if (self.cents == other.cents) {
      Equal
    } else {
      Greater
    }
  }
}

impl Equatable for Money {
  fn eq(self, other: Money) -> Bool {
    self.cents == other.cents
  }
}

impl Displayable for Money {
  fn to_string(self) -> String {
    "${self.cents / 100}.${self.cents % 100} USD"
  }
}

fn main() {
  let coffee = Money { cents: 450 }
  let lunch = Money { cents: 1295 }

  // `<` dispatches through the Comparable impl.
  let max = if (coffee < lunch) { lunch } else { coffee }
  println("larger: ${max.to_string()}")

  // Explicit compare returns an Ordering (Less | Equal | Greater).
  let verdict = match coffee.compare(lunch) {
    Less => "coffee is cheaper"
    Equal => "same price"
    Greater => "coffee costs more"
  }
  println(verdict)

  // Equality through the Equatable impl. (The `==` operator form is
  // spec-equivalent (§18.5) but currently mis-lowers on the js target —
  // see Known divergences — so call `.eq()` explicitly for now.)
  if (coffee.eq(Money { cents: 450 })) {
    println("equality works")
  }
}
```

```text
$ bock run money.bock
larger: 12.95 USD
coffee is cheaper
equality works
```

### 6.5 Tests: `@test`, `expect`, `core.test`

Verified: `bock check` ✓ · `bock test` 4/4 passed

```bock
//! Tests — @test functions live next to the code; run with `bock test`.

module wordstats

use core.test.{assert_none, fail}

@context("Counts non-empty whitespace-separated words in a line.")
public fn count_words(line: String) -> Int {
  line.split(" ").filter((w) => w.len() > 0).len()
}

@context("Longest word in a line, or None for a blank line.")
public fn longest_word(line: String) -> Optional[String] {
  line.split(" ")
    .filter((w) => w.len() > 0)
    .fold(None, (best: Optional[String], w: String) => {
      match best {
        None => Some(w)
        Some(current) => {
          if (w.len() > current.len()) { Some(w) } else { Some(current) }
        }
      }
    })
}

@test
fn counts_simple_words() {
  expect(count_words("the quick brown fox")).to_equal(4)
}

@test
fn ignores_extra_spaces() {
  expect(count_words("  hello   world  ")).to_equal(2)
}

@test
fn finds_longest_word() {
  match longest_word("a bb ccc") {
    Some(w) => expect(w).to_equal("ccc")
    None => fail("expected a word")
  }
}

@test
fn blank_line_has_no_longest() {
  assert_none(longest_word("   "))
}
```

```text
$ bock test wordstats.bock
  PASS wordstats::counts_simple_words
  PASS wordstats::ignores_extra_spaces
  PASS wordstats::finds_longest_word
  PASS wordstats::blank_line_has_no_longest
Tests: 4 passed, 0 failed, 4 total
```

---

## 7. Pitfalls — if you write Bock like another language, you will hit these

1. **Missing parens**: `if x > 0 {` is `E2000`. Conditions (`if`, `while`,
   `guard`, match-arm guards) and lambda parameters are always
   parenthesized: `if (x > 0)`, `(x) => x * 2`.
2. **Semicolons**: record fields, enum variants, and match arms are
   newline-separated. A `;` between fields is a parse error. Don't end
   statements with `;` either — newlines terminate.
3. **Generics**: `[T]`, never `<T>`. `Vec<T>`-style syntax is a parse error;
   `<` is always comparison.
4. **Patterns use bare variant names**: `match s { Active => ... }`.
   `Status.Active =>` does not parse. In expressions, construct fieldless
   variants bare (`Active`) — qualified `Status.Active` checks but fails at
   runtime today (§8).
5. **No exceptions, no null.** `Result[T, E]` + `?` and `Optional[T]` +
   `match`/`if let`/`unwrap_or`. `guard (cond) else { return ... }` is the
   early-exit idiom; the else block must diverge.
6. **`push` needs `mut` and returns `Void`** (`E5004`). Functional building
   (`+`, `concat`, shadowing re-`let`) is the default idiom.
7. **`map.contains(...)` doesn't exist** (`E4013`): use `contains_key` /
   `contains_value`. `Set.contains` is fine.
8. **Imports are braced**: `use core.test.{fail}`. Bare `use core.test` is
   `E4014`; `use x as y` is v1.x.
9. **No implicit numeric coercion**: `Int / Int` truncates toward zero
   (`17 / 5 == 3`); `Int + Float` is a type error — `.to_float()` first.
10. **Effect ops need `with` or a `handling` scope.** Calling `log("x")`
    without `with Log` (or an enclosing `handling` block) gives the
    effect-specific `E6005` — "`log` is an operation of effect `Log`, but
    the effect is neither declared … nor handled here" — with a note stating
    the fix. The lambda-handler form `Log.handler(...)` is `E6006` (reserved
    until v1.x). Also, `handling (...) { ... }` is a statement — assign
    results to a `let mut` declared outside.
11. **Don't invent stdlib.** No file I/O, HTTP, JSON, regex, env vars, or
    random in v1 (§4). The built-in method set is closed; anything else is
    `E4013`. `core.effect.Log`'s op is `log(message)` — one argument, no
    level parameter.
12. **`String.len()` counts characters** (Unicode scalar values), not bytes.
13. **Operators on user types are earned**: implement `Equatable` for `==`,
    `Comparable` for `<` (else `E4005`). Primitive conformances are sealed
    (`E4011`) — newtype to customize. `Bool` is not `Comparable`;
    `Float` is not `Hashable`.
14. **Interpolating user types**: implement `Displayable` but call
    `.to_string()` explicitly inside `${...}` — direct `${value}` dispatch
    through Displayable is not wired yet (§8).
15. **Tests**: `expect(x).to_equal(y)` works without imports; `assert_eq`/
    `assert_ne` now work on primitives in the interpreter too (`Int`/`Float`/
    `Bool`/`String`/`Char`). `assert_*`/`fail` need `use core.test.{...}`.
16. **Annotate public items with `@context`** or every public declaration
    warns (`W8013`) — and fails under `--strict`.
17. **One method namespace per type**: don't define `render` in both the
    class body and an `impl Trait for` block (`E4012`).
18. **Tuple indexing `t.0`** is not v1 (`E2092`) — destructure.
19. **Entry point**: `fn main()` (no args, `Void` or omitted return) in
    `src/main.bock` / module `main`. `bock run` takes a file path;
    `bock build` takes no file args (operates on the cwd project).
20. **Multi-line chains**: end the line with the operator or start the next
    line with `.`/`|>` — otherwise the newline terminates the statement.

---

## 8. Known divergences (as of pack 0.1.1)

True statements about the current implementation that contradict the spec or
naive expectations. Don't fight these; don't "fix" correct user code around
them. Each is tracked for repair — a future pack version removes entries as
they close.

**Fixed since 0.1.0** (entries removed in 0.1.1, all landed by [#342]): the
interpreter's `?` operator now propagates `Err`/`None` cleanly at the call
boundary instead of aborting; `assert_eq`/`assert_ne` now work on primitives
in the interpreter (`Int`/`Float`/`Bool`/`String`/`Char`); and `bock test`
now resolves cross-file `use main.{…}` imports (the test path mirrors project
resolution).

1. **`${value}` does not dispatch through `Displayable`.** The interpreter
   prints a structural form (`Money {cents: 7}`), js prints
   `[object Object]`. Call `.to_string()` explicitly (spec §18.5 says
   interpolation should dispatch; it doesn't yet).
2. **User-type `==` mis-lowers on js** to reference equality even with
   `impl Equatable` (the interpreter is correct). Call `.eq()` explicitly
   in code that ships to targets. Direct `<`/`>` via `Comparable` lower
   correctly on js.
3. **Generic comparisons under a trait bound mis-lower on targets.**
   `fn largest[T: Comparable](a: T, b: T)` using `a < b` (or even
   `a.compare(b)`) returns wrong results on js. Keep comparisons concrete
   (compare extracted `Int`/`String` keys) until this closes.
4. **Qualified variant patterns don't parse** (`Status.Active =>`), despite
   grammar §21.11 — use bare names. Qualified *construction* of fieldless
   variants (`Status.Active` as an expression) passes `bock check` but is
   an interpreter runtime error (`undefined variable: Status`) and per
   §6.7 should be rejected statically.
5. **Spurious `W1001 unused import`** for effect names used only in
   `with` / `handling` / `impl ... for` positions (e.g. `Log`). Keep the
   import; the warning is harmless.
6. **Statement-position `if/else` with arms ending in `println` truncates
   the rest of the function on the python target** (the lowering emits
   `return print(...)`). Bind the result (`let msg = if ... `) and print
   once, or structure so the `if` is the tail expression.
7. **Re-declaring the same `let` name in sibling `handling` blocks
   mis-lowers on js** (second block drops the declaration). Use distinct
   names.

---

*Regenerate / re-verify: `tools/scripts/verify-context-pack.sh` extracts
every ```bock block above and runs `bock check` on each. See
`context-pack/README.md` for versioning rules.*
