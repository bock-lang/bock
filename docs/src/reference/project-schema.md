# Project Schema

A Bock project is rooted at a `bock.project` file (TOML). Dependencies
are declared separately in `bock.package`. This page documents the
fields the v1 compiler actually reads, and clearly separates them from
the fields that the spec reserves for v1.x.

The authoritative field list is Appendix A of
[`spec/bock-spec.md`](../../../spec/bock-spec.md): A.1 (`bock.project`
v1), A.2 (`bock.package` v1), and A.3 (Reserved). Project scaffolding
is §20.7. Docs explain; the spec defines.

> v1's parser ignores unknown fields (and may warn on them under
> `production` strictness). Reserved fields should not appear in
> user-authored `bock.project` files.

## What `bock new` Generates

`bock new <name>` scaffolds the minimal project structure:

```text
my-app/
  bock.project      # [project] block + a commented-out [ai] block
  .gitignore
  src/
    main.bock       # fn main() { println("Hello, world!") }
  tests/            # empty
```

The generated `bock.project` is intentionally minimal:

```toml
[project]
name = "my-app"
version = "0.1.0"

# AI provider configuration (optional)
# Bock uses rule-based code generation by default. Configure an AI
# provider below to enable AI-assisted generation for capability gaps.
# See documentation for setup guides.
#
# [ai]
# provider = "openai-compatible"  # or "anthropic"
# endpoint = "..."
# model = "..."
# api_key_env = "..."
```

The `[ai]` block is generated **commented out**: Bock uses rule-based
codegen by default, and AI is opt-in. Uncomment and complete the block
to enable AI-assisted generation; delete it if you don't want it.
`bock new` does not prompt interactively and does not generate the
`[targets.*]` or `[strictness]` sections — projects rely on inference
and defaults for the common case (§20.7).

The generated `.gitignore` commits `.bock/decisions/build/` and
`.bock/rules/` (they are build inputs) while ignoring the runtime
decision log, the AI response cache, and the tarball cache:

```text
target/
.bock/decisions/runtime/
.bock/ai-cache/
.bock/cache/
```

## `bock.project` (v1)

The fields the v1 compiler reads:

### `[project]`

```toml
[project]
name = "my-app"
version = "0.1.0"
```

| Field     | Read by      | Notes                                                                 |
| --------- | ------------ | --------------------------------------------------------------------- |
| `name`    | scaffolding, docs | Project name; written by `bock new`, read by `bock doc`.         |
| `version` | scaffolding, docs | Project version.                                                  |

Appendix A.1 also lists `type = "bin" | "lib" | "both"` (inferred when
omitted). `bock new` does not emit it, and the v1 build infers project
type rather than requiring it.

### Strictness

```toml
[strictness]
default = "development"
```

| Field     | Read by       | Values                                  | Default            |
| --------- | ------------- | --------------------------------------- | ------------------ |
| `default` | `bock promote`, build/check strictness | `sketch`, `development`, `production` | `sketch` when the key is absent |

