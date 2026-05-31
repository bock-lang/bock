# Phase 2 — Codegen-Completeness Milestone: Traits + Match Completeness

Plan agent (read-only) @ main a3ae42f. Gates the stdlib's trait-using modules (core.compare/convert) running
cross-module on the typed targets. Anchors re-verified at a3ae42f (re-grep before editing — lines shift per PR).

## Ground truth
- #137 `recv_kind` mechanism: checker stamps RECV_KIND_META_KEY (checker.rs:88) via recv_kind_tag (:115) at
  checker.rs:1583-1587 (desugared Call(FieldAccess)) + :1668 (MethodCall). **recv_kind_tag returns None for
  Type::TypeVar → a bounded type-param receiver gets NO annotation today** (item 4 must extend this). Codegen
  reads via generator::primitive_recv_kind/container_recv_kind (:845/:927).
- AIR has full pattern richness (node.rs:597-639: Wildcard/Bind/Literal/Constructor/Record/Tuple/Or/Guard/Range;
  MatchArm.guard :523). The gap is purely codegen LOWERING.
- Default-method bodies survive to AIR (lower.rs:158); a bodyless/required trait method collapses to an EMPTY
  Block → codegen distinguishes default-vs-required by empty-block check (heuristic; DQ1: AIR has_body flag).
- Checker ALREADY resolves bounded dispatch (checker.rs:1468-1494) + core.compare type-checks clean
  cross-module (bock check exit 0). So items 3/4 are CODEGEN problems for typed targets (not checker), except
  the narrow Self-subst (item 3).

## Sequence (all bock-codegen → sequential; item 3 is bock-types)
**P2-α — trait codegen (items 2+1+4 COMBINED), one session. Spans bock-types(recv_kind) + bock-codegen. GATES `use core.*`.**
Internal order: (2) TS trait-self → (1) default methods (builds the registry) → (4) bounded dispatch.
- Item 2 — TS trait-decl `self` untyped (ts.rs TraitDecl :1285 uses collect_typed_params :1709 → bare self →
  tsc 'implicitly any'). Fix: type `self` as the trait's interface type (trait name + generics), like
  collect_impl_typed_params (:1751/:1766). Mind `other: Self` → trait-self type.
- Item 1 — trait default methods broken js/ts/go (default bodies not attached to implementor types: js.rs
  ImplBlock :976 attaches only impl's own methods; go.rs :2122-2145 same; ts.rs :1414). Rust native (verify);
  py verify. Fix: a **trait registry** in generator.rs (collect_trait_decls → name→{default methods, all method
  names}, mirroring the #133 enum registry / #136 collect_generic_decls). For an ImplBlock w/ trait_path,
  synthesize each non-overridden default method onto the target (js/ts prototype, go func). Default-vs-required
  = non-empty AIR body (DQ1).
- Item 4 — generic-bounded dispatch `max[T: Comparable]{a.compare(b)}` broken ts/py/go (js/rust work). Root:
  recv_kind_tag(TypeVar)→None, codegen gets no hint. **Extend #137:** stamp `"TraitBound:<Trait>"` at the
  bounded-TypeVar resolution path (checker.rs:1471-1484, where type_var_bounds is in scope). Codegen (ts: `<T
  extends Comparable>` + constraint; go: `[T Comparable]` + method casing `a.Compare(b)` + Self→T in the
  interface sig; py: plain `a.compare(b)`, verify exact failure). STOP if the new tag ripples into export
  ABI/serialization (hand a diagnosis). Overlaps Q-ts-generic-impl.
- **Payoff: P2-α (items 4+2, +1 for core.convert's From⇒Into) makes `use core.compare.{Ordering,key,max}` run
  on all 5.** key/Ordering already work (#133); max needs item 4; TS needs item 2.

**P2-β — match completeness (item 5), separate session AFTER P2-α (same crate).**
js/ts/go value-`switch` structurally CAN'T express guards-with-fallthrough / or-patterns / nested / tuple.
Fix: a shared **if/else-if-chain lowering** w/ recursive pattern-test + bind (`if (<test> && <guard?>) {<bind>;
<body>} else if …`), kept ADDITIVE behind a `match_needs_ifchain(arms)` predicate (any guard/OrPat/nested
ConstructorPat/TuplePat → if-chain; else keep the working switch — don't regress Optional/enum matches). Reuse
the __matchN scrutinee temp (single eval). Go: trailing `else{panic}` for non-total. **Go binding-drop quick
fix** (go.rs:3987 BindPat→default discards name → `x := <scrutinee>; _ = x`, mirror js.rs:2239) — independent,
land here. Python: native match OK except nested ConstructorPat fields flatten to `_` (py.rs:3193/3214 — add
recursion) + bare-binding-arm mis-order (3.10 SyntaxError — reorder/guard). Rust native (verify green).

**P2-γ — Self-subst (item 3, Q-self-subst), bock-types/checker.rs.** `fn double(self)->Self` / `other: Self`
in an IMPL block → E4001 (checker substitutes Self→concrete only for trait-declared method types, not the
impl method's own sig). Fix: apply substitute_type_params(Self→target) at impl-method registration. NOT on the
`use core.*` critical path (core.compare writes concrete operand types — the workaround); removes the workaround.
**Run P2-γ IN PARALLEL with P2-β** (disjoint crates: β bock-codegen, γ bock-types) — the one safe parallelization.
NOT concurrent with P2-α (both edit checker.rs).

**Item 6 (Go/TS expr-position edge cases, #137 FOUND) → DEFER to P4** (Q-match-exprpos family; expr-position
temp-hoist, distinct from P2's statement-position work; doesn't gate `use core.*`).

## Net order: P2-α (gates stdlib) → [P2-β ∥ P2-γ].
## Cross-item: items 1+4 share the trait registry (→ combine in α); items 3+4 both edit checker.rs (not
concurrent); item 5 needs the shared match if-chain lowering. Item 4 extends recv_kind (#137).
## Design Qs: (1) AIR has_body flag vs empty-block heuristic; (2) TraitBound tag shape; (3) if-chain-vs-switch
predicate; (4) Self-subst scope (sig only; Self::Output deferred); (5) Go bounded-dispatch method casing.
## Gate (every session): build fresh in-worktree (`cargo build -p bock`; NEVER `cd /opt/claude-projects/bock`);
fmt; clippy --workspace --all-targets -D warnings; test --workspace; cargo doc -D warnings; REQUIRE=all run-conformance.
Per-item T1 (red→green ×5).
