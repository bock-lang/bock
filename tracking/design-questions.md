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

> **★ DESIGN BOARD CLEAR (2026-06-06) — except DQ1 (non-core CLI).** Every core-spec design question is decided:
> **DQ27/DQ28** (method namespace / go method generics), **DQ23 + DQ20** (Int division / `?` propagation — cross-target
> correctness), **todo()/guessing-game**, **DQ18** (List `push`/`append` = `mut self` Void mutators; #269) + **DQ22** (reject
> Map `contains`; #269), the **stdlib-surface ratification batch DQ10/DQ11/DQ12/DQ13/DQ14/DQ15/DQ24 + DV17** (changelog
> `20260606-stdlib-surface-ratification`), **DQ17 CLOSED** (Optional repr left non-normative), and **DQ21 → impl backlog** (no
> language decision). **DQ1** (`bock check` default strictness) stays the non-core CLI track (orchestrator + operator). Remaining
> work is implementation + the v1.x deferrals recorded in the entries below, not open decisions.
>
> **★ 2026-06-10 — DQ29 DECIDED + IMPLEMENTED** (Design ruling 02:08 UTC → #347 same day; entry below). **DQ30** (List
> mutator return contracts, §18.3-silent) remains the open ruling — Design's note says it is next. **★ NEW 2026-06-10 —
> DQ31** (container element-equality semantics under an explicit `impl Equatable` — rule-3 corner surfaced by #347's
> cross-target pinning; entry below, escalated). DQ10/DQ11 remain ratification-pending. The compiler v1 backlog is
> Design-gated on DQ30 only.

### DQ31 — container `==` element semantics when elements carry an explicit `impl Equatable`

- **Question:** DQ29's rule 3 makes `List[T]`/`Map[K,V]`/`Set[T]` Equatable iff their parameters are, but does not pin
  WHICH equality the container comparison uses when `T` has an explicit (custom) `impl Equatable`: the impl's `eq` or
  the structural default? Targets disagree today: js/ts honor the element `eq`; rust (container `PartialEq`) and go
  (`reflect.DeepEqual`) compare elements structurally — a custom case-insensitive-key record inside a `List` compares
  differently per target.
- **§:** §18.5 (DQ29 paragraph) · **context:** FOUND by #347's cross-target fixture pinning (the explicit-impl-override
  fixture passes ×5 only at top level; the in-container case is the divergent corner — deliberately not pinned). The
  consistent reading of DQ29 rule 6 (explicit impl wins) suggests element `eq` should be honored inside containers too,
  but rust/go would then need per-element loops instead of native equality — a codegen-cost decision Design should weigh.
- **Status:** escalated → Design (escalations.md, 2026-06-10)

### DQ29 — does structural record/enum equality satisfy `Equatable` for `==`/`!=` operator-gating? — DECIDED (Design, 2026-06-10)

