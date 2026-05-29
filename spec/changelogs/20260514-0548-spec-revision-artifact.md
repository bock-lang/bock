# `spec/bock-spec.md` Revision Artifact

**Date:** 2026-05-14
**Source changelogs:** D1+D2 consolidation (14 entries) + 6 ambiguity fixes confirmed by design chat + §10.4 verification report
**Apply by:** Replacing each existing section's content with the new content below.

This document contains the complete drop-in text for each affected section. For unchanged sections, see the existing `spec/bock-spec.md`. Twenty-one sections are touched in total.

The sections appear in spec order. Each entry has:

- **What changes:** one-paragraph summary of the diff
- **New content:** the complete section text to land in `spec/bock-spec.md`

When applying, replace the current section (heading line through next section heading) with the **New content** block. Cross-references between sections in this document use the same §-number notation as the spec; they resolve correctly after all sections are updated.

---

## §1.3 — Supported Targets

**What changes:** The flat 9-row table is split into two clearly labeled groups: "Ships in v1" (5 targets) and "Planned for v1.x" (4 targets). No language semantics change. Sets up honest expectations and gives marketing the "5 shipping, 4 planned" framing.

**New content:**

### 1.3 — Supported Targets

**Ships in v1.** Bock transpiles to five targets in v1:

| Target | Language   | Use Cases                       |
|--------|------------|---------------------------------|
| js     | JavaScript | Web frontends, Node.js servers  |
| ts     | TypeScript | Type-safe web/Node.js           |
| python | Python     | Data science, scripting, APIs   |
| rust   | Rust       | Systems, performance-critical   |
| go     | Go         | Services, CLI tools, networking |

**Planned for v1.x.** Four additional targets are on the v1.x roadmap:

| Target  | Language | Use Cases                |
|---------|----------|--------------------------|
| java    | Java     | Enterprise, Android      |
| cpp     | C++      | Systems, games, embedded |
| csharp  | C#       | .NET, Unity, Windows     |
| swift   | Swift    | iOS, macOS               |

The v1.x list expresses target ambition and informs the design of the AIR and codegen architecture (target profiles, capability gap synthesis); it is not a v1 commitment. Users requiring these targets in v1 should expect to wait for v1.x.

---

## §4.7 — Refined Types

**What changes:** Refinement types with `where`-clause predicates are marked **Reserved Post-v1**. The basic `type Email = String` alias form (no `where` clause) remains v1. Worked examples preserved with explicit Reserved marker as design intent for the future v1.x or later design pass.

**New content:**

### 4.7 — Refined Types

Basic type aliases work in v1:

```bock
type Email = String
type Port = Int
type NonEmpty[T] = List[T]
```

These declare type aliases without runtime checking. The alias is structural: an `Email` is interchangeable with `String` at use sites; the alias documents intent and clarifies type signatures.

**Reserved Post-v1: refinement predicates.** Refinement types with predicate clauses are a planned future extension to the type system:

```bock
// Reserved Post-v1 — does not compile in v1
type Email = String
  where (matches(r"^[^@]+@[^@]+\.[^@]+$"))
type Port = Int where (1 <= self <= 65535)
type NonEmpty[T] = List[T] where (len(self) > 0)
```

The design questions for refinement types are substantive:

- Predicate evaluation timing (construction-only, every assignment, every use)
- Type compatibility under refinement (`Email` assignable to `String`? to another `Email`?)
- Target codegen costs (runtime predicate checking on every assignment is expensive on hot paths)
- Interaction with AI capability gap synthesis (predicate-aware synthesis is a research direction)

The worked examples above are retained as design intent for the future design pass. v1 compilers reject `where (...)` clauses on type aliases as a "Reserved Post-v1" diagnostic.

---

## §6.1 — Functions

**What changes:** Adds default parameter value support as v1 surface. Current §6.1 shows the basic syntax template but does not document defaults; this revision adds a worked example and a Semantics subsection that locks the four semantic decisions (per-call evaluation, positional-first binding, type-checked-at-definition, call-site effect attribution). Default parameters are scheduled for a paired implementation session; the call-site resolution gap closes before v1 release.

**New content:**

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

**Default parameter values.** Parameters may carry default values that apply at call sites where the argument is omitted:

```bock
fn greet(name: String, greeting: String = "Hello") -> String {
  "${greeting}, ${name}!"
}

greet("Alice")              // returns "Hello, Alice!"
greet("Bob", "Hi")          // returns "Hi, Bob!"
```

#### Semantics

**Evaluation timing.** Default value expressions are evaluated per-call at the call site, not once at function definition. Each invocation that omits the parameter evaluates the default expression fresh.

```bock
fn append_log(msg: String, log: List[String] = List.new()) { ... }
append_log("a")  // log is a fresh empty List
append_log("b")  // log is a different fresh empty List
```

This avoids the Python-style "mutable default argument" gotcha where the default is shared across calls.

