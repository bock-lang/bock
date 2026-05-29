# §20.1 CLI and §20.7 / Appendix A target tables reconciled to v1 reality

**Date:** 2026-05-29
**Affects:** §20.1, §20.7, Appendix A.1, Appendix A.3
**Type:** clarification (editorial reconciliation to v1 reality)

## Change

§20 described several CLI flags and `bock.project` tables ahead of the
v1 implementation. The D3 documentation work
(`docs/src/reference/cli.md`, `tooling.md`, `project-schema.md`)
documented the actual v1 CLI surface; verification against the v1
binary (`bock <cmd> --help`, `bock 0.1.0`) confirms five divergences.
This changelog reconciles §20.1, §20.7, and Appendix A to the actual
v1 surface. No new normative behavior is introduced — missing surfaces
are marked **Reserved for v1.x** (following §20's existing Reserved
markers and the Appendix A.3 deferral pattern), and surfaces that ship
in v1 with a *different syntax* are rewritten to the actual v1 form.

### §20.1 — `bock build` flags (`--optimize` / `--deliverable` / `--no-tests`)

The `bock build` flag list previously included `--deliverable`,
`--no-tests`, and `--optimize`. None of these are present in the v1
`bock build --help`. The v1 flag list is now stated as `--target`,
`--all-targets`, `--source-only`, `--source-map` / `--no-source-map`
(default on), `--strict`, `--pin-all`, `--deterministic` (alias
`--no-ai`), `--release`. The three absent flags are marked **Reserved
for v1.x**, noting they ship with the project-mode build work:
`--deliverable` (deliverable mode, §20.6.2), `--no-tests`
(test-inclusion opt-out, §20.6.2), and `--optimize` (Tier-3 AI
optimization, §17.2).

### §20.1 — `bock inspect --diff`

`--diff` ("changes since last build") is not in v1 `bock inspect`. It
is now marked **Reserved for v1.x**. The actual v1 `inspect` surface
is documented inline: filters `--runtime`, `--all` (prefixed ids),
`--unpinned`, `--module`, `--type`, `--json`, and subcommands
`decisions` (default), `decision <id>`, `cache`, `rules`.

### §20.1 — `bock pin` bulk flags

The spec showed a single bare `--all` flag. v1 exposes three bulk
flags instead: `--all-build`, `--all-runtime`, and `--all-in
<substring>` (plus `--reason <text>`), with the decision id as an
optional positional (`bock pin [decision-id]`). §20.1 now documents the
actual v1 flags and notes a bare `--all` alias spanning every scope as
planned for v1.x.

### §20.1 — `bock override` syntax

The spec showed `bock override <decision-id> --choice=<alternative>`
and `--promote <runtime-id>`. v1 uses a positional replacement value
(`bock override [decision-id] [new-choice]`) or `--from-file <file>`,
a bare `--runtime` scope flag, and a bare `--promote` flag that
operates on the positional `[decision-id]`. §20.1 now documents the
actual v1 surface.

### §20.7 / Appendix A.1 / A.3 — `[targets.<T>]` / `[targets.<T>.scaffolding]`

§20.7 and Appendix A.1 presented the per-target deep-configuration
table (`[targets.<T>]` — `test_framework`, `formatter`, `package`, Go
`module`) and shallow-configuration table
(`[targets.<T>.scaffolding]` — `linter`, `package_manager`) as part of
the v1 `bock.project`. The v1 compiler does not parse these tables:
`bock build` selects targets via `--target` / `--all-targets` (which
builds all five built-in targets) and emits with target-appropriate
defaults. These tables are now marked **Reserved for v1.x** in §20.7
and Appendix A.1, and a corresponding bullet is added to Appendix A.3
alongside the other reserved `bock.project` tables. The variant matrix
in §20.6.2 is preserved as the planned v1.x surface.

## Rationale

The D3 reference docs (just merged, on `main`) document the real v1
CLI, and `docs/src/reference/project-schema.md` already flags the
`[targets.*]` tables as "Spec ahead of implementation" with an OPEN
note. Verification against the v1 binary confirms each divergence:

- `bock build --help` lists only the v1 flags above; `--optimize`,
  `--deliverable`, and `--no-tests` are absent.
- `bock inspect --help` has no `--diff`.
- `bock pin --help` exposes `--all-build` / `--all-runtime` /
  `--all-in`, not a bare `--all`.
- `bock override --help` takes a positional `[NEW_CHOICE]` (and
  `--from-file`) plus bare `--runtime` / `--promote`, not
  `--choice=<alt>` / `--promote <runtime-id>`.
- The build target set is the five built-in targets selected by
  `--target` / `--all-targets`; no `[targets.*]` parsing exists.

This is an editorial reconciliation per a design-decided directive to
align §20 to v1 reality. It makes no new normative CLI or
language-semantics decision: missing surfaces stay reserved (preserved
as design intent), and the syntax rewrites describe what v1 already
does.

### Out-of-scope cross-references (follow-up sync)

Three sections outside this reconciliation's owned scope (§20.1, §20.7,
Appendix A) still reference the now-reconciled surfaces and should be
swept in a follow-up doc-sync, each already cross-referencing §20.1 as
the normative CLI surface:

- §17.2 (Tier 3 — AI Optimization) cites `bock build --optimize`.
- §15 (test functions) cites `bock build --no-tests`.
- §10.8 / §10.4 cite `bock override --promote <selection-id>` (the v1
  form is a bare `--promote` flag on the positional id).

These are not design decisions — §20.1 is the normative CLI section and
is now correct; the cross-references need only be aligned editorially.

## Migration

None. No v1 invocation used the removed flags (`--optimize`,
`--deliverable`, `--no-tests`, `--diff`) — they were never implemented,
so any script using them already failed. The pin/override syntax
rewrites describe the v1 forms that already shipped; scripts using the
real v1 flags are unaffected. The `[targets.*]` tables were inert in v1
(the compiler ignores unknown `bock.project` fields, with a possible
`production`-strictness warning), so no existing project file changes
behavior.
