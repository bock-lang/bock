# Phase 1 — Codegen-Completeness Milestone, "stdlib types" layer

Plan agent (read-only) against main @ 144f879 (P0 landed: #131/#132/#133). Anchors verified at HEAD (audit's
c9a241e line numbers had shifted). Gates Q-stdlib (R1 iter/effect, R2 option/result). All items in bock-codegen
(P1-d also bock-types) → SEQUENTIAL, one session at a time.

## Sequence (4 sessions)
1. **P1-a+b1 (Python: lambdas + generics + typing imports)** — FIRST, smallest, single-root-cause, py.rs-only.
   - Lambda (py.rs:1736) emits `lambda x: int: body` (type-hint SyntaxError) → emit bare param names (flag
     collect_param_strs py.rs:1182 with hints=false for lambdas). `Callable`/`Any`/`Self` emitted without
     `from typing import …` (preamble py.rs:292) → add needs_* flags. Generics: RecordDecl (py.rs:738),
     EnumDecl, ClassDecl, emit_fn_decl (py.rs:1087) ignore generic_params → emit `T = TypeVar("T")` (dedup) +
     `Generic[T]` base + the typing import.
   - Fixtures (all 5): lambda_closure.bock, generic_record.bock.
2. **P1-b2 (Go + TS + Rust generics)** — Go: method receiver (go.rs:1813) `func (self *Box[T])` + struct
   construct (go.rs:2567) `Box[T]{…}` (reuse var_optional_elem/var_list_elem elem-tracking go.rs:469/488) +
   int64 literals. TS: ImplBlock interface-merge (ts.rs:1062-1160) drops `<T>` (impl.generic_params `..`-ignored
   at :1066) → thread target `<T>` into the merged interface + prototype. Rust: ImplBlock (rs.rs:840-863) emits
   bare `impl Box` for `impl Box {…}` where T comes from the record decl → synthesize `impl<T> Box<T>` (pre-scan
   record/enum generic params, mirror the enum-registry pattern) + trait-path args (rs.rs:852) + synthesize
   `T: Clone`/`T: Display` only where the stdlib source declares (don't over-constrain — DQ). NOT py.rs/js.rs.
   - Fixtures: generic_record_method.bock, generic_enum.bock (confirm generic-enum scope vs records).
3. **P1-c (Result runtime + Optional/Result methods)** — AFTER generics (runtime types are generic). Result:
   mirror the Optional runtime — RESULT_RUNTIME_TS (BockResult<T,E>, alongside ts.rs:44), Python _BockOk/_BockErr
   (alongside py.rs:59), Go __bockResult; fix the construction↔match MISMATCH (Result construct works bespoke
   js.rs:1807/ts.rs:2252/py.rs:1925/go.rs:2665, but ConstructorPat py.rs:2128 only special-cases Some/None →
   Ok/Err emit an undeclared Result_Ok dataclass that never matches the dict). Methods: add desugared_optional_
   method/desugared_result_method recognizer in generator.rs (model: desugared_list_method generator.rs:784;
   methods from checker.rs:2698/2704: is_some/is_none/unwrap/unwrap_or/map/…), wire into each backend's Call arm
   before desugared_self_call (insertion point rs.rs:1702). `?` (Propagate js.rs:1634 is a no-op) → minimal
   early-return on Err, or scope to P2.
   - **CRUX:** unwrap_or/map are shared across Optional/Result/List → codegen must disambiguate receiver type.
     If the AIR doesn't carry it, P1-c needs the same checker→codegen annotation as P1-d → P1-c's T1 must
     determine this EMPIRICALLY before committing; may merge/reorder c+d.
   - Fixtures: result_match.bock, optional_methods.bock, result_methods.bock.
4. **P1-d (primitive-bridge dispatch)** — LAST, spans bock-types + bock-codegen. `(1).compare(2)` is
   checker-only (resolve_primitive_canonical_method_return checker.rs:2548) → codegen emits `a.compare(b)`
   (fails 5/5 incl Rust). Approach (A, recommended): the CHECKER annotates the AIR call node when a primitive
   method resolves via a canonical conformance; codegen lowers to the intrinsic + the `Ordering` enum rep (which
   goes through the #133 registry — dovetails). Schedule so no other bock-types session races it. STOP if the
   AIR annotation ripples into the export ABI/serialization (hand a diagnosis, not a mystery).
   - Fixtures: primitive_compare.bock, primitive_trait_method.bock.

## Cross-item deps
- Result does NOT "just work" via the #133 registry — backends exclude Optional/Result, keep bespoke paths.
- **Phase 1 (b2 + c) is what finally makes `use core.*` EXECUTE on typed targets** (#132 wired bundling; the
  bundled core.option/result/iter are generic records/impls returning Optional[T]/Result[T,E] with methods).
  core.iter's generic ListIterator[T] is the canonical consumer (resumes after P1).
- P1-c crux + P1-d both want a "this method/call resolved via a trait/conformance" signal — design to share one
  checker→codegen mechanism.

## Verification (every session): build fresh in-worktree (`cargo build -p bock`; NEVER `cd /opt/claude-projects/bock`
— stale-binary trap). fmt; clippy --workspace --all-targets -D warnings; test --workspace; cargo doc -D warnings;
BOCK_CONFORMANCE_REQUIRE=all run-conformance.sh. Per-item front-loaded T1 (red→green on all 5).

## Design questions → Design: (1) Optional/Result method receiver disambiguation (P1-c crux — may reorder c/d);
(2) `?` operator scope (P1 vs P2); (3) Rust generic-bounds synthesis (only stdlib-declared bounds); (4) generic
enums scope (P1-b2 — Optional/Result are generic-enum-shaped, overlaps c); (5) DQ17 Optional/Result repr
normativity (already filed).