**RULING (Design chat, 2026-06-10 02:08 UTC): R1 with a conditional structural rule.** Records/enums conform to
`Equatable` structurally iff ALL field/payload types conform (recursive; compound built-ins and generic instantiations
compose conditionally; non-Equatable leaves poison with a named-field diagnostic). The conformance is a
compiler-provided default, suppressed by an explicit `impl Equatable` (skip-if-occupied, the #110 blanket precedence).
With it, `==`/`!=` gate behind Equatable exactly as `<`/`>`/`<=`/`>=` gate behind Comparable (#296). **Classes are
excluded** (data/identity line — explicit impl only). Float fields compose with the DQ10 IEEE caveat. The asymmetry is
deliberate and normative: NO structural conformance for Comparable (no canonical structural order) nor Hashable
(v1.x derive-era, paired with `@derive`). Rationale highlights: fixes `T: Equatable` bounds rejecting types whose `==`
works (the #106 bridge bug class); closes the latent cross-target divergence on non-Equatable-leaf `==`; lands §18.5
uniformity pre-1.0 instead of as a v1.x breaking change. **IMPLEMENTED same day: #347** (structural-Equatable witness
predicate, E4015, codegen pinning + divergence fixes ×5, §18.5 normative paragraph + changelog
`20260610-dq29-structural-equatable.md`). Closed Q-equatable-gating-user-types, Q-js-user-equality-reference.
Follow-on: DQ31 (container element-eq corner), DV24 (interp NaN total-order).

### DQ29 (original question, for the record)
- **Question:** §18.5's rule is "implementing the trait gates the operator." It landed for `Comparable` →
  `<`/`>`/`<=`/`>=` on user types (#296 checker gate + #299 codegen). Should `==`/`!=` likewise be gated behind
  `Equatable` for user types? The blocker: records/enums get **free structural `==`** at the codegen level (e.g.
  Python `@dataclass.__eq__`) but have **NO checker-visible `Equatable` conformance** — only primitives are
  registered (`traits.rs register_canonical_conformances`); there is no structural auto-derive, and `@derive` is
  **v1.x-reserved**. So a strict `require_equatable_operand` gate (mirroring #296) would **reject idiomatic
  `record == record`** with no v1 escape hatch.
- **§:** §18.5 · **context:** #296 deferred `==`/`!=` gating for exactly this reason; the wave-6 investigation
  (**PR #300**, doc-only, not merged) confirmed scenario (B) empirically (record/enum `==` type-checks + runs today
  with no `impl Equatable`). Candidate resolutions: **(R1)** structurally auto-conform records/enums to `Equatable`
  then gate; **(R2)** defer `==`/`!=` gating to the v1.x `@derive` era; **(R3)** strict gate requiring explicit
  `impl Equatable` — **rejected** (breaks idiomatic record equality, no v1 escape). Impl is ready to wire (same
  `infer_binop` mechanism as #296) the moment Design rules. Unblocks queue item Q-equatable-gating-user-types.
- **Status:** DECIDED — see the ruling block above (R1 conditional structural rule; implemented #347).

### DQ30 — return-contract for the in-place `List` mutators `pop`/`insert`/`remove`/`reverse`
- **Question:** DQ18 ruled `push`/`append` are `mut self` → `Void` in-place mutators (§18.3, changelog
  `20260606-list-mutation-map-contains`). The four remaining in-place mutators were left value-returning
  (checker.rs:4607-4620 still type all four as the placeholder receiver `List[T]`), and §18.3 is **silent** on their
  return contract. Applying the `mut self` model needs that contract decided first, and the contested axes are a Design
  call: (a) `remove(index)` by-index return — `Optional[T]` (None on out-of-bounds) vs `T` (abort on OOB) vs `Void`;
  (b) out-of-bounds behavior for `insert(index, value)` / `remove(index)` — abort vs Optional-safe; (c) `pop()` on an
  empty list — `Optional[T]` None (recommended, matches Bock's Optional-everywhere ethos and the existing
  `get`/`index_of` Optional returns) vs abort. `reverse() -> Void` is unambiguous.
- **§:** §18.3 · **context:** FOUND 2026-06-06 (#269, the DQ18 follow-up); queue item Q-list-mut-pop-insert-remove.
  Candidate resolutions: **(A) Optional-safe** — `pop()→Optional[T]`, `remove(i)→Optional[T]` (None on OOB),
  `insert(i,v)→Void` (abort on OOB), `reverse()→Void`, all `mut self` (symmetric with `get`/`index_of`; no surprise
  panics on `remove`); **(B) Rust-style panic** — as (A) but `remove(i)→T` aborting on OOB (an OOB index is a
  programmer error; matches `Vec::remove`); **(C) reverse-only now, defer the rest** — land `reverse()→Void` (fully
  unambiguous) and defer `pop`/`insert`/`remove` to v1.x. Recommendation: **(A)**. Once ruled, the codegen is a direct
  extension of the DQ18 per-target mut-self lowering table ×5; impl mechanism is the same checker stamp + per-backend
  arm. Unblocks queue item Q-list-mut-pop-insert-remove. Surfaced to owner 2026-06-09; owner deferred ("will circle back
  with the design decision").
- **Status:** escalated → Design (escalations.md)

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

### DQ17 — canonical Optional codegen representation (normative?) — CLOSED (non-normative, 2026-06-06)
- **Question:** is the cross-target `Optional`/`Some`/`None` codegen representation normative, or
  a free per-backend choice? #124/#126 shipped a tagged representation (`BockOption<T>` TS,
  `__bockOption` Go, `_BockSome`/`_BockNone` Python, tagged object JS) on the defensible "mirror
  JS value representation" default; the spec doesn't pin it. Low priority / reversible.
- **§:** §18 / codegen · **context:** surfaced by #126 (Python repr OPEN→Design). Non-blocking.
- **Status:** escalated → Design (escalations.md)

### DQ18 — List `push`/`append` mutability semantics — DECIDED + DONE (#269)
- **Question:** the checker models `push`/`append` → `List[T]` (value-returning; checker.rs:~2563), which
  conflicts with §5's "immutable by default, explicit `mut` to mutate" model. Decide: (a) value-returning
  functional `push` (clean for GC targets + Go `append`; O(n) Rust clone), or (b) `mut self` void mutation
  (clean Rust `Vec::push` on a `let mut`; needs a mutable receiver; requires changing the checker's return
  type). Determines per-backend mutating-List codegen.
- **§:** §5 / §18.3 · **context:** surfaced by #129 (read-only List methods landed; mutating deferred).
  Non-blocking — core.iter builds result lists via `concat`, not `push`. → Q-codegen-completeness P4.
- **Status:** escalated → Design (escalations.md)

### DQ19 — single-file bundling vs §20.6.1 one-file-per-module output
- **Question:** Phase 0 (#132) emits a cross-module program as a single bundled `main.<ext>` (concatenating the
  `use`-reachable modules), diverging from spec §20.6.1's one-file-per-module build output. Is single-file
  bundling the v1 execution model (per-module tree → a future "library build" mode), or should §20.6.1 be
  preserved (requiring the run model + harness to load a multi-file tree per target)?
- **§:** §20.6.1 · **context:** surfaced by #132 (a non-normative §20.6.1 note + changelog were added). The
  single-file run model (the conformance harness + toolchain run plans run one `main.<ext>`) made bundling the
  pragmatic path. Non-blocking — bundling works on all 5.
- **Status:** **DECIDED 2026-06-02 (owner): per-module tree is the v1 output model** (both application + library
  builds) — NOT bundling. This re-opens DV13 (native per-target cross-file imports must compile+run) and is realized
  by the ItemB milestone (`plans/2026-06-02-itemB-per-module-projectmode-plan.md`, S1–S4). Spec reconciled: §20.6.1
  note rewritten (per-module tree normative; bundling retired as default), changelog
  `20260602-1608-per-module-output-dq19.md`. Bundling stays behind a flag until all 5 run natively, then removed
  (DV13 CLOSED at S4).

### DQ20 — `expr?` (error-propagation operator) lowering — DECIDED (done-by-impl)
- **Question:** the `?` operator (Propagate) is a no-op on js/ts/py/go (Rust emits native `?`). Correct lowering
  must early-return on `Err`/`None`, which needs the enclosing function's return type at the `Propagate` site
  (Err-vs-None) — not currently on the AIR node. Add a checker annotation (like `recv_kind`, #137) for the
  return-type context + an expression→early-return transform, or restrict `?` to certain positions?
- **§:** §error-handling / codegen · **context:** surfaced by #138 (P1-c); deferred from P1 →
  Q-codegen-completeness P4. Non-blocking (no regression; Rust works).
- **Decision (resolved by implementation 2026-06-04):** the obvious §7.10 semantics (unwrap-or-early-return) were
  implemented directly — `Q-propagate-operator-noop` lowered `?` to early-return on js/ts/py/go (#226–#229). No Design
  escalation was needed. Residual: only the LOW `Q-propagate-exprpos-shared` (a nested `?` inside a larger expression like
  `f(g()?)`); no v1 example hits it.
- **Status:** decided (done-by-impl) → closed; reconciled 2026-06-05. NOT a pending Design item.

### DQ21 — distinguishing default vs required trait methods in AIR — → IMPL BACKLOG (no language decision, 2026-06-06)
- **Question:** trait default methods (#140) are detected by an empty-block heuristic (a bodyless/required trait
  method lowers to an empty AIR `Block`; a default has a non-empty body). This misclassifies a genuinely empty
  default body. Add a robust `has_body: bool` flag to the AIR `FnDecl` (a `bock-air` change), or keep the heuristic?
- **§:** codegen / bock-air · **context:** surfaced by #140 (P2-α default-method synthesis). Low priority — the
  heuristic is exact for the current lowerer; the flag is the unambiguous follow-up. Non-blocking.
- **Status:** escalated → Design (escalations.md)

### DQ22 — bare `m.contains(k)` on a Map: reject or alias to `contains_key`? — DECIDED + DONE (#269, reject + suggest)
- **Question:** the checker resolves Map membership as `contains_key`; a bare `m.contains(k)` resolves to a fresh
  var (not a real Map method) → passes `bock check` but has no codegen lowering. Should the checker reject
  `m.contains` on a Map (only `contains_key` valid), or alias `contains`→`contains_key`?
- **§:** §18.3 (collections) / checker · **context:** FOUND #145 (P3-β). Low priority — `contains_key` works
  end-to-end ×5; this is only the spelling of the membership method. Non-blocking.
- **Status:** escalated → Design (escalations.md)

### DQ23 — normative Int/Int division semantics (§3.6) [+ Bool string-conversion spelling] — DECIDED
- **Question:** §3.6 lists `/` as arithmetic but never pins Int/Int result semantics. Codegen diverges:
  Rust/Go truncate (`17/5`→`3`), js/ts/python true-divide (`3.4`). Decide the normative v1 semantic —
  truncating-Int (Rust/Go) or always-Float? Once decided, js/ts/py codegen must match (truncating needs
  `Math.trunc(a/b)` / `a//b` when BOTH operands are Int — requires operand type info / an AIR `IntDiv` vs
  `FloatDiv` distinction). **Bundled small clarification:** string-conversion/interpolation of a Bool should use
  the canonical literal spelling `true`/`false` (harmonize Python's `True`/`False`).
- **§:** §3.6 (+ §3.x Bool) · **context:** surfaced by the P4 design (item 5). Core-spec semantics. Non-blocking
  for R1 (no Int/Int division or Bool-interp in the iter/effect floor); minor R2 `string` output-equality interaction.
- **Feasibility (orchestrator read-only probe 2026-06-05; scopes queue `Q-int-div-semantics`):** operand type is NOT available
  at the codegen `/` site (the checker's type side-table is dropped before codegen; `type_info.resolved_type` is always
  `None`), so a **checker annotation is a prerequisite for either option** — but cheap: it mirrors the `list_concat`/
  `string_concat` stamps the checker already applies to `BinaryOp` nodes at the same site (≈1 bit: "this `Div` has two Int
  operands"). **Option A (truncating-Int):** 3 codegen arms (js/ts `Math.trunc(a/b)`, py `math.trunc` — toward-zero, not `//`
  floor) + 1 stamp; result type stays `Int`; small/safe. **Option B (always-Float):** changes `infer_binop` Div result→`Float`,
  rippling through inference (breaks `let n: Int = a/b`, shifts `.expected` broadly, risks rs/go generated-code compile
  failures); large/risky. **Orchestrator recommendation: Option A.**
- **Decision (Design 2026-06-06):** **Option A — truncating integer division, toward zero.** `Int/Int -> Int` truncating
  toward zero (`17/5==3`, `-17/5==-3`); all sized ints divide the same; `Float/Float` unchanged (IEEE); mixed Int/Float is a
  §4.2 type error (no implicit coercion); `%` is the truncated remainder taking the **dividend's** sign (`-17%5==-2`); integer
  div/mod by zero is a Panic abort (§10.5), equivalent ×5. Bool interpolation/`to_string` is canonical lowercase `true`/`false`.
  Basis (not just cost): §4.2 forbids implicit numeric coercion (always-Float would weld an Int→Float coercion into `/`); the
  other arithmetic ops are type-preserving; `%` presupposes integer division (`(a/b)*b + a%b == a`).
- **Status:** decided → Design 2026-06-06; reconciled #264 (Q-int-div-semantics DONE — checker `int_arith` + `bool_stringify`
  stamps; js/ts/py division+modulo arms with toward-zero truncation, dividend-sign modulo, and zero-divisor abort; rust/go
  already conformant; spec §3.6/§3.5 + changelog `20260606-int-div-semantics.md`). Acceptance fixtures (negative operands,
  zero-divisor abort, large-int precision, Bool spelling) green ×5.

### DQ24 — `core.iter` shipped surface refinements (combinator set + protocol shape)
- **Question:** `core.iter` shipped (#151/#152) on a minimum-useful floor; three surface choices refine DQ12 and
  want Design ratification (all additive/reversible in v1-dev): (1) **the combinator set** — shipped 6 eager,
  `List`-returning, read-only combinators (`to_list`, `count`, `fold`, `map`, `filter`, `take`); `enumerate` was
  omitted and any mutating/`zip`/`flat_map`/lazy combinator is out of the floor. Is this set the normative v1
  surface? (2) **`Iterator` trait impl** — the concrete `ListIterator` ships an *inherent* `next` only; the
  `impl Iterator[T] for ListIterator` trait impl was dropped (it caused a Go duplicate-`Next`, and `Iterable`
  detection keys on `Iterable`, not `Iterator`). Is a value-type satisfying `Iterator` via an inherent method
  (not a trait impl) acceptable, or must the trait impl exist? (3) **§6.5 vs §18.3** — §6.5's example trait
  `Collection { type Item; fn iter(self) -> Iterator[Item = Self.Item] }` uses associated types, which are inert;
  the shipped `core.iter` uses generic `Iterator[T]`/`Iterable[T]` (per DQ12). §6.5's example reads as misleading
  for the stdlib — clarify/annotate, or leave as a generic trait-syntax illustration?
- **§:** §18.3 / §18.5 / §6.5 / §18.2 · **context:** surfaced by core.iter R1 (#151/#152). Non-blocking — the
  floor is shipped and exercised ×5; refining any of the three is additive.
- **Status:** escalated → Design (escalations.md)

### DQ25 — `core.effect` v1 surface (8 sub-questions — the floor is UNDER-SPECIFIED)
- **Question:** §18.3:1728 says only "`core.effect` (v1) — Effect system primitives" with no §18.3.x
  subsection (contrast §18.3.1 core.time). The effect MACHINERY (§10) is fully implemented + resolve-layer
  cross-module-wired, but the v1 module SURFACE is undefined. Sub-questions (recs are Design's to ratify):
  **(1)** primitives-only (vocabulary + one worked handler pattern) vs a library of concrete effects? *rec
  primitives-only.* **(2)** include a standard `Log` effect (`fn log(message: String) -> Void`) as the
  executable example — **conditioned on the feasibility probe passing ×5**? *rec yes-iff-feasible* — THE
  most consequential (decides if the floor has a runnable effect). **(3)** do ambient `Panic`/`Allocate`
  (§10.5, "always available without declaration") need any module surface? *rec no.* **(4)** confirm
  `core.effect` owns neither `Clock` (§18.3.1 — core.time owns it) nor `Cancel` (§13.5, partly Reserved).
  *rec out.* **(5)** effect-handler utility traits/types? *rec none in v1.* **(6)** composite effects
  (§10.1)? *rec not in the floor.* **(7)** Reserved-for-v1.x to restate in docs: adaptive handlers (§10.8),
  lambda handler constructors (§10.4), Layer-3 defaults (§10.3). **(8)** acceptance bar (§18.3:1716 = a
  representative example compile+run ×5): what runs for a primitives-only floor? *rec the cross-module effect
  exec fixture — requires (2) feasible.*
- **§:** §18.3 / §10 · **context:** surfaced by the core.effect plan (`plans/2026-05-31-core-effect-r1-plan.md`).
  Non-blocking the QUEUE (the feasibility probe + scoping proceed), but the floor BUILD waits on Q1/Q2.
- **Status:** **DECIDED 2026-06-01 (owner): Q1 = primitives-only floor; Q2 = YES, ship an executable `Log` effect**
  (`fn log(message: String) -> Void` + `ConsoleLog` record handler + `console_log()` constructor) via the canonical
  §10.4 surface. Q3 (ambient effects no surface), Q4 (Clock/Cancel out), Q5 (no utility traits), Q6 (no composite),
  Q7/Q8 per recommendation. Reconciled in **#157** (module + `spec/changelogs/...core-effect-v1-surface.md`); §18.3
  body unchanged (the surface realizes its "minimum-useful subset" latitude). core.effect = 5/11.

### DQ26 — `core.test` v1 surface — DECIDED (owner)
- **Question:** §18.3 lists `core.test` = "Assertions, BDD grouping, mocking, benchmarking"; which is the v1 floor?
  And the assertion API SHAPE — free-function (`assert_eq`) vs fluent (`expect().to_equal()`)?
- **Status:** **DECIDED 2026-06-01 (owner): ship BOTH** the free-function assertions AND a fluent matcher API, fully
  overlapping, with the fluent layer **powered by** the free functions (minimal duplication). Floor = assertions only
  (`assert_true/false/eq/ne/some/none/ok/err/fail` + `Expectation`/`BoolExpectation`). **BDD grouping** Reserved-v1.x
  (needs a `bock test` runner registration-execution model); **mocking** Reserved-v1.x (the effect-handler-substitution
  pattern is the v1 idiom — no new API); **benchmarking OUT** (§15.4 already REMOVED `@benchmark`; §20.4 delegates to
  native tools). Reconciled in **#169** (module + `spec/changelogs/20260601-1350-core-test-v1-surface.md`). §18.3 body
  unchanged. Residual → DV17.

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

---

## DQ27 — Inherent method vs same-named trait method resolution (overload-less targets) — DECIDED

**Raised:** 2026-06-05 (examples-greening, Q-class-codegen / react-components).
**Question:** When a `class` has an inherent method and a trait requires a method of the same name, how do they resolve —
especially on js/ts/python where a type has ONE namespace per name? react-components writes `impl Button { fn render … }`
+ `impl Component for Button { fn render = self.render() }`; with both bound to one name the trait `render` overwrites the
inherent and `self.render()` recurses infinitely (the reference interpreter also stack-overflows).
**Options:** (a) an inherent method auto-satisfies a same-signature trait requirement — the explicit delegating impl is
redundant or a checker error [orchestrator's recommendation; matches current rust/python/go behavior]; (b) name-mangle
trait methods distinctly from inherent on overload-less targets; (c) forbid same-name inherent+trait at check time.
**Decision (Design 2026-06-05):** option (a), formalized as the **single-method-namespace rule** — a type (record/class) has
ONE method namespace across inherent `impl`, class body, and trait-impl blocks; a trait requirement is satisfied by a
name+signature match anywhere in it; defining the same method name+signature twice for one type is a coherence error
(**E4012**). react-components' delegating `impl Component for Button { fn render = self.render() }` was the duplicate — fixed
to an empty `impl Component for Button {}` (the inherent `render` satisfies the trait), so js/ts no longer recurse. Rejected
(b) name-mangling (non-idiomatic output) and (c) forbid-only (subsumed by (a)). Q-interp-method-collision becomes unreachable
for check-passing programs (the duplicate is rejected before execution).
**Status:** decided → Design 2026-06-05; reconciled #258 (checker E4012 + react-components fix + spec §6.4/6.5/6.7 + changelog
`20260605-method-namespace.md`). Impl items Q-method-collision-inherent-trait + Q-class-codegen DONE; react-components now
runs on all 5.

## DQ28 — Type parameters on methods vs Go's prohibition — DECIDED

**Raised:** 2026-06-05 (examples-greening, type-zoo/go).
**Question:** Bock allows a method to declare its own type params (`Box[T].map[U]`). Go forbids type params on methods. How
should the Go backend lower this — monomorphize per use, lower the method to a free generic function, or restrict the
surface? (js/ts/python/rust already handle it.)
**Options:** (a) monomorphization at codegen [recommended]; (b) free-function lowering; (c) restrict the language surface.
**Decision (Design 2026-06-05):** KEEP the language surface (reject (c)); the Go backend lowers method-level type params.
Design recommends **free-function lowering** (`Box[T].map[U]` → `func Box_Map[T, U](self Box[T], f func(T) U) Box[U]`, call
sites rewritten) over monomorphization — the mechanism is the Go codegen's choice (same observable behavior). No normative
spec change; an optional non-normative note may go in the Go target profile §22.
**Status:** decided → Design 2026-06-05; reconciled #256 (Q-go-method-generics DONE — free-fn lowering in go.rs). NOTE: fully
closing type-zoo/go also needs **Q-checker-method-generic-call-infer** (the checker can't yet infer `U` for a `b.map(dbl)`
call — which is why type-zoo only *declares* `Box.map`).
