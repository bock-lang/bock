# Update Vocabulary

Regenerate the VS Code extension's vocabulary file from authoritative
sources (lexer keywords, stdlib symbols, spec terms).

## Steps

1. **Run the sync script:**
   ```
   ./tools/scripts/sync-vscode-assets.sh
   ```

   This updates:
   - `extensions/vscode/syntaxes/bock.tmLanguage.json`
   - `extensions/vscode/snippets/bock.json`
   - `extensions/vscode/vocab.json`

2. **Verify `vocab.json` is valid JSON:**
   ```
   python3 -m json.tool extensions/vscode/vocab.json > /dev/null
   ```

3. **Verify the extension still builds and lints:**
   ```
   cd extensions/vscode
   npm run lint
   npm run compile
   ```

4. **Diff review.** The script may regenerate large blocks. Skim
   the diff to make sure no manual edits were clobbered. If they
   were, the manual edit belongs upstream (in the lexer or stdlib),
   not in the generated file.

5. **Commit** the regenerated files in a single commit:
   ```
   git add extensions/vscode/syntaxes extensions/vscode/snippets extensions/vscode/vocab.json
   git commit -m "extension: regenerate vocab from current keywords and stdlib"
   ```

## Done When

- All three generated files are valid and committed
- Extension builds clean
- No untracked changes in `extensions/vscode/`
