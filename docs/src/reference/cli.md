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
- `bock check --brief` produces compact, one-line diagnostics
  without source-context snippets.
- `bock build -t <target> --source-only` emits transpiled source
  but does not invoke the target toolchain.
- `bock build --all-targets` builds every target listed in
  `bock.project`.

For each subcommand's full flag list, see `bock <subcommand> --help`.
