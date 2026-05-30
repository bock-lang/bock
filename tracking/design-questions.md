# Design Questions — open decisions

**The one question:** what should the behavior be? (undecided choices)

Not factual mismatches (→ `divergences.md`) and not actionable work
(→ `queue.md`). A divergence whose disposition needs a decision links
here. Migrated from the open D1-refresh / D2-FOUND rows (most are now
decided — see `## Decided`).

**Core-spec rule (see orchestrator contract — Design authority):**
questions touching *core specification* (language semantics, type/effect/
context rules, stdlib surface, the §1.3 target set) are **escalated to
Design and filed in `escalations.md`** — the orchestrator does NOT decide
them, even with the operator present; Design Chat is the authoritative
core-spec voice. The orchestrator uses discretion on what's "core spec",
files the escalation, and **moves on** (doesn't block other work).
Non-core questions (e.g. CLI shape, which §20.1 declares non-normative)
the orchestrator may iterate on with the operator directly.

Schema: `[ID] question · § · context · status(open | escalated→Design |
decided→link)`

---

## Orchestrator design questions (non-core — iterate with operator)

### DQ1 — `bock check` default strictness
- **Question:** should `bock check` default to the project's configured
  strictness instead of requiring explicit `--strict`?
- **§:** §20.1 (CLI shape — non-normative per §20.1) · **context:**
  O1/O2 landed (#87) keeping `--strict` explicit, mirroring `bock build`.
  Non-core; parked as a smaller follow-up.
- **Status:** open

## Escalated to Design (core spec — pending)

### DQ10 — normative primitive-conformance matrix
- **Question:** which (primitive × core-trait) conformances are **normative** for
  v1? §18.2/§18.5 name the traits but never pin the matrix. Specifically: is
  `Bool: Comparable` normative (Rust yes, Swift no)? May `Float` conform to
  `Equatable`/`Hashable` given `NaN != NaN` breaks their laws (Rust: `f64` is
  `PartialEq` not `Eq`/`Hash`)?
- **§:** §18.2 / §18.5 · **context:** surfaced by the Q-bridge plan
  (`plans/2026-05-30-primitive-conformance-bridge-plan.md`). The bridge implements
  a **proposed** matrix (Equatable: Int/Float/String/Bool/Char; Comparable: same
  minus Bool; Displayable: all; Hashable: all minus Float) and proceeds on it;
  Design ratifies/refines (additive, low-cost). Also flags: §18.5 operator-gating
  for *user* types is unimplemented (separate follow-up).
- **Status:** escalated → Design (escalations.md)

### DQ11 — `core.convert` design questions (4 sub-points)
- **Questions** (surfaced by `core.convert` #110; shipped the floor, escalated for
  ratification):
  1. **Normative primitive-conversion matrix** (parallels DQ10): which `From`/
     `TryFrom` conversions are normative for v1? Shipped: `Int→Float`, signed
     widening, `Float32→Float`, `Char→String`, `TryFrom[String] for Int/Float`
     (narrowing excluded).
  2. **Seal scope:** are canonical conversions sealed against user override?
     §18.5's seal is scoped to `(core trait, primitive)`; `From[Int] for Float`
     is `(core trait, primitive→primitive)`. Shipped **unsealed** (conservative).
  3. **`TryFrom` error type:** fixed `ConvertError` or generic `TryFrom[T, E]`?
     §18.3 says only "→ `Result`". Shipped **fixed `ConvertError`**.
  4. **`TryInto` in v1?** Prelude/§18.3 list `Into`/`From`/`TryFrom` but not
     `TryInto`. **Omitted** (no `TryFrom⇒TryInto` blanket).
- **§:** §18.3 / §18.5 · **context:** all four are additive/refineable; the impl
  proceeds on the floor. Reconcile §18.3 if Design ratifies/changes any.
- **Status:** escalated → Design (escalations.md)

### DQ12 — `core.iter` protocol shape
- **Questions** (surfaced by the `core.iter` plan; iter is PAUSED on Q-codegen-fixes,
  so this can ratify in parallel): the protocol shape isn't pinned in §18.3
  ("Iterator trait + combinators"). (1) **generic `Iterator[T]`/`Iterable[T]`** (the
  planned floor; associated types are end-to-end inert today) vs associated-type
  `type Item`; (2) `next()`/`iter()` signatures (`next -> Optional[T]`); (3) **lazy
  vs eager** combinators (floor = eager List-returning); (4) which combinators are
  **normative** for v1 (the changelog lists ~25; the feasible floor is far smaller);
  (5) does `for` require `Iterable` for **built-ins** too, or may List/Map/Set keep
  the native fast path (planned: native fast path for built-ins, protocol for user
  types)?
- **§:** §18.3 / §18.5 / §18.2 · **context:** ship the minimum-useful floor when iter
  resumes; Design ratifies the normative surface. Pairs with DQ10/DQ11 as stdlib-
  surface ratification.
- **Status:** escalated → Design (escalations.md)

### DQ13 — §18.2 prelude membership (`TryFrom`, `Error`)
- **Question:** §18.2's literal trait list (Comparable/Equatable/Hashable/Displayable/
  Serializable/Cloneable/Default/Into/From/Iterator/Iterable) does **not** include
  `TryFrom` or `Error`. The prelude-injection impl (#120) preludes `TryFrom` + `Error`
  too (both defined in `core.convert`/`core.error`, both fundamental). Should §18.2 be
  amended to include them, or should the impl drop them (require explicit `use`)?
- **§:** §18.2 · **context:** surfaced by #120 (the orchestrator's dispatch prompt
  over-specified them). Reversible/low-harm in v1-dev. FYI not a question: the §18.2
  traits without core definitions yet (Serializable/Cloneable/Default/Hashable/
  Iterator/Iterable) are name-level prelude only until their `core.*` ship — expected.
- **Status:** escalated → Design (escalations.md)

### DQ14 — `core.iter`: `Iterable.iter()` return type without `impl Trait`/associated types
- **Question:** the compiler has neither associated types (parsed but inert end-to-end) nor
  `-> impl Iterator`/existential returns, so the DQ12 floor pins `Iterable[T].iter(self) ->
  ListIterator[T]` (the single concrete stdlib iterator). A *user* iterable can then only return
  the stdlib `ListIterator`, not its own iterator type — a real expressiveness limit. Accept this
  v1 floor, have `iter()` return `Self`, or pull forward existential/assoc-type returns?
- **§:** §18.3 · **context:** surfaced by the `core.iter` plan; non-blocking to the floor.
- **Status:** escalated → Design (escalations.md)

### DQ15 — `core.iter`: combinator dispatch — concrete vs generic-bound
- **Question:** are v1 combinators `fn map[I: Iterator[T], U](it: I, …)` (generic-bound dispatch
  via the less-exercised `type_var_bounds` path) or `fn map[T,U](it: ListIterator[T], …)`
  (concrete)? The floor ships concrete (proven path). Ratify the normative surface.
- **§:** §18.3 · **context:** surfaced by the `core.iter` plan; non-blocking.
- **Status:** escalated → Design (escalations.md)

### DQ16 — `core.iter` R1 floor: List-backed vs List-free (**BLOCKS core.iter**)
- **Question:** the DQ12 R1 floor (a `ListIterator[T]` over `List[T]` + 6 List-returning
  combinators) is **not shippable** — List built-in methods don't codegen on any backend
  (`divergences.md` DV10, `queue.md` Q-list-codegen, a substantial workstream). Two paths:
  **(a)** keep the List-backed floor and BLOCK `core.iter` on Q-list-codegen; **(b)** redefine
  the R1 floor to a List-free iterator surface (Counter/Range-style, Int/Float + arithmetic —
  codegen-PROVEN today via `optional_match_in_loop.bock`), omitting the combinators until List
  codegen lands. The for→Iterable desugar itself is proven on all 5 targets.
- **§:** §18.3 · **context:** surfaced by core.iter v3 (2026-05-30); the decision that unblocks
  core.iter. Pairs with the Q-list-codegen scope/roadmap escalation (operator).
- **Status:** DECIDED 2026-05-30 (operator) — **keep the List-backed floor**; build the codegen prerequisite
  rather than redefine (no spec change). Decision #1 "build List codegen first" (→ #129 read-only); decision #2
  escalated to the broader **codegen-completeness milestone** (Q-codegen-completeness). core.iter resumes
  List-backed (and generic) after the milestone's P0/P1. No longer a floor question — gated by milestone progress.

### DQ17 — canonical Optional codegen representation (normative?)
- **Question:** is the cross-target `Optional`/`Some`/`None` codegen representation normative, or
  a free per-backend choice? #124/#126 shipped a tagged representation (`BockOption<T>` TS,
  `__bockOption` Go, `_BockSome`/`_BockNone` Python, tagged object JS) on the defensible "mirror
  JS value representation" default; the spec doesn't pin it. Low priority / reversible.