**Argument binding order.** Parameters are bound positionally first, then named arguments fill remaining parameters by name. Defaults apply to parameters that remain unbound after positional and named arguments are resolved.

**Type checking.** The default expression must produce a value compatible with the parameter's type. Type checking happens at function definition; an incompatible default is a compile-time error.

**Effect tracking.** If the default expression invokes effectful operations (`List.new()` allocates; a hypothetical `current_time()` would require the `Clock` capability), the effect is attributed to the call site that triggers the default, not the function definition. The function's effect signature reflects defaults conservatively.

**Target codegen consistency.** All targets produce the same observable semantics. Targets with native default parameter support (JavaScript, Python) use the native form with care to match per-call evaluation. Targets without native defaults (Go) synthesize equivalent call-site checks.

---

## §6.10 — Derive Macros

**What changes:** `@derive` is **Reserved for v1.x via the plugin system** (Appendix C). Consistent with the §15 annotation taxonomy treating `@derive` as Reserved. The current §6.10 example is preserved as design intent for the v1.x plugin-driven derive surface.

**New content:**

### 6.10 — Derive Macros (Reserved for v1.x)

`@derive` is reserved for v1.x and will be delivered via the plugin system described in Appendix C. v1 has no built-in derive set; v1.x adds derive support as plugins register their derivable trait implementations.

```bock
// Reserved for v1.x — does not compile in v1
@derive(Equatable, Hashable, ToJson, FromJson)
record User {
  id: UserId
  name: String
  email: Email
}
```

v1 users author trait implementations manually via `impl Trait for Type` (§6.7). The convenience of auto-derivation lands in v1.x once the plugin loader ships.

---

## §10.3 — Handler Resolution (Two Layers)

**What changes:** Layer 3 (project-level `[effects]`) is marked Reserved for v1.x in line with Appendix A.3. The section subsection order is rearranged: Local handlers (Layer 1) appear first, then Module-level (Layer 2), then the Reserved Layer 3 marker. This matches the resolution order ("Local > Module > Project") and surfaces v1 content first.

**New content:**

### 10.3 — Handler Resolution

v1 supports two layers of handler resolution. Layer 3 (project-level defaults via `bock.project [effects]`) is **Reserved for v1.x** per Appendix A.3.

**Layer 1 — Local handlers:** Fine-grained control via `handling` blocks.

```bock
handling (Log with test_log, Clock with mock_clock) {
  process(data)
}
```

**Layer 2 — Module-level:** Override defaults for a module.

```bock
handle Log with AuditLogger
```

Resolution order: Local > Module. Innermost handler wins (dynamic scoping).

**Layer 3 — Project defaults (Reserved for v1.x).** A future `[effects]` table in `bock.project` will allow project-level default handlers. The table is Reserved in Appendix A.3 pending the design pass for project-scoped effect configuration. In v1, effect handler resolution uses Layers 1 and 2 only; functions that require an effect without a Layer 1 or Layer 2 handler in scope produce a compile-time error.

---

## §10.4 — Implementing Handlers

**What changes:** Form A (trait `impl` block) remains the only v1 handler form. Form B (named-field lambda via `Effect.handler(...)`) is preserved as design intent with a Reserved marker. Form C (bare lambda via `Effect.handler(lambda)`) was never in the spec; mentioned in passing as a planned v1.x ergonomic. Per the §10.4 verification report, the `Effect.handler(...)` constructor surface fails at name resolution today; the runtime would dispatch the constructed values if the front end produced them, so v1.x work is at parser, AST, and type checker.

**New content:**

### 10.4 — Implementing Handlers

v1 supports one handler form: a `record` (or other type) implementing the effect's trait via an `impl` block.

```bock
record ConsoleLog {}

impl Log for ConsoleLog {
  fn log(level: Level, message: String) -> Void {
    println("[${level}] ${message}")
  }
}
```

The handler is then installed via a `handling` block (§10.3):

```bock
handling (Log with ConsoleLog {}) {
  log(Info, "service started")
}
```

**Reserved for v1.x: lambda-based handler constructors.** The `Effect.handler(...)` constructor surface (named-field and bare-lambda forms) is Reserved for v1.x as ergonomic shorthand for handlers without state:

```bock
// Reserved for v1.x — does not compile in v1
let silent = Log.handler(
  log: (level, message) => {}
)
```

In v1, lambda-style handlers fail at name resolution (the effect name is not bound in the value namespace). The runtime dispatch infrastructure already accepts the value shapes that the lambda forms would produce; v1.x adds parser, AST, and type-checker support for `Effect.handler(...)` as a dedicated constructor that synthesizes the handler value.

---

## §10.6 — Transpilation

**What changes:** The effect erasure optimization is softened from a definite-future ("the compiler can inline...") to permissive language ("the compiler MAY apply..."). Aligns with current impl state (no erasure pass exists) and matches §17.4's MUST/SHOULD/MAY vocabulary.

