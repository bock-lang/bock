# Design Decisions — DQ27, DQ28 + Open-Question Prioritization — 2026-06-05 14:45 UTC

**From:** Design chat
**To:** Orchestrator
**Re:** The two paused questions (DQ27 method/trait collision, DQ28 Go method generics) + a priority ordering for the rest of the open design queue
**Status:** DQ27/DQ28 decided; prioritization advisory

---

## DQ27 — Inherent method vs same-named trait method (overload-less targets)

**Decision: Option (a), formalized as a single-method-namespace rule. The react-components pattern is ill-formed; the fix is to define the method once.**

### The rule

A type — `record` or `class` — has **one method namespace**. Every method that applies to the type, whether declared in an inherent `impl T { }` block, in a `class` body, or in a trait `impl Trait for T { }` block, contributes to that single namespace keyed by method name.

Three consequences:

1. **A trait requirement is satisfied by name + signature match anywhere in the type's namespace.** A method named `render` with a matching signature satisfies `Component`'s `render` requirement whether it was declared as an inherent method, in the class body, or in the `impl Component for Button` block. There is no distinction between "the inherent render" and "the trait render" — there is one `render`.

2. **Defining the same method name twice for one type is a duplicate-definition error**, regardless of which blocks the definitions appear in. An inherent `render` plus a trait-impl `render` for the same type is a coherence violation, caught at check time.

3. **A type cannot satisfy two traits that require the same method name with incompatible signatures.** On the v1 target set this is genuinely unsatisfiable (js/ts/python/go-struct have one slot per name), so the language reflects that constraint rather than papering over it with name-mangling.

### Why this, not (b) or (c)

This is the model that maps cleanly to **every** v1 target. js, ts, python, and Go structs all have a single method slot per name; the Rust model (distinct `Button::render` inherent vs `<Button as Component>::render`, inherent-wins-in-call-syntax) is the *only* target that can represent two same-named methods, and representing it everywhere else requires name-mangling — option (b) — which produces non-idiomatic output (`Button.prototype.__Component_render`) and fights the idiomatic-output principle that is the whole point of per-target codegen. Rejected.

Option (c) (forbid same-name inherent+trait at check time) is the *enforcement half* of (a) without the *auto-satisfy half*. On its own it would force users to never name an inherent method the same as a trait method even when they want that method to satisfy the trait — strictly worse than (a), which says "name it once and that one definition satisfies the trait." (a) subsumes the useful part of (c).

The single-method-namespace model is also already what §6.4's class syntax implies: `class Button : Component { fn render(self) -> View { ... } }` declares the implemented traits in the header and provides the method in the body — no separate `impl Component for Button` block. The class-body `render` satisfies `Component.render` directly. The react-components example added a *redundant* `impl Component for Button { fn render = self.render() }` on top of an inherent/body `render`, which is the duplicate that collides. Under this rule that second definition is the error.

### The example fix

react-components currently has both an inherent `render` and `impl Component for Button { fn render = self.render() }`. The well-formed form defines `render` exactly once: either as the class-body / inherent method (which satisfies `Component`), or inside `impl Component for Button` with the real body — not both. The self-referential forwarder (`fn render = self.render()`) disappears entirely, which is what already unblocked python/rust/go. js/ts then have a single `render` and no recursion.

### Interpreter overflow (Q-interp-method-collision)

Fix regardless, and the fix falls out of the rule: once duplicate method definitions are a check-time coherence error, the ill-formed program is rejected before execution, so the interpreter never reaches the recursive forwarder for a *well-formed* program. The interpreter's method-resolution robustness is still worth hardening as defense-in-depth, but the user-visible overflow becomes unreachable for programs that pass `bock check`.

### Spec reconciliation

§6.4 (classes), §6.5 (traits), §6.7 (impl blocks) are currently **silent** on the inherent/trait same-name case. Add a normative statement of the single-method-namespace rule — a type has one method namespace across inherent and trait-impl blocks; trait requirements are satisfied by name+signature match; duplicate definitions are a coherence error. This is filling a gap in existing sections (method-resolution semantics), not new structure. Pair with a changelog. Mark impl-affecting: the checker gains the duplicate-method coherence check (js/ts codegen then needs no special handling — there is only one method to emit).

---

## DQ28 — Type parameters on methods vs Go's prohibition

**Decision: Keep the language surface. Reject option (c). The Go backend must lower method-level type parameters; free-function lowering (not whole-program monomorphization) is the recommended mechanism, but the mechanism is the Go codegen's implementation choice.**

### The design call

