# Documentation Inventory (D0)

**Scratch file. Drives phases D1–D5; deleted in D5 after the
permanent docs structure has absorbed its work.**

Captured against `main` at commit `7fb6e75`, build date 2026-05-10.

## Executive Summary

| Bucket                                               | Count |
| ---------------------------------------------------- | ----- |
| Docs pages under `docs/src/`                         | 11    |
| Spec section files under `spec/sections/`            | 16    |
| Spec changelogs under `spec/changelogs/`             | 18    |
| Top-level CLI subcommands implemented                | 17    |
| Top-level CLI subcommands listed in docs `cli.md`    | 10    |
| Top-level CLI subcommands listed in spec §20.1       | 17    |
| Build flags implemented on `bock build`              | 9     |
| Build flags listed in spec §20.1                     | 8     |
| Build flags listed in docs `cli.md`                  | 3     |
| Project-config (`bock.project`) sections implemented | 3     |
| Project-config sections in spec Appendix A           | 11    |
| Stdlib `.bock` files                                 | 0     |
| Stdlib public functions                              | 0     |

**Doc gaps (surface item present in impl, absent / stale in docs):**
**41** items
**Spec gaps (impl-only surface, absent from spec):** **5** items
**Drift (spec describes surface not in impl):** **10** items
**Aligned rows:** **17** items
**Total surface rows in matrix:** **73**

### Phase-hour estimates

Rough order-of-magnitude budget; refine in each phase intro.

| Phase | Topic                              | Estimated hours |
| ----- | ---------------------------------- | --------------- |
| D1    | Spec alignment (close drift + spec gaps) | 3–5             |
| D2    | Language reference pages (types, fns, effects, modules, patterns, AIR) | 6–10            |
| D3    | Tooling reference (CLI, build, REPL, LSP, project schema) | 6–8             |
| D4    | Stdlib reference (placeholder — stdlib currently empty) | 1–2             |
| D5    | Contributor docs (architecture, playbook, doc-build CI) | 3–5             |
| **Total** |                                | **19–30 hours** |

