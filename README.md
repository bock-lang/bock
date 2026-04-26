# Bock

> A feature-declarative language that runs everywhere your team already does.

Bock lets you describe what your program should do in one source of
truth, then compiles it to JavaScript, TypeScript, Python, Rust, Go,
and more. One language, many targets, zero per-target rewrites.

[![Status](https://img.shields.io/badge/status-pre--1.0-orange.svg)](ROADMAP.md)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

## Quick Start

```bash
# Install (placeholder — coming with v0.1)
cargo install bock

# Create a project
bock new hello && cd hello

# Check, then build to your target
bock check
bock build -t js
```

## Features

- **Target-agnostic.** Write once, emit idiomatic JS / TS / Python / Rust / Go.
- **Feature-declarative.** Express intent, not boilerplate.
- **Strong static checking.** Catch type and effect errors before codegen.
- **Effect system.** Side effects are explicit and tracked.
- **First-class AI provider integration.** Built-in primitives for LLM-backed code paths.
- **Editor support out of the box.** VS Code extension shipped from the same repo.

## Links

- **Documentation:** [bocklang.org/docs](https://bocklang.org/docs) (coming soon)
- **Language specification:** [`spec/`](spec/)
- **Architecture overview:** [`ARCHITECTURE.md`](ARCHITECTURE.md)
- **Roadmap:** [`ROADMAP.md`](ROADMAP.md)
- **Contributing:** [`CONTRIBUTING.md`](CONTRIBUTING.md)
- **Code of Conduct:** [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md)

## License

[MIT](LICENSE)
