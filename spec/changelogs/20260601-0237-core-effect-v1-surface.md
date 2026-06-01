# core.effect — v1 surface ships

**Date:** 2026-06-01
**Affects:** §18.3 (`core.effect`), references §10 (Algebraic Effects)
**Type:** addition

> **Status: shipped.** The v1 `core.effect` surface below is authored,
> reviewed, and now **builds and runs end to end on all five targets** (js, ts,
> python, rust, go). Shipping it required one parser fix — letting the `effect`
> reserved keyword appear as a module-path segment, so `module core.effect` and
> `use core.effect.{...}` parse — which lands in the same PR. See the
> "Enablement" note at the end of this entry.

## Change

The v1 surface of the `core.effect` standard-library module ships. §18.3 lists
it as "Effect system primitives"; this realizes that "minimum-useful subset" as
**effect-system primitives plus one executable standard effect**. The shipped
`module core.effect` public surface is:

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
`exec_core_effect_log` / `exec_core_effect_log_propagation` fixtures prove the
*module* end to end and pass on all five targets.

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

## Enablement — `effect` is now a legal module-path segment

Building `core.effect` required the parser to accept `effect` (a reserved
keyword, `TokenKind::Effect`) as a segment of a dotted module path. The spec
(§18.3) names the module `core.effect`, so this was a parser bug, now fixed.

The fix is scoped to module-path / import-path parsing only, in
`compiler/crates/bock-parser/src/lib.rs`:

- A new `is_path_segment_token` predicate accepts `Ident` / `TypeIdent` plus the
  effect-family contextual keywords `Effect` / `Handle` / `Handling` as path
  segments. `parse_module_path`, `parse_import_base_path`, and
  `try_parse_path_segment` use it. A keyword segment carries no `literal` text,
  so its segment name is the keyword's textual spelling (e.g. `effect`), taken
  from `TokenKind`'s `Display`.
- The change does **not** touch expression/field-access parsing (`obj.effect`
  field access is unchanged) or item-position effect-declaration parsing
  (`effect Log { ... }` still parses as an `Item::Effect`); those code paths
  never reach the module/import path-parsing functions. Parser unit tests cover
  `module core.effect`, `use core.effect.{Log, console_log}`, and a regression
  that `effect Log { ... }` at item position still parses as an effect decl.

Activating the embedded module (it now compiles into every `bock` invocation)
surfaced one ripple in the interpreter's module-registration order
(`compiler/crates/bock-cli/src/run.rs`): non-entry modules were registered into
the interpreter by iterating a `HashMap`, whose nondeterministic order made the
flat effect-operation map (`op-name → effect-name`) resolve a user effect op
that shares a name with a core op (e.g. `log`) inconsistently. Registration now
iterates the already-computed topological order (dependencies — including the
embedded core modules — before dependents, entry module last), so user effect
ops deterministically shadow core's. This removes the nondeterminism and is
independently correct (HashMap iteration order should never drive runtime
behavior).