- **§:** §18 / codegen · **context:** surfaced by #126 (Python repr OPEN→Design). Non-blocking.
- **Status:** escalated → Design (escalations.md)

### DQ18 — List `push`/`append` mutability semantics
- **Question:** the checker models `push`/`append` → `List[T]` (value-returning; checker.rs:~2563), which
  conflicts with §5's "immutable by default, explicit `mut` to mutate" model. Decide: (a) value-returning
  functional `push` (clean for GC targets + Go `append`; O(n) Rust clone), or (b) `mut self` void mutation
  (clean Rust `Vec::push` on a `let mut`; needs a mutable receiver; requires changing the checker's return
  type). Determines per-backend mutating-List codegen.
- **§:** §5 / §18.3 · **context:** surfaced by #129 (read-only List methods landed; mutating deferred).
  Non-blocking — core.iter builds result lists via `concat`, not `push`. → Q-codegen-completeness P4.
- **Status:** escalated → Design (escalations.md)

## Decided by Design (core spec — 2026-05-30 stdlib batch; reconciled in #106)

Escalated from the stdlib pilot (DQ6–DQ9); decided by Design 2026-05-30 and
reconciled into the spec in #106 (changelog `20260530-0208-specs-changes.md`).
Q1a (the primitive-conformance bridge — DQ6's crux) lands as a separate impl PR
(`Q-bridge`).

### DQ6 — §18: normative implementation model for core modules
- **Question:** should §18 normatively state that `core.*` modules are **Bock
  source compiled with the program + per-target runtime shims** for host
  primitives, distributed **embedded in the compiler**? Today the model lives
  only in tracking-level Design notes (DQ5 / Q-stdlib); §18 doesn't state it,
  and `stdlib/CLAUDE.md`'s shim path is already wrong.
- **§:** §18.1/§18.3 · **context:** all 11 modules build against this contract;
  worth a normative statement + changelog so the model is the source of truth.
- **#104 evidence + sub-question (the crux):** `core.compare` proved stdlib
  trait impls **cannot cover primitive types** until the checker↔bock-core
  bridge exists (`impl Comparable for Int` + call site → E4001; primitive
  receivers consult only the intrinsic table — see `divergences.md` DV4,
  `queue.md` Q-bridge). Building that bridge raises a **precedence/coherence
  question Design must rule:** when a stdlib trait impl and a primitive
  intrinsic both apply to `Int`, which wins, and may user code impl core traits
  for primitives? This is the part of the impl model that gates a *useful*
  stdlib; the interim stdlib-strictness policy (#103: stdlib compiled at
  development strictness, non-error diagnostics suppressed) also wants
  ratification here.
- **Decision:** (a) compiler provides canonical primitive conformances registered
  into the trait-impl table (the bridge → `queue.md` Q-bridge); (b) **sealed** —
  user code may not impl a core trait for a primitive (orphan rule, §18.5);
  (c) the source+shims mechanism stays **non-normative** (contract is §18.1;
  `stdlib/CLAUDE.md` corrected); (d) strictness is **per-package** — a dependency's
  diagnostics never fail the consumer's strict build (§1.4). The bridge's normative
  conformance matrix → DQ10.
- **Status:** decided → Design 2026-05-30; reconciled #106 (impl: Q-bridge).

### DQ7 — canonical v1 `core.error` surface
- **Question:** does `Error` carry `cause(self) -> Optional[Error]`, and does it
  participate in §18.5 trait-language integration / `Displayable`? §18.3 says
  only "base trait."
- **§:** §18.3 · **context:** the pilot ships the minimal surface (`message`
  accessor, `SimpleError`, `error()`); Design ratifies/extends the canonical one.
- **Decision:** v1 = `message(self) -> String` **only**. `cause()`/`source`, an
  `Error: Displayable` supertrait, and context helpers depend on trait objects
  (Reserved v1.x) and ship together as a v1.x error-ergonomics bundle.
  **Supersedes** the 2026-05-29 lean that carried `source` (corrected in the
  20260529-2251 changelog). Pilot already matches — no impl change.
- **Status:** decided → Design 2026-05-30; reconciled #106.

### DQ8 — module-qualified stdlib imports for v1
- **Question:** does v1 require module-qualified `use core.error` (then
  `core.error.Error`) access, or are named imports (`use core.error.{Error}`)
  sufficient? `seed_imports` currently skips `ImportItems::Module`; supporting
  qualified access is a type-checker change affecting all 11 modules.
- **§:** §12 (imports) / §18 · **context:** the pilot relies on named imports
  (supported). Whether qualified access is a v1 requirement is a Design call.
- **Decision:** named (braced) imports are **sufficient for v1**; module-qualified
  access deferred to v1.x (with aliasing). Bare `use core.error` (no brace-list/
  wildcard) is **not** a v1 form — rejected, pointing at the braced form (→ queue
  Q-import-reject). §12.2 noted in #106.
- **Status:** decided → Design 2026-05-30; reconciled #106 (impl: Q-import-reject).

### DQ9 — prelude vs import for the fundamental traits
- **Question:** are `Comparable`/`Equatable` (and similar fundamental traits)
  **prelude** (always available without `use`, per §18.2) or **import-required**
  `core.compare` members (per §18.3)? The spec says both — an internal
  inconsistency (`divergences.md` DV5).
- **§:** §18.2 / §18.3 · **context:** surfaced by `core.compare` (#104); the impl
  matches named-import (no prelude injection; bare `Ordering` → E1001). Interacts
  with DQ6/DQ8 (the import + impl model). Reconcile §18.2/§18.3 once decided.
- **Decision:** model is "defined in core.*, **re-exported into the prelude**" —
  §18.2 and §18.3 are consistent. Implement prelude injection to match §18.2
  (→ queue Q-prelude-inject). §18.2 amended to add `Ordering`/`Less`/`Equal`/
  `Greater` (was an omission). Resolves `divergences.md` DV5.
- **Status:** decided → Design 2026-05-30; reconciled #106 (impl: Q-prelude-inject).

## Decided by Design (core spec — 2026-05-29; reconciled in #100)

These touched core specification (§11/§13/§18 language + stdlib surface),
were escalated to the Design Chat, and the Design Chat (with the operator)
decided them on 2026-05-29. The orchestrator reconciled the spec **and**
the implementation in #100 (changelog `20260529-2251-specs-changes.md`).

### DQ2 — `@performance` budget literal syntax
- **Question:** should `@performance(max_latency: 100, ...)` accept bare
  integers, or require unit-suffixed literals (`100.ms`, `50.mb`)?
- **§:** §11.4 · **context:** the context-audit example used bare ints
  → E8003.
- **Decision:** require unit-suffixed literals; bare ints stay E8003.
  Time units `.ns/.us/.ms/.s/.min/.h`; memory units `.b/.kb/.mb/.gb/.tb`
  (decimal scaling). §11.4 normative paragraph added. Reconciling this
  also exposed and fixed an impl divergence (interpreter required the
  parenthesized `100.ms()` form; now accepts the no-parens literal) — see
  `divergences.md` DV3. Closes `queue.md Q-perf-example`.
- **Status:** decided → Design 2026-05-29; reconciled #100.

### DQ3 — §13.3 channels: bounded-channel v1 status
- **Question:** are bounded channels (`Channel.new(buffer: N)`) v1, or
  Reserved for v1.x?
- **§:** §13.3 · **context:** see `divergences.md` DV2.
- **Decision:** Reserved for v1.x (bundles with `core.concurrency`, itself
  Reserved per DQ5). §13.3 leading note added; example preserved as design
  intent. Resolves DV2.
- **Status:** decided → Design 2026-05-29; reconciled #100.

### DQ4 — §13.4 synchronization primitives: v1 vs Reserved
- **Question:** which of `Mutex/RwLock/Atomic/WaitGroup/OnceCell` ship
  in v1 vs Reserved for v1.x?
- **§:** §13.4 · **context:** same unapplied-0449-claim as DQ3 (DV2).
- **Decision:** all Reserved for v1.x (bundle with `core.concurrency`).
  §13.4 leading note added; enumeration preserved as design intent.
  Resolves DV2.
- **Status:** decided → Design 2026-05-29; reconciled #100.

### DQ5 — §18.3 core-module scope for v1
- **Question:** which of the ~15 §18.3 `core.*` modules are in the v1
  stdlib, and at what surface? (Q-stdlib is decided v1-blocking; this is
  its SCOPE.)
- **§:** §18.3 · **context:** seeds `queue.md Q-stdlib` phase planning.
- **Decision:** **11 v1 modules** — `core.option`, `result`, `collections`,
  `string`, `iter`, `compare`, `convert`, `error`, `effect`, `time`,
  `test` — each at its **minimum-useful subset**; **4 Reserved for v1.x** —
  `core.types`, `math`, `memory`, `concurrency`. §18.3 reframed with the
  tiering + acceptance criterion (conformance + a representative example
  compile/run on every shipping target). Scopes `queue.md Q-stdlib` into
  three rounds: R1 effect/error/compare/convert/iter · R2
  option/result/string/time · R3 collections/test.
- **Status:** decided → Design 2026-05-29; reconciled #100.

---

## Decided (D1-refresh / D2-FOUND — resolved by the spec-revision cycle)

22 of the 25 routed rows are resolved (links in `divergences.md`
"Resolved"): §13.5, §14.1/2, §16.3/4, §20.3, §20.5, §10.3/4/6, §18.5,
§19.7, §20.6, §1.3, §15, §17.6, §12.2 (×2), §4.7, §7.6, §6.1, §11
module-level annotations. Evidence: the per-section changelogs + the
20260514-0548 spec-revision artifact (confirmed applied to the live
spec). Only DQ1 (non-core) remains open; DQ2–DQ5 were decided by Design
2026-05-29 and reconciled in #100 (see "Decided by Design" above).
