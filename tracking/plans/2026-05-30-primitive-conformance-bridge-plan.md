# Implementation Plan: Q-bridge — Canonical Trait Conformances for Primitives

**Status:** approved (orchestrator, 2026-05-30). Implements Design Q1a (mechanism)
+ Q1b (sealing) from the 2026-05-30 stdlib batch. The gate that unblocks the
stdlib module fan-out. Dispatched as `feat/stdlib-primitive-bridge`.

**Goal:** the compiler provides canonical trait conformances for primitive types
(registered into the same trait-impl table user `impl` blocks populate), so v1
core traits (Comparable/Equatable/Displayable/Hashable) cover primitives —
making `1 < 2`, `max[T: Comparable](1, 2)`, and `(1).compare(2)` resolve
uniformly. Codegen keeps the intrinsic fast path (static resolution, no dynamic
dispatch). User impls of a core trait for a primitive are sealed (rejected).

## Verified reality (main @ b56d953)
- `bock-types/src/traits.rs`: `ImplTable` (`register_trait_impl`, `resolve_impl`,
  supertrait graph, E4010 coherence) + `ImplTable::build_from(module)`.
- **CRITICAL latent bug (→ DV6):** `impl_table` (`checker.rs:177`, `Option`) is
  **`None` in the real pipeline** — `build_from` is only called in tests, so
  `check_trait_bounds_at_call` (checker.rs:1172) returns early and `where`-bounds
  are **unenforced in production today**. The bridge must wire the table in.
- `resolve_method_return_type` (checker.rs:2333): primitive receivers resolve via
  a closed intrinsic `match`, never the table (the #104 locus).
- `infer_binop` (checker.rs:3103): operators consult the table for NO type — so
  conformances add no operator double-handling; §18.5 operator-gating for USER
  types is unimplemented and a SEPARATE follow-up.
- Codegen lowers MethodCall/BinaryOp structurally; never sees `impl_table` →
  codegen invariance holds (must be proven by a byte-equality test).

## Tasks (ordered; T1 front-loads the risk)
- **T1 [STOP GATE]** Wire `ImplTable::build_from(module)` into `check_module`
  (checker.rs ~444) → `self.impl_table`. Run `cargo test --workspace` immediately.
  Enabling the previously-dead bound check may surface latent `where`-bound
  failures in existing fixtures/stdlib. **STOP-and-surface if red** (real missing
  conformance vs checker bug vs test-asserting-broken-behavior); do not weaken the
  check to force green.
- **T2** `register_canonical_conformances` (traits.rs, data-driven; reuse
  `register_trait_impl`) after `build_from`; + supertrait edge Comparable→Equatable.
  **Proposed matrix** (the *normative* matrix is escalated as DQ10 — build on this):
  Equatable {Int,Float,String,Bool,Char,+sized}; Comparable {Int,Float,String,Char,
  +sized numerics; NOT Bool}; Displayable {all primitives}; Hashable {Int,String,
  Bool,Char,+sized ints; NOT Float — NaN}.
- **T3** `resolve_method_return_type`: consult the table for primitive receivers
  (gated) ahead of the intrinsic fallback, so `(1).compare(2)`→Ordering,
  `a.eq(b)`→Bool; intrinsic arms stay as fallback.
- **T4** Sealing (Q1b): reject user `impl <CoreTrait> for <Primitive>` in
  `build_from`/`visit_item` (const trait×primitive sets) — recommend new `E4011`
  in `bock-errors/src/catalog.rs`. Order: `build_from` (sealing) FIRST, canonical
  registration SECOND (bypasses the sealing path). Quadrant-scoped only.
- **T5** Verify: fixtures (max[T:Comparable](1,2); primitive .compare/.eq;
  sealing rejection + newtype positive control; codegen byte-equality Rust+Python)
  + traits.rs unit tests. Full gate, ~2275 baseline no-regress.

## Out of scope
§18.5 operator-gating for user types (separate follow-up); core.convert
Into/From/TryFrom (parameterized-trait resolution — separate task); the normative
primitive-conformance matrix (DQ10, escalated).

## Decision classification
- **Implementer's-call:** `is_canonical` flag on impl entries (recommended);
  E4011 vs reuse E4010 (recommend new); call site (`check_module`); register sized
  numerics now (recommend yes).
- **Escalated to Design (DQ10):** which primitive conformances are *normative*;
  `Bool: Comparable`? `Float: Equatable`/`Hashable` given NaN? §18.2/§18.5 name the
  traits but never pin the primitive×trait matrix. Build on the proposed matrix;
  Design ratifies. §18.5 operator-gating for user types flagged as a follow-up.

### Critical files
`bock-types/src/traits.rs`, `bock-types/src/checker.rs`,
`bock-errors/src/catalog.rs`, `compiler/tests/conformance/stdlib/compare/`.
