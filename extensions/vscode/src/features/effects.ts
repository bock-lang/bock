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
import { escapeHtml } from '../shared/webview';
import {
  analyzeEffectFlow,
  EffectFlow,
  HandlerBinding,
  HandlerLayer,
  Location,
} from './effect-analyzer';
import type { Target } from '../shared/types';

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
   *  it's already visible or `bock.effects.autoRender` is enabled. */
  async autoRender(
    document: vscode.TextDocument,
    position: vscode.Position,
  ): Promise<void> {
    const cfg = vscode.workspace.getConfiguration('bock');
    const enabled = cfg.get<boolean>('effects.autoRender', false);
    if (!enabled && !this.panel) return;
    this.currentDoc = document;
    this.currentPos = position;
    this.ensurePanel();
    await this.render();
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
    void this.controller.autoRender(document, position);
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
  const nonce = randomNonce();
  const mermaidUri = webview.asWebviewUri(
    vscode.Uri.joinPath(extensionUri, 'assets', 'mermaid.min.js'),
  );
  const csp = [
    "default-src 'none'",
    `script-src 'nonce-${nonce}' ${webview.cspSource}`,
    `style-src 'unsafe-inline' ${webview.cspSource}`,
    `img-src ${webview.cspSource} data:`,
    `font-src ${webview.cspSource}`,
  ].join('; ');

  const body = flow
    ? renderFlowBody(flow, vocab)
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
  <script nonce="${nonce}" src="${mermaidUri}"></script>
  <script nonce="${nonce}">
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

function renderEmptyState(): string {
  return `
    <h1>Effect Flow</h1>
    <p class="bock-missing">
      Place the cursor inside a function definition in an <code>.bock</code>
      file, then run <code>Bock: Show Effect Flow for Function</code>.
    </p>
    <p class="bock-missing">
      Functions without a <code>with</code> clause are pure — nothing to visualise.
    </p>`;
}

function renderFlowBody(flow: EffectFlow, vocab: VocabService): string {
  const targets = vocab.get().tooling.targets;
  const header = `
    <h1>Effect Flow — <code>${escapeHtml(flow.functionName)}</code></h1>
    <p>
      Declared effects:
      ${
        flow.effects.length > 0
          ? flow.effects
              .map(
                (e) =>
                  `<span class="bock-badge bock-badge-effect bock-nav" data-nav-id="${escapeHtml(
                    nodeId('eff', e),
                  )}">${escapeHtml(e)}</span>`,
              )
              .join(' ')
          : '<span class="bock-missing">none — this function is pure.</span>'
      }
    </p>`;

  const diagram =
    flow.effects.length === 0
      ? '<p class="bock-missing">Pure function — no effect graph.</p>'
      : `<div class="bock-diagram" id="bock-mermaid">Rendering diagram…</div>`;

  const handlersSection = renderHandlers(flow.handlers);

  const targetButton = `
    <div class="bock-targets-row">
      <button id="bock-show-targets" type="button" class="bock-button">Show in targets</button>
    </div>
    <div id="bock-targets-panel" hidden>
      ${renderTargetsTable(flow, targets)}
    </div>`;

  return `${header}${diagram}${handlersSection}${targetButton}`;
}

function renderHandlers(handlers: HandlerBinding[]): string {
  if (handlers.length === 0) {
    return `
      <h2>Handler resolution</h2>
      <p class="bock-missing">
        No handler found in local / module / project layers. The call site
        must provide one via <code>handling (…)</code> before execution.
      </p>`;
  }
  const byLayer: Record<HandlerLayer, HandlerBinding[]> = {
    local: [],
    module: [],
    project: [],
  };
  for (const h of handlers) byLayer[h.layer].push(h);
  const layerSection = (label: string, layer: HandlerLayer): string => {
    const rows = byLayer[layer];
    if (rows.length === 0) return '';
    const items = rows
      .map((h) => {
        const id = nodeId('hnd', `${h.effect}_${h.handler}`);
        const loc = h.location
          ? `<span class="bock-loc">${escapeHtml(locationLabel(h.location))}</span>`
          : '';
        return `<li>
          <code>${escapeHtml(h.effect)}</code>
          <span class="bock-arrow">→</span>
          <a href="#" class="bock-nav" data-nav-id="${escapeHtml(id)}"><code>${escapeHtml(h.handler)}</code></a>
          ${loc}
        </li>`;
      })
      .join('\n');
    return `<h3>${escapeHtml(label)}</h3><ul class="bock-handlers">${items}</ul>`;
  };
  return `
    <h2>Handler resolution</h2>
    ${layerSection('Local (handling blocks)', 'local')}
    ${layerSection('Module (handle declarations)', 'module')}
    ${layerSection('Project (bock.project [effects])', 'project')}`;
}

function renderTargetsTable(flow: EffectFlow, targets: Target[]): string {
  if (flow.effects.length === 0) {
    return `<p class="bock-missing">Pure function — no target strategies needed.</p>`;
  }
  const rows = targets
    .map((t) => {
      const strategy = targetStrategy(t.id);
      return `<tr>
        <td><code>${escapeHtml(t.id)}</code></td>
        <td>${escapeHtml(t.display_name)}</td>
        <td>${escapeHtml(strategy.support)}</td>
        <td>${escapeHtml(strategy.strategy)}</td>
      </tr>`;
    })
    .join('\n');
  return `
    <h2>Target support</h2>
    <p>
      Bock's universal codegen strategy for effects is parameter passing;
      see <a href="#" class="bock-spec-link-inline" data-spec-ref="§13">§13 Transpilation</a>.
    </p>
    <table class="bock-targets">
      <thead>
        <tr><th>Target</th><th>Name</th><th>Support</th><th>Strategy</th></tr>
      </thead>
      <tbody>${rows}</tbody>
    </table>`;
}

function targetStrategy(id: string): { support: string; strategy: string } {
  switch (id) {
    case 'js':
    case 'ts':
    case 'python':
      return { support: 'Emulated', strategy: 'Parameter passing' };
    case 'rust':
      return { support: 'Emulated', strategy: 'Trait parameter' };
    case 'go':
      return { support: 'Emulated', strategy: 'Interface parameter' };
    default:
      return { support: 'Emulated', strategy: 'Parameter passing' };
  }
}

// ─── Mermaid construction ───────────────────────────────────────────────────

function buildMermaid(flow: EffectFlow): string {
  const lines: string[] = ['graph LR'];
  const fnNode = nodeId('fn', flow.functionName);
  const fnLabel = mermaidLabel(`${flow.functionName}(…)`);
  lines.push(`  ${fnNode}[${fnLabel}]:::fnNode`);

  // Effect nodes
  for (const eff of flow.effects) {
    const id = nodeId('eff', eff);
    lines.push(`  ${id}([${mermaidLabel(eff)}]):::effNode`);
  }

  // Operation nodes (only operations we found called in the body).
  const seenOps = new Set<string>();
  for (const call of flow.callees) {
    const key = call.operation;
    if (seenOps.has(key)) continue;
    seenOps.add(key);
    const opId = nodeId('op', key);
    lines.push(`  ${opId}[["${escapeMermaid(call.operation)}()"]]:::opNode`);
  }

  // Fn → Op edges labelled with the with clause.
  const withLabel = flow.effects.join(', ');
  for (const op of seenOps) {
    const opId = nodeId('op', op);
    const label = withLabel
      ? `with ${escapeMermaid(withLabel)}`
      : 'calls';
    lines.push(`  ${fnNode} -->|${label}| ${opId}`);
  }

  // Op → Effect dashed edges (membership).
  for (const op of seenOps) {
    const call = flow.callees.find((c) => c.operation === op);
    const effName = call?.effect;
    if (!effName) continue;
    if (!flow.effects.includes(effName)) continue;
    const opId = nodeId('op', op);
    const effId = nodeId('eff', effName);
    lines.push(`  ${opId} -.->|of| ${effId}`);
  }

  // Handler nodes + Effect → Handler edges, grouped by layer.
  const handlerSeen = new Set<string>();
  for (const h of flow.handlers) {
    const handlerKey = `${h.effect}_${h.handler}`;
    const handlerId = nodeId('hnd', handlerKey);
    const effId = nodeId('eff', h.effect);
    if (!handlerSeen.has(handlerId)) {
      handlerSeen.add(handlerId);
      const layerLabel = layerTag(h.layer);
      const label = mermaidLabel(`${h.handler} [${layerLabel}]`);
      lines.push(`  ${handlerId}[${label}]:::hndNode_${h.layer}`);
    }
    lines.push(`  ${effId} -.->|handled by| ${handlerId}`);
  }

  // Styling classes.
  lines.push(
    `  classDef fnNode fill:#1f6feb,stroke:#58a6ff,color:#ffffff,stroke-width:2px;`,
    `  classDef effNode fill:#8957e5,stroke:#c4b1ff,color:#ffffff;`,
    `  classDef opNode fill:#2d333b,stroke:#768390,color:#adbac7;`,
    `  classDef hndNode_local fill:#2da44e,stroke:#56d364,color:#ffffff;`,
    `  classDef hndNode_module fill:#bf8700,stroke:#e3b341,color:#ffffff;`,
    `  classDef hndNode_project fill:#db6d28,stroke:#f0883e,color:#ffffff;`,
  );

  // Click bindings — route every interactive node through bockNavigate().
  const clickable = new Set<string>();
  clickable.add(fnNode);
  for (const eff of flow.effects) clickable.add(nodeId('eff', eff));
  for (const op of seenOps) clickable.add(nodeId('op', op));
  for (const id of handlerSeen) clickable.add(id);
  for (const id of clickable) {
    lines.push(`  click ${id} bockNavigate`);
  }

  return lines.join('\n');
}