The genuinely design-level part of DQ28 is "do we keep method-level type parameters in the language, or restrict them because one target can't express them?" Keep them. `Box[T].map[U]` is a legitimate, useful construct that four of five targets support natively. Restricting the language surface to Go's lowest common denominator inverts the target-agnostic principle: the surface is the union of what's expressible, lowered appropriately per target — not the intersection of what every target supports natively. The grammar already permits method-level type params (§21.4 `fn_decl` carries `generic_params`, and methods are `fn_decl`s in impl blocks), so this is confirmation, not a surface change. Reject (c).

### The mechanism (Go codegen — recommendation, not a normative ruling)

I recommend **free-function lowering** over monomorphization, reversing the orchestrator's informal lean toward (a):

- `Box[T].map[U](f)` lowers to a free function `func Box_map[T, U](recv Box[T], f func(T) U) Box[U]`, with call sites `box.map(f)` rewritten to `Box_map(box, f)`. Go free functions support multiple type parameters natively, so Go's own generics carry T and U — no monomorphization needed.
- Monomorphization (option a) requires whole-program instantiation collection, per-instantiation name mangling, and produces code bloat, for no semantic benefit over free-function lowering. Go can already express the polymorphism at the free-function level; making the backend enumerate instantiations is doing work the Go compiler will do anyway.
- Free-function lowering is a local transform (declaration site + call sites), keyed `<TypeName>_<methodName>` for collision-free naming.

This is a codegen implementation choice, so the design ruling is narrow: **keep the surface; the Go backend lowers it.** If the Go codegen team has infrastructure that makes monomorphization cheaper than I'm assuming, (a) is acceptable — same observable behavior. No normative spec change; an optional non-normative note in the Go target profile (§22) documenting the lowering is fine but not required.

### Urgency

Confirmed low — one example (type-zoo), one target (Go). Doesn't block release-critical work; closing it greens type-zoo/go. Slot it after the cross-target-correctness items below.

---

## Prioritization — the rest of the open design queue

After DQ27/DQ28, here's the order I'd work the remaining OPEN questions, with reasoning. The organizing principle: **cross-target correctness first** (silent divergences that break the equivalence guarantee), then **semantics that affect real programs**, then **stdlib-surface ratification** (bless what shipped — spec hygiene), then **reversible impl-detail choices**.

### Tier A — cross-target correctness (v1 blockers; do first)

**1. DQ23 — Int/Int division semantics (§3.6) + Bool interpolation spelling.**
This is the most important open question. `17 / 5` produces `3` on Rust/Go and `3.4` on js/ts/python *today* — the same program yields different results per target, which is a direct violation of the cross-target semantic-equivalence guarantee that is Bock's core proposition. The Bool spelling sub-item (`true`/`false` vs Python's `True`/`False` in interpolation) is the same class of silent output divergence. This is a genuine contested decision (truncating-Int per Rust/Go vs always-Float per js/ts/python — both have precedent), which is why it's design's call and why it's first. Whatever is decided, the three non-conforming backends must be made to match (truncating needs operand-type info / an AIR `IntDiv` vs `FloatDiv` distinction per the DQ23 note).

