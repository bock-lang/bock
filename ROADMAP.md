# Roadmap

This file tracks forward-looking plans. For the present state of
the repo, see [`STATUS.md`](STATUS.md).

## Current Phase

**M.3 — Repository scaffold.** Establish the project structure,
contributor docs, and CI surface in an empty repo. No compiler
content yet.

## Next Up

**M.4 — Compiler migration.** Port compiler crates, stdlib, VS Code
extension, language spec, and conformance tests from the prior
working tree, renamed under the `bock` identity.

## v1.0 Release Criteria

- Stable language specification (frozen surface syntax + semantics)
- Conformance suite at 100% pass on supported targets
- Targets at parity: JS, TS, Python, Rust, Go
- Stdlib coverage for I/O, collections, async, errors, time
- VS Code extension with diagnostics, completion, hover, format
- Documentation site live with tutorial, reference, and stdlib docs
- `bock` binary distributed via crates.io and GitHub Releases
- Effect system fully checked across all targets
- Module system stable across files and packages
- AI provider primitives in stdlib (per spec §17.8)

## v1.1 Planned Features

- Incremental compilation with persistent cache
- Language server protocol implementation (decoupled from VS Code)
- Additional codegen targets (candidates: Swift, Kotlin, C#)
- Package registry and dependency resolution
- Macro system (syntactic, hygienic)

## v2 Vision

- Self-hosting compiler (Bock written in Bock)
- Native target via LLVM backend
- WebAssembly target with first-class browser bindings
- Distributed type-checking for monorepo scale
