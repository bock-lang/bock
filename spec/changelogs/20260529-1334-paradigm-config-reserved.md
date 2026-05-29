# Paradigm-mode configuration reserved for v1.x

**Date:** 2026-05-29
**Affects:** §1.5, §6.4, Appendix A.3
**Type:** clarification (editorial reconciliation to v1 reality)

## Change

§1.5 "Paradigm Configuration" previously described configurable paradigm
*modes* (FP / OOP / Multi) selected per project, where the mode controls
which language features are available — e.g., FP mode making `class` a
compile error and `mut` illegal outside `@mutable` scopes, OOP mode making
locals mutable by default. No such configuration exists in v1.

This changelog reconciles §1.5 to v1 reality. v1 ships a single, fixed
paradigm in which all language features are available simultaneously and
bindings are immutable by default — behavior that corresponds to the
**Multi mode** the section already named as the default. Configurable
paradigm-mode switching and the `[paradigm]` project-configuration field
that would select between modes are now marked **Reserved for v1.x**,
following the established deferral pattern used for FFI (§14.1), derive
macros (§6.10), project-level effect routing (§10.4 / Appendix A.3), and
the full cancellation surface (§13.5.2).

### §1.5

- Adds a leading **Status:** note stating that v1 ships one fixed paradigm
  and that configurable modes plus the `[paradigm]` field are Reserved for
  v1.x; the mode descriptions are the planned v1.x surface, informative for
  roadmap tracking, not normative for v1.
- Adds a **v1 behavior (normative)** paragraph: all features are available
  in every v1 project (`class`, traits, functional patterns coexist), with
  no project-level switch; bindings are immutable by default, mutation
  requires explicit `let mut` or a `mut` borrow (§5); v1 does not parse a
  `[paradigm]` field and does not gate `class`, `mut`, or `@mutable` on a
  paradigm mode.
- Keeps the FP / OOP / Multi mode descriptions verbatim, reframed under a
  **Planned for v1.x: paradigm modes** heading, and notes that Multi is the
  only mode v1 implements and the mode v1.x will default to when
  `[paradigm]` is omitted.

### §6.4

- Heading changed from "Classes (OOP and Multi mode)" to "Classes" — modes
  are not a v1 concept, so qualifying the heading by mode was misleading.
- Adds a sentence: classes are always available in v1; the paradigm modes
  that could restrict their use are Reserved for v1.x (§1.5).

### Appendix A.3 (Reserved for future versions)

- Adds a `[paradigm]` bullet documenting paradigm-mode selection
  (`FP` / `OOP` / `Multi`) as a reserved field, consistent with the other
  reserved `bock.project` tables. Notes that v1 ships a fixed paradigm
  equivalent to Multi mode and does not parse the field.

## Rationale

`docs/INVENTORY.md` records `[paradigm]` as drift: spec'd but not
implemented in v1, with no test. Verification against the compiler
confirms this:

- No `paradigm` reference exists anywhere in `compiler/` (lexer, parser,
  types, build, CLI).
- `bock.project` is used only as a project-root marker; it is not
  deserialized into a config struct that accepts `[paradigm]`. `bock new`
  scaffolds only `[project]` and a commented `[ai]` block.
- `class` is parsed unconditionally (`TokenKind::Class => Item::Class(...)`)
  with no mode gating. Mutability is governed by ownership analysis
  (`bock-types/src/ownership.rs`) via `let mut` / `mut` borrows, not by a
  paradigm mode. There is no FP/OOP enforcement.

Delivering configurable paradigm modes would require new language
semantics — a feature-gating layer that rejects `class` in FP mode, flips
the local-binding mutability default in OOP mode, and enforces
encapsulation — none of which exists. Rather than invent that surface or
silently let §1.5 over-promise, this change reconciles §1.5 to what v1
genuinely does (a fixed Multi-equivalent paradigm) and reserves the
configurable-mode surface for v1.x, preserving the descriptions as design
intent. This is an editorial reconciliation; it makes no new normative
language-semantics decision.

## Migration

None. No v1 code or `bock.project` file uses paradigm modes — the field was
never implemented. The compiler ignores unknown `bock.project` fields (it
may warn in `production` strictness), so any `[paradigm]` entry in an
existing project file was already inert. The set of language features
available to v1 programs is unchanged: all features remain available, as
they always have been.
