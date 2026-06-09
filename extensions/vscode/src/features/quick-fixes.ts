// Quick fixes for common Bock diagnostics.
//
// Registers a CodeActionProvider that maps the diagnostics published by
// `bock lsp` (source "bock", string codes like E4013/E4014/E5004/W1001)
// through the pure builders in `quick-fixes-logic.ts` into concrete
// `WorkspaceEdit`-backed code actions. All fix derivation — message
// parsing, document-text validation, stale-diagnostic bail-outs — lives
// in the logic module so it stays headless-testable; this file is only
// the vscode-API adapter.
//
// Distinct from the "Explain {code}" actions in `errors.ts`: those open
// a documentation webview, these edit the document.

import * as vscode from 'vscode';
import {
  buildQuickFixesForAll,
  normalizeDiagnosticCode,
  type QuickFixInput,
  type QuickFixSuggestion,
} from './quick-fixes-logic';

const BOCK_SELECTOR: vscode.DocumentSelector = {
  scheme: 'file',
  language: 'bock',
};

/** Registers the Bock quick-fix CodeActionProvider. */
export function registerQuickFixes(ctx: vscode.ExtensionContext): void {
  ctx.subscriptions.push(
    vscode.languages.registerCodeActionsProvider(
      BOCK_SELECTOR,
      new BockQuickFixProvider(),
      {
        providedCodeActionKinds: BockQuickFixProvider.providedCodeActionKinds,
      },
    ),
  );
}

/** Maps `bock`-sourced diagnostics to edit-applying quick-fix actions. */
export class BockQuickFixProvider implements vscode.CodeActionProvider {
  static readonly providedCodeActionKinds = [vscode.CodeActionKind.QuickFix];

  provideCodeActions(
    doc: vscode.TextDocument,
    _range: vscode.Range | vscode.Selection,
    context: vscode.CodeActionContext,
  ): vscode.CodeAction[] {
    const bockDiags = context.diagnostics.filter(
      (diag) => diag.source === 'bock',
    );
    if (bockDiags.length === 0) return [];

    const documentText = doc.getText();
    const inputs: QuickFixInput[] = [];
    const inputDiags: vscode.Diagnostic[] = [];
    for (const diag of bockDiags) {
      const code = normalizeDiagnosticCode(diag.code);
      if (!code) continue;
      inputs.push({
        code,
        message: diag.message,
        range: {
          startLine: diag.range.start.line,
          startChar: diag.range.start.character,
          endLine: diag.range.end.line,
          endChar: diag.range.end.character,
        },
        documentText,
        relatedMessages: diag.relatedInformation?.map((r) => r.message),
      });
      inputDiags.push(diag);
    }

    const actions: vscode.CodeAction[] = [];
    buildQuickFixesForAll(inputs).forEach((suggestions, i) => {
      for (const suggestion of suggestions) {
        actions.push(toCodeAction(suggestion, inputDiags[i], doc.uri));
      }
    });
    return actions;
  }
}

/** Converts one provider-agnostic suggestion into a `CodeAction`. */
function toCodeAction(
  suggestion: QuickFixSuggestion,
  diagnostic: vscode.Diagnostic,
  uri: vscode.Uri,
): vscode.CodeAction {
  const action = new vscode.CodeAction(
    suggestion.title,
    vscode.CodeActionKind.QuickFix,
  );
  const edit = new vscode.WorkspaceEdit();
  for (const e of suggestion.edits) {
    edit.replace(
      uri,
      new vscode.Range(
        new vscode.Position(e.startLine, e.startChar),
        new vscode.Position(e.endLine, e.endChar),
      ),
      e.newText,
    );
  }
  action.edit = edit;
  action.diagnostics = [diagnostic];
  if (suggestion.isPreferred) {
    action.isPreferred = true;
  }
  return action;
}