`bock promote` reads this field, analyzes the project at the next
level, and (with `--apply`) bumps it. `bock build --strict` and `bock
check --strict` override it to `production` for a single invocation.
See §10.7 and the [`bock promote`](./cli.md#bock-promote) and
[`bock check`](./cli.md#bock-check-files) commands.

### `[ai]`

The AI provider configuration. A missing `[ai]` section yields a
usable stub provider (rule-based codegen only). All fields are read by
the v1 compiler:

```toml
[ai]
provider = "openai-compatible"    # built-in: "openai-compatible" | "anthropic"
endpoint = "https://api.example.com/v1"
model = "model-name"
api_key_env = "AI_API_KEY"        # env var name holding the key — not the key itself
confidence_threshold = 0.75       # accept AI output at or above this (0.0–1.0)
deterministic_fallback = true     # fall back to rule-based codegen on AI failure
auto_pin = false                  # auto-pin AI decisions in development mode
cache = true                      # cache AI responses (content-addressed)
max_retries = 3
timeout_seconds = 30
```

| Field                    | Default               | Meaning                                                          |
| ------------------------ | --------------------- | ---------------------------------------------------------------- |
| `provider`               | `"stub"` (no `[ai]`)  | `"openai-compatible"` or `"anthropic"`.                          |
| `endpoint`               | `""`                  | API endpoint base URL.                                           |
| `model`                  | `""`                  | Model identifier understood by the provider.                     |
| `api_key_env`            | none                  | Name of the env var holding the key. Keys never appear in files. |
| `confidence_threshold`   | `0.75`                | Accept AI output at or above this confidence.                    |
| `deterministic_fallback` | `true`                | Fall back to Tier-2 rule-based codegen on AI failure.            |
| `auto_pin`               | `false`               | Auto-pin AI decisions in development.                            |
| `cache`                  | `true`                | Cache AI responses (content-addressed).                          |
| `max_retries`            | `3`                   | Provider retry count.                                            |
| `timeout_seconds`        | `30`                  | Per-request timeout.                                             |

`bock model show` / `bock model set` query and edit this
configuration from the command line. API keys are always referenced by
environment-variable *name*; they never appear in the project file.
See §17 and §20.7.

### `[registries]`

Package registry endpoints, read by `bock pkg`:

```toml
[registries]
default = "https://registry.bock-lang.org"
internal = "https://bock.company.internal"
```

| Field           | Meaning                                                              |
| --------------- | ------------------------------------------------------------------- |
| `default`       | Registry used when `bock pkg add` is given no `--registry`.         |
| `<name> = URL`  | Named private registries.                                           |

`bock pkg add --registry <URL>` overrides the configured default for a
single invocation. See §19.

## `bock.package`

Dependencies are declared in **`bock.package`**, not `bock.project`
(Appendix A.2, §19). `bock pkg init` creates it; `bock pkg add` /
`remove` maintain it.

```toml
[package]
name = "my-app"
version = "0.1.0"

[dependencies]
core-http = "^1.0"
```

| Section          | Field          | Meaning                                  |
| ---------------- | -------------- | ---------------------------------------- |
| `[package]`      | `name`         | Package name.                            |
| `[package]`      | `version`      | Package version.                         |
| `[dependencies]` | `<name> = REQ` | A dependency and its version requirement.|

A `[workspace]` form (with `members` and shared `dependencies`) lets
multiple packages share one repository; see §19 for the full manifest
grammar.

## Per-Target Configuration `[targets.<T>]`

The v1 compiler **parses and validates** the per-target configuration
tables — `[targets.<T>]` for *deep* config (`test_framework`,
`formatter`, `package`, Go `module`) and `[targets.<T>.scaffolding]`
for *shallow* config (`linter`, `package_manager`). These tables
configure project-mode output (see [Output Modes](#output-modes)
below); `bock build` selects which targets to build from `--target` /
`--all-targets` and applies target-appropriate defaults for any field
left unset.

```toml
[targets.go]
module = "github.com/user/my-app"     # deep

[targets.python]
package = "my_app"                     # deep: overrides default normalization
test_framework = "pytest"              # deep: affects test codegen
formatter = "black"                    # deep: affects code style

[targets.python.scaffolding]
linter = "ruff"                        # shallow: adds a config file only
package_manager = "uv"                 # shallow: affects README only
```

`[targets.<T>]` configures what Bock *emits* (deep); `[targets.<T>.
scaffolding]` configures files added *alongside* the emitted code
(shallow).

### Supported values (v1 matrix)

The compiler validates every supplied value against the matrix below
(§20.6.2). An unknown value is a **build error** that names the
documented options for that target, raised before any output is
written. Rust and Go formatters and test frameworks are universal and
always-on (rustfmt/gofmt, `cargo test`/`go test`), so they are not
user-selectable; supplying `formatter` or `test_framework` for those
targets is an error.

| Target | `test_framework`           | `formatter`                   | `linter`         | `package_manager`           |
| ------ | -------------------------- | ----------------------------- | ---------------- | --------------------------- |
| js     | `vitest` (def), `jest`     | `prettier` (def), `none`      | `eslint`         | `npm` (def), `pnpm`, `yarn` |
| ts     | `vitest` (def), `jest`     | `prettier` (def), `none`      | `eslint`         | `npm` (def), `pnpm`, `yarn` |
| python | `pytest` (def), `unittest` | `black` (def), `ruff`, `none` | `ruff`, `pylint` | `pip` (def), `poetry`, `uv` |
| rust   | universal (`cargo test`)   | universal (rustfmt)           | `clippy`         | cargo only                  |
| go     | universal (stdlib)         | universal (gofmt)             | `golangci-lint`  | go mod only                 |

`package` (Python) and Go `module` are free-form identifiers and are
not enum-validated.

### Output Modes

`bock build` produces output in one of two v1 modes (§20.6.2):

- **Project mode** (default) — source files **plus** target-ecosystem
  scaffolding (manifests, entry-point wiring, formatter configs, a
  README), runnable in the target's normal toolchain. The
  `[targets.*]` tables above configure this output.
- **Source mode** (`--source-only`) — bare transpilation: target source
  files only, suitable for integration into an existing target-language
  project the user already manages.

In project mode each target now emits a rich manifest referencing its
test framework, a formatter config where applicable, an opt-in linter
config (only when `[targets.<T>.scaffolding].linter` is set), and a
`README.md` first-contact honoring the package-manager hint — see
[Project-Mode Scaffolding](./tooling.md#project-mode-scaffolding) for the
per-target file list. The transpiled `@test` *files* (so `npm test` /
`cargo test` / `pytest` / `go test` execute your Bock tests) and the
formatter-clean release gate land in a later milestone; the manifests
already reference the framework.

The supported per-target variant matrix is also carried normatively in
§20.6.2.

## Reserved for v1.x

These fields appear in older spec drafts and are reserved for v1.x or
later (Appendix A.3). v1 does not activate them, and they should not
appear in user-authored project files.

| Field                                  | Reserved for                                                       |
| -------------------------------------- | ------------------------------------------------------------------ |
| `[project] authors`                    | Author metadata, activated alongside `bock pkg publish` (§19).     |
| `[strictness.overrides]`               | Per-path glob-based strictness mappings; v1 ships flat project-level strictness. |
| `[paradigm]`                           | Paradigm-mode selection (`FP`/`OOP`/`Multi`); v1 ships a single fixed paradigm. |
| `[effects]`, `[effects.overrides.<env>]`| Project-level effect handler routing; v1 uses inline + module-level resolution (§10). |
| `[plugins]`                            | Plugin declarations, pending the plugin loader (Appendix C).       |
| `[testing]`                            | Smart-target-selection thresholds, alongside the cross-target test flags (§20.4). |
| `[build.hooks]`                        | Pre/post-build script hooks.                                       |
| `[build.cache] remote`                 | Remote build cache.                                                |
| `[build] min_bock`                     | Minimum compiler version; v1 does not enforce one.                 |

See Appendix A.3 of the spec for the full list and rationale. For
Reserved command-line surfaces, see the
[CLI Reference](./cli.md#reserved-for-v1x).
