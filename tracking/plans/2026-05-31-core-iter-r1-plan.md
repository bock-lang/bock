# Implementation Plan: `core.iter` R1 Floor

**Date:** 2026-05-31
**For:** Q-stdlib R1 — `core.iter` (the 4th of 11 v1 core modules)
**Status:** approved by orchestrator; dispatch pending
**Designed by:** Plan agent (2026-05-31), grounded in the post-#149 codegen substrate

> Re-resume of `core.iter` after the codegen-completeness milestone (#131–#149).
> The 5 prior STOPs were all codegen gaps now CLOSED;
> `compiler/tests/conformance/exec/generic_iter_concrete_match.bock` proves the
> exact desugar shape compiles+runs green on all 5 targets. The preserved
> `/tmp/bock-iter-module-preserved.bock` draft was lost over the night pause —
> the module is re-authored here from the proven shape + the floor decisions.

## 0. Grounding (verified before planning)

- **Proven shape:** `conformance/exec/generic_iter_concrete_match.bock` — generic
  `record ListIter[T]` with `fn next(mut self) -> Optional[T]` driven by manual
  `loop { match c.next() { Some(x)=>… None=>break } }`. Green ×5 (#149). `next`
  advances an `Int` cursor and yields `self.xs.get(self.cursor)` (read-only).
- **Embedding is automatic:** `bock-cli/build.rs` walks `stdlib/core/**/*.bock`
  with `rerun-if-changed`; adding `stdlib/core/iter/iter.bock` needs NO build-script
  edit. `use core.iter.{...}` resolves automatically.
- **Prelude:** `bock-types/src/seed_imports.rs` — `Iterator`/`Iterable` are
  name-level-only today (no embedded def). Add to `PRELUDE_FROM_CORE` once the
  module ships, mirroring `Comparable`/`Into`/`Error`.
- **For checker arm:** `checker.rs` ~1949–1974 resolves element type only for
  `List`/`Range` generics, else `fresh_var()`. The desugar extends THIS.
- **For codegen is NATIVE on all 5** (e.g. `js.rs:1772` `for (const x of …)`).
  A user `Iterable` record is not natively rangeable → element-typing in the
  checker ALONE is insufficient; the desugar must change what codegen sees.
- **Reused machinery (no new resolution):** `resolve_impl(TraitRef, ty, impls)`
  (`traits.rs`) for Iterable detection; `resolve_method_return_type` generic
  substitution (`checker.rs` ~2854) for element typing; `infer_match` /
  `bind_pattern_type` Some-payload binding (proven by #149) for the arm.
- **Read-only List ops only** (DQ18 defers mutation): `len/get/first/last/concat`
  emit; `push/pop/insert/remove` do NOT. Combinators build results with
  `concat` + list literals — never `.push`.
- **Exec lane placement:** `compiler/tests/execution.rs` scans ONLY
  `conformance/exec/`. The all-5 for→Iterable exec fixture MUST live there
  (it builds with the real embedded `bock`, so `use core.iter` works).

## 1. Investigation steps (do FIRST)

- **I1 — desugar mechanism.** Choose (A) checker AST-rewrite of the `For` node into
  `{ let mut __it = iter.iter(); loop { match __it.next() { Some(pat)=>body; None=>break } } }`
  vs (B) a metadata tag + per-backend lowering (5 backends). **Recommend (A)** —
  localizes to `bock-types`, reuses already-green `Loop`/`Match`/`Break` codegen,
  matches the proven fixture exactly. Confirm the checker can synthesize `AIRNode`s
  with fresh node ids/spans (grep for a node-id allocator in `TypeChecker`; if none,
  thread one in / gensym). Synthesize BEFORE recursing `infer_node` so the new
  `match`/`Some(pat)` subtree types via the existing path. NOTE: no existing
  precedent for the checker rewriting a `NodeKind` variant — this is the new code.
- **I2 — element-type chain.** `resolve_method_return_type(iter_ty,"iter")` →
  `ListIterator[T]` → `…("next")` → `Optional[T]` → unwrap `T`. Fallback: read `T`
  off the `Iterable[T]` trait arg via the `ImplTable` entry. Pick the robust one.
- **I3 — break/continue.** User `break`/`continue` land inside the synthesized
  `Some(pat)` arm; confirm they target the synthesized `loop` (label-depth handling,
  `js.rs:1834`). Low risk (the proven fixture uses `None=>break`); cover explicitly
  in the break/continue exec fixture.

## 2. `stdlib/core/iter/iter.bock` surface (DQ12/14/15/16)

```
module core.iter

public trait Iterator[T]  { fn next(mut self) -> Optional[T] }
public trait Iterable[T]  { fn iter(self) -> ListIterator[T] }      // DQ14: single concrete iterator

public record ListIterator[T] { xs: List[T]  cursor: Int }
impl ListIterator {
  public fn next(mut self) -> Optional[T] {
    match self.xs.get(self.cursor) {
      Some(v) => { self.cursor = self.cursor + 1; return Some(v) }
      None => return None
    }
  }
}
public fn list_iter[T](xs: List[T]) -> ListIterator[T] { ListIterator { xs: xs, cursor: 0 } }
```

**Combinators (concrete over `ListIterator[T]`, eager, List-returning, read-only):**
each drains via its own `loop { match it.next() }` (dogfoods the proven shape):

1. `to_list[T](it) -> List[T]` — `out = out.concat([x])`
2. `count[T](it) -> Int`
3. `fold[T,A](it, init: A, f: (A,T)->A) -> A`
4. `map[T,U](it, f: (T)->U) -> List[U]`
5. `filter[T](it, pred: (T)->Bool) -> List[T]`
6. `take[T](it, n: Int) -> List[T]`
7. *(optional)* `enumerate[T](it) -> List[(Int,T)]` — include only if it type-checks
   cleanly ×5; else defer.

**OUT of floor (orchestrator ratifies w/ Design):** any `.push`/mutating combinator,
lazy combinators, generic-bound `[I: Iterator[T]]` signatures, `zip`/`flat_map`.

**Surface reconciliation:** §6.5 shows an associated-type `Collection` trait; DQ12
overrides for `core.iter` (generic, since assoc-types are inert). §18.3 under-specifies
the combinator set and licenses a minimum-useful subset. The chosen 6 ARE that subset.
Two items the orchestrator confirms with Design before/around merge: (a) generic-vs-
assoc-type (already DQ12) and (b) the exact combinator set.

## 3. The checker for→Iterable desugar (approach A)

Files: `bock-types/src/checker.rs` (For-arm + node synthesis) + `seed_imports.rs`
(prelude). **Likely NO codegen change** (synthesized `Loop`/`Match`/`Break` already
green ×5).

In the `NodeKind::For` arm:
1. `iter_ty = infer_node(iterable)` (unchanged).
2. **Built-ins unchanged:** `List`/`Range` (and Map/Set) keep the native fast path —
   do NOT touch (DQ12).
3. **New branch — user `Iterable`:** if `iter_ty` is a user type AND
   `resolve_impl(Iterable, iter_ty, impl_table).is_some()`:
   - resolve `T` (I2),
   - rewrite `node.kind` → the `{ let mut __it = iterable.iter(); loop { match
     __it.next() { Some(pattern)=>body; None=>break } } }` subtree (`mem::take` the
     moved children; fresh node ids; reuse the For span; gensym `__it` to avoid
     nested-loop collisions),
   - `infer_node` the synthesized subtree (Some(pat) binds `pat: T`).
4. **Fallback:** non-Iterable user type keeps today's `fresh_var()` (optionally a
   "does not implement Iterable" diagnostic per §18.5 — add only if it doesn't churn
   existing fixtures; else defer).

Prelude: add `("core.iter","Iterator")`, `("core.iter","Iterable")` to
`PRELUDE_FROM_CORE`; update the line-79 comment.

## 4. Conformance fixtures

**Directive (`conformance/stdlib/iter/`, `// EXPECT: no_errors`):**
- `iter_traits_resolve.bock` — traits/record/constructor resolve through the embedded module.
- `iter_combinators_typecheck.bock` — every combinator type-checks.
- `iter_user_iterable_typecheck.bock` — user `impl Iterable[Int] for Bag` + `for x in bag` desugar type-checks (`x: Int`).

**Exec (`conformance/exec/`, `// EXPECT: output …` + `// EXPECT: targets js, ts, python, rust, go`):**
- `exec_iter_manual_drive.bock` — module-level `list_iter([1,2,3])` manual loop → `"sum=6"`.
- `exec_iter_combinators.bock` — e.g. `fold(list_iter([1,2,3]),0,add)` → `"6"`.
- **`exec_for_user_iterable.bock`** — THE e2e desugar fixture: `Bag` + `impl Iterable` + `for x in Bag{...}` → `"sum=6"` (REQUIRED ×5).
- `exec_for_user_iterable_break_continue.bock` — break + continue + exhaustion (§18.5).
- `exec_for_user_iterable_nested.bock` — nested `for`/`for` over user Iterables (§18.5); verify `__it` names don't collide.

*(optional)* `bock-cli/tests/stdlib_iter.rs` smoke. Caveat: the interpreter path
constructing `Some`/`None` inside a cross-module stdlib impl was a prior gap; the
EXEC lane (real toolchains) is unaffected and is the authoritative ×5 proof.

## 5. Owned-files (engineer session)

- `stdlib/core/iter/iter.bock` *(new)*
- `compiler/crates/bock-types/src/checker.rs` *(For-arm desugar)*
- `compiler/crates/bock-types/src/seed_imports.rs` *(prelude)*
- `compiler/tests/conformance/stdlib/iter/*.bock` *(new dir)*
- `compiler/tests/conformance/exec/exec_iter_*.bock`, `exec_for_user_iterable*.bock` *(new)*
- *(optional)* `compiler/crates/bock-cli/tests/stdlib_iter.rs` *(new)*

**Sequencing:** `checker.rs` is also wanted by P4-hygiene → the two are SEQUENTIAL;
core.iter runs first. No other owned file conflicts with a known concurrent task.

## 6. Risk / sequencing

**Riskiest: the checker rewrite** (no precedent for `NodeKind` restructuring; node-id
alloc + re-infer + nested-loop `__it` collisions). Mitigated by (A) reusing green
codegen.

**Within-session phasing (incremental value, never STOP):**
- **Phase 1 (always lands):** `iter.bock` + prelude + directive fixtures +
  `exec_iter_manual_drive` + `exec_iter_combinators`. Proves the module compiles+runs
  ×5 using ONLY the proven manual shape + existing native codegen — **zero desugar risk.**
- **Phase 2 (the desugar):** For-arm rewrite + `exec_for_user_iterable*` + the
  user-iterable typecheck fixture.

**FALLBACK if the desugar balloons:** ship **Phase 1 alone** as the R1 floor (module +
6 combinators + manual conformance ×5) and split the for→Iterable desugar to an
immediate fast-follow PR. The module is independently useful (`list_iter(...).next()`
+ combinators called directly). Flag §18.5 for-loop integration as the deferred slice.
**Do NOT block the module on the desugar; do NOT STOP.**

## 7. Verification (pre-PR gate; all must pass)

1. `cargo fmt --all -- --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo test --workspace`
4. `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features`
5. `tools/scripts/run-conformance.sh` (directive + cross-target exec lanes)

Editing `iter.bock` forces a `bock-cli` rebuild (build.rs rerun-if-changed); the exec
lane spawns the freshly built `bock`, picking up the new module. Ensure the all-5 claim
runs on a lane with all toolchains present (`BOCK_CONFORMANCE_REQUIRE`).
