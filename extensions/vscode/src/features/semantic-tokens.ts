// Client-side semantic tokens for Bock.
//
// Registers a full-document `DocumentSemanticTokensProvider` for the
// 'bock' language, layering vocab- and effect-aware highlighting on top of
// the TextMate grammar. All tokenization lives in the pure, headless-tested
// scanner (`semantic-scan.ts`); this module only adapts it to the VS Code
// API: legend construction, builder encoding, vocab plumbing, and a
// refresh event when the vocabulary reloads.
//
// The legend uses ONLY standard token types and modifiers (see the mapping
// table in `semantic-scan.ts`), so no `contributes.semanticTokenTypes` /
// theme additions are needed — every built-in theme renders these.
//
// Degraded-vocab behaviour: an empty or partially-corrupt vocab (the
// fallback path in `VocabService.load`) yields empty prelude name lists,
// which simply suppresses the `defaultLibrary` tokens — structural tokens
// (declarations, effects, annotations, module paths) still work. A scanner
// throw is caught and rendered as "no semantic tokens" rather than an
// error: the TextMate layer always covers the base.

import * as vscode from 'vscode';
import { VocabService } from '../vocab';
import {
  scanSemanticTokens,
  semanticVocabInput,
  SEMANTIC_TOKEN_TYPES,
  SEMANTIC_TOKEN_MODIFIERS,
  SemanticToken,
} from './semantic-scan';

/** The legend for every token this extension emits — standard entries only. */
export const legend = new vscode.SemanticTokensLegend(
  [...SEMANTIC_TOKEN_TYPES],
  [...SEMANTIC_TOKEN_MODIFIERS],
);

class BockSemanticTokensProvider
  implements vscode.DocumentSemanticTokensProvider, vscode.Disposable
{
  private readonly changeEmitter = new vscode.EventEmitter<void>();

  /** Lets VS Code re-request tokens after a vocabulary reload. */
  readonly onDidChangeSemanticTokens = this.changeEmitter.event;

  constructor(private readonly vocab: VocabService) {}

  refresh(): void {
    this.changeEmitter.fire();
  }

  dispose(): void {
    this.changeEmitter.dispose();
  }

  provideDocumentSemanticTokens(
    document: vscode.TextDocument,
    _token: vscode.CancellationToken,
  ): vscode.SemanticTokens {
    let tokens: SemanticToken[] = [];
    try {
      tokens = scanSemanticTokens(
        document.getText(),
        semanticVocabInput(this.vocab.get()),
      );
    } catch {
      // Never let a scanner bug take down rendering — emit nothing and let
      // the TextMate grammar carry the highlighting.
    }
    const builder = new vscode.SemanticTokensBuilder(legend);
    for (const t of tokens) {
      builder.push(
        new vscode.Range(
          new vscode.Position(t.line, t.char),
          new vscode.Position(t.line, t.char + t.length),
        ),
        t.tokenType,
        t.tokenModifiers,
      );
    }
    return builder.build();
  }
}

/** Wires the semantic-token provider into the extension. */
export function registerSemanticTokens(
  ctx: vscode.ExtensionContext,
  vocab: VocabService,
): void {
  const provider = new BockSemanticTokensProvider(vocab);
  ctx.subscriptions.push(
    provider,
    vocab.onDidChange(() => provider.refresh()),
    vscode.languages.registerDocumentSemanticTokensProvider(
      { language: 'bock' },
      provider,
      legend,
    ),
  );
}
