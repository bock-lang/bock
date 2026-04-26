// Minimal webview host used by feature panels (spec, effects, decisions).
// Subclasses override `render()` to produce HTML; this class handles
// creation, disposal, and a nonce-protected CSP boilerplate.
//
// `WebviewManager` is a lower-level alternative used by features that
// want to drive the panel imperatively (e.g. error explanations, where
// the rendered content depends on the specific diagnostic code).

import * as vscode from 'vscode';
import { renderMarkdown } from './markdown';

export abstract class WebviewPanelBase {
  protected panel?: vscode.WebviewPanel;

  constructor(
    protected readonly viewType: string,
    protected readonly title: string,
    protected readonly ctx: vscode.ExtensionContext,
  ) {}

  show(column: vscode.ViewColumn = vscode.ViewColumn.Beside): void {
    if (this.panel) {
      this.panel.reveal(column);
      return;
    }
    this.panel = vscode.window.createWebviewPanel(
      this.viewType,
      this.title,
      column,
      {
        enableScripts: true,
        retainContextWhenHidden: true,
        localResourceRoots: [
          vscode.Uri.joinPath(this.ctx.extensionUri, 'assets'),
          vscode.Uri.joinPath(this.ctx.extensionUri, 'out'),
        ],
      },
    );
    this.panel.onDidDispose(() => (this.panel = undefined));
    this.panel.webview.html = this.wrap(this.render());
  }

  protected abstract render(): string;

  protected refresh(): void {
    if (this.panel) this.panel.webview.html = this.wrap(this.render());
  }

  private wrap(body: string): string {
    const nonce = randomNonce();
    const csp = [
      "default-src 'none'",
      `style-src 'unsafe-inline'`,
      `script-src 'nonce-${nonce}'`,
    ].join('; ');
    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta http-equiv="Content-Security-Policy" content="${csp}" />
  <title>${escapeHtml(this.title)}</title>
  <style>
    body { font-family: var(--vscode-font-family); padding: 1rem; }
    h1, h2, h3 { color: var(--vscode-editor-foreground); }
    code { font-family: var(--vscode-editor-font-family); background: var(--vscode-textBlockQuote-background); padding: 0.1em 0.3em; border-radius: 3px; }
    a { color: var(--vscode-textLink-foreground); }
  </style>
</head>
<body>${body}</body>
</html>`;
  }
}

function randomNonce(): string {
  const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
  let out = '';
  for (let i = 0; i < 32; i++) {
    out += chars.charAt(Math.floor(Math.random() * chars.length));
  }
  return out;
}

export function escapeHtml(s: string): string {
  return s
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

// ─── WebviewManager ─────────────────────────────────────────────────────────
//
// Manager-style helper for features that build HTML dynamically instead of
// subclassing `WebviewPanelBase`. Keeps a single panel per `viewType` so
// repeated invocations update the existing panel rather than spawning new
// ones (matches the pattern of the spec panel and decision manifest).

export interface WebviewContent {
  body: string;
  /** Inline script bodies (no wrapping `<script>`). Injected with the page nonce. */
  scripts?: string[];
}

export interface WebviewHandle {
  readonly panel: vscode.WebviewPanel;
  update(content: WebviewContent | string): void;
}

export class WebviewManager {
  private readonly panels = new Map<string, WebviewHandle>();

  constructor(private readonly ctx: vscode.ExtensionContext) {}

  /**
   * Open (or reveal) a panel identified by `viewType`. The content is
   * wrapped with CSP boilerplate, theme-aware styles, and nonce-scoped
   * inline scripts.
   */
  create(
    viewType: string,
    title: string,
    content: WebviewContent | string,
    column: vscode.ViewColumn = vscode.ViewColumn.Beside,
  ): WebviewHandle {
    const existing = this.panels.get(viewType);
    if (existing) {
      existing.panel.title = title;
      existing.update(content);
      existing.panel.reveal(column);
      return existing;
    }

    const panel = vscode.window.createWebviewPanel(viewType, title, column, {
      enableScripts: true,
      retainContextWhenHidden: true,
      localResourceRoots: [
        vscode.Uri.joinPath(this.ctx.extensionUri, 'assets'),
        vscode.Uri.joinPath(this.ctx.extensionUri, 'out'),
      ],
    });

    const handle: WebviewHandle = {
      panel,
      update: (next) => {
        panel.webview.html = this.wrap(title, normalize(next));
      },
    };
    handle.update(content);
    panel.onDidDispose(() => this.panels.delete(viewType));
    this.panels.set(viewType, handle);
    return handle;
  }

  /** Replace the body HTML (and scripts) of an existing panel. */
  update(handle: WebviewHandle, content: WebviewContent | string): void {
    handle.update(content);
  }

  /** Render markdown via the shared marked wrapper. */
  renderMarkdown(md: string): string {
    return renderMarkdown(md);
  }

  /** VS Code theme-aware stylesheet used by webview bodies. */
  embedStyles(): string {
    return `
      body {
        color: var(--vscode-foreground);
        background: var(--vscode-editor-background);
        font-family: var(--vscode-font-family);
        font-size: var(--vscode-font-size);
        line-height: 1.5;
        padding: 1rem 1.25rem;
      }
      h1, h2, h3 { color: var(--vscode-editor-foreground); margin-top: 1.25em; }
      h1 { font-size: 1.4em; border-bottom: 1px solid var(--vscode-panel-border); padding-bottom: 0.3em; }
      h2 { font-size: 1.15em; }
      p { margin: 0.5em 0; }
      code {
        font-family: var(--vscode-editor-font-family);
        background: var(--vscode-textCodeBlock-background);
        padding: 0.1em 0.3em;
        border-radius: 3px;
      }
      pre {
        background: var(--vscode-textCodeBlock-background);
        padding: 0.75em 1em;
        border-radius: 4px;
        overflow-x: auto;
      }
      pre code { background: transparent; padding: 0; }
      a, .bock-spec-link, .bock-related-code {
        color: var(--vscode-textLink-foreground);
        text-decoration: none;
        cursor: pointer;
      }
      a:hover, .bock-spec-link:hover, .bock-related-code:hover {
        color: var(--vscode-textLink-activeForeground);
        text-decoration: underline;
      }
      .bock-badge {
        display: inline-block;
        padding: 0.1em 0.5em;
        border-radius: 3px;
        font-size: 0.85em;
        margin-left: 0.5em;
        background: var(--vscode-badge-background);
        color: var(--vscode-badge-foreground);
      }
      .bock-missing {
        color: var(--vscode-descriptionForeground);
        font-style: italic;
      }
      ul { padding-left: 1.25em; }
    `;
  }

  private wrap(title: string, content: WebviewContent): string {
    const nonce = randomNonce();
    const csp = [
      "default-src 'none'",
      `style-src 'unsafe-inline'`,
      `script-src 'nonce-${nonce}'`,
    ].join('; ');
    const scripts = (content.scripts ?? [])
      .map((src) => `<script nonce="${nonce}">${src}</script>`)
      .join('\n');
    return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta http-equiv="Content-Security-Policy" content="${csp}" />
  <title>${escapeHtml(title)}</title>
  <style>${this.embedStyles()}</style>
</head>
<body>${content.body}${scripts}</body>
</html>`;
  }
}

function normalize(content: WebviewContent | string): WebviewContent {
  return typeof content === 'string' ? { body: content } : content;
}
