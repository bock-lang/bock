# Specification — Claude Conventions

This subtree holds the Bock language specification and its
changelog history.

## Layout

```
spec/
  changelogs/    Dated entries describing spec changes (RFC outcomes)
```

## Editing Rules

- **The spec is normative.** If the compiler and the spec disagree,
  one of them is wrong — file an issue rather than silently letting
  drift continue.
- **Every change to `bock-spec.md` requires a changelog entry** under
  `changelogs/<YYYYMMDD>-<short-name>.md`.
- **Breaking changes pre-1.0** are allowed but must be explicit in
  the changelog. Post-1.0, breaking changes need an RFC and a
  deprecation cycle.

## Changelog Entry Format

`spec/changelogs/<YYYYMMDD>-<short-name>.md`:

```markdown
# <Title>

**Date:** YYYY-MM-DD
**Affects:** <list of spec sections>
**Type:** clarification | addition | breaking change

## Change
What changed, in normative terms.

## Rationale
Why this change. Reference the RFC or issue.

## Migration
What existing code needs to do (if anything).
```

## Authoritative Sources

Spec text is the source of truth for **language semantics**. Stdlib
docs in `stdlib/` and tutorials in `docs/` reference the spec; they
do not redefine it. If you need to repeat a rule, link instead.
