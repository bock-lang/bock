# In-place List mutator contracts: `pop` / `remove_at` / `insert` / `reverse`, and `set` OOB pinned (DQ30)

**Date:** 2026-06-10
**Affects:** §18.3 (`core.collections`); cross-references §10.5 (Panic ambient effect), §3.6 (DQ23 divide-by-zero precedent), §5 (`mut` model)
**Type:** clarification / addition (impl-affecting; records the `remove` → `remove_at` rename forward)

## Change

DQ30 extends DQ18's in-place `List` mutation model (`push`/`append`, changelog
`20260606-list-mutation-map-contains`) to the remaining mutators. All are
**`mut self`** (a non-`mut` receiver is the same `E5004` compile error DQ18
introduced) and mutate the list in place:

| method | signature | failure behavior |
| ------ | --------- | ---------------- |
| `pop` | `pop(mut self) -> Optional[T]` | `None` on empty — emptiness is a normal state |
| `remove_at` | `remove_at(mut self, index: Int) -> T` | abort on out-of-bounds (Panic ambient effect; message carries op, index, len) |
| `insert` | `insert(mut self, index: Int, value: T) -> Void` | valid range `0..=len` (`len` = append position); abort on OOB; explicitly NOT Python's clamp |
| `reverse` | `reverse(mut self) -> Void` | — |

A `mut self` method returning a value (`pop`, `remove_at`) is well-formed; the
receiver contract is independent of the return type (Rust's `Vec::pop`/`remove`
are the same shape). Previously these four type-checked as value-returning
placeholders (returning the receiver `List[T]`) with no lowering on any target.

**The rename: `remove_at`, not `remove`.** The by-index removal method is
`remove_at` — by-index explicit (the Kotlin `removeAt` / Swift `remove(at:)`
resolution). Bare `List.remove` is rejected (`E4013`, with a "did you mean
`remove_at`?" suggestion); `remove(value)` stays free for a future by-value
removal. `Set.remove(e)` and `Map.delete(k)` keep their unambiguous receiver
semantics. The Q1-floor changelog `20260529-2251` lists `remove` among the
aspirational List methods; per the historical-record rule that entry is **not**
edited — this entry records the rename forward with this cross-reference (the
DQ24 reconciliation pattern, cf. `20260606-stdlib-surface-ratification`).

**`set(i, v)` OOB pinned.** Indexed `set(mut self, index: Int, value: T) ->
Void` is in the ratified §18.3 floor but its out-of-bounds behavior was
unspecified — and it was in fact unimplemented everywhere (the checker rejected
`xs.set(...)` with `E4013`; no backend or interpreter supported it). Under this
ruling it is implemented on all five targets + the interpreter and **aborts on
OOB** under the same principle, with explicit bounds checks excluding JS's
silent array extension and Python's negative indexing.

**The normative principle (§18.3).** *Queries that can miss return `Optional`;
violated index contracts abort.* Read-path absence the caller is expected to
handle is `Optional` (`get`/`first`/`last`/`index_of`/`pop`); a write-path
out-of-bounds index (`remove_at`/`insert`/`set`) is a runtime abort — a Panic
ambient effect (§10.5), equivalent on every target, the same principle as
integer division by zero (§3.6, DQ23). The read/write asymmetry is deliberate:
`get(i) -> Optional[T]` stays total, while a write to a nonexistent position
has no meaningful partial result.

Per-target lowering (extends the DQ18 table; abort pathway per the DQ23
conventions):

| target | `pop` | `remove_at(i)` | `insert(i, x)` | `reverse` | `set(i, x)` |
| ------ | ----- | -------------- | -------------- | --------- | ----------- |
| rust | `v.pop()` (native `Option`) | `v.remove(i as usize)` (native panic) | `v.insert(i as usize, x)` (native panic) | `v.reverse()` | `v[i as usize] = x` (native panic) |
| js / ts | length-check → tagged Optional around `arr.pop()` | bounds-check + `arr.splice(i, 1)[0]` | bounds-check + `arr.splice(i, 0, x)` | `arr.reverse()` | bounds-check + `arr[i] = x` |
| python | emptiness-check → tagged Optional around `lst.pop()` | pre-check + `lst.pop(i)` | **pre-check REQUIRED** + `lst.insert(i, x)` (native `insert` clamps) | `lst.reverse()` | pre-check + `lst[i] = x` |
| go | len-check + slice-shrink, receiver reassigned through a pointer (`*__r = …`) | bounds-check + grab + `append((*__r)[:i], (*__r)[i+1:]...)` reassign | bounds-check + append-grow + `copy` shift | in-place swap loop | bounds-check + `s[i] = x` |
| interp | identical contracts (receiver write-back; R11 oracle parity) | | | | |

Go's length-changing lowerings spell the DQ18 receiver-reassign pattern through
a pointer parameter so `pop`/`remove_at` compose in expression position; the
`mut`-receiver guarantee makes `&recv` addressable.

**Abort messages (DQ23 reconciliation).** The synthesized checks (js/ts/
python/go and the interpreter) abort with the normalized message
`List.<op>: index <i> out of bounds (len <n>)`. The Rust backend keeps `Vec`'s
**native** panics — which also carry the operation kind, index, and length
(`removal index (is 5) should be < len (is 3)`, etc.) — exactly as DQ23 kept
Rust/Go's native divide-by-zero panics rather than wrapping them. A negative
Bock index on Rust wraps through `as usize` into the same native bounds panic
(abort preserved; the printed index is the wrapped value).

## Rationale

DQ30 design ruling; unblocks `Q-list-mut-pop-insert-remove`, the last
Design-gated item on the compiler v1 backlog. DQ18 fixed `push`/`append` but
left the other mutators value-returning placeholders, an inconsistent surface
(mutation via `push`, copies via `pop`?) that also blocked the checker from
rejecting `acc = acc.pop()`-style ambiguity. The Optional-vs-abort split
follows the existing language precedents (`get -> Optional` on the read path;
DQ23 aborts on violated arithmetic contracts) instead of inventing a third
behavior (clamping, sentinel values, silent extension) that the target
languages disagree on.

## Migration

None for `pop`/`insert`/`reverse`/`set`: no example, stdlib, or fixture code
called them as mutators (the checker placeholders were unreachable in
practice, and `set` did not resolve at all). `List.remove(i)` call sites — of
which the repository had none — must rename to `remove_at(i)`. Interpreter
behavior is now identical to the compiled targets; programs that (incorrectly)
relied on the interpreter's old value-returning `push`/`pop`/`insert`/
`remove`/`reverse` registry entries observe in-place semantics under
`bock run` as well.
