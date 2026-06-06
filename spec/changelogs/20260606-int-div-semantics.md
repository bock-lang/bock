# Integer division/modulo semantics and canonical Bool spelling (DQ23)

**Date:** 2026-06-06
**Affects:** §3.6 (Operators), §3.5 (Literals), cross-ref §4.2 (Primitive Types)
**Type:** clarification (impl-affecting)

## Change

Normative semantics for integer `/` and `%` and for `Bool` stringification, all
identical across every target (js, ts, python, rust, go):

- **Integer division (`/`).** When both operands are an integer type (`Int` or any
  sized variant `Int8`…`Int128`, `UInt8`…`UInt64`), `/` yields an integer that is
  **truncated toward zero**, not floored:
  `17 / 5 == 3`, `-17 / 5 == -3`, `17 / -5 == -3`, `-17 / -5 == 3`.
  All sized integer types divide identically.
- **Integer remainder (`%`).** `%` is the remainder of the truncated division and
  takes the sign of the **dividend** (C/Rust/Go convention):
  `-17 % 5 == -2`, `17 % -5 == 2`. The identity `(a / b) * b + (a % b) == a`
  holds for all integer operands. Division and modulo are one coherent pair.
- **Float division** is IEEE 754 true division and float `%` is the floating-point
  remainder; both are unchanged.
- **Mixed `Int`/`Float`** operands are a type error — no implicit coercion (§4.2).
- **Division or modulo by zero** (integer) is a runtime abort — a `Panic` ambient
  effect (§10.5) — equivalent on every target.
- **Canonical Bool spelling.** A `Bool` stringified through `${expr}`
  interpolation or `.to_string()` produces exactly lowercase `"true"` / `"false"`
  on every target, matching the §3.5 literals.

## Rationale

DQ23 design ruling. The spec previously listed `/` and `%` among the arithmetic
operators without fixing the integer division direction, the remainder sign, the
divide-by-zero behavior, or the Bool stringification spelling, leaving each to the
target language's defaults — which diverge. The defaults that diverged, and the
traps a naive lowering re-introduces:

- **Python `//` floors** (`-17 // 5 == -4`), and `int(a / b)` routes through lossy
  float true-division; the implementation lowers integer `/` to an integer-only
  toward-zero helper. Python's `%` follows floor division (`-17 % 5 == 3`); the
  implementation lowers integer `%` to a dividend-sign remainder helper.
- **JS/TS `Math.trunc(a / b)` yields `Infinity` on a zero divisor**, not an abort;
  the implementation inserts an explicit zero-check that throws for both `/` and
  `%`. (JS `%` already takes the dividend's sign.)
- **Python `f"{b}"` / `str(b)` print `True` / `False`** (capitalized); the
  implementation lowercases `Bool` interpolation parts and `Bool.to_string()`.
- **Rust and Go** already truncate toward zero, give a dividend-sign `%`, panic on
  integer division/modulo by zero, and print lowercase bools — so their backends
  are unchanged; cross-target conformance fixtures confirm equivalence.

## Migration

No source changes required. Programs relying on the previous (target-dependent)
behavior of integer `/`, `%`, or `Bool` stringification now observe the single
specified semantics on every target. In particular, code that previously saw
Python's floor division or floor-remainder for negative operands, or JS's
non-aborting `Infinity`/`NaN` on a zero divisor, now sees toward-zero division,
dividend-sign remainder, and a uniform runtime abort.
