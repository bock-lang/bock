# OPEN: does structural record equality satisfy `Equatable` for `==`/`!=` gating? (§18.5)

**Date:** 2026-06-08
**Affects:** §18.5 (Trait-Language Integration), §10/records, §<derive> (Appendix C)
**Type:** design question (escalation) — **no normative text and no implementation changed**

## Status

`OPEN: §18.5 — does structural record equality satisfy Equatable for ==/!= gating?`

This entry records an unresolved design question discovered while investigating
`Q-equatable-gating-user-types`. **No checker change was made.** The investigation
concluded the gate cannot be implemented cleanly without a Design decision; this
note is the durable artifact for routing that decision.

## Background

PR #296 (`20260608-operator-gating-user-types.md`) gated the **ordering** operators
(`<`/`>`/`<=`/`>=`) behind `impl Comparable` for user (`Type::Named`) operands. It
**intentionally deferred** `==`/`!=` (`Equatable`) with the note: "records carry
structural equality, and gating equality is a separate item." This investigation
resolves that deferral — and finds it is a **design question**, not a mechanical
mirror of #296.

## Findings (empirical)

How user-type equality works **today** (verified against the implementation):

1. **Records, enums, and classes get structural `==`/`!=` for free at the codegen
   level**, with **no `Equatable` conformance the checker can see.** A bare
   `record == record` / `enum == enum` type-checks clean today and runs correctly
   on every target (e.g. Python lowers records to `@dataclass`, which provides
   structural `__eq__`). Confirmed end-to-end: a two-field `record` compared with
   `==`/`!=` checks clean and prints `true;false` after Python codegen + execution.

2. **Only primitives are registered as `Equatable` in the `impl_table`**
   (`register_canonical_conformances`, `bock-types/src/traits.rs` ~:1206–1264).
   User types conform to `Equatable` **only** via an explicit `impl Equatable`.
   There is **no** auto-derive / structural-conformance registration for records.

3. Therefore a naive `require_equatable_operand` mirroring #296's
   `require_comparable_operand` (`bock-types/src/checker.rs` ~:5162) would
   **reject** `record == record` and `enum == enum` — idiomatic, currently-working,
   structurally-sound code.

This is scenario **(B)** from the task framing: structural equality exists at
codegen but is **invisible to the checker as `Equatable` conformance.**

### Fallout measurement

A throwaway experimental gate was applied locally (then reverted) to measure blast
radius:

- **Full conformance exec suite: 199/199 fixtures pass on all 5 targets**
  (js, ts, python, rust, go); diagnostic fixtures pass.
- **All 20 example projects `bock check` clean.**

The corpus is green **only because no fixture/example does a *direct*
`record == record` / `enum == enum` comparison** — they compare via primitive
fields or via explicit `impl`. That is a **coverage gap, not safety**: the gate
still turns idiomatic structural equality into a type error for users.

## Why this is a design question, not impl-completeness

§18.5 line ~1920 claims operator-gating for user types "is an impl-completeness
item, **not a design question**." That claim holds for **`Comparable`** (#296 landed
cleanly — comparing user records with `<` is rare and always done via an explicit
impl). It does **not** hold for **`Equatable`**, because of an unresolved collision:

- §18.5 (bidirectional rule, line ~1901) and the conformance surface (line ~1930)
  imply `==` requires `impl Equatable`.
- The `(core trait, user type)` quadrant is explicitly **"not specified here"**
  (line ~1903) — there is **no** normative statement that records auto-derive
  `Equatable`.
- **`@derive` is explicitly reserved for v1.x** (line ~671: "v1 has no built-in
  derive set"). So in v1 a user has **no ergonomic way** to satisfy an `Equatable`
  gate for a record short of hand-writing `impl Equatable { fn eq(self, other:
  Self) -> Bool { … } }` field-by-field for **every** record — a severe regression
  versus today's free structural equality.

The open question Design must answer:

> **Does structural (field-wise) record/enum equality count as `Equatable`
> conformance for the purposes of `==`/`!=` operator gating in v1?**

Plausible resolutions (for Design, not decided here):

- **(R1) Auto-conform records/enums to `Equatable` structurally** — register a
  `Named`-type `Equatable` conformance in the impl_table when every field is
  `Equatable`, then gate `==`/`!=` on top. Gate becomes clean (records pass,
  genuinely non-equatable user types — e.g. those containing function-typed fields
  — are rejected). This is the path that makes the gate land without regression,
  but it is a **semantic addition** (implicit structural-Equatable derive),
  which is exactly what `@derive` was reserved to provide in v1.x.
- **(R2) Do not gate `==`/`!=` in v1** — keep structural equality ungated; revisit
  when `@derive(Equatable)` ships in v1.x. Records keep free `==`; the §18.5
  bidirectional rule is documented as applying to `Comparable` (and the other
  non-structural traits) in v1, with `Equatable` gating deferred to the derive era.
- **(R3) Gate `==`/`!=` strictly (require explicit `impl Equatable`)** — rejected
  here as a non-starter for v1: it breaks idiomatic record/enum equality with no
  ergonomic escape hatch (`@derive` is v1.x).

## Decision made by this investigation

**STOP — do not force the gate.** No checker change, no stdlib/example `impl`
additions (none were warranted — the corpus already passes; adding `impl Equatable`
to every record would be a mass-edit papering over the design question). This note
is the only artifact; route the OPEN to Design.

## Migration

None — no behavior changed.
