# Changelog

All notable changes to the **Bock Language** VS Code extension are documented
here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] — 2026-04-19

First public release. Ships the eight core features that make Bock a
first-class experience in VS Code.

### Added

- **Language support** — TextMate grammar covering keywords, operators,
  annotations, string/char literals, numeric formats (hex, octal, binary),
  doc comments, and set literals. Language configuration provides
  auto-closing pairs, bracket matching, folding, and indentation rules.
- **LSP client** — spawns `bock lsp` over stdio and wires up diagnostics,
  types, and go-to-definition. Respects the `bock.lspPath` setting; falls
  back to `PATH` lookup and warns gracefully when the binary is missing.
- **Hover with spec links** — vocabulary-enriched hover popups for
  keywords, operators, annotations, primitives, prelude symbols, and
  stdlib signatures. Each hover includes a clickable `§X.Y` link that
  opens the spec panel at the referenced section.
- **Interactive error explanations** — lightbulb code action
  (`Explain {code}`) and command (`Bock: Explain Error at Cursor`)
  open a webview with a rich description, good/bad examples, related
  codes, and quick-fix teasers. A status bar indicator surfaces the
  diagnostic under the cursor for one-click access.
- **Annotation insight panel** — `Bock Annotations` tree view in the
  Explorer groups every `@annotation` occurrence by name, with a nested
  webview for cross-file usage analysis. Hover tooltips pull spec links
  from the compiler vocabulary.
- **Effect flow visualization** — Mermaid-rendered effect graph for the
  function at the cursor. Handler resolution is analyzed across three
  layers (local `handling` blocks, module `handle`, project defaults).
  Mermaid is bundled for offline use.
- **Decision manifest UI** — `Bock Decisions` tree view loads records
  from `.bock/decisions/{build,runtime}/**/*.json`, grouped by module
  with a scope toggle (Build ↔ Runtime ↔ All). Pin, unpin, override,
  and promote commands shell out to the `bock` CLI. A detail webview
  renders reasoning, alternatives, and confidence. A status bar item
  surfaces unpinned runtime decisions.
- **Searchable spec side panel** — loads `assets/spec/bock-spec.md`
  (or an `bock.specPath` override), parses the heading tree, and
  renders sections with Bock-aware syntax highlighting. Client-side
  search and cross-section links work inside the webview. The
  `bock.openSpecAt §X.Y` command normalizes section references and
  scrolls to the target.
- **Auto-sync vocabulary** — loads compiler-emitted `assets/vocab.json`
  at startup and watches the file for changes. Manual reload via the
  `Bock: Refresh Vocabulary` command.

### Commands

`Bock: Show Spec`, `Bock: Show Decisions`, `Bock: Explain Error at
Cursor`, `Bock: Show Effect Flow for Function`, `Bock: Open Spec at
Section…`, `Bock: Refresh Vocabulary`, plus contextual pin / unpin /
override / promote commands on decision items.

### Settings

- `bock.lspPath` — explicit path to the `bock` binary.
- `bock.specPath` — local spec directory override.
- `bock.hover.showSpecLinks` — include spec links in hover popups.
- `bock.effects.autoRender` — auto-render effect flow on hover.
- `bock.decisions.showUnpinnedBadge` — badge the Decisions view
  when unpinned runtime decisions exist.

### Requirements

- VS Code 1.75.0 or newer.
- The `bock` compiler binary on `PATH`, or an explicit `bock.lspPath`
  pointing to it.
