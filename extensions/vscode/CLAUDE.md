# VS Code Extension — Claude Conventions

The official Bock language extension for VS Code. Provides syntax
highlighting, snippets, diagnostics, and (eventually) language
server features.

## TypeScript Conventions

- `"strict": true` in `tsconfig.json` — non-negotiable.
- ESLint clean. No `any` without an inline `// eslint-disable-next-line`
  and a comment explaining why.
- One module per language feature (highlighter config, completion
  provider, diagnostic bridge, etc.).
- `async/await` over raw promises.

## Dev Loop

```bash
cd extensions/vscode
npm install                     # first time
npm run compile                 # tsc build
npm run watch                   # tsc --watch for the inner loop
npm test                        # mocha + ts-node unit tests (headless)
npm run lint                    # eslint
```

To launch the extension in a host VS Code window:
- Open `extensions/vscode/` in VS Code
- Press F5 ("Run Extension") — opens an extension dev host

## Vocab Sync Reminder

The syntax grammar, snippets, and `vocab.json` are **generated** —
do not hand-edit. If you find yourself wanting to, the right fix
is upstream:

- New keyword? Add it to the lexer.
- New stdlib symbol? Add it to the stdlib.
- Then regenerate via `/project:update-vocab`.

Hand-editing generated files will be reverted on the next sync run.

## Packaging

```bash
npx @vscode/vsce package        # produces .vsix
```

Publishing happens through `release.yml` on tag push, not manually.

## Testing

Unit tests live in `test/` and run headlessly with **Mocha + ts-node**
— no Extension Host, no Electron download, so they run in plain CI.

```bash
npm test                        # type-checks test/ then runs the suite
```

What this means in practice:

- Tests target **pure logic** — parsers and helpers that don't need the
  live `vscode` API (e.g. `extractEffects` / `parseProjectEffects` in
  `features/effect-analyzer.ts`, `escapeHtml` in `shared/webview.ts`).
- Source modules still carry a top-level `import * as vscode from 'vscode'`
  for type annotations. `test/register-vscode.ts` (a Mocha `--require`
  hook) intercepts `require('vscode')` and returns `test/vscode-stub.ts`,
  a minimal stand-in exposing only the runtime constructors the tested
  code touches (`Position`, `Range`, `Uri`). Extend the stub if a newly
  tested function references more of the API.
- `npm test` first runs `tsc --noEmit -p test/tsconfig.json` so the test
  sources are genuinely type-checked (ts-node executes transpile-only at
  runtime), then runs Mocha. Both must be green.
- Logic that genuinely needs the live `vscode` API (commands, webview
  panels, tree views) is **not** covered here — that needs
  `@vscode/test-electron`. Prefer extracting pure helpers and testing
  those over reaching for the Electron harness.

`test/` and `.mocharc.json` are excluded from the packaged `.vsix` via
`.vscodeignore`.
