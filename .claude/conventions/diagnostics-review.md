# Diagnostics Review Criterion

Standing review checklist for ANY PR that adds or changes a compiler
diagnostic (new code, changed message, changed span, changed exit
behavior). Bock is AI-first: diagnostics are the UX for agents — an
agent repairs code from exactly what the compiler prints. A diagnostic
that a human can squint at but a machine cannot act on is a defect.

Provenance: 2026-06-09 milestone design audit, R3(3)/R-A mitigation 3
("audit error messages for machine-actionability"), and the
`bock check` exit-code bug — the lesson learned once that this
checklist makes a standing criterion. Baseline findings:
Q-diagnostics-agent-audit (PR: chore/diagnostics-agent-audit).

## Checklist — every diagnostic change must satisfy

1. **Stable, registered code.** The diagnostic carries an `E`/`W` code
   constructed via `DiagnosticBag` (never bare `eprintln!` for
   per-construct errors), and the code has an entry in
   `compiler/crates/bock-errors/src/catalog.rs` with summary,
   description, and spec refs. No reusing a number already claimed by
   another pass (the E1001 lexer/resolver collision is the
   cautionary tale).
2. **Precise span.** The primary span points at the offending
   construct, not the enclosing item, and renders as
   `file:line:col` in default output.
3. **Names the construct.** The message quotes the offending
   identifier/operator/type (`` `secret` ``, `` `contains` ``) in
   surface Bock syntax — never internal debug forms like
   `Primitive(Int)`.
4. **States the violated rule.** The message says what constraint
   failed (and which side is *expected* vs *found* for mismatches),
   ideally with the spec section in the catalog entry.
5. **Suggests the fix when determinable.** Use `.note(...)` with a
   concrete edit (`declare it public`, `let mut`, `(x) => ...`). A
   suggestion must be directionally correct for the actual error —
   a wrong suggestion is worse than none.
6. **Deterministic, parseable format.** Same input → same output,
   stable ordering, no duplicate emissions of one root cause, and no
   ANSI escapes when the output is not a terminal.
7. **Correct exit code.** Errors → non-zero; warnings only → zero;
   the outcome flows through `CheckOutcome`/`main`, never a stray
   `process::exit`.
8. **Conformance fixture updated.** A fixture under
   `compiler/tests/conformance/` declares
   `// EXPECT: error E<code> at <line>:<col>` for the new/changed
   diagnostic — and lives in a directory the harness actually
   asserts against `bock check` output (`effects/`,
   `types-diagnostics/`), not one where directives are merely parsed.

## Reviewer prompt

Ask of the diff: "Could an agent, given only this diagnostic text,
produce the correct one-edit repair without reading compiler source?"
If no, request changes.
