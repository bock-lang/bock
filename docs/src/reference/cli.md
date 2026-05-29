# CLI Reference

> Generated coverage will live here. For the up-to-date command list
> use `bock --help`.

## Subcommands at a Glance

| Command       | What it does                                   |
| ------------- | ---------------------------------------------- |
| `bock new`    | Scaffold a project at `<name>/`.               |
| `bock check`  | Type-check and lint without building.          |
| `bock build`  | Transpile and (optionally) compile a project.  |
| `bock run`    | Execute via the interpreter.                   |
| `bock test`   | Run tests under the project.                   |
| `bock fmt`    | Format `.bock` files in place.                 |
| `bock doc`    | Generate API documentation.                    |
| `bock pkg`    | Manage dependencies and lockfile.              |
| `bock repl`   | Interactive evaluator.                         |
| `bock cache`  | Inspect and clean the build cache.             |

## Common Flags

- `bock check` runs over the current directory by default; pass
  paths to limit it.
- `bock check --only=<aspect>` restricts the check to specific
  aspects of analysis; the v1 aspects are `types` and `context`.
  The flag accepts a comma-separated list (`--only=types,context`)
  and may be repeated (`--only=types --only=context`); omitting it
  runs the full check. Unknown aspects are rejected with the list
  of valid values.
  - `--only=types` runs type checking.
  - `--only=context` runs the context-system validation aspect:
    capability (`@requires`) verification **and** the
    context-validation pass — annotation consistency (security-level
    monotonicity, performance-budget validity, recognized
    capability/security names) plus completeness (public items and
    modules carrying `@context`). Completeness is gated by
    strictness (see `--strict` below). Cross-module PII/security
    *composition* is not part of this aspect in v1 — it is reserved
    for a future dedicated security pass.
- `bock check --brief` produces compact, one-line diagnostics
  without source-context snippets.
- `bock check --strict` forces production strictness for the check
  (mirrors `bock build --strict`). At the default development
  strictness, completeness gaps — a public item or module missing
  `@context` — are **warnings**, so the check still exits 0. Under
  `--strict` those same gaps become **errors** and the check exits
  non-zero. `bock check` exits non-zero if and only if it produces
  at least one error; warnings never fail the check.
- `bock build -t <target> --source-only` emits transpiled source
  but does not invoke the target toolchain.
- `bock build --all-targets` builds every target listed in
  `bock.project`.

For each subcommand's full flag list, see `bock <subcommand> --help`.
