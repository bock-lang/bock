# `todo()` / `unreachable()` semantics — prelude divergent utilities

**Date:** 2026-06-05
**Affects:** §18.2 (Prelude)
**Type:** clarification

## Change

§18.2 previously listed `todo` and `unreachable` as prelude utility functions with no
defined behavior, and codegen had been improvising. A normative paragraph now defines them:

- **Type: `Never`.** Both are divergent expressions and may stand in for a value of any
  type, so `fn f() -> Int { todo() }` type-checks (`Never` is assignable to every type).
- **Runtime: abort via the Panic ambient effect (§10.5),** lowering to the target's native
  abort — `panic!` (Rust), `panic` (Go), `throw` (JS/TS), `raise` (Python).
- **Intent / diagnostic differ:** `todo()` marks code *not yet written* and aborts with a
  "not yet implemented" message; `unreachable()` asserts a branch is *logically impossible*.
- **Optional message:** `todo("implement scoring")` is accepted; the message is included in
  the abort diagnostic.
- The existing asymmetry is documented and intentional: `unreachable` is also a reserved
  keyword with a dedicated grammar production (§3.4/§21.10), whereas `todo` is a prelude
  function name only.

## Rationale

Decided by the Design chat (2026-06-05) — the same situation as DQ27: the spec was silent on
a language-semantics question, codegen needed a defined contract, and Design ruled. The
`Never` typing is what lets `todo()` stub an arbitrary return type; the examples already
rely on it (e.g. `guessing-game`).

## Migration

None — this makes an already-implemented behavior normative. Codegen already lowers
`todo()`/`unreachable()` to native aborts on all five targets. The one behavioral note (a
`todo()` in return/tail position emits a bare abort statement, not `return throw …`/`return
raise …`, which would be a syntax error) is already implemented. A consequence for tooling:
an example whose path reaches a `todo()` is behaving *correctly* by aborting — it is
compile-verified, not run-to-completion (see the `guessing-game` stub-showcase disposition in
the examples-exec audit).
