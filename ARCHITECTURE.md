# Architecture

A 30-minute orientation to the Bock compiler. Read this before
your first non-trivial contribution.

## Pipeline

```
Source (.bock)
    │
    ▼
  Parse        (bock-lexer → bock-ast → bock-parser)
    │
    ▼
  Type         (bock-types → bock-checker)
    │
    ▼
   AIR         (bock-air — Analysis Intermediate Representation)
    │
    ▼
 Codegen       (bock-codegen-{js,ts,py,rs,go})
    │
    ▼
Target source
```

## Stage Responsibilities

### Parse

- **`bock-lexer`** — UTF-8 source into tokens with spans. No semantic
  decisions here; lexing is context-free.
- **`bock-ast`** — AST node definitions. Pure data, no logic.
- **`bock-parser`** — Pratt parser producing AST. Reports syntax
  errors via `bock-errors::DiagnosticBag`.

### Type

- **`bock-types`** — type representations, unification, substitution.
- **`bock-checker`** — name resolution, type inference, effect
  inference. Walks AST, produces typed nodes.

### AIR (Analysis Intermediate Representation)

- **`bock-air`** — desugared, fully-typed, effect-annotated tree.
  Codegen consumes AIR, not AST. AIR is target-agnostic but
  lowered enough that each codegen backend is mostly mechanical.

### Codegen

- One crate per target. Each consumes AIR and produces idiomatic
  source in its target language.
- Targets share the AIR contract; if a target needs information
  AIR doesn't carry, the answer is to extend AIR, not to re-walk
  the AST.

## Crate Relationships

Upstream → downstream dependency order:

```
bock-errors
  → bock-source
    → bock-lexer
      → bock-ast
        → bock-parser
          → bock-types
            → bock-checker
              → bock-air
                → bock-codegen-*
                  → bock-cli
```

Lower crates never depend on higher ones. Violations are caught by
`cargo check` from the workspace root.

## AI Provider Architecture (per spec §17.8)

Bock has first-class support for AI-backed code paths through a
provider abstraction in `stdlib/std/ai/`. A provider implements:

- A typed request/response contract
- Retry, backoff, and budgeting policy
- Effect annotations so the checker tracks AI calls as side effects

Targets emit calls to a runtime shim that dispatches to the
configured provider at runtime. The compiler does not embed
credentials or network calls; it only generates the wiring.

## Effect System

Every function has an inferred effect set. Effects propagate up
through call sites and are checked at boundaries:

- **`pure`** — no observable effect
- **`io`** — filesystem, network, stdout/stderr
- **`mut`** — mutates a borrowed reference
- **`ai`** — invokes an AI provider
- **`unsafe`** — escapes the effect system (must be opted into)

Effect inference lives in `bock-checker`; effect representation in
`bock-types`. Codegen targets may use effect info to choose between
sync and async emission.

## Where to Dive Deeper

- **Grammar:** `spec/sections/grammar.md` (forthcoming)
- **Type system rules:** `spec/sections/types.md` (forthcoming)
- **Effect system:** `spec/sections/effects.md` (forthcoming)
- **AIR shape:** `compiler/crates/bock-air/src/lib.rs`
- **Codegen contracts:** `compiler/crates/bock-codegen-js/README.md`
- **Test strategy:** `compiler/tests/conformance/README.md`

## Maintenance Note

This file describes target architecture, not necessarily what is
implemented today. As architecture evolves, this file is updated
in the same PR that lands the change. If `ARCHITECTURE.md` and the
code disagree, the code wins — and that PR's reviewer should have
caught the doc drift.
