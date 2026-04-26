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
npm test                        # vscode-test runner
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
