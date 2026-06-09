# CLI Reference

The `bock` binary is a single tool that contains the whole toolchain:
build, run, check, format, package management, AI-decision management,
documentation, and the language server. This page documents every
subcommand and flag that ships in **v1**, with a short runnable
example for each, and a final section listing the surfaces that are
**Reserved for v1.x**.

This page explains the CLI; the spec defines it. For the normative
capability list see §20.1 of
[`spec/bock-spec.md`](../../../spec/bock-spec.md). The build system,
REPL, formatter, LSP, and testing tiers have their own page
([Build & Tooling](./tooling.md)); the project manifest has
[its own page](./project-schema.md).

> The authoritative, always-current command list is `bock --help`,
> and every subcommand prints its own flags with
> `bock <subcommand> --help`. The text below matches the v1 binary
> (`bock 0.1.0`); if the two ever disagree, `--help` wins and this
> page is the bug.

## Subcommands at a Glance

| Command       | What it does                                              |
| ------------- | --------------------------------------------------------- |
| `bock new`    | Scaffold a new project at `<name>/`.                      |
| `bock build`  | Transpile and (optionally) compile a project.             |
| `bock run`    | Execute a program through the interpreter.                |
| `bock check`  | Type-check, lint, and validate context without building.  |
| `bock test`   | Run tests on the interpreter.                             |
| `bock fmt`    | Format `.bock` files (one canonical style).               |
| `bock repl`   | Start the interactive evaluator.                          |
| `bock inspect`| Browse AI decisions, caches, and dump the AIR for a file. |
| `bock pin`    | Pin AI decisions so they replay deterministically.        |
| `bock unpin`  | Remove pin metadata from a decision.                      |
| `bock override`| Override or promote an AI decision.                      |
| `bock cache`  | Manage the on-disk AI / decision / rule caches.           |
| `bock promote`| Analyze (and optionally raise) the project strictness.    |
| `bock pkg`    | Package management (dependencies, lockfile, tarball cache).|
| `bock doc`    | Generate API documentation.                               |
| `bock model`  | Query or set AI model configuration.                      |
| `bock lsp`    | Start the language server over stdio.                     |

`bock help` and `bock help <command>` print the same information as
`--help`. The two global options are `-h`/`--help` and `-V`/`--version` (`bock --version` prints `bock 0.1.0`).

## Build and Execute

### `bock new <NAME>`

Scaffold a new project at `<NAME>/`. Generates `bock.project` (with a
commented-out, opt-in `[ai]` block), `.gitignore`, `src/main.bock`,
and an empty `tests/` directory. See the
[Project Schema](./project-schema.md) page for the generated layout
and §20.7 for the scaffolding contract.

```bash
bock new my-app
cd my-app
bock run
```

| Argument | Meaning             |
| -------- | ------------------- |
| `<NAME>` | Project directory.  |

### `bock build`

