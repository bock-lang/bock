# Bock Language for VS Code

Official VS Code extension for the [Bock programming language](https://github.com/bock-lang/bock).

## Features

### Shipped in v1

- [x] Language support (syntax highlighting, grammar)
- [x] LSP client integration (diagnostics, types, definitions)
- [x] Hover with spec links
- [x] Interactive error explanations
- [x] Annotation insight panel
- [x] Effect flow visualization
- [x] Decision manifest UI (build + runtime)
- [x] Searchable spec side panel
- [x] Auto-sync vocabulary with compiler

### Shipped in v1.1 (unreleased)

- [x] AIR tree viewer (powered by `bock inspect air --json`; auto-refresh
      on save, click-to-reveal source spans)
- [x] Target preview (JS/TS/Python/Rust/Go side-by-side, via
      `bock build --source-only`)
- [x] Strictness level picker (status bar + command, edits `bock.project`)
- [x] Code actions / quick fixes (E4013, E4014, E5004, W1001)
- [x] Semantic tokens (richer highlighting; client-side, vocabulary- and
      effect-aware)
- [x] Inlay hints for inferred types (server-side, needs a current `bock`)
- [x] Symbol rename (server-side, with identifier validation)
- [x] Find references (server-side)
- [x] Richer hovers: operators, builtin methods (with candidate receiver
      types), and effect operations declared in the current file
- [x] Spec panel: ranked multi-term search with match highlighting and
      keyboard navigation
- [x] Decisions view: filtering, sort modes, jump-to-source
- [x] Annotations view: group → file → usage tree, workspace usage badge,
      per-file breakdown in the usage analysis

### Planned for v1.1

- [ ] Strictness migration assistant

### Planned for v2

- [ ] Debugger support (DAP)
- [ ] Test runner integration
- [ ] Package manager UI
- [ ] AI provider configuration UI
- [ ] Multi-root workspace support
- [ ] Telemetry + crash reports (opt-in)
- [ ] Interactive tutorials
- [ ] Playground mode (no workspace needed)

## Installation

### From the VS Code Marketplace

```
ext install bock-lang.bock-lang
```

Or search for **Bock Language** in the Extensions view (`Ctrl+Shift+X`
/ `Cmd+Shift+X`).

### From a `.vsix` file

```bash
code --install-extension bock-lang-0.1.1.vsix
```

You can also open the Extensions view, click the `⋯` menu in the top-right,
and choose **Install from VSIX…**.

### Build from source

```bash
git clone https://github.com/bock-lang/bock
cd bock/extensions/vscode
npm install
npm run build
npx vsce package
code --install-extension bock-lang-0.1.1.vsix
```

## Requirements

- Bock compiler installed and on `PATH`, **or**
- `bock.lspPath` setting pointing to the `bock` binary

The extension will warn if the compiler cannot be located. Language
features degrade gracefully — syntax highlighting and the spec panel
continue to work without the compiler.

## Extension Settings

- `bock.lspPath` — explicit path to `bock` binary (optional)
- `bock.specPath` — local spec override (optional)
- `bock.hover.showSpecLinks` — include spec links in hover (default: `true`)
- `bock.effects.autoRender` — show effect flow on hover (default: `false`)
- `bock.decisions.showUnpinnedBadge` — badge unpinned decisions (default: `true`)

## Commands

All commands are registered under the `Bock:` prefix in the Command Palette.

- `Bock: Show Spec` — open the spec side panel
- `Bock: Show Decisions` — reveal the decisions tree view
- `Bock: Explain Error at Cursor` — open a detailed diagnostic explanation
- `Bock: Show Effect Flow for Function` — render the effect graph
- `Bock: Show AIR Tree` — open the AIR tree view for the active file
- `Bock: Preview Transpiled Output` — build the project with
  `--source-only` for a chosen target (or all five) and open the emitted
  file(s) beside the editor
- `Bock: Set Strictness Level` — edit the `[strictness]` table's
  `default` key in `bock.project` (also available from the status bar)
- `Bock: Pin All Build Decisions` — run `bock pin --all-build` and
  refresh the decisions view
- `Bock: Open Spec at Section…` — jump to a specific `§X.Y` section
- `Bock: Refresh Vocabulary` — manually reload `assets/vocab.json`
- `Bock: Restart Language Server` — restart the `bock lsp` process

Tree-view commands (refresh, filter, sort, scope toggles, jump to source
JSON, pin/unpin/override/promote) appear on the view title bars and item
context menus rather than the palette.

## Development

This extension is part of the Bock monorepo. To build:

```bash
cd extensions/vscode
npm install
npm run build
```

Launch an Extension Development Host with `F5` from the extension folder in VS Code.

To regenerate the bundled vocabulary and spec from the compiler, run the sync
script from the repo root (it builds `bock-dump-vocab`, writes
`assets/vocab.json`, and copies `spec/sections/` into `assets/spec/`):

```bash
./tools/scripts/sync-vocab.sh
```

### Layout

```
src/
  extension.ts           entry point — activation, feature wiring
  vocab.ts               VocabService — loads assets/vocab.json
  lsp.ts                 LanguageClient setup (spawns `bock lsp`)
  features/
    hover.ts             hover with spec links (F1.5.3); pure rendering in hover-render.ts
    errors.ts            interactive error explanations (F1.5.4)
    annotations.ts       annotation insight tree (F1.5.5); pure scanner in annotations-scan.ts
    effects.ts           effect flow webview (F1.5.6); pure helpers in effect-analyzer.ts + effects-flow.ts
    decisions.ts         decision manifest UI (F1.5.7)
    spec-panel.ts        searchable spec panel (F1.5.8)
    semantic-tokens.ts   semantic tokens provider; pure scanner in semantic-scan.ts
    quick-fixes.ts       diagnostic code actions; pure logic in quick-fixes-logic.ts
    target-preview.ts    target preview (--source-only builds); path mapping in preview-paths.ts
    strictness.ts        strictness status-bar picker; pure TOML editing in strictness-toml.ts
    air-viewer.ts        AIR tree view (bock inspect air); pure model in air-model.ts
  shared/
    webview.ts           WebviewManager + CSP/nonce/escape helpers
    markdown.ts          marked.js wrapper
    strings.ts           pure string helpers
    types.ts             TS types mirroring crates/bock-vocab/schema.rs
assets/
  vocab.json             compiler vocabulary (generated)
  spec/                  bundled spec markdown (generated)
snippets/
  bock.code-snippets     starter snippets (hand-maintained)
syntaxes/
  bock.tmLanguage.json   TextMate grammar
```

## License

MIT
