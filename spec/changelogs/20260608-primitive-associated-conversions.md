# Primitive associated conversions (`Int.from`/`Float.from`/`Int.try_from`) check + execute ×5

**Date:** 2026-06-08
**Affects:** §18.3 (`core.convert`) — no normative text change; implementation completion
**Type:** implementation fix (no spec-surface change)

## Change

The **associated-function** form of the canonical primitive conversions —
`Float.from(x)`, `Int.from(x)`, `String.from(x)`, and the fallible
`Int.try_from(s)` / `Float.try_from(s)` — now both **type-check** and
**execute** on all five v1 targets (js, ts, python, rust, go). It is the
`Type.from(x)` / `Type.try_from(s)` counterpart of the already-working
return-type-driven `(x).into()` direction.

The conversion **matrix is unchanged** — it is exactly the lossless set
already registered by `register_canonical_conversions` (Q-prim-assoc; the
normative matrix remains escalated to Design parallel to DQ10):

- `From[Int] for Float`, `From[Float32] for Float`
- signed-integer widening (each narrower signed int → each wider, and every
  sized signed int → the unsized `Int`)
- `From[Char] for String`
- `TryFrom[String] for Int`, `TryFrom[String] for Float`

No new conversion semantics were introduced. Lossy / narrowing directions
remain rejected (`E4012`), as does any `Prim.from`/`Prim.try_from` whose
source type has no canonical conversion to the target.

## Background

PR #274 added the checker resolution for this form but had to revert it
because the codegen then emitted garbage on every target (`float.from(3)`
on JS; `from` is a Python keyword, so `float.from(...)` is a syntax error;
no-such-type on Rust/Go). This change lands the checker resolution **coupled
with** per-target codegen, so the clean `E4012`/`E4002` diagnostic is
replaced by working output rather than broken output.

- **Checker (bock-types):** a primitive type-name callee
  (`Float.from(x)` / `Int.try_from(s)`) resolves against the canonical
  `From`/`TryFrom` impls; `from` yields the target primitive, `try_from`
  yields `Result[Prim, ConvertError]`.
- **Codegen (all 5):** `from` lowers to each target's native coercion
  (`(x)` / `Number`-cast on JS/TS, `float(...)`/`int(...)`/`str(...)` on
  Python — `from` is a keyword there, `float64(...)`/`int64(...)`/
  `string(...)` on Go, `x as f64`/`x as i64`/`char::to_string` on Rust).
  `try_from` parses and returns each target's Bock `Result` shape with a
  `ConvertError` payload.

## FOUND (incidental fix)

The Rust backend emitted **associated** trait methods (`From::from`,
`TryFrom::try_from`) with a spurious `&self` receiver, making the embedded
`core.convert` `From`/`TryFrom` trait declarations uncompilable on Rust
(`E0186`). The trait-declaration emitter now omits the receiver for
associated functions (consistent with the impl side and the documented
`assoc_fn_def` model) and adds `where Self: Sized` for an associated
function returning a `Self`-bearing type by value. This unblocks any Rust
program that imports `core.convert` types.
