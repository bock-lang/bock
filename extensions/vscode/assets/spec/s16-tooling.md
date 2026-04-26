# Spec Excerpt: Tooling

## CLI Commands
`bock new` — scaffold. `bock build` — transpile+compile.
`bock run` — execute (interpreter default). `bock check` — types+lint.
`bock test` — run tests. `bock fmt` — format.
`bock repl` — interactive. `bock inspect` — AI decisions.
`bock override` — pin/change decisions. `bock promote` — strictness.
`bock pkg` — package manager. `bock doc` — documentation.

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
