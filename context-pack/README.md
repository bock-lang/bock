# Bock Context Pack

`BOCK-CONTEXT-PACK.md` is a single, self-contained primer that makes a
frontier LLM a competent Bock author at session start. It exists because
models write Python from billions of training examples and Bock from zero
(design audit 2026-06-09, risk R-A): for an AI-first language, **docs are
training data** — so every claim in the pack must be true of the current
implementation and every example must be verified against the real compiler.

## Who consumes it

- **Agents / models**: paste the pack (or attach the file) into the model's
  context before asking for Bock code. It is sized to fit comfortably
  alongside a working context (~1,000 lines).
- **Humans**: it doubles as a fast on-ramp, but `spec/bock-spec.md` and
  `docs/` remain the authoritative references.

## How to verify / regenerate

The drift-guard is `tools/scripts/verify-context-pack.sh`:

```bash
./tools/scripts/verify-context-pack.sh          # builds bock, checks every
                                                # ```bock block in the pack
BOCK_BIN=/path/to/bock ./tools/scripts/verify-context-pack.sh   # reuse a binary
```

It extracts every fenced ```bock code block from the pack and runs
`bock check` on each, exiting non-zero on any failure. Intentionally-wrong
snippets in the pack use ```text fences, so the rule is simple: **every
```bock block must check clean.**

The script verifies *checkability*, not execution. The worked examples in
pack §6 were additionally executed when the pack was authored (interpreter
and/or transpiled targets — each example states which); re-run them manually
when regenerating after compiler changes, and reconcile pack §8 (Known
divergences) against current behavior.

## Versioning rule

The pack carries its own version (header field `Pack version`), independent
of the compiler version:

- **Bump the minor version** (0.1.0 → 0.2.0) when spec-meaningful content
  changes: language surface, stdlib surface, error-code table, the v1
  boundary, or any entry added/removed in Known divergences.
- **Bump the patch version** for wording, formatting, or example polish that
  doesn't change what a model would do.
- Always refresh the header's `Repo commit` and spec-derivation line when
  bumping, and re-run the verify script.

The pack is versioned in-repo. Publishing it externally (website, model
provider docs) is operator-gated — see OQ2 in the tracking hub.

## Sources of truth

| Pack section | Derived from |
|---|---|
| Mental model, primer, v1 boundary | `spec/bock-spec.md` (§1–§21) |
| Error-code table | `compiler/crates/bock-errors/src/catalog.rs` + emission sites in `bock-lexer`, `bock-parser`, `bock-air`, `bock-types` |
| Worked examples | Authored for the pack; style per `examples/` and `CLAUDE.md` Bock code style |
| Pitfalls / divergences | `compiler/tests/conformance/` fixtures + direct observation of the built compiler at the pinned commit |
