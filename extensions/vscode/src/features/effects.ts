// Effect flow visualization (F1.5.6).
//
// Opens a webview that renders a Mermaid diagram of how effects flow through
// the function at the cursor: which operations the body calls, which effects
// those operations belong to, and which handlers will resolve them across
// the three layers (local `handling`, module `handle`, project defaults).
//
// Data collection is best-effort regex parsing (see ./effect-analyzer.ts).
// If/when the LSP grows an `bock/effectFlow` custom request, this file is
// the call site that should switch over — the analyzer stays as a fallback.
//
// The diagram and target-support table are rendered by mermaid.min.js,
// bundled into `assets/` so the webview stays functional offline. No CDN.
//
// Interactivity:
//   - Click an effect/handler/operation node → jump to its definition.
//   - "Show in targets" button → reveals the per-target strategy table.
//   - Live refresh: edits to the active document re-run analysis (debounced
//     at 300 ms) and update the panel in place.

import * as vscode from 'vscode';
import type { LanguageClient } from 'vscode-languageclient/node';
import { VocabService } from '../vocab';
import { nonce } from '../shared/webview';
import { analyzeEffectFlow, EffectFlow } from './effect-analyzer';
import {
  buildMermaid,
  buildNavigationMap,
  renderEmptyState,
  renderFlowBody,
} from './effects-flow';

// ─── Public entry point ─────────────────────────────────────────────────────

export function registerEffects(
  ctx: vscode.ExtensionContext,
  vocab: VocabService,
  _client: LanguageClient | undefined,
): void {
  const controller = new EffectFlowController(ctx, vocab);
  ctx.subscriptions.push(
    vscode.commands.registerCommand('bock.showEffectFlow', async () => {
      await controller.show();
    }),
    vscode.workspace.onDidChangeTextDocument((evt) => {
      controller.onDocumentChanged(evt.document);
    }),
    vscode.window.onDidChangeTextEditorSelection((evt) => {
      controller.onSelectionChanged(evt.textEditor);
    }),
  );

  // Automatic render on hover, if the user opted in.
  const autoRenderHover = new AutoRenderProvider(controller);
  ctx.subscriptions.push(
    vscode.languages.registerHoverProvider(
      { scheme: 'file', language: 'bock' },
      autoRenderHover,
    ),
  );
}

// ─── Controller ─────────────────────────────────────────────────────────────

class EffectFlowController {
  private panel?: vscode.WebviewPanel;
  private currentDoc?: vscode.TextDocument;
  private currentPos?: vscode.Position;
  private refreshTimer?: NodeJS.Timeout;

  constructor(
    private readonly ctx: vscode.ExtensionContext,
    private readonly vocab: VocabService,
  ) {}

  async show(): Promise<void> {
    const editor = vscode.window.activeTextEditor;
    if (!editor || editor.document.languageId !== 'bock') {
      void vscode.window.showInformationMessage(
        'Bock: open an .bock file and place the cursor inside a function to visualise its effects.',
      );
      return;
    }
    this.currentDoc = editor.document;
    this.currentPos = editor.selection.active;
    this.ensurePanel();
    await this.render();
  }

  onDocumentChanged(doc: vscode.TextDocument): void {
    if (!this.panel || !this.currentDoc) return;
    if (doc.uri.toString() !== this.currentDoc.uri.toString()) return;
    this.currentDoc = doc;
    this.scheduleRefresh();
  }

  onSelectionChanged(editor: vscode.TextEditor): void {
    if (!this.panel) return;
    if (editor.document.languageId !== 'bock') return;
    this.currentDoc = editor.document;
    this.currentPos = editor.selection.active;
    this.scheduleRefresh();
  }

  /** Auto-render path used by the hover provider. Only opens the panel if
   *  it's already visible or `bock.effects.autoRender` is enabled.
   *
   *  Hover fires on every cursor movement, so this must NOT call `render()`
   *  directly: `render()` runs `analyzeEffectFlow`, which scans every
   *  `.bock` file in the workspace plus `bock.project`. Route through the
   *  shared ~300 ms `scheduleRefresh()` debounce instead, so a burst of
   *  hovers collapses into a single workspace re-analysis rather than one
   *  synchronous scan per hover. */
  autoRender(document: vscode.TextDocument, position: vscode.Position): void {
    const cfg = vscode.workspace.getConfiguration('bock');
    const enabled = cfg.get<boolean>('effects.autoRender', false);
    if (!enabled && !this.panel) return;
    this.currentDoc = document;
    this.currentPos = position;
    this.ensurePanel();
    this.scheduleRefresh();
  }

