# Context

Context is structured semantic metadata attached to
declarations. It serves three audiences simultaneously: the AI
transpiler (informing code generation choices), the compiler
(enabling capability and security verification), and human
readers (self-documenting code). Context annotations use the
`@` prefix and form a unified system covering intent,
capabilities, performance budgets, invariants, security
classifications, and domain tags.

## `@context` — Free-Form Intent

`@context` carries free-form prose describing what a piece of
code is for, what it assumes, and what constraints it must
honour. The AI transpiler reads it; the compiler stores it for
tooling.

```bock
@context("Validates user credentials against the auth store.")
fn check_credentials(login: String, pw: String) -> Bool {
  login.len() > 0 && pw.len() > 0
}

fn main() {
  let login = "alice"
  let pw = "secret"
  println("${check_credentials(login, pw)}")
}
```

<!-- verify: bock-check -->

For longer descriptions, use a multi-line string literal. The
free-form text supports optional structured markers that
tooling recognises:

```
@intent:      what this code is supposed to do
@assumption:  what it assumes about its environment
@constraint:  performance or correctness constraints
@security:    security-relevant facts about the operation
@related:     other declarations a reader should know about
```

These markers are conventions inside the prose, not separate
annotations.

## `@requires` — Capabilities

`@requires` declares the platform capabilities a function
needs. The compiler propagates capabilities through the call
graph: if `f` calls `g` and `g` requires `Capability.Network`,
then `f` must also declare it (or a caller higher up must).

```bock
@requires(Capability.Network)
fn fetch(url: String) -> String {
  "fetched ${url}"
}

@requires(Capability.Network, Capability.Storage)
fn fetch_and_cache(url: String) -> String {
  let data = fetch(url)
  "cached ${data}"
}

fn main() {
  println(fetch_and_cache("https://example.com"))
}
```

<!-- verify: bock-check -->

The capability taxonomy is fixed in the prelude:

```
Network        Storage        Crypto         GPU
Camera         Microphone     Location       Notifications
Bluetooth      Biometrics     Clipboard      SystemProcess
FFI            Environment    Clock          Random
```

Capabilities are platform-permission-shaped: each one maps to
a specific runtime permission on each target (an Android
manifest entry, a macOS entitlement, a browser permission
prompt). The compiler uses the union of all `@requires` in a
build to generate the platform's permission manifest.

## `@performance` — Performance Budgets

`@performance` declares a function's expected latency and
memory ceiling. The AI transpiler uses the values to pick
optimization strategies — a tight budget may select an early-
exit algorithm; a generous budget may select clarity over
speed.

```bock
@performance(max_latency: 100, max_memory: 50)
fn fast_search(items: List[Int], target: Int) -> Optional[Int] {
  if (items.len() == 0) { None } else { Some(target) }
}

fn main() {
  match fast_search([1, 2, 3], 2) {
    Some(i) => println("found ${i}")
    None => println("missing")
  }
}
```

<!-- verify: bock-check -->

Tooling can also generate runtime monitoring that flags
functions exceeding their budgets — useful for production
observability.

## `@invariant` — Verified Constraints

`@invariant` declares a predicate that should hold over the
function's inputs or outputs. The compiler attempts static
verification; when it cannot prove the invariant, it inserts a
runtime assertion as a fallback.

```bock
@invariant(true)
fn filter_positive(numbers: List[Int]) -> List[Int] {
  numbers.filter((n) => n > 0)
}

fn main() {
  let r = filter_positive([-1, 2, -3, 4])
  println("len=${r.len()}")
}
```

<!-- verify: bock-check -->

Invariant expressions can refer to function parameters and to
the result via `self`. Today the compiler accepts the
annotation but the verification and runtime-assertion machinery
is still being built out; the annotation is a forward-looking
declaration of intent.

## `@security` — Security Classification

`@security` classifies a declaration's security sensitivity.
The compiler uses the classification to drive type-level
propagation (PII flows are tracked through function signatures)
and to gate certain operations (logging a PII-tainted value
generates a warning regardless of the calling context).

```bock
@security(level: "confidential", pii: true)
record UserProfile {
  name: String
  email: String
  id: Int
}

impl UserProfile {
  fn safe_display(self) -> String { "User #${self.id}" }
}

fn main() {
  let u = UserProfile { name: "Alice", email: "a@x", id: 1 }
  println(u.safe_display())
}
```

<!-- verify: bock-check -->

The level field is informational. `pii: true` is the marker the
compiler uses to track personally identifiable information at
the type level. A type is PII-tainted if:

- it is directly annotated `@security(pii: true)`, or
- it contains a field whose type is PII-tainted, or
- it is a generic instantiation where any type parameter is
  PII-tainted.

The compiler tracks what types cross function signatures, not
what happens to data inside function bodies — this is type-
level analysis, not value-level taint tracking. Passing a
PII-tainted type to a logging function (`print`, `log`, anything
with the `Log` effect) generates a warning regardless of the
calling module's context.

## `@domain` — Domain Tags

`@domain` tags a declaration with one or more domain labels.
The labels help the AI transpiler manage its context window
across large codebases — it can preferentially load related
modules into the prompt context.

```bock
@domain("e-commerce")
fn checkout() -> Void {
  println("checkout")
}

fn main() {
  checkout()
}
```

<!-- verify: bock-check -->

Domains are project-local strings. There is no global taxonomy
— each project defines its own.

## Annotation Composition

A declaration may carry multiple annotations. They stack in any
order; the compiler reads them all:

```bock
@context("Payment processing entry point. Tokenizes card data.")
@requires(Capability.Network)
@performance(max_latency: 500)
fn process_payment(amount: Int, token: String) -> Result[Int, String] {
  if (amount <= 0) { Err("invalid amount") } else { Ok(amount) }
}

fn main() {
  match process_payment(100, "tok_abc") {
    Ok(v) => println("ok ${v}")
    Err(e) => println("err ${e}")
  }
}
```

<!-- verify: bock-check -->

Module-level annotations (those attached to a `module`
declaration) propagate down to every declaration in the file.
Declaration-level annotations override module-level annotations
of the same kind, except for `@requires` — which is **additive**.
A declaration's capability set unions with the module's, never
narrows it.

> Note: in the current implementation, annotation
> propagation from `module` declarations is not yet enforced
> at the parser level — the syntactic ability to attach
> annotations to `module` is still being wired up. The
> per-declaration forms shown above work today.

## Context and Effects

Context describes *intent and capability requirements*; effects
describe *which observable operations a function performs*. The
two systems are independent but related:

- `@requires(Capability.Network)` says "this function needs
  network permission at runtime."
- `with Network` (if you defined a `Network` effect) says "this
  function calls into the Network effect."

A function that performs HTTP requests would typically declare
both: `@requires(Capability.Network)` for the permission, and
the `Network` effect to make the operation explicit at the
type level. The annotation governs *who can run the code*; the
effect governs *what handler supplies the operation*.

See [Effects](./effects.md) for the full effect system.

## Strictness Interaction

Context completeness is enforced in `production` strictness.
In `sketch` and `development`, missing capability declarations
are warnings; in `production` they are errors. The same applies
to `@security` annotations on PII-tainted types whose modules
lack security context. This lets prototypes move fast and
forces production code to be explicit.

The full per-annotation strictness table lives in §15 of
`spec/bock-spec.md`.