**2. DQ20 — `expr?` error-propagation lowering.**
`?` is a no-op on js/ts/py/go (only Rust emits native `?`), so `let user = find_user(id)?` does **not** early-return on `Err`/`None` on four of five targets — a core language feature (§7.10) is silently broken cross-target. The *decision* is easy (yes, `?` must early-return per §7.10's stated semantics); the work is threading the enclosing function's return type to the Propagate AIR node plus an expression→early-return transform. I rank it #2 because the design ruling is nearly a rubber stamp of §7.10 — the value is unblocking the impl, and it's a correctness hole that can't ship.

### Tier B — semantics affecting real programs (do next)

**3. DQ18 — List `push`/`append` mutability semantics (§5 / §18.3).**
Touches the coherence of §5's "immutable by default, explicit `mut` to mutate" model: is `push` value-returning-functional or `mut self` void mutation? It determines per-backend mutating-collection codegen and bears directly on real-world programs (examples-hardening), where mutable list-building is common. Genuine decision with an ownership-model interaction, so it's design's call and ranks above the ratifications.

**4. DQ22 — bare `m.contains(k)` on a Map (§18.3 / checker).**
A `m.contains(k)` passes `bock check` (resolves to a fresh var) but has no codegen lowering — a check-passes-then-fails trap. Narrow blast radius (`contains_key` works ×5), but it's a real correctness hole and the decision is small (reject `contains` on Map, or alias to `contains_key`). Quick to resolve.

### Tier C — stdlib-surface ratification (batch as one pass; spec hygiene before v1 freeze)

These bless what the impl already shipped on safe defaults. Mostly additive/reversible; low engineering risk. Doing them as a single consolidated ratification keeps the spec accurate before any v1 freeze without churning the queue.

**5. One batched ratification covering DQ10, DQ11, DQ12, DQ13, DQ14, DQ15, DQ24, and DV17:**
- **DQ10** — normative (primitive × core-trait) conformance matrix. Notable real sub-decisions: `Float: Hashable` (recommend **no** — NaN breaks hash laws, follow Rust's `f64`-not-`Hash`), `Float: Equatable` (recommend **yes**, with IEEE NaN≠NaN semantics documented), `Bool: Comparable` (recommend **yes**, `false < true` is well-defined and harmless). Ratify the bridge's shipped matrix with these refinements.
- **DQ11** — core.convert conversions (the shipped `From`/`TryFrom` set, seal scope, fixed `ConvertError`, `TryInto` omitted). Ratify as shipped.
- **DQ12 + DQ24** — core.iter protocol shape + shipped surface refinements (generic `Iterator[T]`/`Iterable[T]`, the 6-combinator floor, inherent-`next` satisfying `Iterator`, §6.5's associated-type example now misleading). Ratify the shipped floor; clarify §6.5's example as illustrative (and note it interacts with the DQ27 single-namespace rule).
- **DQ13** — §18.2 prelude membership of `TryFrom` + `Error`. Recommend **amend §18.2 to include them** (consistent with the DQ9 ruling that added Ordering/Less/Equal/Greater — both are core-defined and fundamental; the impl already preludes them).
- **DQ14 + DQ15** — core.iter `iter()` return-type limit and concrete-vs-generic-bound combinator dispatch. Ratify the shipped concrete floor; record the existential/associated-type return as a v1.x expressiveness item.
- **DV17** — §18.3 still lists "benchmarking" for core.test, but §15.4 removed `@benchmark` and §20.4 delegates benchmarking to native tools. Correct §18.3's core.test line to drop "benchmarking" (and reconcile "BDD grouping, mocking" to the Reserved-v1.x dispositions from DQ26).

### Tier D — reversible impl-detail choices (lowest; can ratify lazily or defer to v1.x)

**6. DQ17 — canonical Optional codegen representation: normative or per-backend?** Recommend leaving it **non-normative** (per-backend free choice, mirroring the JS value representation) unless a concrete interop need surfaces — pinning a cross-target representation normatively constrains backends for no v1 benefit. Quick to close.

**7. DQ21 — `has_body: bool` flag vs the empty-block heuristic in AIR.** Pure impl-detail (bock-air). The heuristic is exact for the current lowerer; the flag is the unambiguous follow-up. Recommend the flag as a low-priority hygiene item, but it carries no language-semantics decision — arguably this shouldn't be a Design question at all; it can move to the impl backlog.

### Separate track — non-core

**DQ1 — `bock check` default strictness (§20.1, non-normative CLI shape).** Per the core-spec rule this is the orchestrator's to iterate with the operator directly, not a Design ruling. Keep it off the Design queue.

---

## Suggested working order, condensed

```
(now)  DQ27, DQ28          ← decided above
  1.   DQ23                ← Int/Int division + Bool spelling (cross-target correctness)
  2.   DQ20                ← expr? propagation (core feature broken 4/5)
  3.   DQ18                ← List mutability (§5 coherence + real programs)
  4.   DQ22                ← m.contains trap (quick)
  5.   DQ10/11/12/13/14/15/24/DV17  ← one batched stdlib-surface ratification
  6.   DQ17                ← Optional repr (recommend non-normative; quick)
  7.   DQ21               ← has_body flag (recommend → impl backlog, off Design queue)
 (sep) DQ1                ← non-core CLI; orchestrator + operator
```

The leverage argument for the ordering: DQ23 and DQ20 are the two places where Bock currently produces *different observable behavior across targets* for the same source — they erode the central guarantee and almost certainly account for some of the examples-hardening reds beyond the two we just decided. They come before the ratifications, which are spec-accuracy work that blocks nothing. DQ27/DQ28 (just decided) plus DQ23/DQ20 are the four with the highest impact on getting examples-hardening from 84/100 toward green.

One caveat on DQ23: if the decision is truncating-Int, confirm with the impl whether the AIR already carries enough operand-type information at the division site to distinguish Int/Int from Float/Float, or whether that's a checker-annotation prerequisite (parallel to the `recv_kind` annotation from #137). If it's a prerequisite, DQ23's reconciliation has an impl dependency that should be scoped before the backends are touched.