  private ensurePanel(): void {
    if (this.panel) {
      this.panel.reveal(vscode.ViewColumn.Beside, true);
      return;
    }
    const panel = vscode.window.createWebviewPanel(
      'bock.effectFlow',
      'Bock Effect Flow',
      { viewColumn: vscode.ViewColumn.Beside, preserveFocus: true },
      {
        enableScripts: true,
        retainContextWhenHidden: true,
        localResourceRoots: [
          vscode.Uri.joinPath(this.ctx.extensionUri, 'assets'),
          vscode.Uri.joinPath(this.ctx.extensionUri, 'out'),
        ],
      },
    );
    panel.onDidDispose(() => {
      this.panel = undefined;
      if (this.refreshTimer) {
        clearTimeout(this.refreshTimer);
        this.refreshTimer = undefined;
      }
    });
    panel.webview.onDidReceiveMessage((msg) => this.handleMessage(msg));
    this.panel = panel;
  }

  private scheduleRefresh(): void {
    if (this.refreshTimer) clearTimeout(this.refreshTimer);
    this.refreshTimer = setTimeout(() => {
      this.refreshTimer = undefined;
      void this.render();
    }, 300);
  }

  private async render(): Promise<void> {
    if (!this.panel || !this.currentDoc || !this.currentPos) return;
    const flow = await analyzeEffectFlow(this.currentDoc, this.currentPos);
    const html = renderHtml(
      this.panel.webview,
      this.ctx.extensionUri,
      flow,
      this.vocab,
    );
    this.panel.webview.html = html;
  }

  private async handleMessage(msg: unknown): Promise<void> {
    if (!msg || typeof msg !== 'object') return;
    const m = msg as Record<string, unknown>;
    if (m.type === 'navigate' && typeof m.uri === 'string') {
      const line = typeof m.line === 'number' ? m.line : 0;
      const column = typeof m.column === 'number' ? m.column : 0;
      const target = vscode.Uri.parse(m.uri);
      const pos = new vscode.Position(line, column);
      await vscode.window.showTextDocument(target, {
        selection: new vscode.Range(pos, pos),
        preserveFocus: false,
      });
    } else if (m.type === 'openSpec' && typeof m.ref === 'string') {
      await vscode.commands.executeCommand('bock.openSpecAt', m.ref);
    }
  }
}

// ─── Hover-triggered auto render ────────────────────────────────────────────

class AutoRenderProvider implements vscode.HoverProvider {
  constructor(private readonly controller: EffectFlowController) {}

  provideHover(
    document: vscode.TextDocument,
    position: vscode.Position,
  ): vscode.ProviderResult<vscode.Hover> {
    this.controller.autoRender(document, position);
    // Never contribute a hover body — we only piggy-back on the event.
    return undefined;
  }
}

// ─── HTML rendering ─────────────────────────────────────────────────────────

