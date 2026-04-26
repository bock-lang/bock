# Getting Started

This page walks you from a fresh checkout to a running Bock program.

## Install

Bock currently ships from source. You will need:

- Rust 1.75 or newer (`rustup install stable`)
- Node.js 20+ (only for the VS Code extension)

Build the compiler:

```bash
git clone https://github.com/bocklang/bock
cd bock
cargo build --release
# The CLI is at ./target/release/bock
```

Add `target/release` to your `PATH`, or move the `bock` binary to a
directory already on it.

## Your First Project

```bash
bock new hello
cd hello
```

This scaffolds a project with `bock.project` (TOML metadata) and
`src/main.bock`:

```bock
module main

public fn main() {
  print("Hello, Bock!")
}
```

## Type-Check

```bash
bock check
```

Run with no arguments, `bock check` scans the current directory
recursively for `.bock` files and reports type and lint diagnostics.

## Build for a Target

```bash
bock build -t js              # JavaScript
bock build -t ts              # TypeScript
bock build -t python          # Python
bock build -t rust            # Rust
bock build -t go              # Go
```

The transpiled source is placed under `.bock/build/<target>/`.

For source-only output (no toolchain invocation):

```bash
bock build -t ts --source-only
```

## Run via the Interpreter

For quick iteration without target compilation:

```bash
bock run src/main.bock
```

## Where to Go Next

- The [Language Guide](./language-guide/types.md) covers the type
  system, effect inference, and the module system.
- The [CLI Reference](./reference/cli.md) lists every subcommand and
  flag.
- For the formal grammar and semantics, see `spec/bock-spec.md`.
