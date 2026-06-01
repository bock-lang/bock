# core.test — v1 surface ships

**Date:** 2026-06-01
**Affects:** §18.3 (`core.test`), references §15.4 (`@test`/`@benchmark`), §20.4 (performance delegation)
**Type:** addition

> **Status: shipped.** The v1 `core.test` surface below is authored, reviewed,
> and **builds and runs end to end on all five targets** (js, ts, python, rust,
> go) via the `exec_core_test` conformance fixture. This is the **11th and final**
> of the 11 v1 `core.*` modules. No compiler change was required to ship it
> (codegen-only); the `bock test`-runner reconciliation and the cross-target
> generic limitations are recorded as FOUNDs below, not fixed here.

## Change

The v1 surface of the `core.test` standard-library module ships. §18.3 lists
`core.test` (v1) as "Assertions, BDD grouping, mocking, benchmarking". The
project owner (DQ26) set the v1 **floor** as **assertions only**, shipping
**both** a free-function assertion API and a fluent matcher API, fully
overlapping, with the fluent layer **powered by the free functions** (the free
`assert_*` functions are the primitives; the fluent matcher methods are thin
forwarders to them). The shipped `module core.test` public surface is:

**Free-function assertions** (each built over the §18.2 prelude `assert(cond)`):

- `assert_true(cond: Bool)`, `assert_false(cond: Bool)` — boolean assertions.
- `assert_eq[T: Equatable](actual, expected)`, `assert_ne[T: Equatable](...)` —
  equality via the `Equatable.eq` method (trait-method dispatch through the
  bound, the shape `core.compare.max[T: Comparable]` uses).
- `assert_some[T](o: Optional[T])`, `assert_none[T](o)` — Optional tag tests.
- `assert_ok[T, E](r: Result[T, E])`, `assert_err[T, E](r)` — Result tag tests.
- `fail(message: String)` — unconditional failure helper.

**Fluent matchers** (powered by the free functions):

- `record Expectation[T: Equatable] { value: T }`, `fn expect[T: Equatable](value) ->
  Expectation[T]`, with methods `to_equal(other)` / `to_not_equal(other)`.
- `record BoolExpectation { value: Bool }`, `fn expect_bool(value: Bool) ->
  BoolExpectation`, with methods `to_be_true()` / `to_be_false()`.

The module is **pure Bock** (no per-target runtime shim, like `core.option`/
`core.iter`/`core.compare`). Every assertion bottoms out in the prelude
`assert`, which lowers ×5 and — under `bock test` — raises the assertion-failure
path the runner reports as a failing test.

### Design constraints baked into the surface (cross-target reach)

The shape above is what the **proven** v1 cross-target surface supports; three
codegen gaps (FOUNDs below) shaped it:

- **Equality is bounded `Equatable` and reaches user types, not primitives.**
  `assert_eq`/`assert_ne` and the fluent `to_equal`/`to_not_equal` lower on all
  five targets for a **user type that implements `Equatable`**. A *primitive*
  (`Int`/`String`/`Bool`) reached through the generic bound does **not** lower on
  the static targets (Rust/Go/TS reject `T: Equatable` / `T == T`; the dynamic
  targets erase the bound but then fail at run time). For primitive equality the
  surface directs callers to `assert_true(a == b)` over concrete operands.
- **Booleans are a separate non-generic entry point.** A generic
  `Expectation[T: Equatable]` cannot hold a `Bool` (`Bool` is not `Equatable`
  behind a generic on the static targets), so `to_be_true`/`to_be_false` live on
  a non-generic `BoolExpectation` reached via `expect_bool`, not on `expect`.
- **`Optional`/`Result` matchers are free functions**, not `Expectation` methods:
  a method on `Expectation[T]` cannot refine `T` to `Optional[U]`/`Result[U,E]`
  (no method-level `where T = …`), so those are `assert_some`/`assert_none`/
  `assert_ok`/`assert_err`.

## Reserved for v1.x (explicitly OUT of the v1 `core.test` floor)

- **BDD grouping** (`describe`/`it`/`context` nesting) — §18.3 lists it for v1;
  deferred. `@test` functions (§15.4) are the v1 grouping unit.
- **Mocking** — deferred. The **effect-handler pattern** (§10.4: swap a record
  `impl` of an effect for a test double, installed via `handling`) is the v1
  idiom for test doubles; `core.test` ships no dedicated mock surface.
- **Ergonomic matcher extras** beyond the floor (e.g. `to_throw`, `to_contain`,
  numeric/collection matchers, custom failure messages) — deferred.
- **Property testing, snapshot testing** — already Reserved by §18.3; unchanged.

## Benchmarking is OUT (not part of `core.test`)

Benchmarking is **not** shipped and **not** Reserved for `core.test`. §15.4
removed `@benchmark` "entirely … not Reserved", and §20.4 delegates performance
benchmarking to target-native tools (`cargo bench`, `pytest-benchmark`, `go test
-bench`, …). `core.test` owns correctness assertions, not performance
measurement.