function renderHtml(
  webview: vscode.Webview,
  extensionUri: vscode.Uri,
  flow: EffectFlow | undefined,
  vocab: VocabService,
): string {
  const pageNonce = nonce();
  const mermaidUri = webview.asWebviewUri(
    vscode.Uri.joinPath(extensionUri, 'assets', 'mermaid.min.js'),
  );
  const csp = [
    "default-src 'none'",
    `script-src 'nonce-${pageNonce}' ${webview.cspSource}`,
    `style-src 'unsafe-inline' ${webview.cspSource}`,
    `img-src ${webview.cspSource} data:`,
    `font-src ${webview.cspSource}`,
  ].join('; ');

  const body = flow
    ? renderFlowBody(flow, vocab.get().tooling.targets)
    : renderEmptyState();

  const mermaidSource = flow ? buildMermaid(flow) : '';
  const navigationMap = flow ? buildNavigationMap(flow) : {};

  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta http-equiv="Content-Security-Policy" content="${csp}" />
  <title>Bock Effect Flow</title>
  <style>${styles()}</style>
</head>
<body>
  ${body}
  <script nonce="${pageNonce}" src="${mermaidUri}"></script>
  <script nonce="${pageNonce}">
    (function () {
      const vscode = acquireVsCodeApi();
      const navMap = ${JSON.stringify(navigationMap)};
      window.bockNavigate = function (nodeId) {
        const target = navMap[nodeId];
        if (!target) return;
        vscode.postMessage({ type: 'navigate', ...target });
      };
      if (typeof mermaid !== 'undefined' && ${JSON.stringify(Boolean(mermaidSource))}) {
        mermaid.initialize({
          startOnLoad: false,
          theme: 'dark',
          securityLevel: 'loose',
          flowchart: { curve: 'basis' },
        });
        const el = document.getElementById('bock-mermaid');
        if (el) {
          const src = ${JSON.stringify(mermaidSource)};
          mermaid.render('bock-mermaid-svg', src).then((out) => {
            el.innerHTML = out.svg;
            if (out.bindFunctions) out.bindFunctions(el);
          }).catch((err) => {
            el.innerHTML = '<pre class="bock-error">Mermaid render failed: ' + String(err && err.message || err) + '</pre>';
          });
        }
      }
      const targetsBtn = document.getElementById('bock-show-targets');
      const targetsPanel = document.getElementById('bock-targets-panel');
      if (targetsBtn && targetsPanel) {
        targetsBtn.addEventListener('click', () => {
          targetsPanel.hidden = !targetsPanel.hidden;
          targetsBtn.textContent = targetsPanel.hidden
            ? 'Show in targets'
            : 'Hide targets';
        });
      }
      document.querySelectorAll('.bock-nav').forEach((el) => {
        el.addEventListener('click', (e) => {
          e.preventDefault();
          const id = el.getAttribute('data-nav-id');
          if (id) window.bockNavigate(id);
        });
      });
    })();
  </script>
</body>
</html>`;
}

function styles(): string {
  return `
    body {
      font-family: var(--vscode-font-family);
      color: var(--vscode-foreground);
      background: var(--vscode-editor-background);
      padding: 1rem 1.25rem;
      line-height: 1.5;
    }
    h1, h2, h3 { color: var(--vscode-editor-foreground); margin-top: 1.25em; }
    h1 { font-size: 1.35em; border-bottom: 1px solid var(--vscode-panel-border); padding-bottom: 0.3em; }
    code {
      font-family: var(--vscode-editor-font-family);
      background: var(--vscode-textCodeBlock-background);
      padding: 0.1em 0.3em;
      border-radius: 3px;
    }
    a, .bock-nav, .bock-spec-link-inline {
      color: var(--vscode-textLink-foreground);
      text-decoration: none;
      cursor: pointer;
    }
    a:hover, .bock-nav:hover, .bock-spec-link-inline:hover {
      color: var(--vscode-textLink-activeForeground);
      text-decoration: underline;
    }
    .bock-badge {
      display: inline-block;
      padding: 0.15em 0.55em;
      border-radius: 3px;
      font-size: 0.9em;
      margin-right: 0.35em;
      background: var(--vscode-badge-background);
      color: var(--vscode-badge-foreground);
    }
    .bock-badge-effect {
      background: #8957e5;
      color: #ffffff;
    }
    .bock-missing {
      color: var(--vscode-descriptionForeground);
      font-style: italic;
    }
    .bock-diagram {
      background: var(--vscode-textCodeBlock-background);
      border-radius: 4px;
      padding: 1rem;
      margin: 0.75rem 0;
      overflow-x: auto;
    }
    .bock-diagram svg { max-width: 100%; }
    .bock-error {
      color: var(--vscode-errorForeground);
      white-space: pre-wrap;
    }
    .bock-handlers { padding-left: 1.25em; }
    .bock-handlers li { margin-bottom: 0.2em; }
    .bock-loc {
      color: var(--vscode-descriptionForeground);
      font-size: 0.85em;
      margin-left: 0.5em;
    }
    .bock-arrow {
      color: var(--vscode-descriptionForeground);
      margin: 0 0.25em;
    }
    .bock-button {
      background: var(--vscode-button-background);
      color: var(--vscode-button-foreground);
      border: 1px solid var(--vscode-button-border, transparent);
      padding: 0.35em 0.9em;
      border-radius: 3px;
      cursor: pointer;
      font-size: 0.95em;
    }
    .bock-button:hover { background: var(--vscode-button-hoverBackground); }
    .bock-targets-row { margin-top: 1.25rem; }
    .bock-targets {
      border-collapse: collapse;
      margin-top: 0.5rem;
      width: 100%;
    }
    .bock-targets th, .bock-targets td {
      border: 1px solid var(--vscode-panel-border);
      padding: 0.3em 0.6em;
      text-align: left;
    }
    .bock-targets th {
      background: var(--vscode-editor-background);
      font-weight: 600;
    }
  `;
}
