# Stdlib-surface ratification batch (DQ10–DQ15, DQ24, DV17)

**Date:** 2026-06-06
**Affects:** §18.2 (prelude), §18.3 (`core.iter`, `core.test`), §18.5 (trait integration), §6.5 (traits)
**Type:** clarification (six pure ratifications) + addition (four small edits)

## Change

Consolidated spec-hygiene pass blessing the stdlib surface that actually shipped, on safe minimum-useful defaults, so §18 records reality before any v1 freeze. Decided by the Design chat 2026-06-06.

Six are pure ratifications (record shipped as normative; no spec restructuring, no impl change): **DQ10** (primitive-conformance matrix), **DQ11** (`core.convert` surface), **DQ12** (`core.iter` protocol shape), **DQ14** (`Iterable.iter()` return type), **DQ15** (combinator dispatch), **DQ24.2** (inherent-`next` trait satisfaction).

Four are small edits within existing sections:

- **DQ13 — §18.2:** added `TryFrom` and `Error` to the prelude list (ratifies the impl, which already preludes them, #120). Same fill-an-omission as the DQ9 `Ordering`/`Less`/`Equal`/`Greater` addition.
- **DQ24.1 — §18.3 `core.iter`:** the normative v1 combinator set is the **six** shipped (`to_list`, `count`, `fold`, `map`, `filter`, `take`); recorded the protocol shape (generic `Iterator[T]`/`Iterable[T]`, eager/`List`-returning, dual built-in/protocol `for` model, `next(mut self)`/`iter(self) -> ListIterator[T]` signatures, concrete `ListIterator` dispatch) and the v1.x deferrals (associated-type iterators, lazy combinators, generic-bound dispatch, custom iterator-return types, the larger combinator list).
- **DQ24.3 — §6.5:** added a Reserved-for-v1.x leading note to the associated-type (`type Item`) `Collection` example — associated types are syntax-parsed with semantics Reserved for v1.x; the v1 iteration protocol is generic `Iterator[T]`/`Iterable[T]` (§18.3).
- **DV17 — §18.3 `core.test`:** dropped "benchmarking" (there is no Bock-level benchmarking, §15.4 `@benchmark` removed / §20.4 delegates to native tools); trimmed to the shipped "assertions (free + fluent)"; moved BDD grouping, mocking, property testing, and snapshot testing to Reserved for v1.x.

Plus **DQ10 — §18.5:** recorded the normative primitive-conformance matrix (the four-trait table) with three normative caveats: `Float` is `Equatable`/`Comparable` with IEEE 754 partial semantics (`NaN != NaN`); `Float` is not `Hashable`; `Bool` is not `Comparable`.

## Rationale

The stdlib was built on conservative minimum-useful defaults precisely so Design could ratify or refine later; in every case the default holds up. The four corrections add two prelude names, reduce one aspirational combinator list to what shipped, annotate one misleading example, and drop one stale word.

DQ24.1 historical-record guardrail: the ~25-combinator "minimum-useful v1 surface" recorded in changelog `20260529-2251` was an aspirational forward-looking surface, later refined by the feasibility constraints surfaced by DQ16 (the List-codegen blocker). Per the historical-changelog rule, `20260529-2251` is **not** edited; the refinement is recorded forward here (the normative v1 floor is the six shipped; the larger set is the v1.x target), with a cross-reference. This is the one place where the "may ship more, must not ship less" rule is consciously overridden by a later feasibility-informed Design decision.

## Migration

None. Six items ratify the shipped surface unchanged. The four edits are spec-accuracy corrections matching what the impl already does (the prelude already exports `TryFrom`/`Error`; `core.iter` already ships the six combinators; `core.test` already ships assertions only; the matrix already excludes Float-Hashable and Bool-Comparable). No code changes.

Linked impl-completeness follow-ups (not part of this ratification; tracked in `queue.md`): §18.5 operator-gating for user types; the broader "unknown method on a concrete type is a checker error, not a fresh-var resolution" hygiene fix (general form of the DQ22 trap); `pop`/`insert`/`remove`/`reverse` mutating-method semantics (DQ18 follow-up).
