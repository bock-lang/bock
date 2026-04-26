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

### Planned for v1.1

- [ ] AIR tree viewer
- [ ] Target preview (JS/TS/Python/Rust/Go side-by-side)
- [ ] Strictness level picker + migration assistant
- [ ] Code actions / quick fixes
- [ ] Semantic tokens (richer highlighting)
- [ ] Inlay hints for inferred types
- [ ] Symbol rename
- [ ] Find references

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
code --install-extension bock-lang-0.1.0.vsix
```

You can also open the Extensions view, click the `⋯` menu in the top-right,
and choose **Install from VSIX…**.

### Build from source

```bash
git clone https://github.com/bock-lang/bock
cd bock/extensions/vscode/bock-lang
npm install
npm run build
npx vsce package
code --install-extension bock-lang-0.1.0.vsix
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
- `Bock: Open Spec at Section…` — jump to a specific `§X.Y` section
- `Bock: Refresh Vocabulary` — manually reload `assets/vocab.json`

## Development

This extension is part of the Bock monorepo. To build:

```bash
cd extensions/vscode/bock-lang
npm install
npm run build
```

Launch an Extension Development Host with `F5` from the extension folder in VS Code.

To regenerate the bundled vocabulary and spec from the compiler:

```bash
./scripts/sync-vscode-assets.sh
```

### Layout

```
src/
  extension.ts           entry point — activation, feature wiring
  vocab.ts               VocabService — loads assets/vocab.json
  lsp.ts                 LanguageClient setup (spawns `bock lsp`)
  features/
    hover.ts             hover with spec links (F1.5.3)
    errors.ts            interactive error explanations (F1.5.4)
    annotations.ts       annotation insight tree (F1.5.5)
    effects.ts           effect flow webview (F1.5.6)
    decisions.ts         decision manifest UI (F1.5.7)
    spec-panel.ts        searchable spec panel (F1.5.8)
  shared/
    webview.ts           CSP-protected webview base class
    markdown.ts          marked.js wrapper
    types.ts             TS types mirroring crates/bock-vocab/schema.rs
assets/
  vocab.json             compiler vocabulary (generated)
  spec/                  bundled spec markdown (generated)
syntaxes/
  bock.tmLanguage.json   TextMate grammar
```

## License

MIT
