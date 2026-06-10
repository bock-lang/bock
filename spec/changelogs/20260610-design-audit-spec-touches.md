# Design-audit spec touches — Tier default posture (§17.2) + v1.x LSP sequencing note (§20.3)

**Date:** 2026-06-10
**Affects:** §17.2, §20.3
**Type:** clarification

## Change

Two small corrections routed from the 2026-06-09 strategic design audit
(`tracking/designs/2026-06-09-design-audit.md`, recommendations R2 and R4):

1. **§17.2 tier labels (audit R2, ruled in audit §4.5).** The spec was
   internally inconsistent about the generation default: §17.2 labeled
   Tier 1 (AI generation) "default" while §20.7 states "Bock uses
   rule-based code generation by default; AI configuration is opt-in" —
   and §20.7 matches both the implementation and the intended posture.
   The tier labels are amended accordingly:
   - Tier 1 — AI Generation **(when configured)**
   - Tier 2 — Rule-Based Generation **(default and fallback)**

   Tier 2's body is clarified in the same spirit: it is the default
   generation path, and `bock build --deterministic` / `@deterministic`
   force Tier-2-*only* operation (AI disabled even where configured),
   rather than being what "activates" Tier 2.

2. **§20.3 v1.x sequencing note (audit R4).** A non-normative note
   reorients the v1.x editor-adjacent work agent-first: the lead v1.x
   tooling item is a `bock-mcp` server exposing `check`/`build`/`test`/
   `inspect`/conformance as MCP tools; the five human-facing Bock-specific
   LSP extensions follow behind it. The note also records that Target
   Preview (and several capabilities beyond the v1 LSP floor) already
   shipped early via the 2026-06-09 editor wave. The five-extension list
   itself is preserved as design intent, unchanged.

## Rationale

The 2026-06-09 design audit found §17.2's "default" label contradicted
§20.7 and reality (the compiler was built almost entirely on the
deterministic path; the conformance suite runs without an API key), and
that the v1.x extension list was designed for a human-in-IDE world while
the highest-leverage 2026 surface is the agent-facing MCP server. R2 was
ruled by Design in the audit itself; R4's spec touch is the small note the
audit's routing table prescribes. Neither changes behavior.

## Migration

None. No syntax, semantics, or tooling behavior changes; the §17.2
amendment documents the posture the implementation already has.
