# Bock

> A feature-declarative programming language. Compiles to JavaScript, TypeScript, Python, Rust, and Go. No runtime to ship.

[![Status](https://img.shields.io/badge/status-pre--1.0-orange.svg)](ROADMAP.md)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

**[bocklang.org](https://bocklang.org)**

## What Bock is

A function in Bock declares what it does, what types it operates on, what
effects it has, and what guarantees it requires. Annotations and
signatures carry the intent; the body carries the work. The compiler
resolves the rest. A function annotated `@concurrent` becomes
`Promise.all` in JavaScript, `tokio::join!` in Rust, and goroutines in Go.
An effect declared with `with Log` becomes a structured logger parameter
in every target. The same source describes the program. The compiler
describes how each target should run it.

Most cross-platform languages add a runtime: a virtual machine, a
translation layer, a library your program depends on at execution time.
Bock does not. The output of a `bock build` is plain code in the target
language, ready to drop into your existing project. Run it on Node, ship
it as a Python package, link it into a Rust binary, deploy it as a Go
service. The Bock runtime supports development; it does not ship.

## Status

Bock is pre-1.0 and under active development. The compiler pipeline runs
end to end: the CLI (`bock`) scaffolds, checks, builds, runs, tests, and
formats projects; the interpreter executes source directly for fast
iteration; codegen emits all five targets as per-module native source
trees; the v1 standard library (11 `core` modules) executes across every
target; and the website and VS Code extension build from this repo. The
work in flight is codegen polish for real-world programs: a focused
conformance suite passes on all five targets, while the broader example
programs are being hardened target by target (tracked in
[`STATUS.md`](STATUS.md) and [`ROADMAP.md`](ROADMAP.md)).

AI participates in compilation, not at your program's runtime. The
pipeline is deterministic from parsing through type checking, ownership
analysis, effect tracking, and target lowering; an `[ai]` block in
`bock.project` is the opt-in seam where the compiler may consult an AI
model at capability gaps, with deterministic fallback and pinned replay
in production builds. This path is configured but not yet exercised in
real-world usage; Bock uses rule-based code generation by default.
Additional codegen targets (Java, C++, C#, Swift) are roadmap candidates
for after v1, not v1 commitments.

## A small Bock sample

A function declares its effects in the signature. Here `prepare` says it
uses `Logger`; the handler is injected at the call site rather than
imported as a global. The body is a pipeline.

```bock
module main

public record Document {
  text: String
  quality: Float
}

public effect Logger {
  fn log(message: String) -> Void
}

public fn normalize(doc: Document) -> Document {
  Document { text: doc.text.trim().to_lower(), quality: doc.quality }
}

public fn keep_quality(docs: List[Document]) -> List[Document] {
  docs.filter((d) => d.quality > 0.5)
}

public fn prepare(docs: List[Document]) -> List[Document] with Logger {
  log("preparing ${docs.length()} documents")
  docs
    |> keep_quality
    |> ((ds) => ds.map(normalize))
}
```

## Quick start

Bock ships as a single binary. Install with Cargo, or download a
pre-built binary from [GitHub Releases](https://github.com/bock-lang/bock/releases).

```bash
cargo install bock
bock --version
```

Scaffold a project, then check, build, and run it:

```bash
bock new hello
cd hello

bock check                 # type-check, lint, validate context
bock run                   # execute via the interpreter (no codegen)
bock build --target js     # emit JavaScript into build/js/
bock build --target go     # emit Go into build/go/
```

`bock new` generates `bock.project` (TOML), a `src/main.bock` entry
point, a `tests/` directory, and a `.gitignore`. The `bock.project`
includes a commented-out `[ai]` block; AI-assisted generation is opt-in.
The compiled output runs on its target with nothing from Bock imported at
runtime: `node build/js/main.js` prints the program's output directly.

The full walkthrough is at
[bocklang.org/get-started](https://bocklang.org/get-started).

## Repository layout

| Path | Contents |
|------|----------|
| `compiler/` | The compiler, a Cargo workspace of `bock-*` crates plus the `bock` CLI, and the conformance suite. |
| `spec/` | The language specification (`spec/bock-spec.md`) and its dated changelogs. |
| `stdlib/` | The Bock standard library (`core.*` modules), shipped as source. |
| `extensions/vscode/` | The VS Code extension, with vocabulary synced from the compiler. |
| `examples/` | Example Bock projects, from fundamentals to real-world shapes. |
| `docs/` | The mdBook documentation source. |
| `website/` | The bocklang.org marketing site (Astro). |
| `branding/` | Brand assets. Most content is gitignored; `branding/assets/logo/` is committed for contributor access. |

## Documentation

- **Website:** [bocklang.org](https://bocklang.org)
- **Get started:** [bocklang.org/get-started](https://bocklang.org/get-started)
- **Documentation:** [`docs/`](docs/src/SUMMARY.md)
- **Language specification:** [`spec/bock-spec.md`](spec/bock-spec.md)
- **Architecture overview:** [`ARCHITECTURE.md`](ARCHITECTURE.md)
- **Roadmap and status:** [`ROADMAP.md`](ROADMAP.md), [`STATUS.md`](STATUS.md)

## Contributing

Bock is pre-1.0 and contributions of every size are welcome. Start with
[`CONTRIBUTING.md`](CONTRIBUTING.md) for local setup and the workflow, and
[`ARCHITECTURE.md`](ARCHITECTURE.md) for a tour of the compiler pipeline.
Participation is governed by the [Code of Conduct](CODE_OF_CONDUCT.md).

## License

Bock is open source under the [MIT License](LICENSE).

## Community

- **Discussions:** [GitHub Discussions](https://github.com/bock-lang/bock/discussions)
- **GitHub:** [bock-lang/bock](https://github.com/bock-lang/bock) ([issues](https://github.com/bock-lang/bock/issues), [examples](https://github.com/bock-lang/bock/tree/main/examples))
- **Reddit:** [r/bocklang](https://reddit.com/r/bocklang)
- **Web:** [bocklang.org](https://bocklang.org)
