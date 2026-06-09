# Changelog

All notable changes to the **Bock Language** VS Code extension are documented
here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.1] — Unreleased

### Added

- **AIR tree viewer** — `Bock: Show AIR Tree` opens an AIR outline of the
  active file in the Bock activity-bar container, powered by
  `bock inspect air --json` (#325): auto-refresh on save, click-to-reveal
  source spans, and in-view error states. (#329)
- **Target preview** — `Bock: Preview Transpiled Output` builds the project
  with `bock build -t <target> --source-only` for a chosen target (or all
  five) and opens the emitted file(s) beside the editor. (#326)
- **Strictness picker** — a status-bar item and `Bock: Set Strictness Level`
  command that edit the `[strictness]` table's `default` key in
  `bock.project`. (#326)
- **Quick fixes** — code actions for E4013 (method-name suggestion, e.g.
  Map `contains` → `contains_key`), E4014 (bare → braced import), E5004
  (insert `mut` on the binding), and W1001 (remove unused import entry). (#327)
- **Semantic tokens** — a client-side semantic-token provider using standard
  token types only, vocabulary- and effect-aware. (#328)
- **Language server: references, rename, document symbols** — `bock lsp`
  now provides find references, symbol rename (validated: identifier shape,
  keyword rejection, case-class preservation), and hierarchical document
  symbols. Requires a current `bock` binary. (#324)
- **Language server: inlay hints** — inferred-type hints on unannotated
  `let` / `let mut` / destructuring / `for` binders; hints for unresolved or
  error types are suppressed, and renders are capped at 60 characters.
  Requires a current `bock` binary. (#330)
- `Bock: Restart Language Server` command and starter snippets
  (`snippets/bock.code-snippets`). (#317)

### Changed

- Migrated to `vscode-languageclient` v10. The minimum supported editor
  version is now **VS Code 1.91.0** (raised from 1.75.0). (#290)
- Hover now also covers operators, builtin methods (listing candidate
  receiver types), and effect operations declared in the current file. (#321)
- Spec-panel search ranks multi-term queries (title over body,
  word-boundary over substring matches), highlights matches, and supports
  keyboard navigation (`↑`/`↓`/`Enter`/`Escape`). (#322)
- The Decisions view gained filtering (type / pinned / minimum confidence)
  via a filter menu, sort modes, a clear-filters command, and a
  "Jump to Source JSON" context action; active filters and sort are shown
  in the view description. (#323)
- The Annotations tree is now group → file → usage with usage and file
  counts, the view badge shows the workspace-wide usage total, and the
  usage-analysis webview adds a per-file breakdown and a parameter-pattern
  summary. (#320)
- The extension now requires **workspace trust** before spawning the `bock`
  language server / CLI. The server binary is resolved only from `PATH` or a
  machine-scoped `bock.lspPath` (which supports `${workspaceFolder}` and `~`)
  — it is no longer auto-discovered from a workspace's `target/` directory,
  which was an arbitrary-code-execution risk when opening untrusted repos.
  (#318)

### Fixed

- Activation is now resilient: a missing or broken `bock` binary, or a
  corrupt vocabulary file, degrades gracefully instead of disabling the
  whole extension. (#308)
- Decision-manifest records are validated on load, so a malformed file can
  no longer crash the Decisions view. (#309)
- The effect-flow panel correctly parses the effect clause on single-line
  `-> T with E` signatures. (#313)
- The annotation scanner no longer mis-handles triple-quoted strings. (#309)

### Performance

- Effect-flow auto-render is debounced, and annotation scanning is now
  incremental. (#309)

### Internal

- Unified the webview CSP/nonce handling (crypto-secure) and removed dead
  code. (#310, #311)
- The headless test suite grew from 7 to 168 tests (#314, #315), then to
  435 across the v1.1 feature wave. (#320–#330)

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