Transpile, and unless told otherwise compile, a project. By default it
produces a scaffolded target-language project (*project mode*); see
[output modes](./tooling.md#output-modes) for the three modes and
§20.6 for the build system.

```bash
bock build -t rust              # transpile + scaffold a Rust project
bock build -t js --source-only  # emit JS source only, no scaffolding
bock build --all-targets        # build every supported target
```

| Flag                  | Meaning                                                                                |
| --------------------- | -------------------------------------------------------------------------------------- |
| `-t`, `--target <T>`  | Target language: `js`, `ts`, `python`, `rust`, or `go`. Defaults to `js` when omitted. |
| `--all-targets`       | Build every supported target (`js`, `ts`, `python`, `rust`, `go`).                     |
| `--release`           | Enable release optimizations.                                                          |
| `--source-only`       | Emit generated source without invoking the target toolchain (*source mode*).           |
| `--deterministic`     | Use only rule-based codegen; skip AI-assisted generation. Alias: `--no-ai`.            |
| `--strict`            | Force production strictness for this build; fails if any build-scope decision is unpinned. |
| `--pin-all`           | After a successful build, pin every build-scope decision under `.bock/decisions/build/`. |
| `--source-map`        | Emit source-map files alongside generated code (default: on).                          |
| `--no-source-map`     | Suppress source-map output.                                                            |

The `--pin-all` / `--strict` pair drives the develop → ship workflow:
build with `--pin-all` in development, commit the resulting pins, then
ship with `--strict`. See §17.4 for the pinning model and
[Build & Tooling](./tooling.md) for source maps.

### `bock run [FILE] [-- ARGS…]`

Build and execute a program through the interpreter. Arguments after
`--` are passed to the Bock program.

```bash
bock run                        # run the project entry point
bock run src/main.bock -- --verbose
```

| Argument / Flag    | Meaning                                                          |
| ------------------ | ---------------------------------------------------------------- |
| `[FILE]`           | Entry file. Defaults to the project entry point.                 |
| `[-- ARGS…]`       | Arguments forwarded to the running program.                      |
| `--watch`          | Re-run on file changes. **Not yet implemented** (accepted no-op).|

### `bock check [FILES…]`

Type-check, lint, and validate the context system without building.
With no arguments it checks every `.bock` file in the current
directory; pass paths to narrow it.

```bash
bock check                         # check the whole directory
bock check src/main.bock           # check one file
bock check --only=types            # type checking only
bock check --only=types,context    # two aspects
bock check --brief                 # compact one-line diagnostics
bock check --strict                # production strictness (gaps are errors)
```

| Flag               | Meaning                                                                                       |
| ------------------ | --------------------------------------------------------------------------------------------- |
| `--only <ASPECT>`  | Restrict to specific aspects. Valid v1 aspects: `types`, `context`. Comma-separated or repeated. |
| `--brief`          | Compact, one-line diagnostics with no source-context snippets.                                |
| `--strict`         | Force production strictness; turns completeness *warnings* into *errors*. Mirrors `bock build --strict`. |

`bock check` exits non-zero **if and only if** it produces at least
one error; warnings never fail the check. At the default development
strictness a public declaration missing `@context` is a warning (exit
0); under `--strict` the same gap is an error (non-zero exit).
Unknown `--only` values are rejected with the list of valid aspects:

```text
$ bock check --only=bogus
error: unknown check aspect 'bogus'. Valid aspects: types, context
```

The `--only` aspect surface — what `types` and `context` cover, the
per-item (not per-module) completeness rule, and the strictness
ladder — is specified in §20.1.1. Module-level `@context`
completeness and the `lint` aspect (`--only=lint`) are
[Reserved for v1.x](#reserved-for-v1x).

### `bock test [FILES…]`

Run `@test` functions on the interpreter (fast, target-independent —
the canonical semantics; see §20.4). With no arguments it discovers
every `.bock` file recursively.

```bash
bock test                         # run all tests
bock test --filter parse          # only tests whose name matches "parse"
```

| Flag                | Meaning                          |
| ------------------- | -------------------------------- |
| `--filter <FILTER>` | Run only tests matching pattern. |

Cross-target test execution (`--target`, `--all-targets`, `--smart`),
coverage, and snapshot testing are
[Reserved for v1.x](#reserved-for-v1x); see §20.4.

### `bock fmt`

Format `.bock` files in place using the single canonical style (no
configuration; see §20.2 and [Build & Tooling](./tooling.md#formatter)).

```bash
bock fmt                          # format in place
bock fmt --check                  # CI: exit non-zero if any file would change
```

| Flag      | Meaning                                            |
| --------- | -------------------------------------------------- |
| `--check` | Report formatting drift without modifying files.   |

### `bock repl`

Start the interactive evaluator. Supports special `:`-commands
(`:type`, `:air`, `:target`, `:effects`, `:context`, `:load`,
`:paste`, `:help`, `:quit`). See
[Build & Tooling — REPL](./tooling.md#repl) for the command list.

```bash
bock repl
```

`bock repl` takes no flags in v1.

## AI Decision and Cache Management

Bock records every AI-assisted code-generation decision in a manifest
so builds can replay deterministically. These commands browse and
manage those decisions and the caches behind them (§17.4, §10.8).

### `bock inspect [decisions|decision|cache|rules|air] [FILTERS]`

Read-only browsing of AI decisions, the rule cache, and the AI
response cache, plus compiler introspection (`air`). With no
subcommand it lists build-scope decisions.

```bash
bock inspect                      # list build decisions
bock inspect --all                # build + runtime, with prefixed ids
bock inspect --unpinned           # only decisions not yet pinned
bock inspect decision build:abc123
bock inspect cache                # summarise the AI response cache
bock inspect rules --target rust  # learned codegen rules for one target
bock inspect air src/main.bock    # dump the lowered AIR tree for one file
```

| Subcommand  | What it shows                                              |
| ----------- | --------------------------------------------------------- |
| `decisions` | Decisions matching the filters (the default subcommand).  |
| `decision`  | One decision in detail (`<ID>`; prefixed or bare).        |
| `cache`     | A summary of the on-disk AI response cache.               |
| `rules`     | Learned codegen rules.                                    |
| `air`       | The lowered AIR tree for one source file (see below).     |

| Filter (on `inspect` / `inspect decisions`) | Meaning                                          |
| -------------------------------------------- | ------------------------------------------------ |
| `--runtime`                                  | Runtime-scope decisions only.                    |
| `--all`                                      | Both build and runtime decisions, prefixed ids.  |
| `--unpinned`                                 | Only decisions not yet pinned.                   |
| `--module <MODULE>`                          | Filter by module-path substring.                 |
| `--type <TYPE_FILTER>`                       | Filter by decision type (e.g. `codegen`, `repair`).|
| `--json`                                     | Machine-readable JSON instead of the table.      |

`inspect cache --size` always reports on-disk size; `inspect rules
--target <T>` scopes to one target; `inspect decision <ID> --json`
emits JSON.

### `bock inspect air <FILE>`

Compiler introspection rather than decision browsing: run the
frontend (lex, parse, name resolution, AIR lowering) on a single
file and dump the resulting AIR tree. Type checking does not run —
the output is structure plus spans. The embedded core stdlib is
loaded exactly as in `bock check`, so `use core.*` imports resolve
the same way. Exit code mirrors `bock check`: 0 when the file
lowers cleanly, 1 on any frontend error.

```bash
bock inspect air src/main.bock         # human-readable indented tree
bock inspect air src/main.bock --json  # machine-readable JSON tree
```

| Argument / Flag | Meaning                                          |
| --------------- | ------------------------------------------------ |
| `<FILE>`        | The `.bock` file to lower.                       |
| `--json`        | Emit the stable JSON tree instead of the human view. |

The default view prints one node per line — kind, name (when the
node has one), `@line:col` of the node's start, and its byte range:

```text
Module Demo @1:1 (0..53)
  FnDecl add @3:1 (13..52)
    Param @3:8 (20..27)
      BindPat a @3:8 (20..21)
      TypeNamed Int @3:11 (23..26)
    ...
    Block @3:31 (43..52)
      BinaryOp @3:33 (45..50)
        Identifier a @3:33 (45..46)
        Identifier b @3:37 (49..50)
```

**JSON shape (stable contract).** With `--json`, stdout carries a
single JSON object — the root `Module` node. Every node has exactly
four fields (object key order is not significant):

```json
{
  "kind": "FnDecl",
  "name": "add",
  "span": { "start": 13, "end": 52, "line": 3, "col": 1 },
  "children": []
}
```

- `kind` — the AIR node kind, e.g. `"Module"`, `"FnDecl"`,
  `"BinaryOp"`, `"BindPat"`.
- `name` — the node's source-level name when it has one
  (declaration names, identifier references, field/method names,
  dotted module/type paths, literal text), otherwise `null`.
- `span` — `start`/`end` are byte offsets into the file (`end`
  exclusive); `line`/`col` are the 1-based line and column (column
  counted in characters) of `start`. Compiler-synthesized nodes
  report `0..0`.
- `children` — the node's AIR children in traversal order (may be
  empty).

On failure (unreadable file, lex/parse/name-resolution errors) the
command exits 1. Without `--json` the standard diagnostics render
on stderr; with `--json`, stdout carries a JSON error object
instead of a tree — consumers distinguish the two by the top-level
`error` key:

```json
{
  "error": {
    "message": "parsing failed",
    "diagnostics": [
      {
        "severity": "error",
        "code": "E2000",
        "message": "expected `)`, found `{`",
        "span": { "start": 3, "end": 4, "line": 1, "col": 4 }
      }
    ]
  }
}
```

This JSON shape is the contract consumed by editor tooling (the
VS Code extension's AIR tree viewer); it only changes additively.
For full diagnostics (types, ownership, effects, context), use
[`bock check`](#bock-check-files) — `inspect air` reports only the
errors that stop lowering.

### `bock pin [DECISION]`

Pin a decision so it replays deterministically. Pass a single
decision id, or a bulk flag.

```bash
bock pin build:abc123 --reason "reviewed in PR #42"
bock pin --all-build              # pin every unpinned build decision
```

| Argument / Flag        | Meaning                                                         |
| ---------------------- | -------------------------------------------------------------- |
| `[DECISION]`           | Decision id (`build:id` / `runtime:id` or bare). Omit with a bulk flag. |
| `--all-in <SUBSTRING>` | Pin every unpinned decision whose module path contains the substring. |
| `--all-build`          | Pin every unpinned build-scope decision.                       |
| `--all-runtime`        | Pin every unpinned runtime-scope decision.                     |
| `--reason <REASON>`    | Free-form reason recorded on the pin metadata.                 |

### `bock unpin <DECISION>`

Clear pin metadata from a single decision.

```bash
bock unpin build:abc123
```

| Argument     | Meaning                              |
| ------------ | ------------------------------------ |
| `<DECISION>` | Decision id (prefixed or bare).      |

### `bock override [DECISION] [NEW_CHOICE]`

Override or promote an AI decision in the manifest. With a bare id it
pins the decision in place; with a replacement value (positional or
`--from-file`) it replaces the decision's `choice` and auto-pins; with
`--promote` it copies a pinned runtime decision into the build
manifest (the §10.8 promotion path).

```bash
bock override build:abc123 "useArrayBuffer"          # replace + pin
bock override build:abc123 --from-file choice.txt    # replace from file
bock override runtime:def456 --promote               # runtime → build
```

| Argument / Flag        | Meaning                                                          |
| ---------------------- | --------------------------------------------------------------- |
| `[DECISION]`           | Decision id to operate on.                                      |
| `[NEW_CHOICE]`         | Inline replacement for the decision's `choice`.                 |
| `--from-file <FILE>`   | Read the replacement choice from a file.                        |
| `--runtime`            | Treat `DECISION` as a runtime-scope id (when pinning without promoting). |
| `--promote`            | Promote a pinned runtime decision into the build manifest (§10.8). |
| `--reason <REASON>`    | Free-form reason recorded alongside the pin.                    |

### `bock cache [stats|clear]`

Manage the on-disk caches: the AI response cache, the decision
manifests, and the rule cache. (This is distinct from `bock pkg
cache`, which manages the dependency tarball cache.)

```bash
bock cache stats                  # summary statistics
bock cache clear                  # wipe the AI response cache (default)
bock cache clear --rules          # wipe the local rule cache instead
bock cache clear --decisions --runtime   # wipe only the runtime manifest
```

| Subcommand | Flags                                                   | Meaning                                      |
| ---------- | ------------------------------------------------------- | -------------------------------------------- |
| `stats`    | —                                                       | Summary stats for AI cache, manifests, rules.|
| `clear`    | (default)                                               | Wipe the AI response cache.                  |
| `clear`    | `--decisions` (with optional `--runtime` / `--build`)   | Wipe decision manifests (scope-restrictable).|
| `clear`    | `--rules`                                               | Wipe the local rule cache.                   |

The v1.x `bock cache list` and `bock cache prune` subcommands are
[Reserved](#reserved-for-v1x).

## Project Lifecycle

### `bock promote`

Analyze the project at the *next* strictness level (sketch →
development → production) and report issues that would fail there.
With `--apply` it fixes simple cases and bumps the project's
`[strictness] default`. See §10.7 and the
[Project Schema](./project-schema.md#strictness) page.

```bash
bock promote                      # report only (the default; same as --check)
bock promote --apply              # fix safe cases + bump [strictness] default
```

| Flag      | Meaning                                                                       |
| --------- | ----------------------------------------------------------------------------- |
| `--apply` | Apply safe fixes and update `bock.project` after a clean check.               |
| `--check` | Report issues without modifying anything (the default behavior).              |

### `bock doc [PATH]`

Generate API documentation for a file or directory (defaults to the
current directory).

```bash
bock doc                          # document cwd → ./docs (markdown)
bock doc src --format html --output public/api
```

| Argument / Flag      | Meaning                                                          |
| -------------------- | --------------------------------------------------------------- |
| `[PATH]`             | File or directory to document. Defaults to cwd.                 |
| `--output <OUTPUT>`  | Output directory. Defaults to `<path>/docs`.                    |
| `--format <FORMAT>`  | `markdown` (default) or `html`.                                 |

### `bock pkg [init|add|remove|tree|list|cache]`

Package management: dependencies, the lockfile, and the tarball
cache. Dependencies live in `bock.package`, not `bock.project`
(see [Project Schema](./project-schema.md#bockpackage) and §19).

```bash
bock pkg init                     # initialize a package manifest
bock pkg add core-http -v "^1.0"  # add a dependency at a version
bock pkg add core-http --offline  # use only cached tarballs
bock pkg tree                     # show the dependency tree
bock pkg remove core-http
bock pkg cache clear              # empty the tarball cache (.bock/cache/)
```

| Subcommand     | Args / Flags                                              | Meaning                                  |
| -------------- | --------------------------------------------------------- | ---------------------------------------- |
| `init`         | —                                                         | Initialize a new package manifest.       |
| `add <NAME>`   | `-v, --version <REQ>`, `--offline`, `--registry <URL>`    | Add a dependency and update the lockfile.|
| `remove <NAME>`| —                                                         | Remove a dependency.                     |
| `tree`         | —                                                         | Print the dependency tree.               |
| `list`         | —                                                         | List dependencies.                       |
| `cache clear`  | —                                                         | Remove every tarball from `.bock/cache/`.|

The registry-publishing subcommands (`update`, `audit`, `publish`,
`search`) are [Reserved for v1.x](#reserved-for-v1x), shipping
alongside the public registry.

### `bock model [show|set]`

Query or set AI model configuration (the same settings expressible in
the `[ai]` block of `bock.project`; see
[Project Schema](./project-schema.md#ai)).

```bash
bock model show                   # current model configuration
bock model set provider anthropic
```

| Subcommand          | Args                          | Meaning                              |
| ------------------- | ----------------------------- | ------------------------------------ |
| `show`              | —                             | Show current model configuration.    |
| `set <KEY> <VALUE>` | configuration key and value   | Set a model configuration value.     |

Local-model management (`bock model list` / `install` / `use`) is
[Reserved for v1.x](#reserved-for-v1x) alongside local provider
support.

## Language Server

### `bock lsp`

Start the Bock language server, speaking LSP over stdio. Point an
editor's LSP client at this command. See
[Build & Tooling — LSP](./tooling.md#language-server-lsp) for the
v1 capability set.

```bash
bock lsp
bock lsp --stdio                  # same as above; --stdio is accepted for convention
```

| Flag       | Meaning                                                                   |
| ---------- | ------------------------------------------------------------------------- |
| `--stdio`  | Use stdio transport. It is already the default and only v1 transport; the flag is accepted for LSP-client compatibility. |

## Reserved for v1.x

These commands and flags appear in spec §20 as design intent but are
**not implemented in v1**. They are listed here so that the docs never
present a planned surface as available. Do not rely on them in v1.

| Surface                                            | Status / Notes                                                       |
| -------------------------------------------------- | -------------------------------------------------------------------- |
| `bock fix`                                          | Auto-fix for lint warnings; pairs with `bock check --only=lint`.     |
| `bock migrate`                                      | AI-assisted import from other source languages.                      |
| `bock target` (top-level)                           | Target management as a verb. In v1, customize via `--target` / `bock.project`. |
| `bock ci`                                           | Run all CI checks in one command.                                    |
| `bock check --only=lint`                            | The `lint` aspect ships with `bock fix`; the lint pass itself runs as part of the default `bock check`. |
| `bock test --target` / `--all-targets` / `--smart`  | Cross-target test execution (§20.4).                                 |
| `bock test` coverage / snapshot                     | Coverage and snapshot testing (§20.4).                               |
| `bock cache list` / `bock cache prune`              | Additional cache subcommands (§20.1).                                |
| `bock model list` / `install` / `use`              | Local model management, alongside local provider support.            |
| `bock pkg update` / `audit` / `publish` / `search` | Registry-publishing subcommands, alongside the public registry (§19).|
| `bock run --watch`                                  | Accepted but a no-op in v1 ("not yet implemented").                  |

For the project-file fields reserved for v1.x (`[paradigm]`,
`[effects]`, `[plugins]`, `[testing]`, `[build.hooks]`,
`[build.cache] remote`, and others), see the
[Project Schema](./project-schema.md#reserved-for-v1x) page and
Appendix A.3 of the spec.