**New content:**

### 10.6 — Transpilation

Effects compile to parameter passing universally. Target-optimized strategies (dependency injection in Java, protocol witnesses in Swift) are applied by the AI transpiler.

Effect erasure is a permissive optimization. The compiler MAY apply effect erasure to code paths where the effect set is provably empty after specialization. Implementation of effect erasure is not required for v1 conformance. v1.x may schedule erasure as a dedicated optimization pass; see ROADMAP.md.

---

## §12.2 — Imports

**What changes:** Brace-less single-name `use module.name` is removed; all single-name imports standardize on braced form `use module.{name}`. Wildcard `use module.*` preserved (current v1 surface, marked discouraged). Aliasing `use module as alias` noted as a planned v1.x ergonomic (never in current spec; not "deferred" but "planned addition").

**New content:**

### 12.2 — Imports

```bock
use core.collections.{List, Map}
use app.models.{User}
use app.services.*                 // wildcard (discouraged)
```

All named imports use braced form. The braced form scales naturally to multiple names (`use core.collections.{List, Map, Set}`) without a separate single-name shorthand.

**Planned for v1.x: aliased imports.** The `use module as alias` form is planned for v1.x as a syntactic convenience for resolving naming conflicts. v1 ships without aliasing because the first-party stdlib has no naming conflicts; aliasing becomes important once a third-party package ecosystem creates them:

```bock
// Planned for v1.x — does not compile in v1
use std.collections.HashMap as Map
```

In v1, users with naming conflicts resolve them via the braced form's explicit naming or by qualifying the type at the use site.

---

## §13.5 — Cancellation

**What changes:** §13.5 splits into two subsections. §13.5.1 documents the v1 adaptive-handler cancellation integration (which works today). §13.5.2 preserves the full design sketch (cooperative checkpoints, `check_cancel()`, `task.cancel()`, target mapping, strictness gating) as Reserved for v1.x with explicit roadmap entry points. The four-layer v1.x design pass scope (API surface, checkpoint insertion, target mapping codegen, strictness gating) is named in the spec so v1.x has clear sub-projects.

**New content:**

### 13.5 — Cancellation

Cancellation is modeled as an ambient effect (`Cancel`) available in async contexts. v1 supports cancellation through the adaptive handler integration described in §13.5.1, which exercises the `Cancel` effect at recovery-strategy boundaries. The full cancellation surface described in §13.5.2 (cooperative checkpoints throughout user code, explicit `check_cancel()`, `task.cancel()` API, target mapping, strictness gating) is Reserved for v1.x.

#### 13.5.1 — Cancellation in adaptive handlers (v1)

Adaptive handlers per §10.8 integrate with cancellation:

- The `RecoveryStrategy` trait's `attempt` method returns `Result[T, E] | Cancelled`, allowing strategies to surface cancellation explicitly
- The `on_cancel(self, context: RecoveryContext) -> Void` hook (default no-op) fires when the enclosing task is cancelled while a strategy is executing, enabling cleanup of external state
- Built-in combinators (`retry`, `circuit_break`, `use_cached`, `degrade`, `escalate`) check cancellation at their internal await points
- A strategy returning `Cancelled` halts the adaptive handler immediately; no further strategies are attempted
- The adaptive handler propagates `Cancelled` to its caller through the same channel as ordinary errors

v1 user code observes cancellation in two ways:

