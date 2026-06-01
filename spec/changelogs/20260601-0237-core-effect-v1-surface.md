# core.effect — v1 surface decided (build BLOCKED on a parser fix)

**Date:** 2026-06-01
**Affects:** §18.3 (`core.effect`), references §10 (Algebraic Effects)
**Type:** addition

> **Status: surface decided, build BLOCKED.** The v1 `core.effect` surface
> below is authored and reviewed, but the module **cannot ship yet**: the
> module name `core.effect` collides with the `effect` reserved keyword, which
> the parser does not accept as a module-path segment. `module core.effect` and
> `use core.effect.{...}` both fail to parse (E2000), and because the embedded
> stdlib is parsed on every `bock` invocation, embedding the module bricks the
> whole compiler. This is a compiler-crate fix (out of scope for the authoring
> session) — see the FOUND note at the end of this entry. The authored module,
> fixtures, and a turnkey reproduction are staged under `blocked/` directories
> (as `*.bock.blocked`, invisible to the embed glob and the test harness)
> pending that fix.

## Change

The v1 surface of the `core.effect` standard-library module is hereby fixed
(pending the parser fix that lets it build). §18.3 lists it as "Effect system
primitives"; this realizes that "minimum-useful subset" as **effect-system
primitives plus one executable standard effect**. The decided `module
core.effect` public surface is:

- `effect Log { fn log(message: String) -> Void }` — the canonical logging
  effect, a single operation taking a `String` message and returning `Void`.
- `record ConsoleLog {}` with `impl Log for ConsoleLog` whose `log` body is
  `println("[log] ${message}")` — the one v1 handler form (a record + `impl`,
  per §10.4), writing each message to standard output prefixed with `[log] `.
- `fn console_log() -> ConsoleLog` — the ergonomic constructor for the handler,
  installed via `handling (Log with console_log()) { ... }` (§10.3 Layer 1) or
  a module-level `handle Log with console_log()` declaration (§10.3 Layer 2).

The "effect-system primitives" of §18.3 are the language's `effect` / `handler`
/ `handling` machinery (§10), which is a compiler feature, not stdlib surface;
`core.effect` exercises rather than re-declares them and adds the one standard
effect above. The module is pure Bock and needs no per-target runtime shim; its
effect dispatch lowers and executes identically on all five v1 targets (js, ts,
python, rust, go) — the underlying §10.2/§10.4 forms are proven ×5 by the
`exec_effect_*` fixtures landed in #155. The `core.effect`-specific
`exec_core_effect_log` / `exec_core_effect_log_propagation` fixtures that would
prove the *module* end to end are authored but cannot run until the parser fix
below lands; they are staged under `conformance/exec/blocked/`.

**Reserved for v1.x** (explicitly OUT of the v1 `core.effect` surface):

- **Adaptive effect handlers** (`Effect.adaptive(...)`, §10.8) — runtime
  strategy selection via the AI provider.
- **Lambda-based handler constructors** (`Effect.handler(...)`, §10.4) — the
  stateless-handler shorthand. The v1 handler form is the record + `impl` form.
- **Layer-3 project-default handlers** (`bock.project [effects]`, §10.3) — v1
  resolves handlers through Layer 1 (`handling` blocks) and Layer 2
  (module-level `handle`) only.
- **`Cancel`** (§13.5) — the ambient cancellation effect, Reserved with the
  broader cancellation surface.

Not part of `core.effect` (homed elsewhere, by design — unchanged here):

- **Ambient effects `Panic` / `Allocate`** (§10.5) are compiler-intrinsic,
  always available without declaration, and need no stdlib surface.
- **`Clock`** (§10.2 / §18.3.1) is owned by `core.time`; `std.time` ships its
  default `SystemClock` handler.

## Rationale

`core.effect` was the under-specified entry in the §18.3 v1 list (just "Effect
system primitives"). The effect foundation was hardened in #155 (the §10.2/§10.4
bare-op forms and record-handler installation execute on all five targets), so
the project owner set the v1 floor as primitives + one canonical, executable
standard effect (`Log`) — enough to make the effect system usable from the
stdlib without pulling in the still-evolving ergonomic surface (adaptive/lambda
handlers, project defaults) ahead of its design pass. This is the 5th of the 11
v1 `core.*` modules.

## Migration

None. This is purely additive: a new `core.effect` module with no prior surface.
Users opt in with `use core.effect.{Log, ConsoleLog, console_log}`. No effect
name is added to the §18.2 prelude, so existing programs are unaffected.

## FOUND — blocker: `effect` keyword cannot be a module-path segment

Building `core.effect` requires the parser to accept `effect` (a reserved
keyword, `TokenKind::Effect`) as a segment of a dotted module path. It does not.
Two surfaces break:

- **Module declaration** `module core.effect` — `parse_module_path`
  (`compiler/crates/bock-parser/src/lib.rs`) only continues a `.segment` when
  the lookahead is `Ident` or `TypeIdent` (it `break`s on `TokenKind::Effect`),
  so the path parses as just `core`, then the stray `.effect` desyncs the parser
  (observed: `E2000 expected '{', found 'public'`).
- **Import** `use core.effect.{...}` — `parse_import_base_path` (same file) has
  the identical `Ident | TypeIdent`-only continuation, so it stops before
  `.effect`, leaving `.effect.{...}` → `E2000 expected '{', found '.'`.

Because `compiler/crates/bock-cli/build.rs` embeds every `stdlib/core/**/*.bock`
and the embedded sources are parsed on **every** `bock` invocation, placing
`effect.bock` at its live path bricks the whole compiler (every `bock check`
fails, and ~all stdlib conformance tests fail). The feasibility probe in #155
used module names `main`/`logging`, so it never exercised `module core.effect`
or `use core.effect.{...}` — the collision was not caught there.

**Fix (compiler-crate change, out of the authoring session's scope):** allow the
effect-family contextual keywords as module-path segments. Minimal change: in
both `parse_module_path` and `parse_import_base_path`, extend the `.segment`
continuation match (and `try_parse_path_segment`) to accept `TokenKind::Effect`
(and, for symmetry / future-proofing, `Handle` / `Handling`), treating them as
ordinary path segments — emitting their textual spelling via
`TokenKind::display` rather than `literal`. A scoped alternative is a general
"keywords are valid module-path segments" rule. Either way it needs parser
tests for `module core.effect` and `use core.effect.{...}`.

Until that lands, the authored artifacts are staged (non-embedded, non-discovered
`*.bock.blocked`) at:

- `stdlib/core/effect/blocked/effect.bock.blocked` — the module source.
- `compiler/tests/conformance/stdlib/effect/blocked/effect_module_no_errors.bock.blocked`
- `compiler/tests/conformance/exec/blocked/exec_core_effect_log.bock.blocked`
- `compiler/tests/conformance/exec/blocked/exec_core_effect_log_propagation.bock.blocked`

The follow-up: land the parser fix, then `git mv` each `*.bock.blocked` back to
its live path (dropping the `.blocked` suffix and the `blocked/` dir), rebuild
(re-embeds `effect.bock`), and run the conformance gate — the fixtures are
written to pass as-is.
