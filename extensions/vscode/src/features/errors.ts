// Interactive error explanations (F1.5.4).
//
// Every Bock diagnostic carries a structured code (E1001, FC-29, …). This
// module turns those codes into learning opportunities:
//
//   1. A CodeActionProvider attaches "Explain {code}" to every diagnostic
//      so it surfaces in the lightbulb menu.
//   2. The `bock.explainError` command opens a webview rendered from the
//      vocab entry — summary, description, bad/good examples, spec links,
//      related codes, and any quick-fix teasers.
//   3. A status bar indicator tracks the diagnostic under the cursor and
//      offers a one-click shortcut to the same webview.

import * as vscode from 'vscode';
import type { LanguageClient } from 'vscode-languageclient/node';
import { VocabService } from '../vocab';
import { escapeHtml, WebviewManager } from '../shared/webview';
import type { DiagnosticCode } from '../shared/types';

const BOCK_SELECTOR: vscode.DocumentSelector = {
  scheme: 'file',
  language: 'bock',
};

export function registerErrors(
  ctx: vscode.ExtensionContext,
  vocab: VocabService,
  _client: LanguageClient | undefined,
): void {
  const manager = new WebviewManager(ctx);

  ctx.subscriptions.push(
    vscode.commands.registerCommand(
      'bock.explainError',
      async (code?: string, range?: vscode.Range) => {
        const resolved = code ?? findDiagnosticCodeAtCursor();
        if (!resolved) {
          vscode.window.showInformationMessage(
            'Bock: no diagnostic under cursor.',
          );
          return;
        }
        const entry = vocab.getDiagnostic(resolved);
        if (!entry) {
          vscode.window.showWarningMessage(
            `Bock: no explanation available for ${resolved}.`,
          );
          return;
        }
        openExplanation(manager, entry, range);
      },
    ),
  );

  ctx.subscriptions.push(
    vscode.languages.registerCodeActionsProvider(
      BOCK_SELECTOR,
      new ExplainCodeActionProvider(),
      { providedCodeActionKinds: [vscode.CodeActionKind.QuickFix] },
    ),
  );

  registerStatusBar(ctx);
}

// ─── CodeActionProvider ─────────────────────────────────────────────────────

class ExplainCodeActionProvider implements vscode.CodeActionProvider {
  static readonly providedCodeActionKinds = [vscode.CodeActionKind.QuickFix];

  provideCodeActions(
    _doc: vscode.TextDocument,
    _range: vscode.Range | vscode.Selection,
    context: vscode.CodeActionContext,
  ): vscode.CodeAction[] {
    const actions: vscode.CodeAction[] = [];
    for (const diag of context.diagnostics) {
      const code = diagnosticCodeString(diag);
      if (!code) continue;
      const action = new vscode.CodeAction(
        `Explain ${code}`,
        vscode.CodeActionKind.QuickFix,
      );
      action.command = {
        command: 'bock.explainError',
        title: `Explain ${code}`,
        arguments: [code, diag.range],
      };
      action.diagnostics = [diag];
      actions.push(action);
    }
    return actions;
  }
}

// ─── Status bar indicator ───────────────────────────────────────────────────

function registerStatusBar(ctx: vscode.ExtensionContext): void {
  const item = vscode.window.createStatusBarItem(
    vscode.StatusBarAlignment.Right,
    100,
  );
  item.command = 'bock.explainError';
  ctx.subscriptions.push(item);

  const refresh = () => {
    const code = findDiagnosticCodeAtCursor();
    if (!code) {
      item.hide();
      return;
    }
    item.text = `$(info) Explain ${code}`;
    item.tooltip = `Open the Bock explanation for ${code}.`;
    item.show();
  };

  ctx.subscriptions.push(
    vscode.window.onDidChangeActiveTextEditor(refresh),
    vscode.window.onDidChangeTextEditorSelection(refresh),
    vscode.languages.onDidChangeDiagnostics(refresh),
  );
  refresh();
}

// ─── Cursor-based diagnostic lookup ─────────────────────────────────────────

function findDiagnosticCodeAtCursor(): string | undefined {
  const editor = vscode.window.activeTextEditor;
  if (!editor || editor.document.languageId !== 'bock') return undefined;

  const pos = editor.selection.active;
  const diags = vscode.languages
    .getDiagnostics(editor.document.uri)
    .filter((d) => d.range.contains(pos));

  for (const diag of diags) {
    const code = diagnosticCodeString(diag);
    if (code) return code;
  }
  return undefined;
}

function diagnosticCodeString(diag: vscode.Diagnostic): string | undefined {
  const raw = diag.code;
  if (raw === undefined || raw === null) return undefined;
  if (typeof raw === 'string') return raw;
  if (typeof raw === 'number') return String(raw);
  if (typeof raw === 'object' && 'value' in raw) {
    const v = raw.value;
    return typeof v === 'string' ? v : String(v);
  }
  return undefined;
}

