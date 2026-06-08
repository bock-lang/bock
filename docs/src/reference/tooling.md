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

> **v1 note (per-module output).** The per-module mirrored tree above is
> the v1 output model (DQ19 resolved, §20.6.1), and **all five targets**
> emit it: a cross-module program compiles to one file per reached module
> (keyed on each module's *declared* path — `module core.option` ⇒
> `core/option.<ext>`), wired with the target's native imports/modules,
> plus the minimum manifest each needs to run as a project. A program that
> imports nothing emits only its own entry module.
>
> - **Python** — package imports (`from core.option import …`), plus a
>   shared `_bock_runtime.py` for the Optional/Result/Ordering/concurrency
>   runtimes. Runs as `python3 main.py` from the build root, where `core`
>   resolves as a namespace package.
> - **JS / TS** — ES-module imports, public declarations `export`ed, plus a
>   shared `_bock_runtime.{js,ts}` for the concurrency/range helpers (and, for
>   TS, the Optional/Result runtime types). The relative import specifier
>   differs by target: **JS** references the emitted `.js`
>   (`import { … } from "./core/option.js"`), while **TS** references the
>   emitted `.ts` source directly (`import { … } from "./core/option.ts"`) so
>   the tree runs verbatim under `node --experimental-strip-types main.ts`
>   (whose loader resolves specifiers as written and never rewrites
>   `.js`→`.ts`). A `package.json` (`{"type":"module"}` + test script + the
>   selected test-framework dev-dependency) is emitted at the
>   `build/<target>/` root so Node treats the tree as ES modules; TS also gets
>   a `tsconfig.json` with `rewriteRelativeImportExtensions` (TypeScript ≥ 5.7),
>   which lets `tsc` accept the `.ts` specifiers and rewrite them to `.js` on
>   emit. JS runs as `node main.js`; TS compiles the project with `tsc -p .`
>   (honoring the scaffolded `tsconfig.json`) then runs `node main.js`.
> - **Rust** — a real Cargo crate: a `Cargo.toml` (`[package]` + a
>   `[[bin]]`, plus an empty `[workspace]` table so the crate is its own
>   workspace root and builds even when the output lands inside a parent
>   Cargo workspace) plus a `src/`-rooted module tree (`src/main.rs`,
>   `src/core/option.rs`, the `mod`/`pub mod` wiring files), with
>   cross-module references resolved by `use crate::core::option::…;`. The
>   `Cargo.toml` carries a `tokio` dependency whenever the emitted Rust uses
>   it — the `Channel`/`spawn` concurrency runtime (which lives once in
>   `src/bock_runtime.rs`) or a bare host `sleep(..)` with no installed
>   `Clock` handler (which lowers to `tokio::time::sleep` under a
>   `#[tokio::main]` async `fn main`); a program that uses neither gets a
>   dependency-free crate. Built and run with `cargo run` from `build/rust/`.
> - **Go** — a real Go module: a `go.mod` (module path + go version) plus
>   flat per-module files all in one `package main` (`main.go`,
>   `core_option.go`, …, and a shared `bock_runtime.go` for the
>   Optional/Result/concurrency/range runtimes). Same-package symbols are
>   visible across files without an import. Run with `go run .` over
>   `build/go/`.
>
> In project mode (the default) these manifests are the *rich* project-mode
> scaffolding (test-framework references, formatter/linter configs, a README
> first-contact) described under [Project-Mode Scaffolding](#project-mode-scaffolding)
> below; `--source-only` emits none of them. Project mode also writes the
> transpiled `@test` *files* (see [Transpiled Tests](#transpiled-tests)) so the
> scaffolded project's own test runner exercises them.

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

### Project-Mode Scaffolding

In project mode (the default), `bock build` writes target-ecosystem
scaffolding alongside the transpiled source so the output is runnable in
the target's normal toolchain. Each target gets a manifest referencing its
test framework, a formatter config where applicable, an opt-in linter
config, and a `README.md` first-contact. The choices come from the
per-target `[targets.<T>]` (deep) and `[targets.<T>.scaffolding]` (shallow)
tables in `bock.project` (see [Project Schema](./project-schema.md));
anything left unset takes the target-appropriate default below.

| Target | Manifest | Test framework (default, alternatives) | Formatter (default) | Linter config (opt-in) | Package-manager hint (default) |
| ------ | -------- | -------------------------------------- | ------------------- | ---------------------- | ------------------------------ |
| JS     | `package.json` | Vitest, Jest | Prettier (`.prettierrc.json`; `none` disables) | ESLint (`eslint.config.js`) | npm, pnpm, yarn |
| TS     | `package.json` + `tsconfig.json` | Vitest, Jest | Prettier | ESLint | npm, pnpm, yarn |
| Python | `pyproject.toml` | pytest, unittest | Black (`[tool.black]`), Ruff format (`[tool.ruff.format]`), `none` | Ruff check, Pylint | pip, Poetry, uv |
| Rust   | `Cargo.toml` | cargo test (universal) | rustfmt (universal — always on, no config) | Clippy (`clippy.toml`) | (cargo only) |
| Go     | `go.mod` | `go test` / stdlib `testing` (universal) | gofmt (universal — always on, no config) | golangci-lint (`.golangci.yml`) | (go mod only) |

Notes:

- **Test framework** (deep config) changes what Bock emits. For Rust and
  Go the framework is universal (`cargo test` / `go test`), so it is not
  user-selectable; selecting one for those targets is a build error.
- **Formatter** (deep config). Prettier (js/ts) and Black/Ruff (python)
  emit a config file; `formatter = "none"` suppresses it. rustfmt and
  gofmt are universal/always-on and emit no config.
- **Linter** (shallow config) is **opt-in**: a config file is emitted only
  when `[targets.<T>.scaffolding].linter` is set. Omitting it emits no
  linter config.
- **Package manager** (shallow config) affects only the README's
  install/test commands; it does not change the emitted code.

Unknown values in either table produce a build error pointing at the
documented options for that target. See §20.6.2 for the normative matrix.

### Transpiled Tests

Project mode transpiles every Bock `@test` function into the target's
idiomatic test framework, placed where that framework looks for tests and
wired into the scaffolded project so the project's own runner executes them.
The `expect(actual).to_equal(expected)` assertion DSL (and the
`to_be_true`/`to_be_false`/`to_be_some`/`to_be_none`/`to_be_ok`/`to_be_err`
predicates) lower to each framework's native assertion idiom. `@test`
functions are emitted **only** into these test files — never into the runtime
module tree.

| Target | Test file | Shape | Run with |
| ------ | --------- | ----- | -------- |
| JS     | `bock.test.js` | `describe`/`it` + `expect(...).toEqual/toBe(...)` | `npm test` (Vitest, or Jest when `test_framework = "jest"`) |
| TS     | `bock.test.ts` | same as JS (imports the emitted `.js`) | `npm test` |
| Python | `test_bock.py` | `def test_*()` with `assert` (pytest), or a `unittest.TestCase` when `test_framework = "unittest"` | `pytest` / `python -m unittest` |
| Rust   | `src/bock_tests.rs` (inline `#[cfg(test)] mod`, wired from `src/main.rs`) | `#[test]` fns with `assert_eq!`/`assert!` | `cargo test` |
| Go     | `bock_test.go` (`package main`) | `func TestXxx(t *testing.T)` with `if … { t.Errorf(...) }` | `go test ./...` |

The test-framework variant follows the deep-config `test_framework` field
(defaulting per §20.6.2). The emitted JS/TS/Python imports reference the
functions under test by name from their generated modules; the Rust inline
module and the Go same-package file see the program's items directly.

Example — this Bock source:

```bock
public fn add(a: Int, b: Int) -> Int {
  a + b
}

@test
fn test_add_works() {
  expect(add(1, 2)).to_equal(3)
}
```

transpiles (rust) to `src/bock_tests.rs`:

```rust
#[cfg(test)]
mod bock_tests {
    use super::*;

    #[test]
    fn test_add_works() {
        assert_eq!(add(1, 2), 3);
    }
}
```

and (Python, pytest) to `test_bock.py`:

```python
from main import add

def test_add_works():
    assert (add(1, 2)) == (3)
```

**Formatter-clean output.** The emitted code passes its target's formatter
cleanly — the §20.6.2 codegen-formatter agreement, enforced as a
release-readiness check:

| Target | Formatter gate |
| ------ | -------------- |
| Rust   | `rustfmt --check` (universal/always-on) |
| Go     | `gofmt -l` (universal/always-on) |
| JS/TS  | `prettier --check` (against the scaffolded `.prettierrc.json`) |
| Python | `black --check` (against the scaffolded `[tool.black]`) |

For **Rust and Go the formatter is universal and always-on**, so the
*entire* emitted tree (runtime source, entry wiring, scaffolding, and
transpiled tests) is guaranteed `rustfmt --check` / `gofmt -l`-clean.
Project-mode builds achieve this with a **post-emit formatter pass**: after
codegen writes the files, `bock build --target go` runs `gofmt -w` over the
output directory and `bock build --target rust` runs `rustfmt` over the
emitted `.rs` files. Both formatters ship with the target toolchain the build
already invokes to validate, so they are always present; if a formatter is
somehow absent the pass is skipped with a warning rather than failing the
build. The pass runs in **project mode** only — `--source-only` emits bare,
unformatted transpilation for integration into a project the user already
manages. Go output has no source-map dependency (the `//# sourceMappingURL`
affordance is JS/TS-only), so reformatting it is always safe.

JS/TS/Python formatting is **user-optional** (the user's configured
Prettier/Black, run on demand); Bock emits code that passes those formatters'
`--check` on first generation but does not reflow it post-emit (post-emit
Prettier would break JS/TS source maps).

**Run-verified, all five targets.** The compiler's own CI (the Linux test lane)
provisions the JS/TS/Python test runners and formatters and *runs* every
target's transpiled tests — `cargo test`, `go test`, `npm test` (Vitest),
`pytest`, and `python -m unittest` — asserting they pass, and gates each
target's emitted output with the formatter above (for Rust and Go the gate
covers the *full emitted tree* via `rustfmt --check` / `gofmt -l`, not just the
test file). The harness is
skip-if-absent for dev hosts that lack a given toolchain; the CI lane sets
`BOCK_PROJECTMODE_REQUIRE=all` so an absent runner/formatter fails the build
instead of silently skipping.

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
