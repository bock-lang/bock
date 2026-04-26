# Docs — Claude Conventions

This subtree is the mdBook source for the Bock documentation site
at bocklang.org/docs.

## Layout

```
docs/
  book.toml          mdBook config
  src/
    SUMMARY.md       Table of contents — drives navigation
    introduction.md
    getting-started/
    tutorial/
    reference/       Generated and hand-written language reference
    stdlib/          Generated stdlib reference
    contributing/    Contributor-facing docs (playbook, architecture)
```

## Build

```bash
mdbook build docs              # produces docs/book/
mdbook serve docs              # local preview at localhost:3000
mdbook test docs               # runs code samples
```

## Writing Style

- **Direct.** "Run `bock check`." Not "you can run" or "you might
  consider running".
- **Concrete.** Every concept gets a working code sample within a
  few paragraphs.
- **No marketing voice in the docs.** Save that for `website/`.
- **Per-page lede.** First paragraph answers: what is this page
  about and who is it for?
- **Cross-link freely** — but link to the spec for normative
  language behavior. Docs explain; the spec defines.

## Code Samples

- All samples are real, runnable Bock or shell.
- Samples that demonstrate compiler output should match the actual
  output. If the compiler changes, samples need to be updated in
  the same PR.
- Use ` ```bock ` for Bock source, ` ```bash ` for shell.

## Generated Sections

`reference/` (language reference) and `stdlib/` (API docs) are
partly generated from compiler doc comments and stdlib docstrings.
Hand-written prose lives alongside the generated content; the build
script merges them. Do not edit generated files directly — edit the
source they're generated from.

## When Adding a Page

1. Write the page under the appropriate directory.
2. Add it to `src/SUMMARY.md` (mdBook ignores files not listed there).
3. `mdbook build docs` and `mdbook test docs` must pass.
4. Cross-link from related pages.