function mermaidLabel(s: string): string {
  return `"${escapeMermaid(s)}"`;
}

function escapeMermaid(s: string): string {
  return s.replace(/"/g, '#quot;').replace(/\|/g, '\\|');
}

// ─── Node IDs + navigation map ──────────────────────────────────────────────

function nodeId(kind: 'fn' | 'eff' | 'op' | 'hnd', name: string): string {
  return `${kind}_${name.replace(/[^A-Za-z0-9_]/g, '_')}`;
}

interface NavTarget {
  uri: string;
  line: number;
  column: number;
}

function buildNavigationMap(flow: EffectFlow): Record<string, NavTarget> {
  const map: Record<string, NavTarget> = {};
  const fnId = nodeId('fn', flow.functionName);
  map[fnId] = {
    uri: flow.documentUri.toString(),
    line: flow.functionRange.start.line,
    column: flow.functionRange.start.character,
  };

  for (const eff of flow.effects) {
    const id = nodeId('eff', eff);
    const def = flow.effectDefs.find((d) => d.name === eff);
    if (def?.defined) {
      map[id] = locationToNav(def.defined);
    }
  }

  for (const call of flow.callees) {
    const id = nodeId('op', call.operation);
    if (!map[id]) map[id] = locationToNav(call.location);
  }

  for (const h of flow.handlers) {
    const id = nodeId('hnd', `${h.effect}_${h.handler}`);
    if (h.location && !map[id]) map[id] = locationToNav(h.location);
  }
  return map;
}

function locationToNav(loc: Location): NavTarget {
  return { uri: loc.uri.toString(), line: loc.line, column: loc.column };
}

// ─── Presentation helpers ───────────────────────────────────────────────────

function layerTag(layer: HandlerLayer): string {
  switch (layer) {
    case 'local':
      return 'local';
    case 'module':
      return 'module';
    case 'project':
      return 'project';
  }
}

function locationLabel(loc: Location): string {
  const base = loc.uri.fsPath.split(/[\\/]/).pop() ?? loc.uri.fsPath;
  return `${base}:${loc.line + 1}`;
}

function randomNonce(): string {
  const chars =
    'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
  let out = '';
  for (let i = 0; i < 32; i++) {
    out += chars.charAt(Math.floor(Math.random() * chars.length));
  }
  return out;
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