// ─── Webview rendering ──────────────────────────────────────────────────────

function openExplanation(
  manager: WebviewManager,
  entry: DiagnosticCode,
  range: vscode.Range | undefined,
): void {
  const title = `Bock — ${entry.code}`;
  const content = renderExplanation(manager, entry, range);
  const handle = manager.create('bock.explainError', title, content);
  handle.panel.webview.onDidReceiveMessage((msg) => {
    if (msg?.type === 'openSpec' && typeof msg.ref === 'string') {
      void vscode.commands.executeCommand('bock.openSpecAt', msg.ref);
    } else if (msg?.type === 'explainCode' && typeof msg.code === 'string') {
      void vscode.commands.executeCommand('bock.explainError', msg.code);
    }
  });
}

function renderExplanation(
  manager: WebviewManager,
  entry: DiagnosticCode,
  range: vscode.Range | undefined,
): { body: string; scripts: string[] } {
  const mdParts: string[] = [];
  mdParts.push(`# ${entry.code} — ${entry.summary}`);
  if (range) {
    mdParts.push(locationLine(range));
  }
  mdParts.push('');
  mdParts.push('## What went wrong');
  mdParts.push(entry.description || '_No description provided._');

  mdParts.push('');
  mdParts.push('## Example (incorrect)');
  mdParts.push(codeFenceOrPlaceholder(entry.bad_example));

  mdParts.push('');
  mdParts.push('## Example (correct)');
  mdParts.push(codeFenceOrPlaceholder(entry.good_example));

  const markdownBody = manager.renderMarkdown(mdParts.join('\n'));
  const footer = renderFooter(entry);
  const severityBadge = renderSeverityBadge(entry.severity);
  return {
    body: `${severityBadge}${markdownBody}${footer}`,
    scripts: [interactionScript()],
  };
}

function locationLine(range: vscode.Range): string {
  const line = range.start.line + 1;
  const col = range.start.character + 1;
  return `_Reported at line ${line}, column ${col}._`;
}

function codeFenceOrPlaceholder(src: string | undefined): string {
  if (src && src.trim().length > 0) {
    return '```bock\n' + src + '\n```';
  }
  return '<span class="bock-missing">Example not yet available.</span>';
}

function renderSeverityBadge(severity: string): string {
  const label = severity.charAt(0).toUpperCase() + severity.slice(1);
  return `<div><span class="bock-badge">${escapeHtml(label)}</span></div>`;
}

function renderFooter(entry: DiagnosticCode): string {
  const parts: string[] = ['<h2>Learn more</h2>', '<ul>'];
  if (entry.spec_refs.length > 0) {
    for (const ref of entry.spec_refs) {
      const encoded = escapeHtml(ref);
      parts.push(
        `<li><a href="#" class="bock-spec-link" data-spec-ref="${encoded}">${encoded} →</a></li>`,
      );
    }
  } else {
    parts.push(
      '<li><span class="bock-missing">No spec reference recorded.</span></li>',
    );
  }

  if (entry.related_codes.length > 0) {
    const links = entry.related_codes
      .map((c) => {
        const encoded = escapeHtml(c);
        return `<a href="#" class="bock-related-code" data-code="${encoded}">${encoded}</a>`;
      })
      .join(', ');
    parts.push(`<li>Related: ${links}</li>`);
  }
  parts.push('</ul>');

  const quickFixes = (entry as unknown as { quick_fixes?: unknown })
    .quick_fixes;
  if (Array.isArray(quickFixes) && quickFixes.length > 0) {
    parts.push('<h2>Quick fixes</h2>');
    parts.push('<ul>');
    for (const fix of quickFixes) {
      const label =
        typeof fix === 'string'
          ? fix
          : typeof (fix as { title?: unknown })?.title === 'string'
            ? (fix as { title: string }).title
            : JSON.stringify(fix);
      parts.push(`<li>Would apply: ${escapeHtml(label)}</li>`);
    }
    parts.push('</ul>');
  }

  return parts.join('\n');
}

function interactionScript(): string {
  return `
const vscode = acquireVsCodeApi();
document.querySelectorAll('.bock-spec-link').forEach((el) => {
  el.addEventListener('click', (e) => {
    e.preventDefault();
    vscode.postMessage({ type: 'openSpec', ref: el.dataset.specRef });
  });
});
document.querySelectorAll('.bock-related-code').forEach((el) => {
  el.addEventListener('click', (e) => {
    e.preventDefault();
    vscode.postMessage({ type: 'explainCode', code: el.dataset.code });
  });
});
`;
}
