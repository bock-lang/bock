# Plan: Effect foundation hardening (executed → #155)

**Date:** 2026-05-31
**For:** unblocking `core.effect` — make the §10 effect surface work + actually tested ×5
**Status:** DONE → #155
**Designed by:** Plan agent (2026-05-31); built by an engineer session

> The core.effect feasibility probe found the effect surface had gaps hidden by an
> INERT test suite (`conformance/effects/` was never checked or executed). This
> hardened the foundation before building `core.effect` on it.

## Headline (the Plan pass's key finding)

The §10.4 bare-op-in-`handling`-block gap was a **fixable resolver/checker bug, NOT
a v1-scope limitation.** Codegen already established the handler binding and rewrote
the bare op against it; only name-injection (resolver + checker) was missing. The
v1 "effects compile to parameter passing" model (§10.6) does NOT require the op call
to be inside a `with`-clause fn — a `handling` block establishes the handler as a
local binding the op rewrites to. So: implement the normative spec, no Design gate.

## Divergence map (pre-#155 → resolved)

| Form | Spec | Pre-#155 | Root cause |
|------|------|----------|------------|
| bare op in `fn … with E` | §10.2 | worked ×5 | `with`-clause injects ops (resolve.rs:816) |
| …op in `${…}` interpolation | §10.2 ex | js/ts/py/go ok, **Rust E0425** | rs.rs:2917 sub-ctx dropped `effect_ops`/`current_handler_vars` |
| bare op in `handling { }` | §10.4 | **E1001** | `resolve_handling` didn't inject ops |
| bare op w/ Layer-2 `handle` | §10.3 | **E1001** | `Item::ModuleHandle` didn't inject ops |
| unhandled bare op | §10.3 | E1001 (not E8020) | `EffectOp` AIR nodes only built in test code |

## What landed (#155, 3 phases)

- **Phase B (resolver+checker):** `resolve_handling` pushes a scope + injects each
  handled effect's ops (reusing `inject_ops_for_effect`); new `inject_module_handle_operations`
  for Layer-2; `checker.rs` `HandlingBlock` arm + a module-handle pass mirror it (scoped
  env injection). Injection stays handler-in-scope, so unhandled ops still error.
- **Phase C (Rust codegen):** the `Interpolation` sub-context now clones
  `effect_ops`/`current_handler_vars`/`fn_effects`/`composite_effects`.
- **Phase A (harness+fixtures):** 6 executable `exec_effect_*.bock` (with_clause incl.
  op-in-interpolation, handling_block, multiple_effects, innermost_handler, module_handler,
  cross_module), each ×5; the inert `effects/` fixtures converted to these; a check-driven
  harness test now runs `bock check` over the diagnostic fixtures.

## Result
ALL §10 invocation forms execute ×5; 330 exec pairs (0 failed under `REQUIRE=all`);
the effect system is tested for the first time. DV16 RESOLVED.

## Residue (filed, non-blocking)
- **Q-effect-op-node-lowering:** unhandled bare op surfaces E1001 not E8020 (lower bare
  unhandled calls to `EffectOp` nodes so E8020 fires). Non-urgent; code non-normative.
- **Q-effect-import-unused:** imported effect used only in effect position → cosmetic W1001.
- `pure_function.bock` removed — `pure fn` is not in the grammar (speculative fixture; §10.5
  pure-effect determinism is `@deterministic`). No real gap.
