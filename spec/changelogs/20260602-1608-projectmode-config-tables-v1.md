# Project-mode config tables pulled into v1

**Date:** 2026-06-02
**Affects:** §20.7 (Project Scaffolding), Appendix A.3 (Reserved for future versions)
**Type:** addition (un-reserves a previously v1.x surface)

## Change

The per-target tooling-configuration tables in `bock.project` are **parsed in v1** by the
project-mode build (§20.6.2), reversing their prior "Reserved for v1.x" status:

- **`[targets.<T>]`** — *deep* configuration that changes what code Bock emits:
  `test_framework` (Vitest|Jest, pytest|unittest), `formatter` (Prettier|none,
  Black|Ruff format|none), plus `package` and the Go `module` path.
- **`[targets.<T>.scaffolding]`** — *shallow* configuration that adds config files
  alongside the emitted code without changing it: `linter` (ESLint, Ruff check|Pylint,
  Clippy, golangci-lint), `package_manager` (npm|pnpm|yarn, pip|Poetry|uv).

The v1-supported variant matrix is the one already documented in §20.6.2. Fields left unset
fall back to the target-appropriate defaults; unknown values produce a build error pointing
at the documented options for that target (§20.6.2). Rust/Go formatters remain universal and
always-on (rustfmt/gofmt); their codegen must emit formatter-clean output.

## Rationale

Deep configuration (e.g. Jest vs Vitest) is an adoption blocker for teams standardized on a
particular framework — a team cannot adopt Bock without it. Pulling the tables into v1 lets
project mode (§20.6.2) emit a project that drops into an existing team's toolchain on first
build, which is the point of project mode. Shallow configuration is cheap to support and
rides along.

## Migration

No migration for existing projects: the tables are optional and `bock new` does not generate
them by default; omitting them yields target-appropriate defaults (unchanged behavior). Users
who want a specific framework/formatter/linter add the relevant table.

## Implementation sequencing

Realized by the ItemB milestone (`tracking/plans/2026-06-02-itemB-per-module-projectmode-plan.md`):
the `bock.project` parser gains the tables + unknown-value validation in the scaffolding-framework
stage (S5), and per-target codegen branches on deep config in the per-target scaffolder stage (S6).
`--deliverable` and `--no-tests` (§20.1) remain Reserved for v1.x.
