# List `push`/`append` mutation model & `Map.contains` rejection

**Date:** 2026-06-06
**Affects:** §18.3 (`core.collections`); cross-references §5 (`mut` model)
**Type:** clarification / addition (impl-affecting)

## Change

Two §18.3-collections rulings are now normative and enforced by the compiler.

**DQ18 — `List.push` / `List.append` are in-place, `Void`-returning mutators.**
`push` and `append` on a `List[T]`:

- require a **mutable receiver** — a `let mut` binding, a `mut` parameter, or a
  field reachable through a `mut` receiver. A `push`/`append` on a non-`mut`
  receiver is a compile error (`E5004`, the ownership pass) that points at the
  receiver and suggests `let mut`;
- **mutate the list in place** and return **`Void`** (previously they typed as
  returning the receiver `List[T]`).

Functional list-building stays on `+` / `concat`, which are **value-returning**
(produce a new list), require no `mut`, and never mutate their operands.
`append` remains the spelling alias for `push`; both lower identically.

Per-target lowering of an in-place mutator (statement position, value-less):

| target      | lowering                          |
| ----------- | --------------------------------- |
| js / ts     | `recv.push(x)`                    |
| python      | `recv.append(x)`                  |
| rust        | `recv.push(x)` (receiver is `mut`)|
| go          | `recv = append(recv, x)`          |

Go grows a slice by reassignment, so the in-place mutator lowers to an
assignment statement (`recv = append(recv, x)`, or `r.items = append(r.items,
x)` for a field receiver); the `mut`-receiver guarantee makes the left-hand side
a valid lvalue.

**DQ22 — `contains` is not a `Map` method.** `Map` membership is `contains_key`
(key) and `contains_value` (value). The compiler rejects `map.contains(...)`
with `E4013` and a "did you mean `contains_key`?" suggestion — it is **not**
aliased to `contains_key`. `Set.contains(e)` (element membership) and
`Map.contains_key` / `contains_value` are unchanged.

## Rationale

`push`/`append` returning the receiver invited an unbounded functional/mutating
ambiguity (`acc = acc.push(x)` vs. `acc.push(x)`); fixing the semantics as
`mut self` + `Void` before codegen was built keeps the two list-building styles
distinct: mutation via `push`/`append`, value-building via `+` / `concat`. The
`mut`-receiver rule is the existing §5 `mut` model applied to a built-in method,
so it needs no new mechanism. Rejecting `Map.contains` (rather than aliasing it)
keeps membership unambiguous on a type that has both keys and values, while
`Set.contains` stays valid because a set has only elements.

## Migration

None. No example or stdlib code called value-returning `push`/`append` or
`Map.contains`, so there is nothing to migrate. Code that built a list
functionally already used `+` / `concat` (unchanged); code that tested map
membership already used `contains_key` (unchanged).
