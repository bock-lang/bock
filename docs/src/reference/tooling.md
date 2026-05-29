# Build & Tooling

This page covers the parts of the toolchain that go beyond a single
command-line flag: the build system and its output modes, the
interactive REPL, the formatter, the language server, and the testing
and debugging surfaces. The per-command flag tables live on the
[CLI Reference](./cli.md) page; this page explains the behavior behind
them.

The tooling is specified in §20 of
[`spec/bock-spec.md`](../../../spec/bock-spec.md). Docs explain; the
spec defines — for normative behavior, follow the section references.

## Build System

`bock build` runs the pipeline:

```text
Parse → Type Check → Context Resolve → Target Analyze →
Code Generate → Verify → Target Compile → Assemble Deliverable
```

In v1 the build system supports **incremental compilation** at module
granularity (via content hashing), **parallel builds** across
packages, and **per-target output isolation**. Remote cache reuse,
build hooks (Bock scripts), and distributed CI builds are
[Reserved for v1.x](#reserved-for-v1x) (§20.6).

The supported targets are `js`, `ts`, `python`, `rust`, and `go`.
When `-t/--target` is omitted, `bock build` defaults to `js`;
`--all-targets` builds all five.

### Output Layout

Build output mirrors the source filesystem structure. A source file
at `src/<path>.bock` produces output at `build/<target>/<path>.<ext>`,
where `<ext>` is the target's idiomatic extension — `src/foo/bar.bock`
becomes `build/js/foo/bar.js`, `build/py/foo/bar.py`, and so on.
Target-ecosystem scaffolding (manifests, package descriptors, entry
points) is generated at the `build/<target>/` root per the target's
conventions. By default `src/main.bock` is the entry point if present.
See §20.6.1.

### Output Modes

`bock build` produces output in one of three **modes**, selected by
flag. (Modes describe output *completeness*; they are distinct from
the AI-involvement *tiers* of §17.2.) See §20.6.2.

| Mode             | Flag             | Output                                                              |
| ---------------- | ---------------- | ------------------------------------------------------------------ |
| **Source**       | `--source-only`  | Bare transpiled source files mirroring the project structure — no manifests, scaffolding, or entry-point wiring. For integrating into an existing target-language project. |
| **Project**      | (default)        | Source plus target-ecosystem scaffolding: package descriptors, entry-point wiring, formatter configs, README. Runnable in the target's normal toolchain (`npm test`, `cargo run`, `python -m`, `go test`). |
| **Deliverable**  | `--deliverable`  | A final runnable artifact for production deployment (APK, IPA, Docker image, deployment package). See §17.5. **Reserved for v1.x** (see below). |

```bash
bock build -t rust                    # project mode (default)
bock build -t js --source-only        # source mode
```

**Project mode includes transpiled tests** by default: after `bock
build --target js`, running `npm test` executes your Bock `@test`
functions as Vitest/Jest tests; `cargo test`, `pytest`, and `go test`
do the same on their targets. If a test passes on the interpreter but
fails on a target, that is a transpiler bug, not a bug in your code
(§20.4). Use `--no-tests` to opt out of test inclusion (vendor
distribution, library-only consumers, security-sensitive contexts).

> **v1 vs Reserved.** Source mode (`--source-only`) and project mode
> (the default) ship in v1 and have working flags. Deliverable mode
> (`--deliverable`) and the test-inclusion opt-out (`--no-tests`) are
> described by §20.6.2 but the flags are **not present** in the v1
> `bock build --help`; treat them as Reserved for v1.x. See the
> [CLI Reference](./cli.md#bock-build) for the flags that exist today.

### Source Maps

`bock build` emits source-map files alongside generated code by
default (`--source-map`; suppress with `--no-source-map`). Source maps
let you debug transpiled output in the target language's own debugger
(Node.js inspector, py-spy, rust-gdb, and so on). See
[Debugging](#debugging).

## REPL

`bock repl` starts an interactive evaluator. Besides ordinary
expressions and statements, it accepts `:`-prefixed commands:

| Command            | Effect                                               |
| ------------------ | ---------------------------------------------------- |
| `:type <expr>`     | Show the inferred type of an expression.             |
| `:air <stmt>`      | Show the AIR (annotated IR) representation.          |
| `:target <T> <s>`  | Show target-specific output. (Stub in v1.)           |
| `:effects`         | Show registered effects.                             |
| `:context`         | Show current variable bindings.                      |
| `:load <file>`     | Load and execute a Bock file.                        |
| `:paste`           | Enter multi-line paste mode.                         |
| `:help` (`:h`)     | List the REPL commands.                              |
| `:quit` (`:q`)     | Exit the REPL.                                       |

```text
$ bock repl
Bock REPL v0.1.0
Type :help for available commands, :quit to exit.

> :type 1 + 2
Int
> :quit
```

`:target` is a stub in v1: it acknowledges the command but does not
yet emit per-target output in the REPL. The aspect surface it would
display is the same one `bock build` produces.

## Formatter

`bock fmt` applies a single canonical style with **zero
configuration** (§20.2). There is nothing to configure and no style
options:

- 2-space indentation
- 80-character soft limit, 100 hard limit
- Opening brace on the same line
- Newline-terminated statements (semicolons optional)
- Trailing commas in multi-line constructs
- Sorted imports (core → std → external → local)
- Consistent wrapping for long signatures

`bock fmt` rewrites files in place; `bock fmt --check` reports drift
without modifying files (use it in CI).

## Language Server (LSP)

`bock lsp` starts the Bock language server, speaking LSP over stdio.
Point any LSP client (VS Code, Neovim, Emacs `lsp-mode`, etc.) at the
`bock lsp` command. v1 ships a **full LSP implementation** of the
standard protocol capabilities:

- Completion
- Hover
- Go-to-definition
- Diagnostics

`--stdio` is accepted for convention but stdio is already the default
and only transport in v1.

### Reserved for v1.x: Bock-specific extensions

Five Bock-specific LSP extensions are planned but **not in v1**
(§20.3). They are preserved as design intent:

| Extension            | What it would provide                                                            |
| -------------------- | -------------------------------------------------------------------------------- |
| AI Context Panel     | Live view of what the AI transpiler sees at the cursor — context, capabilities, effects, ownership, active handlers. |
| Target Preview       | Live transpiled output for any function, switchable between targets.             |
| Capability Graph     | Visual call-graph with capability and effect propagation.                        |
| Smart Completions    | Ownership-, effect-, and pipe-aware completions.                                 |
| Inline Diagnostics   | Ownership-transfer warnings, capability-narrowing hints, AI-decision previews.   |

## Testing Tiers

`bock test` runs on the interpreter — **Tier 1** (semantic) tests:
fast, target-independent, and the canonical semantics reference. Two
further tiers are defined by §20.4:

| Tier   | What runs                                                                 | v1 status            |
| ------ | ------------------------------------------------------------------------- | -------------------- |
| Tier 1 | Semantic tests on the interpreter.                                        | Ships in v1 (`bock test`). |
| Tier 2 | The same tests compiled to a target language and run there.               | Emitted by project-mode `bock build`; the cross-target *test runner flags* are Reserved. |
| Tier 3 | Platform-specific integration tests (`@target`, `@platform` annotated).   | Reserved for v1.x.   |

The principle: *semantic pass + target fail = transpiler bug, not user
code bug.* Project mode makes this empirically checkable — the
transpiled Tier 2 tests run in your CI under `npm test` / `cargo test`
/ `pytest` / `go test`.

Bock owns cross-target **correctness**, not performance. There is no
`@benchmark` annotation and no built-in benchmark surface: every
target ships mature benchmark tooling (`cargo bench`,
`pytest-benchmark`, `npm run bench`, `go test -bench`), and you
benchmark the transpiled output with the target's native tools.

Cross-target test execution from `bock test` (`--target`,
`--all-targets`, `--smart`), coverage, and snapshot testing are
[Reserved for v1.x](#reserved-for-v1x).

## Debugging

v1 ships **source-map generation** (`--source-map`, on by default;
`--no-source-map` to suppress). Source maps enable debugging the
transpiled output through standard target-specific tooling — the
Node.js inspector, py-spy, rust-gdb, and so on.

The built-in interpreter debugger UI (breakpoints, stepping,
expression evaluation, ownership-state inspection, effect-handler
display, context viewing) is **Reserved for v1.x** (§20.5).

## Reserved for v1.x

Summary of the tooling surfaces described above that are **not in
v1**:

- **Build:** remote cache reuse, build hooks (Bock scripts),
  distributed CI builds (§20.6); deliverable mode
  (`--deliverable`) and the `--no-tests` opt-out (§20.6.2 — flags not
  present in v1 `bock build --help`).
- **REPL:** `:target` emits no per-target output yet (stub).
- **LSP:** the five Bock-specific extensions (§20.3).
- **Testing:** Tier 3 integration tests; cross-target test execution,
  coverage, and snapshot testing (§20.4).
- **Debugging:** the built-in interpreter debugger UI (§20.5).

For Reserved command-line surfaces see the
[CLI Reference](./cli.md#reserved-for-v1x); for Reserved project-file
fields see the [Project Schema](./project-schema.md#reserved-for-v1x).
