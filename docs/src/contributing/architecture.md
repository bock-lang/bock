# Architecture

This page orients you to the compiler's shape: the stages a Bock
program passes through, and the crates that own each stage. It is a
map, not a deep dive — read it to find *where* a change belongs, then
read the crate itself.

The compiler is a Cargo workspace under `compiler/crates/`. Everything
is a library crate except the `bock` binary; the libraries compose into
the pipeline below.

## The pipeline

A source file moves through the stages roughly in this order:

```text
source text
   │  bock-lexer        tokenize
   ▼
 tokens
   │  bock-parser       parse to AST
   ▼
   AST  (bock-ast)
   │  bock-types        type-check + infer; resolve scopes
   ▼
   AIR  (bock-air)      typed, scope-resolved, effect-aware IR
   │                    effect/capability verification
   ▼
   ├─ bock-codegen      emit JS · TS · Python · Rust · Go
   └─ bock-interp       tree-walking interpreter (`bock run`)
```

Spans and diagnostics (`bock-errors`, `bock-source`) thread through
every stage; the core registry of built-in traits and primitives
(`bock-core`) backs type-checking and the standard library.

## The crates

**Front end — text to AST**

| Crate | Responsibility |
|-------|----------------|
| `bock-source` | Source-file management and `FileId` mapping |
| `bock-lexer`  | Tokenizer |
| `bock-parser` | Parser — token streams to AST nodes |
| `bock-ast`    | Abstract syntax tree definitions |
| `bock-errors` | Diagnostic types and source-span machinery |
| `bock-vocab`  | Self-describing language vocabulary (keywords, errors, prelude, stdlib, annotations) emitted for tooling |

**Middle — meaning**

| Crate | Responsibility |
|-------|----------------|
| `bock-types` | Type system, type checking, and inference (this is the type checker — there is no separate `bock-checker` crate) |
| `bock-air`   | Annotated Intermediate Representation: typed, scope-resolved, effect-aware; effect and capability verification run here |
| `bock-core`  | Core stdlib registry and runtime primitives — the built-in trait-impl table and canonical primitive conformances |

**Back end — output**

| Crate | Responsibility |
|-------|----------------|
| `bock-codegen` | Multi-target code generation — JS, TS, Python, Rust, and Go all live in this one crate (one module per target), not in separate per-target crates |
| `bock-interp`  | Tree-walking interpreter, used by `bock run` and tests |

**Driver and tooling**

| Crate | Responsibility |
|-------|----------------|
| `bock` (`bock-cli`) | The `bock` binary: `new`, `build`, `run`, `check`, `test`, `fmt`, `repl`, `inspect`, `pin`/`unpin`/`override`, `cache`, `promote`, `pkg`, `doc`, `model`, `lsp` |
| `bock-build` | Build pipeline coordinating parse, check, codegen, and target-toolchain compilation |
| `bock-pkg`   | Package management — dependency resolution and lockfiles |
| `bock-fmt`   | Source formatter |
| `bock-lsp`   | Language Server Protocol implementation |
| `bock-ai`    | AI provider interface for the AI-native codegen pipeline (Generate, Repair, Optimize, Select) |

## A note on `ARCHITECTURE.md`

The repository-root `ARCHITECTURE.md` is a longer narrative tour of the
pipeline. Where it and this page disagree about crate names, **this page
reflects the current workspace** (run `ls compiler/crates/` to confirm):
type-checking lives in `bock-types`, and all five targets share the
single `bock-codegen` crate.

## Two kinds of tests

Knowing which kind you're adding matters when you declare what a change
touches:

- **Crate integration tests** are colocated with their crate, under
  `compiler/crates/<crate>/tests/`. CLI integration tests for the
  `bock` binary live at `compiler/crates/bock-cli/tests/`. There is no
  central `compiler/tests/cli/` directory.
- **Language conformance fixtures** are central, under
  `compiler/tests/conformance/<category>/`. See [Development
  workflow](./workflow.md#conformance) for how they run.
