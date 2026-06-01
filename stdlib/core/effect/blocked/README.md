# BLOCKED — `core.effect` module source (staged, not embedded)

`effect.bock.blocked` is the authored v1 `core.effect` module. It is **not**
named `*.bock` and lives under `blocked/` so the embed glob in
`compiler/crates/bock-cli/build.rs` (`stdlib/core/**/*.bock`) does **not** pick
it up — embedding it would brick the compiler.

**Why blocked:** the module name `core.effect` collides with the `effect`
reserved keyword, which the parser does not accept as a module-path segment, so
`module core.effect` fails to parse. See the FOUND section of
`spec/changelogs/20260601-0237-core-effect-v1-surface.md` for the root cause and
the exact one-line parser fix.

**To unblock:** land the parser fix, then
`git mv stdlib/core/effect/blocked/effect.bock.blocked stdlib/core/effect/effect.bock`
(and the two conformance fixtures from their `blocked/` dirs), rebuild, and run
the conformance gate. The module and fixtures are written to pass as-is.