The D4 estimate is small only because `stdlib/` is empty
(see [Implementation Surface](#stdlib-public-functions)). Once
stdlib lands, D4 will grow substantially; D4 in this scope is
limited to setting up the reference scaffolding and the
`bock doc`-generated index hookup.

---

## Docs Inventory

Every `.md` file under `docs/src/`. "Last meaningful commit" is
the most recent commit touching the file.

| Path                              | Title (H1)             | Topic                                        | Last commit                              | Status |
| --------------------------------- | ---------------------- | -------------------------------------------- | ---------------------------------------- | ------ |
| `SUMMARY.md`                      | Summary                | mdBook TOC for the entire reference          | 2026-04-26 `bc9c2f1` bootstrap           | stub   |
| `introduction.md`                 | Introduction           | Marketing-grade overview, defers to spec     | 2026-04-26 `bc9c2f1` bootstrap           | stub   |
| `getting-started.md`              | Getting Started        | Install, scaffold, check, build, run         | 2026-04-26 `e3122fc` repo-link update    | stub   |
| `language-guide/types.md`         | Types                  | Stub; defers to spec §2                      | 2026-04-26 `bc9c2f1` bootstrap           | stub   |
| `language-guide/functions.md`     | Functions              | Stub; defers to spec §4                      | 2026-04-26 `bc9c2f1` bootstrap           | stub   |
| `language-guide/effects.md`       | Effects                | Stub; defers to spec §8                      | 2026-04-26 `bc9c2f1` bootstrap           | stub   |
| `language-guide/modules.md`       | Modules                | Stub; defers to spec §10                     | 2026-04-26 `bc9c2f1` bootstrap           | stub   |
| `reference/cli.md`                | CLI Reference          | Subcommand sketch (10 commands); 3 flags     | 2026-04-26 `bc9c2f1` bootstrap           | stale  |
| `reference/stdlib.md`             | Standard Library       | Module list pointing at `stdlib/` (empty)    | 2026-04-26 `bc9c2f1` bootstrap           | stale  |
| `reference/spec.md`               | Specification Index    | Table of section files                       | 2026-04-26 `bc9c2f1` bootstrap           | current |
| `contributing.md`                 | Contributing           | Build commands, spec-change process          | 2026-04-26 `bc9c2f1` bootstrap           | stub   |

Status legend:

- **stub** — page exists but defers to spec/ rather than
  presenting reference content here.
- **stale** — page contains specific claims about the current
  impl that have drifted (e.g., command list, flag list).
- **current** — actually matches today's repo state.

Every docs page (except `reference/spec.md`) was last touched
during the docs-bootstrap commit on 2026-04-26 and has not been
updated to reflect changes landed since (target formatter spec,
output-modes split, project-mode pass, CLI surface alignment).

---

## Spec Inventory

| Section file                | Section title                  | Topic                                        | Last commit                                | Referenced in changelogs                                                                          |
| --------------------------- | ------------------------------ | -------------------------------------------- | ------------------------------------------ | ------------------------------------------------------------------------------------------------- |
| `s01-lexical.md`            | Lexical Structure              | Encoding, identifiers, literals, operators   | 2026-04-26 `54e2ad1`                       | 03-04 1651, 03-10 1500, 03-10 1530, 04-03 1000, 04-20 0413                                        |
| `s02-types.md`              | Type System                    | Algebra, generics, refinements, capabilities | 2026-04-26 `54e2ad1`                       | 03-04 1651, 04-03 1000, 04-20 0413                                                                |
| `s03-ownership.md`          | Ownership Model                | Move/borrow, target mapping, `@managed`      | 2026-04-26 `54e2ad1`                       | 03-04 1651, 04-20 0413, 04-23 1830                                                                |
| `s04-declarations.md`       | Declarations                   | fn/record/enum/trait/class/alias             | 2026-04-26 `54e2ad1`                       | 03-04 1651, 04-03 1000, 04-20 0413                                                                |
| `s05-expressions.md`        | Expressions                    | Pipe, partial app, ranges, string interp     | 2026-04-26 `54e2ad1`                       | 03-04 1651, 04-03 1000, 04-20 0413                                                                |
| `s06-statements.md`         | Statements / Control Flow      | let, if/else, guard, for, while, loop        | 2026-04-26 `54e2ad1`                       | 03-04 1651, 04-20 0413                                                                            |
| `s07-patterns.md`           | Pattern Matching               | Patterns, exhaustiveness                     | 2026-04-26 `54e2ad1`                       | 03-04 1651, 04-20 0413                                                                            |
| `s08-effects.md`            | Effect System                  | Effects, handlers, adaptive recovery         | 2026-04-26 `54e2ad1`                       | 03-04 1651, 04-08 0900, 04-16 1000, 04-20 0413, 04-20 1100, 04-20 1400                            |
| `s09-context.md`            | Context System                 | `@context`/`@requires`/`@invariant` …        | 2026-04-26 `54e2ad1`                       | 03-04 1651, 04-20 0413                                                                            |
| `s10-modules.md`            | Module System                  | Files, imports, visibility, re-exports       | 2026-04-26 `54e2ad1`                       | 03-04 1651, 04-20 0413                                                                            |
| `s11-grammar.md`            | Formal Grammar                 | Key productions                              | 2026-04-26 `54e2ad1`                       | 03-04 1651, 04-20 0413                                                                            |
| `s12-air.md`                | AIR (Annotated IR)             | Four-layer model, node structure             | 2026-04-26 `54e2ad1`                       | 03-04 1651, 04-20 0413                                                                            |
| `s13-transpilation.md`      | Transpilation Pipeline         | AI-first, decision manifest, verification, target formatters | **2026-05-10 `6085c9c`** | 03-04 1651, 04-04 1400, 04-20 0413, 04-20 1100, 04-20 1400, 04-23 1830, 05-06 1630, 05-10 2100, 05-10 2300 |
| `s14-stdlib.md`             | Standard Library               | Two-tier, prelude, core modules              | 2026-04-26 `54e2ad1`                       | 03-04 1651, 04-08 0900, 04-20 0413                                                                |
| `s15-packaging.md`          | Package Manager                | `bock.package`, lockfile, registries         | 2026-04-26 `54e2ad1`                       | 03-04 1651, 04-20 0413                                                                            |
| `s16-tooling.md`            | Tooling                        | CLI, REPL, formatter, build system           | **2026-05-10 `019fb1e`**                   | 03-04 1651, 04-20 0413, 05-06 0900, 05-10 2100                                                    |

`spec/bock-spec.md` is the canonical assembled spec (2379 lines).
It is the authoritative source for §17.x (transpilation), §18.x
(stdlib), §19.x (packaging), §20.x (tooling) and Appendix A
(project configuration). The `s*-` section files under
`spec/sections/` are **excerpts** — most are shorter than the
corresponding sections in `bock-spec.md`.

There is no section file corresponding to §17 (Transpilation —
covered partly by `s13-transpilation.md`), §18 (Stdlib — covered
by `s14-stdlib.md`), §19 (Packaging — covered by
`s15-packaging.md`), or §20 (Tooling — covered partly by
`s16-tooling.md`). The numbering mismatch between section files
and `bock-spec.md` sections is documented here for D1 to resolve.

---

## Implementation Surface

### CLI Commands and Flags

Captured from `bock --help` against the release binary built
from this branch. Sub-subcommand flags omitted where they have
no docs/spec footprint.

| Command                            | Args / Flags                                                                                                | Docs? | Spec?                | Status   |
| ---------------------------------- | ----------------------------------------------------------------------------------------------------------- | ----- | -------------------- | -------- |
| `bock new <NAME>`                  | name (positional)                                                                                           | yes   | §20.1 / §20.7        | aligned  |
| `bock build`                       | (see flags below)                                                                                           | yes   | §20.1 / §20.6        | aligned* |
| `bock build --target/-t`           | target language                                                                                             | yes   | §20.1                | aligned  |
| `bock build --all-targets`         |                                                                                                             | yes   | §20.1                | aligned  |
| `bock build --release`             | release optimizations                                                                                       | no    | §20.1                | doc gap  |
| `bock build --source-only`         | emit transpiled source, no toolchain                                                                        | yes   | §20.1                | aligned  |
| `bock build --deterministic`       | skip AI; alias `--no-ai`                                                                                    | no    | §20.1                | doc gap  |
| `bock build --strict`              | force production strictness for this build                                                                  | no    | implied by §17.4 / §10.7; not named | doc gap + spec gap |
| `bock build --pin-all`             | pin every build decision after success                                                                      | no    | implied by §17.4; not named | doc gap + spec gap |
| `bock build --source-map`          | emit source map files (default on)                                                                          | no    | not specified        | doc gap + spec gap |
| `bock build --no-source-map`       | suppress source map output                                                                                  | no    | not specified        | doc gap + spec gap |
| `bock build --deliverable`         | *(spec says: produce binary-ready deliverable)*                                                             | no    | §20.1                | drift (spec only; impl missing) |
| `bock build --no-tests`            | *(spec says: skip transpiled tests)*                                                                        | no    | §20.1                | drift (spec only; impl missing) |
| `bock build --optimize`            | *(spec says: enable target-side optimization)*                                                              | no    | §20.1                | drift (spec only; impl missing) |
| `bock run [FILE] [-- ARGS…]`       | `--watch` (stub: not yet implemented)                                                                        | yes   | §20.1                | aligned* (--watch stub) |
| `bock check [FILES…]`              | `--types`, `--lint`, `--no-context`                                                                          | partial | §20.1 (`--types`, `--lint`, `--context`) | drift (`--no-context` vs `--context` polarity) |
| `bock test [FILES…]`               | `--filter <pattern>`                                                                                         | yes   | §20.1 (also names `--target`, `--all-targets`, `--smart`, `--coverage`, `--snapshot`) | drift (impl missing several spec'd flags) |
| `bock fmt`                         | `--check`                                                                                                    | yes   | §20.1 / §20.2        | aligned  |
| `bock repl`                        | (no flags; spec lists `:type`, `:air`, `:target` interactive commands) | no    | §20.1                | doc gap  |
| `bock inspect [SUBCMD] [FILTERS]`  | `--runtime`, `--all`, `--unpinned`, `--module`, `--type`, `--json`; subs `decisions`, `decision`, `cache`, `rules` | no    | §20.1 / §17.4 | doc gap |
| `bock pin [DECISION]`              | `--all-in`, `--all-build`, `--all-runtime`, `--reason`                                                       | no    | §20.1                | doc gap  |
| `bock unpin <DECISION>`            |                                                                                                              | no    | §20.1                | doc gap  |
| `bock override [DECISION] [NEW_CHOICE]` | `--from-file`, `--runtime`, `--promote`, `--reason`                                                     | no    | §20.1 / §10.8        | doc gap  |
| `bock cache`                       | subs `stats`, `clear` (with `--decisions`, `--runtime`, `--build`, `--rules`)                                | listed (parent only) | §20.1 (lists `list`, `clear`, `prune`, `stats`) | drift (impl missing `list`, `prune`) |
| `bock promote`                     | `--apply`, `--check`                                                                                         | no    | §20.1 / §10.7 / §20.7 | doc gap |
| `bock pkg`                         | subs `init`, `add`, `remove`, `tree`, `list`, `cache clear`                                                  | listed (parent only) | §20.1 (lists `add`, `remove`, `update`, `audit`, `publish`, `search`); §19 for manifest | drift (impl missing `update`, `audit`, `publish`, `search`; impl-only `init`, `tree`, `list`, `cache`) |
| `bock model`                       | subs `show`, `set <KEY> <VALUE>`                                                                             | no    | §20.1 (also lists `list`, `install`, `use`) | drift (impl missing `list`, `install`, `use`) |
| `bock doc [PATH]`                  | `--output`, `--format` (`markdown` / `html`)                                                                 | no    | §20.1                | doc gap  |
| `bock lsp`                         | `--stdio`                                                                                                    | no    | §20.3                | doc gap  |
| `bock fix`                         | *(spec says: auto-fix lint warnings)*                                                                        | no    | §20.1                | drift (spec only; impl missing) |
| `bock migrate`                     | *(spec says: AI-assisted import from other languages)*                                                       | no    | §20.1                | drift (spec only; impl missing) |
| `bock target`                      | *(spec says: target management — list/add/info)*                                                             | no    | §20.1                | drift (spec only; impl missing) |
| `bock ci`                          | *(spec says: run all CI checks in one command)*                                                              | no    | §20.1                | drift (spec only; impl missing) |

Notes on the table:

- `aligned*` on `bock build`: parent command aligned; per-flag rows
  capture the drift below.
- `aligned*` on `bock run --watch`: documented as "not yet
  implemented" in the impl help text, so this is a known stub
  rather than a doc gap.
- `bock check --no-context` (impl) vs `--context` (spec) is a
  polarity difference — same capability, opposite default.

### Stdlib Public Functions

```
$ grep -rE '^public fn ' stdlib/ | wc -l
0
```

No stdlib `.bock` files exist yet. `stdlib/` contains only a
`CLAUDE.md` describing the future layout. Once stdlib lands,
this section drives D4's per-symbol page generation via
`bock doc`.

**Status:** every prospective stdlib row is **doc gap + drift**
in the trivial sense (spec describes modules in §18 / `s14-stdlib`
that the impl doesn't yet ship). D4 in this PR cycle scaffolds
the reference structure; it does not fill in non-existent symbols.

### `bock.project` Schema

Schema as actually parsed by the compiler:

| Section / field           | Read by                                                | Docs?                              | Spec? (Appendix A) | Status   |
| ------------------------- | ------------------------------------------------------ | ---------------------------------- | ------------------ | -------- |
| `[project] name`          | `bock-cli/doc.rs::read_project_meta`, `bock new` write | partial (`getting-started.md`)     | yes                | doc gap  |
| `[project] version`       | `bock-cli/doc.rs::read_project_meta`, `bock new` write | partial                            | yes                | doc gap  |
| `[project] authors`       | *(not read)*                                            | no                                 | yes                | drift    |
| `[strictness] default`    | `bock-cli/promote.rs::read_strictness`, `update_strictness` | no                            | yes                | doc gap  |
| `[strictness.overrides]`  | *(not read)*                                            | no                                 | yes                | drift    |
| `[ai] provider`           | `bock-ai/config.rs::AiConfig`                          | no                                 | yes                | doc gap  |
| `[ai] endpoint`           | `bock-ai/config.rs::AiConfig`                          | no                                 | yes                | doc gap  |
| `[ai] model`              | `bock-ai/config.rs::AiConfig`                          | no                                 | yes                | doc gap  |
| `[ai] api_key_env`        | `bock-ai/config.rs::AiConfig`                          | no                                 | yes                | doc gap  |
| `[ai] deterministic_fallback` | `bock-ai/config.rs::AiConfig`                       | no                                 | yes                | doc gap  |
| `[ai] auto_pin`           | `bock-ai/config.rs::AiConfig`                          | no                                 | yes                | doc gap  |
| `[ai] cache`              | `bock-ai/config.rs::AiConfig`                          | no                                 | yes                | doc gap  |
| `[ai] confidence_threshold` | `bock-ai/config.rs::AiConfig`                        | no                                 | yes                | doc gap  |
| `[ai] max_retries`        | `bock-ai/config.rs::AiConfig`                          | no                                 | yes                | doc gap  |
| `[ai] timeout_seconds`    | `bock-ai/config.rs::AiConfig`                          | no                                 | yes                | doc gap  |
| `[paradigm] default`      | *(not read)*                                            | no                                 | yes                | drift    |
| `[targets] primary`       | *(not read)*                                            | no                                 | yes                | drift    |
| `[targets] additional`    | *(not read)*                                            | no                                 | yes                | drift    |
| `[effects.*]`             | *(not read)*                                            | no                                 | yes                | drift    |
| `[plugins.*]`             | *(not read)*                                            | no                                 | yes                | drift    |
| `[testing.*]`             | *(not read)*                                            | no                                 | yes                | drift    |
| `[build.*]`               | *(not read)*                                            | no                                 | yes                | drift    |
| `[registries.*]`          | *(not read)*                                            | no                                 | yes                | drift    |
| `[dependencies]` (in `bock.project`) | *(not read at project root — only in `bock.package`)* | no                       | yes                | drift    |

The project root marker is `bock.project` per CLAUDE.md and
`bock-cli/run.rs::find_project_root`. The actual schema parsed
by the compiler is a small subset of Appendix A. Every
unparsed section is "drift" — spec promises behavior the impl
silently ignores. D1 resolves the question of whether spec
should be trimmed, impl should be expanded, or sections should
be marked "Reserved for future use."

---

## Cross-Reference Matrix

Sorted with `spec gap` and `doc gap` at the top so the work
list is immediately visible. Rows below are referenced by ID
from the [Phase Work Allocation](#phase-work-allocation) section.

| #   | Surface element                       | docs? | spec? | impl? | Status   |
| --- | ------------------------------------- | ----- | ----- | ----- | -------- |
| C01 | `bock build --strict`                 | no    | no    | yes   | spec gap + doc gap |
| C02 | `bock build --pin-all`                | no    | no    | yes   | spec gap + doc gap |
| C03 | `bock build --source-map` / `--no-source-map` | no | no | yes   | spec gap + doc gap |
| C04 | `bock pkg init`                       | no    | no    | yes   | spec gap + doc gap |
| C05 | `bock pkg tree`, `bock pkg list`, `bock pkg cache` | no | no | yes | spec gap + doc gap |
| D01 | `bock build --release`                | no    | yes   | yes   | doc gap  |
| D02 | `bock build --deterministic` / `--no-ai` | no | yes   | yes   | doc gap  |
| D03 | `bock repl` (interactive commands)    | no    | yes   | yes   | doc gap  |
| D04 | `bock inspect` + 4 subs               | no    | yes   | yes   | doc gap  |
| D05 | `bock pin`                            | no    | yes   | yes   | doc gap  |
| D06 | `bock unpin`                          | no    | yes   | yes   | doc gap  |
| D07 | `bock override` (+ `--promote`)       | no    | yes   | yes   | doc gap  |
| D08 | `bock promote`                        | no    | yes   | yes   | doc gap  |
| D09 | `bock model show/set`                 | no    | partial | yes | doc gap  |
| D10 | `bock doc`                            | no    | yes   | yes   | doc gap  |
| D11 | `bock lsp`                            | no    | yes (§20.3) | yes | doc gap |
| D12 | `[project] name`/`version`            | partial | yes | yes   | doc gap  |
| D13 | `[strictness] default`                | no    | yes   | yes   | doc gap  |
| D14 | `[ai]` block (10 fields)              | no    | yes   | yes   | doc gap  |
| F01 | `bock build --deliverable`            | no    | yes   | no    | drift    |
| F02 | `bock build --no-tests`               | no    | yes   | no    | drift    |
| F03 | `bock build --optimize`               | no    | yes   | no    | drift    |
| F04 | `bock check --context` vs `--no-context` polarity | partial | yes | yes | drift |
| F05 | `bock test --target`, `--all-targets`, `--smart`, `--coverage`, `--snapshot` | no | yes | no | drift |
| F06 | `bock cache list`, `bock cache prune` | partial | yes | no | drift    |
| F07 | `bock pkg update`, `audit`, `publish`, `search` | no | yes | no | drift    |
| F08 | `bock model list`, `install`, `use`   | no    | yes   | no    | drift    |
| F09 | `bock fix`                            | no    | yes   | no    | drift    |
| F10 | `bock migrate`                        | no    | yes   | no    | drift    |
| F11 | `bock target`                         | no    | yes   | no    | drift    |
| F12 | `bock ci`                             | no    | yes   | no    | drift    |
| F13 | `[project] authors`                   | no    | yes   | no    | drift    |
| F14 | `[strictness.overrides]`              | no    | yes   | no    | drift    |
| F15 | `[paradigm]`                          | no    | yes   | no    | drift    |
| F16 | `[targets]`                           | no    | yes   | no    | drift    |
| F17 | `[effects]` + `[effects.overrides.*]` | no    | yes   | no    | drift    |
| F18 | `[plugins]`                           | no    | yes   | no    | drift    |
| F19 | `[testing]`                           | no    | yes   | no    | drift    |
| F20 | `[build]` (incl. `hooks`, `cache.remote`) | no | yes | no    | drift    |
| F21 | `[registries]`                        | no    | yes   | no    | drift    |
| F22 | `[dependencies]` at project root      | no    | yes   | no    | drift (resides only in `bock.package`) |
| A01 | `bock new`                            | yes   | yes   | yes   | aligned  |
| A02 | `bock build --target`                 | yes   | yes   | yes   | aligned  |
| A03 | `bock build --all-targets`            | yes   | yes   | yes   | aligned  |
| A04 | `bock build --source-only`            | yes   | yes   | yes   | aligned  |
| A05 | `bock run`                            | yes   | yes   | yes   | aligned  |
| A06 | `bock check` (basic)                  | yes   | yes   | yes   | aligned  |
| A07 | `bock test` (basic)                   | yes   | yes   | yes   | aligned  |
| A08 | `bock fmt` / `--check`                | yes   | yes   | yes   | aligned  |
| A09 | spec-section TOC at `docs/src/reference/spec.md` | yes | n/a | n/a | aligned |
| L01 | language-guide/types.md (stub)        | stub  | yes (§2 / §4 / s02) | n/a | doc gap |
| L02 | language-guide/functions.md (stub)    | stub  | yes (§6 / s04)       | n/a | doc gap |
| L03 | language-guide/effects.md (stub)      | stub  | yes (§10 / s08)      | n/a | doc gap |
| L04 | language-guide/modules.md (stub)      | stub  | yes (§12 / s10)      | n/a | doc gap |
| L05 | Pattern matching narrative            | no    | yes (§9 / s07)       | n/a | doc gap |
| L06 | Ownership narrative                   | no    | yes (§5 / s03)       | n/a | doc gap |
| L07 | Context system narrative              | no    | yes (§11 / s09)      | n/a | doc gap |
| L08 | Annotations narrative                 | no    | yes (§15)            | n/a | doc gap |
| L09 | Concurrency narrative                 | no    | yes (§13)            | n/a | doc gap |
| L10 | Interop / FFI narrative               | no    | yes (§14)            | n/a | doc gap |
| L11 | AIR narrative (for contributors)      | no    | yes (§16 / s12)      | n/a | doc gap |
| L12 | Statements / control-flow narrative   | no    | yes (§8 / s06)       | n/a | doc gap |
| L13 | Expressions narrative                 | no    | yes (§7 / s05)       | n/a | doc gap |
| S01 | Stdlib reference page (top-level modules) | stale | yes (§18 / s14)  | empty | doc gap |
| S02 | `std.collections` symbols             | no    | yes               | empty | doc gap (placeholder) |
| S03 | `std.string` symbols                  | no    | yes               | empty | doc gap (placeholder) |
| S04 | `std.math` symbols                    | no    | yes               | empty | doc gap (placeholder) |
| S05 | `std.io` symbols                      | no    | yes               | empty | doc gap (placeholder) |
| S06 | `std.time` symbols (`core.time`)      | no    | yes               | empty | doc gap (placeholder) |
| K01 | Architecture orientation              | no (root `ARCHITECTURE.md` is short) | n/a | n/a | doc gap |
| K02 | Implementation playbook               | referenced in `contributing.md` as `docs/src/contributing/playbook.md` but path doesn't exist | n/a | n/a | doc gap (dangling reference) |
| K03 | `docs/` build CI job                  | n/a   | n/a   | n/a   | doc gap (gate referenced in session command but no workflow file confirmed) |
| K04 | Numbering mismatch s01-s16 vs §1-§23  | n/a   | yes   | n/a   | spec gap (D1 to reconcile) |

Status legend:

- **aligned** — docs, spec, and impl all agree.
- **doc gap** — impl exists, spec covers it, docs don't.
- **spec gap** — impl exists, spec doesn't cover it.
- **drift** — spec or docs describe something the impl doesn't
  do (or vice versa) such that the three disagree.
- **stub** in docs column — page exists but content defers.

---

## Phase Work Allocation

Each phase's work list points back to the matrix row by ID so
the implementation chat for that phase knows exactly what to
close.

### D1 — Spec alignment

Resolve spec/impl divergence before writing reference content
that would otherwise document phantom behavior or omit real
behavior. **Every row below is either a `spec gap` (impl ahead
of spec) or `drift` (spec ahead of impl).**

For each row, D1 decides: amend spec to match impl, amend impl
to match spec, or mark the spec entry "Reserved for future use"
with a changelog. The decisions chat owns the resolution.

- C01–C03: spec must describe `--strict`, `--pin-all`, source-map flags on `bock build`.
- C04–C05: spec must describe (or explicitly reserve) `bock pkg init|tree|list|cache`.
- F01–F03: decide for each whether `--deliverable`, `--no-tests`, `--optimize` are deferred or removed from §20.1.
- F04: settle `bock check --context` polarity in spec.
- F05: decide which `bock test` flags are deferred.
- F06: `bock cache list`/`prune` — keep spec, or trim.
- F07–F08, F10–F12: same disposition for `bock pkg update/audit/publish/search`, `bock model list/install/use`, `bock fix`, `bock migrate`, `bock target`, `bock ci`.
- F09: `bock fix` — likely defer; called out separately because it's the most user-facing missing command.
- F13–F22: decide which Appendix A sections impl actually plans to read in v1 vs which are aspirational; mark accordingly.
- K04: reconcile section file numbering (`s01-s16`) with the assembled spec's `§1-§23`. Either rename section files or document the mapping in `docs/src/reference/spec.md`.

Estimated cost: 3–5 hours, mostly decision-routing.

### D2 — Language reference

Replace docs/language-guide stubs with reference content keyed
to the spec sections they currently defer to. Add the language
topics not yet covered.

- L01: rewrite `language-guide/types.md` as a reference page (not a stub).
- L02: rewrite `language-guide/functions.md`.
- L03: rewrite `language-guide/effects.md`.
- L04: rewrite `language-guide/modules.md`.
- L05: add `language-guide/patterns.md`.
- L06: add `language-guide/ownership.md`.
- L07: add `language-guide/context.md`.
- L08: add `language-guide/annotations.md`.
- L09: add `language-guide/concurrency.md`.
- L10: add `language-guide/interop.md`.
- L12: add `language-guide/control-flow.md`.
- L13: add `language-guide/expressions.md`.

L11 (AIR) is a contributor topic, deferred to D5.

Each page links back to its spec section as the authoritative
source — the docs page is narrative + worked examples, not a
re-derivation.

Estimated cost: 6–10 hours.

### D3 — Tooling reference

Replace stale `reference/cli.md` with a generated-leaning
reference covering every implemented subcommand and flag. Add
the project-config reference page that doesn't yet exist.

CLI command pages (one each, structured the same way):

- A01–A08: aligned commands — short pages or one combined page.
- D01–D11: every doc-gap CLI row. Each becomes a section or page.
- C01–C05: impl-only flags — documented as impl-current; cross-link to D1's spec resolution.

Project-config page:

- D12–D14: `[project]`, `[strictness]`, `[ai]` — sections actually parsed today.
- Cross-link F13–F22 with each row's D1 disposition (deferred / reserved).

Build-system / output-layout reference:

- `.bock/build/` layout per §20.6.1 / §20.6.2.
- Cache layout per `bock-build/cache.rs` and `[build.cache]`.

LSP reference:

- D11 — `bock lsp --stdio`; cross-reference §20.3.

Estimated cost: 6–8 hours.

### D4 — Stdlib reference

Stdlib is currently empty. D4 scaffolds the reference structure
so `bock doc` output and per-package narrative slots in cleanly
when stdlib lands.

- S01: rewrite `reference/stdlib.md` to be honest about current
  status (no symbols yet) and explain the path for future
  content (auto-generated index + per-package narrative).
- S02–S06: leave per-package symbol pages as placeholders or
  defer until stdlib has files.
- Wire `bock doc --format markdown` output location as the
  intended sink for per-symbol pages.

Estimated cost: 1–2 hours.

### D5 — Contributor docs

Architecture orientation, playbook, doc-build CI, and
INVENTORY.md teardown.

- K01: expand or fold root `ARCHITECTURE.md` into
  `docs/src/contributing/architecture.md`.
- K02: write `docs/src/contributing/playbook.md` (currently a
  dangling reference from `contributing.md`).
- L11: add `docs/src/contributing/air.md` for the AIR
  contributor view.
- K03: add `mdbook build docs/` gating in CI (already in
  `/project:session` teardown; mirror in workflow file).
- Delete `docs/INVENTORY.md` once D1–D4 have absorbed every
  row above.

Estimated cost: 3–5 hours.

---

## Notes for downstream phases

- The PR opening session for each phase should pull the matrix
  rows it owns into its task list verbatim. The "Cross-Reference
  Matrix" row IDs are stable; subsequent phases append rows but
  do not renumber existing ones.
- `bock-spec.md` is the canonical source for §17.x onward;
  section excerpts under `spec/sections/` lag and have not
  consistently kept pace with `bock-spec.md` edits. D1 should
  flag any drift between excerpts and the assembled spec.
- `stdlib/` is empty. D4 is a scaffolding phase only; the real
  stdlib documentation cycle happens once `stdlib/std/<name>/`
  packages start landing.
- The doc-build verification gate is already enforced in
  `/project:session` teardown via `mdbook build docs/`. D5
  should add the equivalent to GitHub Actions so external
  contributors get the same gate.
