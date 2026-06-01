# Spec changes and the changelog

Changing the language is deliberate. This page covers the process for a
normative change, how the consumer-facing changelog is produced, and
which repository files are generated rather than hand-edited.

## Changing the specification

The specification at `spec/bock-spec.md` is the normative definition of
the language. A change to grammar, type rules, effect rules, context
rules, or any public surface follows this path:

1. **Open an issue** describing the proposed change and the motivation.
2. **Add a changelog entry** under `spec/changelogs/` using the
   `YYYYMMDD-HHMM-specs-changes.md` filename convention (use UTC, e.g.
   `date -u`).
3. **Edit the affected section(s)** of `spec/bock-spec.md`.
4. **Add a conformance fixture** under
   `compiler/tests/conformance/<category>/` that exercises the new
   behavior, so the change is pinned by an executable test. See
   [Development workflow → Conformance](./workflow.md#conformance).

When the *implementation* turns out to diverge from the spec, do not
quietly change one to match the other. Surface the divergence for design
review. If review confirms the spec was wrong, the fix lands with a
changelog entry recording the amendment; the implementation is not
treated as authoritative by default.

## The changelog

`CHANGELOG.md` has a `## Unreleased` section that is **generated from
merged-PR history**, not written by hand and not written by CI. Git
history is the source of truth.

```bash
tools/scripts/gen-changelog.sh            # rewrite CHANGELOG.md in place
tools/scripts/gen-changelog.sh --stdout   # preview without writing
tools/scripts/gen-changelog.sh --check    # exit non-zero if stale (CI guard)
```

How it works:

- It collects every PR number already filed under a released
  (`## vX...`) section, then lists each squash-merge subject ending in
  `(#NN)` whose number is not yet released. Those become the Unreleased
  entries.
- It is **idempotent** — running it twice produces no diff — and
  rewrites only the `## Unreleased` block. Released sections and the file
  header are left untouched.
- Purely internal `tracking:`-prefixed PRs are excluded from this
  consumer-facing changelog.
- It is **tag-independent**: it walks full history today (no release
  tags yet) and will prefer the latest `v*` tag as its base once one
  exists.

CI **never writes** the changelog. Writing to the protected `main`
branch from CI is both impossible (no direct pushes) and a supply-chain
risk, so the changelog lands through the same reviewed pull-request flow
as any other change. The release workflow only *verifies* the section is
in sync with `--check`.

## Generated files

Some files in the repository are generated and must not be hand-edited;
editing them directly is reverted by a CI `--check`. Change the source,
then regenerate.

| Generated file | Source | Generator |
|----------------|--------|-----------|
| `STATUS.md` | the `tracking/` hub (`snapshot` + `queue`) | `tools/scripts/gen-tracking-views.sh` |
| `ROADMAP.md` | `tracking/milestones.md` | `tools/scripts/gen-tracking-views.sh` |
| `CHANGELOG.md` (`## Unreleased`) | merged-PR history | `tools/scripts/gen-changelog.sh` |

The API reference and stdlib reference under `docs/src/reference/` are
likewise partly generated from compiler doc comments and stdlib
docstrings. Edit the doc comments in the source, not the generated
output; hand-written prose lives alongside the generated content.
