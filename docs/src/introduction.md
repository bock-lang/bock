# Introduction

**Bock** is a feature-declarative, target-agnostic programming language
that compiles to JavaScript, TypeScript, Python, Rust, and Go. You
write your application's logic, types, and effects once in Bock; the
compiler emits idiomatic source code in any of its supported targets.

## Why Bock?

- **One language, many targets.** Reuse domain logic across a Node.js
  backend, a Python data pipeline, a Rust CLI, and a Go service —
  without translating it by hand.
- **Effect tracking by default.** The compiler infers and tracks I/O,
  network, randomness, and time effects on every function. You always
  know what a function can do.
- **Static types with inference.** Strong typing without verbose
  annotations. The checker catches mismatches before codegen.
- **Targeted output, not a runtime.** Bock emits source code. There is
  no Bock VM and no runtime layer — generated code reads like
  hand-written code in the target language.

## Status

Bock is in active development. The language reference is at
`spec/bock-spec.md`; the formal grammar is in `spec/sections/`. See
[Getting Started](./getting-started.md) for the first end-to-end
example.

## Layout of this Book

- **Language Guide** — narrative introduction to the type system,
  functions, effects, and modules.
- **Reference** — CLI flags, standard library, and pointers into the
  specification.
- **Contributing** — how to file an issue, propose a spec change, and
  get changes through review.

For everything not yet documented here, the specification at
`spec/bock-spec.md` is authoritative.
