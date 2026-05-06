# Spec Excerpt: Tooling

## CLI Commands
Build/execute: `bock new`, `bock build`, `bock run`, `bock check`,
`bock test`, `bock fmt`, `bock fix`, `bock repl`.

Decisions/manifests: `bock inspect` (browse decisions/rules/cache),
`bock pin` / `bock unpin` (pin lifecycle),
`bock override --choice=X` (change selection),
`bock override --promote <id>` (runtime → build),
`bock cache` (list/clear/prune/stats).

Lifecycle: `bock promote` (strictness), `bock migrate`, `bock doc`,
`bock pkg`, `bock model`, `bock target`, `bock ci`.

CLI shape may evolve through implementation experience; spec follows.

## Project Scaffolding
`bock new <name>` generates `bock.project`, `.gitignore`,
`src/main.bock`, `tests/`. The `bock.project` includes a
commented-out `[ai]` block — AI is opt-in, rule-based codegen
is the default. No interactive prompts during scaffolding.

## Formatter (Zero Config)
- 2-space indent, no tabs
- 80 char soft limit, 100 hard limit
- Opening brace same line
- Trailing commas in multi-line
- Sorted imports: core → std → external → local
- Short sigs one line, long sigs one-param-per-line

## Testing Tiers
1. Semantic (interpreter — fast, target-independent)
2. Transpilation (per-target, compiled + executed)
3. Integration (platform-specific runtimes)

## REPL Commands
`:type expr`, `:air stmt`, `:target T stmt`, `:effects`,
`:context`, `:load file`, `:paste`, `:quit`

## Build System
Incremental at module granularity. Content hashing.
Parallel compilation. Remote build cache.
Pipeline: Parse→TypeCheck→Context→Target→CodeGen→Verify→Compile