> **OPEN (for Design) — §18.3 vs §15.4/§20.4 benchmarking contradiction.**
> §18.3 still enumerates "benchmarking" in the `core.test` (v1) line, which
> directly contradicts §15.4 (`@benchmark` *removed entirely, not Reserved*) and
> §20.4 (benchmarking delegated to native tools). This changelog does **not**
> amend the §18.3 body (out of session scope); it records the divergence so
> Design can strike "benchmarking" from the §18.3 `core.test` line to make the
> three sections consistent.

## Reconciliation with the interpreter-only `expect` built-ins (under `bock test`)

`bock test` already exposed an `expect(x).to_equal(y)` matcher chain and an
`expect` global as **interpreter-only built-ins** (registered via
`register_test_builtins`; never lowered in codegen). The reconciliation outcome:

- **Codegen (×5): clean supersede — no conflict.** The interpreter-only
  built-ins never lowered, so on every codegen target the stdlib `core.test` is
  the sole authority. Re-expressing the fluent layer in pure Bock makes it lower
  ×5 with no built-in interference.
- **`bock test`: the stdlib module is not yet reachable — FOUND, not fixed.**
  `bock test`'s pipeline compiles only the single user file; unlike
  `check`/`run`/`build` it does **not** prepend the embedded `core.*` sources,
  has no `ModuleRegistry`, and does not seed imports — so a `@test` body cannot
  `use core.test.{...}` today (the import fails name resolution). The
  interpreter-only `expect`/`assert` built-ins fill that gap for `@test` files
  and **must not be removed** (they are the only assertion mechanism `bock test`
  has). Wiring `bock test` to load embedded core (so the stdlib `core.test`
  becomes directly importable from a `@test` body, and the interpreter-only
  built-ins can then be retired) is a follow-up compiler change, out of this
  session's scope. An integration test
  (`bock-cli/tests/stdlib_test.rs::use_core_test_under_bock_test_is_not_yet_supported`)
  locks this gap so it cannot close silently.

## FOUNDs (codegen gaps surfaced while shipping `core.test`; none are stdlib defects)

1. **Statement-form `assert` in tail position mis-lowers.** A bare `assert(cond)`
   in the tail (value) position of a `Void` function is wrapped by codegen in
   `return …`, but `assert` lowers to a *statement* (`assert cond` on Python,
   `if !cond { panic }` on Go/Rust) — `return <statement>` is invalid syntax.
   Worked around in-module by calling `assert(...)` then `return` explicitly.
2. **Generic trait dispatch over primitives does not lower on the static
   targets.** `a.eq(b)` / `a == b` / `"${a}"` / `a.to_string()` over a
   bare/bounded type variable monomorphised to a primitive fails on Rust/Go/TS
   (and at run time on the erased dynamic targets) — the primitive bridge fires
   only for a *concrete* primitive receiver. This bounds `assert_eq`'s reach to
   user `Equatable` types.
3. **A non-`Copy` field passed by value out of `&self` is not auto-cloned.**
   Codegen clones a *returned* field (`return self.value`) but not a field passed
   as a *call argument* (`f(self.value)`), so a fluent matcher that forwards
   `self.value` into a free function fails Rust borrow-checking. Worked around by
   forwarding the *borrowed* comparison `self.value.eq(other)` (Rust emits
   `self.value.eq(&other)`) to `assert_true`/`assert_false`.
4. **The checker's `assert` signature omits the optional message.** Codegen and
   the interpreter accept `assert(cond, msg)`, but the type checker types `assert`
   as `(Bool) -> Void`, so a two-argument call fails to type-check. `core.test`
   uses the single-argument `assert(cond)` form throughout; widening the checker
   signature would let assertions carry a failure message.
5. **Go method receivers are pointers, uncallable on a temporary.** A fluent
   chain `expect(x).to_equal(y)` calls a pointer-receiver method on the
   non-addressable result of `expect(...)`, which Go rejects. The surface directs
   Go callers to bind the expectation to a `let` first (the same addressability
   constraint affects every user-record method on a temporary).

## Rationale

`core.test` was the last and most under-determined §18.3 v1 entry (the listed
"BDD grouping, mocking, benchmarking" overshoot what the proven cross-target
surface and the v1 design support). The owner (DQ26) set a tight assertions-only
floor with a dual free/fluent API, deferring grouping and mocking (the latter to
the existing effect-handler idiom) and confirming benchmarking is OUT. This
completes the v1 `core.*` set at 11/11.

## Migration

None. Purely additive: a new `core.test` module with no prior stdlib surface.
Users opt in with `use core.test.{assert_true, assert_eq, expect, …}` from
`bock check`/`build`/`run` contexts (where embedded core loads). No symbol is
added to the §18.2 prelude, so existing programs are unaffected. The
interpreter-only `bock test` built-ins (`assert`, `expect`) are unchanged.