1. By implementing a custom `RecoveryStrategy` that handles `Cancelled` in its `attempt` return
2. By being called from within an adaptive handler whose enclosing task is cancelled (cancellation propagates through the handler's `Cancelled` return)

See §10.8 for the full adaptive handler specification including `RecoveryContext`, the built-in combinator semantics, and the manifest treatment of cancellation events.

#### 13.5.2 — Full cancellation surface (Reserved for v1.x)

The broader cancellation model described in this subsection is **Reserved for v1.x**. v1 compilers reject the constructs described below at the relevant pipeline stage (parse, type-check, or codegen) with a "Reserved for v1.x" diagnostic. v1 users who need cancellation observe it through the adaptive handler integration in §13.5.1; the constructs below are the v1.x extension that makes cancellation available throughout user code.

**Cooperative checkpoints.** The compiler inserts cancellation checks at well-defined points:

- Every `await` expression
- Every effect operation invocation (`with Clock`, `with Network`, etc.)
- Explicit `check_cancel()` calls for tight loops that don't otherwise reach a checkpoint
- Loop iteration boundaries in `@concurrent` blocks

At each checkpoint, if cancellation has been signaled, the task propagates a `Cancelled` value through the call stack. This is not an exception; it is a typed return value tracked by the type system like any other `Result`-like outcome.

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

**Cancellation and cleanup.** Code that holds resources across a checkpoint must handle cancellation explicitly. The `with` handler mechanism provides the standard cleanup pattern; handlers can register cleanup on the `Cancel` effect to release resources when the enclosing task is cancelled.

**Strictness interaction.**

| Level | Cancellation behavior |
|---|---|
| `sketch` | Checkpoints auto-inserted; no annotations required |
| `development` | Long-running operations (loops, recursion) warned if no `check_cancel()` reachable |
| `production` | Error if a `@concurrent` or `async` function has no reachable checkpoint within a configurable depth bound |

**v1.x design pass scope.** v1.x cancellation has four implementation layers, each of which warrants its own design pass within the v1.x roadmap:

1. **Builtin and API surface:** `check_cancel()` prelude function, `task.cancel()` method on task handles, `Cancelled` type, `Cancel` effect availability in non-async contexts (if any).
2. **Compiler-inserted checkpoints:** AIR lowering pass that inserts cancellation checks at `await` expressions, effect operations, and loop boundaries. Interaction with existing AIR optimization passes and effect erasure (§10.6).
3. **Target mapping codegen:** per-target codegen that maps the `Cancel` effect to the table above. Each target has its own native cancellation primitive with its own propagation semantics; the codegen pass produces equivalent observable behavior per the §20.4 cross-target correctness principle.
4. **Strictness gating:** static analysis to identify long-running operations without reachable checkpoints; warning and error diagnostics per the strictness table above.

These layers may ship across v1.x point releases rather than all at once. The v1.x roadmap entry decides whether to ship them as a bundle or incrementally.

---

## §14.1 — Native Blocks

**What changes:** Leading "Reserved for v1.x" status note added. The `native` keyword reservation is preserved (tokenized in v1) so v1.x can introduce the full surface without a breaking lexical change. The existing native block example is retained as planned v1.x surface.

**New content:**

### 14.1 — Native Blocks

**Status:** The `native` keyword is reserved (tokenized) in v1. The full native block surface (parsing, per-target inline code validation, capability gap interaction) is planned for v1.x. v1 code that uses `native` blocks fails at parse time with a "Reserved for v1.x" diagnostic. The keyword reservation prevents v1.x from being a breaking lexical change.

The planned v1.x surface:

```bock
@target(js)
native fn query_selector(sel: String) -> Optional[Element] {
  `document.querySelector(${sel})`
}
```

FFI is a discrete capability that warrants its own design pass when the time comes to ship it. The pass needs to resolve several questions including backtick tokenization edge cases, per-target inline code validation, interaction with the capability gap synthesis in §17.6, and interaction with the `@target` and `@platform` annotations (§15) which are deferred alongside this surface.

---

## §14.2 — Platform Abstraction Layer

**What changes:** Leading "Reserved for v1.x" status note added; defers with §14.1. The FFI linter warning is meaningless until §14.1's native blocks parse.

**New content:**

### 14.2 — Platform Abstraction Layer

**Status:** Reserved for v1.x with §14.1. The platform-trait surface and the FFI linter warning are deferred together; the warning is meaningless until native blocks parse.

The planned v1.x surface: for structured multi-target APIs, `platform trait` defines an interface with per-target implementations. FFI usage in multi-target projects triggers a linter warning suggesting migration to a platform trait.

---

## §15 — Annotations

**What changes:** Substantial restructure. The flat categorization (Compiler directives / Capabilities / Target / Context / Testing / Code generation) becomes a four-subsection taxonomy: §15.0 Recognition policy, §15.1 Codegen-consumed, §15.2 Test-runner-consumed, §15.3 Application sites, §15.4 Reserved for v1.x (including runtime guardrails as a named future direction). Recognition policy is error-everywhere. `@benchmark` is removed entirely (not deferred). `@target`/`@platform` defer with §14 FFI. `@property` and `@derive` Reserved for v1.x. Module-level annotations Reserved for v1.x. Runtime guardrails for the semantic context family (`@performance`, `@invariant`, `@security`, `@domain`) named with the verb-keyed payload pattern.

**New content:**

## 15. Annotations

Annotations use the `@` prefix and form a unified metadata system. The complete v1 taxonomy below is organized by routing: which compiler subsystem consumes each annotation.

### 15.0 — Recognition policy

Unknown annotations are a compile-time error in all strictness modes. A v1 compiler that encounters `@foo` without knowing what `@foo` means rejects the source with an "unknown annotation" diagnostic.

This policy applies uniformly: there is no "silent" or "warn" tier. Bock is feature-declarative: annotations encode semantic intent (capabilities, security boundaries, performance hints). Silent failure on typos like `@invarient` or `@requirs(Auth)` would be dangerous, and a warn tier creates the question of "which strictness am I in?" that the uniform-error policy eliminates.

Annotations are "known" when they appear in:

- The §15 taxonomy below (built-in annotations)
- A registered plugin's annotation surface (per Appendix C plugin system, Reserved for v1.x)

v1 has no plugin system; the built-in taxonomy is the complete known set. v1.x extends "known" to include plugin-declared annotations.

### 15.1 — Codegen-consumed annotations

These annotations are consumed by the C-AIR context interpreter and inform codegen decisions and AI provider Generate mode context.

| Annotation | Purpose |
|------------|---------|
| `@context("...")` | Declares contextual scope for code blocks; flows to AI provider |
| `@requires(Capability.X)` | Declares required capability; verified at compile time |
| `@performance` | Marks performance-critical code; influences synthesis (see §15.4 for runtime guardrail v1.x extension) |
| `@invariant` | Declarative state invariant (see §15.4 for runtime guardrail v1.x extension) |
| `@security` | Security-relevant marker; interacts with §11.8 PII propagation (see §15.4 for runtime guardrail v1.x extension) |
| `@domain` | Domain boundary marker (see §15.4 for runtime guardrail v1.x extension) |
| `@concurrent` | Marks code as concurrent-safe |
| `@managed` | Marks code as memory-managed (vs. ownership-tracked) |
| `@deterministic` | Declares pure determinism |
| `@inline` | Codegen inlining hint |
| `@cold` | Hot-path optimization hint (cold = rarely executed) |
| `@hot` | Hot-path optimization hint (hot = frequently executed) |
| `@deprecated("use X")` | Deprecation marker; produces compile-time diagnostic on use |

### 15.2 — Test-runner-consumed annotations

These annotations are consumed by `bock test`, not by the C-AIR context interpreter. The test runner discovers test functions by annotation and routes them to the configured test framework per target.

| Annotation | Purpose |
|------------|---------|
| `@test` | Marks a function as a test; included in `bock test` runs |
| `@test(skip: "reason")` | Marks a test as skipped without removing it |

The test runner reads these annotations directly from source; they do not flow through C-AIR codegen paths. Test functions are excluded from production builds (`bock build --no-tests` per §20.1).

### 15.3 — Application sites

In v1, annotations apply to individual declarations: `fn`, `record`, `enum`, `trait`, `effect`, `impl` blocks, and module members. Each annotation attaches to the declaration immediately following it.

**Module-level annotations on the `module` declaration itself are Reserved for v1.x.** The form `@context @requires(Auth) module accounts.api { ... }` (applying annotations across every declaration in a module) is planned for v1.x as a syntactic convenience. v1 users who want module-wide annotation semantics annotate each declaration individually.

### 15.4 — Reserved for v1.x

The following annotation surfaces are Reserved for v1.x. The reserved syntax is rejected by v1 compilers per §15.0; v1.x adds the routing.

**Annotation deferrals.**

| Annotation | Routing | Status |
|------------|---------|--------|
| `@property` | Property-based testing framework (extends `@test` infrastructure) | Reserved for v1.x pending stdlib property-based testing |
| `@derive(...)` | Codegen extension (generates impls) | Reserved for v1.x via plugin system (Appendix C) |
| `@target(...)` | Conditional compilation by codegen target | Reserved for v1.x with FFI (§14.1) |
| `@platform(...)` | Conditional compilation by platform | Reserved for v1.x with FFI (§14.1) |

**Runtime guardrails.** The semantic context annotations (`@performance`, `@invariant`, `@security`, `@domain`) are compile-time context in v1, used to inform codegen and AI provider Generate mode. v1.x is reserved for runtime guardrail variants of each, distinguished by a verb-keyed payload:

| Annotation form | Verb | Planned runtime semantics |
|-----------------|------|--------------------------|
| `@performance(track: ...)` | track | Runtime timing instrumentation with threshold-based reporting |
| `@invariant(assert: ...)` | assert | Runtime assertion checking; violation fails per configured severity |
| `@security(audit: ...)` | audit | Runtime audit event emission on access |
| `@domain(enforce: ...)` | enforce | Runtime domain boundary enforcement at call sites |

The runtime guardrails are not "deferred" in the sense of a half-built feature; they are a named future direction whose design pass happens when v1 stabilizes and concrete use cases inform the verb semantics. v1 compilers reject the verb-keyed payload forms as unknown annotation variants per §15.0.

**Removed entirely.** `@benchmark` was enumerated in earlier spec drafts. It is removed in v1 and not Reserved. Performance benchmarking is delegated to target-native tools (see §20.4); a Bock-level benchmark annotation does not fit the architecture.

---

## §16.3 — Serialization

**What changes:** Leading "Reserved Post-v1" status note added. AIR is internal in v1; serialization is not exposed. The AIR-T and AIR-B format descriptions preserved as planning sketch for future cross-tool interop, build cache reuse, and binary package distribution.

**New content:**

### 16.3 — Serialization

**Status:** Reserved Post-v1. AIR is an internal intermediate representation in v1; serialization is not exposed. The AIR-T and AIR-B formats remain in this section as a planning sketch for future cross-tool interop, build cache reuse, and binary package distribution work. Format details are non-normative.

**AIR-T (text format):** Human-readable, designed for AI consumption. This is what the AI transpiler receives.

**AIR-B (binary format):** Compact, content-addressed, module-level granularity. Used for build caches and binary package distribution.

---

## §16.4 — Binary Package Compatibility

**What changes:** Leading "Reserved Post-v1" status note added; defers with §16.3. v1 distributes packages in source form only. The compatibility rules are preserved as planning sketch.

**New content:**

### 16.4 — Binary Package Compatibility

**Status:** Reserved Post-v1 with §16.3. v1 distributes packages in source form only.

The planned mechanism: packages distribute pre-compiled T-AIR alongside source. Compatibility rules:

- Patch releases (1.2.x): Always compatible.
- Minor releases (1.x.0): Backward compatible (new features not pre-compiled).
- Major releases (x.0.0): Recompile from source (automatic fallback).

The compiler would check AIR format version and fall back to source compilation transparently when incompatible.

---

## §17.6 — Capability Gap Resolution

**What changes:** Adds a normative principle paragraph stating the synthesis rule (Generate mode per §17.8 against §17.4 confidence threshold) before the existing table. The existing six-row table is reframed under an "Illustrative synthesis examples" subheading. Resolves the normative-vs-illustrative ambiguity that D1 flagged: principle normative; specific syntheses illustrative.

**New content:**

### 17.6 — Capability Gap Resolution

At capability gaps (constructs where the target lacks a direct equivalent), codegen invokes the AI provider's Generate mode (§17.8) with the target profile and surrounding context. Generate's output is accepted when its confidence meets or exceeds the threshold in §17.4 (default 0.75 with `--strict` raising to 0.90); below threshold, codegen falls back to the deterministic strategy described in the target profile or surfaces an unrecoverable capability gap to the user.

The synthesis strategies in the table below are illustrative examples of how this principle manifests for common (construct, target) pairs. They are not normative: codegen may produce alternative syntheses that satisfy the confidence threshold and pass target profile verification. The table is informative for users tracking how their code is likely to be synthesized.

#### Illustrative synthesis examples

| AIR Construct    | Gap Example          | Synthesis                    |
|------------------|----------------------|------------------------------|
| Algebraic types  | JS (no ADTs)         | Tagged objects + switch      |
| Pattern matching | Go (no match)        | if/else chains               |
| Ownership/Move   | JS, Python (GC)      | Erase annotations            |
| Channels         | JS (no native)       | AsyncIterator + Queue class  |
| Refinement types | All targets          | Validation at boundary       |
| Effects          | All targets          | Parameter passing            |

---

## §18.5 — Trait-Language Integration

**What changes:** The single-paragraph current §18.5 expands to an explicit enumeration of nine trait → language integrations plus a conformance test surface. `Equatable` added explicitly (was implicit; required by `==`/`!=` operators). Conformance tests scoped at minimum coverage per integration with cross-target equivalence as the acceptance bar.

**New content:**

### 18.5 — Trait-Language Integration

Core traits opt types into language features. Implementing a trait on a user-defined type enables the corresponding syntactic form for values of that type. The following integrations are normative for v1:

| Trait (from `core.*`) | Language feature enabled |
|-----------------------|--------------------------|
| `Equatable` | `==`, `!=` operators |
| `Comparable` | `<`, `>`, `<=`, `>=` operators |
| `Iterable` | `for x in collection` loop syntax |
| `Displayable` | `${expr}` string interpolation |
| `Add` | `+` binary operator |
| `Sub` | `-` binary operator |
| `Mul` | `*` binary operator |
| `Div` | `/` binary operator |
| `Mod` | `%` binary operator |

The integration is bidirectional: a type's `impl` block declares trait conformance, and the compiler uses that conformance to permit the corresponding syntactic form for values of that type. A type without `impl Comparable for MyType` cannot use `<` on `MyType` values; the compiler rejects the code at type-check time with a "type does not implement Comparable" diagnostic.

Trait conformance is checked at every site where the syntactic form is used: arithmetic expressions, comparison in `if` conditions, comparison in `match` guards, ordering in sorts, equality in pattern matching, iteration in `for` loops, interpolation in string expressions. The integration is uniform: there are no "Comparable for if-conditions only" partial conformances; declaring `impl Comparable for MyType` enables `<` on `MyType` everywhere the operator is valid.

#### Conformance test surface

Each integration in the table above has a corresponding conformance test in the v1 conformance suite. Per the §20.4 architectural principle (Bock owns cross-target correctness), conformance is verified by running the same test program across all shipping targets and confirming equivalent observable behavior.

The conformance suite for §18.5 verifies, at minimum:

- **Equatable:** a user-defined type implementing `Equatable` supports `==` and `!=` in expressions, `if` conditions, `match` guards, and pattern matching equality checks.
- **Comparable:** a user-defined type implementing `Comparable` supports `<`, `>`, `<=`, `>=` in expressions, `if` conditions, `match` guards, and as ordering for sort operations. Comparable conformance implies Equatable conformance.
- **Iterable:** a user-defined type implementing `Iterable` supports `for x in collection` iteration, with correct handling of `break`, `continue`, and exhaustion. Nested iteration over Iterable types must work.
- **Displayable:** a user-defined type implementing `Displayable` supports `${expr}` string interpolation, producing the implementation's defined output. Interpolation in nested contexts (interpolating a Displayable inside another interpolation) must work.
- **Numeric operator overloading:** a user-defined type implementing `Add`, `Sub`, `Mul`, `Div`, or `Mod` supports the corresponding binary operator with correct operator precedence, associativity, and type checking on mixed operand types.

A conformance test passes when its program produces equivalent observable output on every shipping target (JavaScript, TypeScript, Python, Rust, Go for v1; Java, C++, C#, Swift when added in v1.x). A failure on any target is a transpilation bug (§20.4), not a user-code bug.

---

## §19.7 — Versioning and Stability

**What changes:** Leading "Reserved for v1.x" status note added. Stability tiers presuppose an ecosystem of third-party packages; v1 ships first-party stdlib only. The tier scheme and production-strictness rejection logic deferred to v1.x.

**New content:**

### 19.7 — Versioning and Stability

Strict semver applies to all packages in v1.

**Reserved for v1.x: stability tiers.** Stability tiers become useful once an ecosystem of third-party Bock packages exists. v1 ships with first-party stdlib packages only; the tier scheme and the production-strictness rejection logic are Reserved for v1.x. The planned mechanism:

> Stability tiers: `stable`, `beta`, `experimental`. Production strictness can reject dependencies below a stability threshold.

This mechanism is preserved here as design intent for the v1.x release when third-party packages create the conflicts the tier scheme resolves.

---

## §20.3 — Language Server (LSP)

**What changes:** All five Bock-specific extensions (AI Context Panel, Target Preview, Capability Graph, Smart Completions, Inline Diagnostics) marked Reserved for v1.x. Basic LSP capabilities (completion, hover, definition, diagnostics via standard protocol) remain v1. Resolves the ambiguity-fix #4: five extensions, not four.

**New content:**

### 20.3 — Language Server (LSP)

v1 ships a Full LSP implementation supporting standard protocol capabilities: completion, hover, go-to-definition, and diagnostics.

**Reserved for v1.x: Bock-specific extensions.** The following five Bock-specific LSP extensions are planned for v1.x. They are preserved here as design intent:

- **AI Context Panel:** Real-time view of what the AI transpiler sees at cursor position — context annotations, capabilities, effects, ownership state, active handlers.
- **Target Preview:** Live transpiled output for any function, switchable between targets.
- **Capability Graph:** Visual call-graph with capability and effect propagation.
- **Smart Completions:** Ownership-aware (marks consuming methods), effect-aware (suggests effect operations), pipe-aware (suggests type-compatible functions).
- **Inline Diagnostics:** Ownership transfer warnings, capability narrowing hints, AI decision previews.

These extensions go beyond standard LSP and require dedicated UX design work. v1 users get the basic LSP surface; the Bock-specific augmentations ship in v1.x.

---

## §20.4 — Testing Tiers

**What changes:** Adds an architectural principle paragraph (cross-target correctness owned by Bock; performance delegated to target-native tools) after the existing tier definitions. The principle operationalizes why `@test` fits and `@benchmark` doesn't.

**New content:**

### 20.4 — Testing Tiers

**Tier 1 — Semantic tests:** Run on the Bock interpreter. Fast. Target-independent. The canonical semantics reference.

**Tier 2 — Transpilation tests:** Same tests compiled to target languages. Per-target execution.

**Tier 3 — Integration tests:** Platform-specific tests (`@target`, `@platform` annotated) requiring actual runtimes.

**Smart target selection:** Analyzes which AIR constructs changed and which targets are affected. Tests targets where changed constructs must be emulated (high risk), skips targets with native support (low risk).

Principle: semantic pass + target fail = transpiler bug, not user code bug.

#### Cross-target correctness vs. performance

Bock owns cross-target correctness verification: a `@test` function that passes on JavaScript must also pass on Python, Rust, and every other target the codebase ships to. This is the architectural value of the cross-target testing tier: it operationalizes the cross-target semantic equivalence claim that distinguishes Bock from a target-by-target source-to-source transpiler.

Performance, by contrast, is delegated to target-native tools. Bock does not own benchmarking. Performance varies wildly across targets by design (target choice is itself a performance decision), and every target ships mature benchmark tooling (`cargo bench`, `pytest-benchmark`, `npm run bench`, `go test -bench`). Users who care about performance benchmark the transpiled output with the target's native tools directly. This is why §15 does not include a `@benchmark` annotation; the cross-target value proposition of a unifying benchmark surface does not exist.

Runtime guardrail variants of the semantic context annotations (§15.4) are a separate concern: they verify constraints at runtime (assertions, audit logging, performance thresholds), not benchmark performance for comparison. The guardrails express user-declared invariants that should hold; they do not measure aggregate performance characteristics.

**Project mode validation gate.** When project mode (§20.6.2) includes transpiled tests in build output, Tier 2 transpilation tests serve as the gate that determines whether the output is trustworthy. A target's codegen is considered project-mode-ready when its Tier 2 tests pass on a representative test suite. Targets where Tier 2 tests fail intermittently or on common patterns should not ship project mode by default — they ship source mode (`--source-only`) until the transpilation gap closes. This is not a user-facing distinction but a release-readiness criterion for each target's codegen package.

The gate also includes formatter cleanliness: a target's project mode output must pass the target's configured formatter (`prettier --check`, `gofmt -l`, `rustfmt --check`, `black --check`, etc.) without modification. A formatter that wants to rewrite Bock's emitted code introduces version-control churn on every user's first commit — the validation gate prevents shipping that. For targets with multiple supported formatters (Python's Black and Ruff format), each variant has its own gate; a target may ship project mode with Black support while Ruff format support is still maturing.

---

## §20.5 — Debugger

**What changes:** Leading "Reserved for v1.x" status note added. Source maps (covered by `--source-map`/`--no-source-map` flags from prior CLI amendment) ship in v1 and enable external debugger integration. The built-in interpreter debugger UI is the deferred piece.

**New content:**

### 20.5 — Debugger

**Status:** Source map generation (`--source-map` / `--no-source-map`) ships in v1 and enables external debugger integration through standard target-specific tooling (Node.js inspector, py-spy, rust-gdb, etc.). The built-in interpreter debugger UI described below is **Reserved for v1.x**. The UI design is preserved as v1.x intent.

The planned v1.x UI surface: built-in interpreter debugger with breakpoints, stepping, expression evaluation, ownership state inspection, effect handler display, and context viewing. Source maps enable debugging transpiled code in target-language debuggers.

---

## §20.6 — Build System

**What changes:** The opening paragraph's feature list (incremental builds, parallel compilation, remote build cache, build hooks, distributed builds) is qualified to match Appendix A.3's existing deferrals. Remote cache, build hooks, and distributed builds Reserved for v1.x; incremental and parallel builds remain v1. The §20.6.1 and §20.6.2 subsections are unchanged.

**New content:**

### 20.6 — Build System

Bock's build system supports incremental compilation at module granularity via content hashing, parallel builds across packages, and per-target output isolation as described in §20.6.1 and §20.6.2. Additional capabilities including remote cache reuse, build hooks (Bock scripts), and distributed builds for CI are **Reserved for v1.x**; their configuration surfaces are marked Reserved in Appendix A.3.

Build pipeline: Parse → Type Check → Context Resolve → Target Analyze → Code Generate → Verify → Target Compile → Assemble Deliverable.

[§20.6.1 and §20.6.2 continue unchanged from current spec.]

---

## End of Revision Artifact

**Summary of changes:**

- **21 sections touched** across §1, §4, §6, §10, §12, §13, §14, §15, §16, §17, §18, §19, §20
- **14 changelogs integrated:** `20260513-0500` through `20260513-0550` (D1+D2 batch) plus `20260514-0408`, `20260514-0412`, `20260514-0449` (pending items)
- **6 ambiguity fixes applied:** tuple indexing dropped, §6.10 Reserved consistent with §15, §10.4 Form B preserved with Reserved marker (Form A v1 normative), §20.3 fifth LSP extension added, §17.6 title preserved as "Capability Gap Resolution", §10.3 layer ordering rearranged minimally
- **§10.4 verification confirmed:** only Form A works today; Forms B and C both Reserved (single entry per "A" choice)
- **No changes to:** §1.2 (kept as design-goal framing), §1.5 (paradigm — flagged separately as out-of-batch cleanup), Appendix A (already amended by prior changelogs), Appendix B (no changes needed), Appendix C (referenced but unchanged), Appendix D (referenced but unchanged)

**After applying:**

1. Verify the spec table of contents anchors still resolve (no section headings renumbered; only content within sections changed)
2. Run `mdbook build docs` to confirm no broken cross-references introduced
3. Spot-check cross-references between revised sections (§13.5 references §10.8; §15 references §11.8 and §20.4; §18.5 references §20.4)
4. Commit as a single spec revision or split per-changelog if preferred for git history granularity

**Pending follow-up handoffs to spawn after the spec lands** (not part of this revision):

1. **CLI exit code bug:** `bock check` returns 0 despite errors per the §10.4 verification report. Small impl task with concrete reproductions.
2. **Handler dispatch conformance fixture:** `compiler/tests/conformance/effects/` has no handler-dispatch coverage. Backlog item for impl chat alongside the §18.5 conformance suite work.
3. **§1.5 paradigm cleanup:** untethered from removed `[paradigm]` config; needs prose realignment. Small spec edit, separate from this batch.
