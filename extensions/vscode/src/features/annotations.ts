// Annotation insight tree view (F1.5.5).
//
// Scans every `.bock` file in the workspace for top-level annotations,
// groups them by annotation name (e.g. `@managed`, `@context`,
// `@performance`), and exposes them as a tree in the Explorer. Each
// leaf carries the source location and a preview of the annotation's
// parameters; clicking jumps to the file. Hover on either level pulls
// the annotation's purpose and spec link from the compiler-emitted
// vocabulary so the UI stays in sync with the language.
//
// A bundled webview (`bock.annotations.showUsage`) renders a usage
// analysis for a single annotation kind — its purpose, the systems it
// influences, and every occurrence in the workspace.

import * as vscode from 'vscode';
import * as path from 'path';
import type { LanguageClient } from 'vscode-languageclient/node';
import { VocabService } from '../vocab';
import type { Annotation } from '../shared/types';
import { WebviewManager, escapeHtml } from '../shared/webview';

// ─── Types ──────────────────────────────────────────────────────────────────

interface AnnotationUsage {
  /** Name without the leading `@`. */
  name: string;
  /** Raw parameter text, empty if the annotation has no arguments. */
  params: string;
  /** Workspace file URI where this usage lives. */
  uri: vscode.Uri;
  /** Zero-based line number of the `@name` token. */
  line: number;
  /** Zero-based column of the `@` character. */
  column: number;
}

type AnnoNode = GroupNode | UsageNode;

interface GroupNode {
  kind: 'group';
  name: string;
  usages: AnnotationUsage[];
}

interface UsageNode {
  kind: 'usage';
  usage: AnnotationUsage;
}

// ─── TreeDataProvider ───────────────────────────────────────────────────────

class AnnotationsTreeProvider implements vscode.TreeDataProvider<AnnoNode> {
  private readonly emitter = new vscode.EventEmitter<AnnoNode | undefined>();
  readonly onDidChangeTreeData = this.emitter.event;

  private groups: GroupNode[] = [];

  constructor(private readonly vocab: VocabService) {}

  setUsages(usages: AnnotationUsage[]): void {
    const byName = new Map<string, AnnotationUsage[]>();
    for (const u of usages) {
      const arr = byName.get(u.name);
      if (arr) arr.push(u);
      else byName.set(u.name, [u]);
    }
    const groups: GroupNode[] = Array.from(byName.entries()).map(
      ([name, group]) => ({
        kind: 'group',
        name,
        usages: group.sort(sortByLocation),
      }),
    );
    groups.sort((a, b) => a.name.localeCompare(b.name));
    this.groups = groups;
    this.emitter.fire(undefined);
  }

  getChildren(element?: AnnoNode): AnnoNode[] {
    if (!element) return this.groups;
    if (element.kind === 'group') {
      return element.usages.map((usage) => ({ kind: 'usage', usage }));
    }
    return [];
  }

  getTreeItem(element: AnnoNode): vscode.TreeItem {
    if (element.kind === 'group') {
      const label = `@${element.name} (${element.usages.length})`;
      const item = new vscode.TreeItem(
        label,
        vscode.TreeItemCollapsibleState.Collapsed,
      );
      item.iconPath = new vscode.ThemeIcon('tag');
      item.contextValue = 'annotationGroup';
      item.tooltip = buildGroupTooltip(element.name, this.vocab.getAnnotation(element.name));
      return item;
    }

    const { usage } = element;
    const ann = this.vocab.getAnnotation(usage.name);
    const folder = vscode.workspace.getWorkspaceFolder(usage.uri);
    const relative = folder
      ? path.relative(folder.uri.fsPath, usage.uri.fsPath)
      : usage.uri.fsPath;
    const label = usage.params
      ? `${truncate(usage.params, 60)} — ${relative}:${usage.line + 1}`
      : `${relative}:${usage.line + 1}`;

    const item = new vscode.TreeItem(label, vscode.TreeItemCollapsibleState.None);
    item.iconPath = new vscode.ThemeIcon('symbol-field');
    item.tooltip = buildUsageTooltip(usage, ann);
    item.contextValue = 'annotationUsage';
    item.command = {
      command: 'bock.annotations.revealUsage',
      title: 'Open annotation',
      arguments: [usage],
    };
    return item;
  }
}

function sortByLocation(a: AnnotationUsage, b: AnnotationUsage): number {
  const pathCmp = a.uri.fsPath.localeCompare(b.uri.fsPath);
  if (pathCmp !== 0) return pathCmp;
  if (a.line !== b.line) return a.line - b.line;
  return a.column - b.column;
}

function truncate(s: string, n: number): string {
  const trimmed = s.trim();
  return trimmed.length > n ? `${trimmed.slice(0, n - 1)}…` : trimmed;
}

// ─── Tooltips ───────────────────────────────────────────────────────────────

function buildGroupTooltip(
  name: string,
  ann: Annotation | undefined,
): vscode.MarkdownString {
  const md = new vscode.MarkdownString(undefined, true);
  md.isTrusted = true;
  md.appendMarkdown(`**@${name}** — annotation\n\n`);
  if (ann) {
    md.appendMarkdown(`${ann.purpose}\n\n`);
    if (ann.params) md.appendMarkdown(`_Parameters:_ \`${ann.params}\`\n\n`);
    md.appendMarkdown(`_Influences:_ ${affectedSystems(name)}\n\n`);
    if (ann.spec_ref) {
      const encoded = encodeURIComponent(JSON.stringify([ann.spec_ref]));
      md.appendMarkdown(`[${ann.spec_ref} →](command:bock.openSpecAt?${encoded})`);
    }
  } else {
    md.appendMarkdown('_Not documented in vocabulary._');
  }
  return md;
}

function buildUsageTooltip(
  usage: AnnotationUsage,
  ann: Annotation | undefined,
): vscode.MarkdownString {
  const md = new vscode.MarkdownString(undefined, true);
  md.isTrusted = true;
  const header = usage.params
    ? `**@${usage.name}(${truncate(usage.params, 80)})**`
    : `**@${usage.name}**`;
  md.appendMarkdown(`${header}\n\n`);
  if (ann) {
    md.appendMarkdown(`${ann.purpose}\n\n`);
    md.appendMarkdown(`_Influences:_ ${affectedSystems(usage.name)}\n\n`);
  }
  md.appendMarkdown(`_Location:_ ${usage.uri.fsPath}:${usage.line + 1}\n`);
  return md;
}

/** Human-readable list of systems an annotation feeds into. */
function affectedSystems(name: string): string {
  switch (name) {
    case 'context':
      return 'AI codegen, decision context, documentation';
    case 'requires':
      return 'capability checker, platform permission manifests';
    case 'performance':
      return 'AI optimization, runtime monitoring';
    case 'invariant':
      return 'static verification, runtime assertions';
    case 'security':
      return 'audit trails, PII propagation, safe-logging checks';
    case 'domain':
      return 'AI context window, codebase navigation';
    case 'derive':
      return 'trait implementation codegen';
    case 'managed':
      return 'ownership/move checking (suppressed)';
    case 'test':
      return '`bock test` discovery';
    default:
      return 'language tooling';
  }
}

// ─── Workspace scanning ─────────────────────────────────────────────────────

// Matches a top-level annotation token at the start of a (possibly
// indented) line. We intentionally stop at the first `(` on that line
// and capture the text up to the first unnested `)` when the caller
// asks for full parameters — multi-line parameter lists (e.g. `@context`
// with a triple-quoted string) may have no closing paren on the same
// line, in which case the params field is left empty.
const ANNOTATION_RE = /^[\t ]*@([A-Za-z_][A-Za-z0-9_]*)\b/;

async function scanWorkspace(): Promise<AnnotationUsage[]> {
  const files = await vscode.workspace.findFiles(
    '**/*.bock',
    '**/{node_modules,target,dist,out,.git}/**',
  );
  const all: AnnotationUsage[] = [];
  for (const uri of files) {
    try {
      const bytes = await vscode.workspace.fs.readFile(uri);
      const text = Buffer.from(bytes).toString('utf8');
      all.push(...scanText(uri, text));
    } catch {
      // Skip unreadable files; they'll reappear next refresh if they
      // become readable.
    }
  }
  return all;
}

/** Parse annotation usages out of a single file's text. Exported for tests. */
export function scanText(uri: vscode.Uri, text: string): AnnotationUsage[] {
  const out: AnnotationUsage[] = [];
  const lines = text.split(/\r?\n/);
  let inTripleString = false;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const tripleCount = countTripleQuotes(line);
    const startedInString = inTripleString;
    if (tripleCount % 2 !== 0) {
      inTripleString = !inTripleString;
    }
    // Skip the line if it opened fully inside a triple-quoted string.
    // Annotations buried inside a `@context("""...""")` body (e.g.
    // `@intent:`) are documentation markers, not top-level annotations.
    if (startedInString) continue;

    const match = ANNOTATION_RE.exec(line);
    if (!match) continue;
    const name = match[1];
    const column = line.indexOf('@');
    const params = extractParams(line, column);
    out.push({ name, params, uri, line: i, column });
  }
  return out;
}

function countTripleQuotes(line: string): number {
  let count = 0;
  let idx = 0;
  while ((idx = line.indexOf('"""', idx)) !== -1) {
    count++;
    idx += 3;
  }
  return count;
}

function extractParams(line: string, atColumn: number): string {
  const open = line.indexOf('(', atColumn);
  if (open === -1) return '';
  let depth = 1;
  for (let i = open + 1; i < line.length; i++) {
    const c = line[i];
    if (c === '(') depth++;
    else if (c === ')') {
      depth--;
      if (depth === 0) return line.slice(open + 1, i);
    }
  }
  // Unclosed on this line (e.g. `@context("""` spanning multiple lines).
  return line.slice(open + 1).trimEnd();
}

// ─── Registration ───────────────────────────────────────────────────────────

export function registerAnnotations(
  ctx: vscode.ExtensionContext,
  vocab: VocabService,
  _client: LanguageClient | undefined,
): void {
  const provider = new AnnotationsTreeProvider(vocab);
  const webviews = new WebviewManager(ctx);

  const view = vscode.window.createTreeView('bock.annotations', {
    treeDataProvider: provider,
    showCollapseAll: true,
  });
  ctx.subscriptions.push(view);

  let pending: NodeJS.Timeout | undefined;
  const refresh = async (): Promise<void> => {
    const usages = await scanWorkspace();
    provider.setUsages(usages);
  };
  const scheduleRefresh = (): void => {
    if (pending) clearTimeout(pending);
    pending = setTimeout(() => {
      pending = undefined;
      void refresh();
    }, 200);
  };

  // Initial scan — run asynchronously so activation isn't blocked on IO.
  void refresh();

  const watcher = vscode.workspace.createFileSystemWatcher('**/*.bock');
  watcher.onDidChange(scheduleRefresh);
  watcher.onDidCreate(scheduleRefresh);
  watcher.onDidDelete(scheduleRefresh);
  ctx.subscriptions.push(watcher);

  ctx.subscriptions.push(
    vscode.commands.registerCommand('bock.annotations.refresh', () => {
      void refresh();
    }),
    vscode.commands.registerCommand(
      'bock.annotations.revealUsage',
      async (usage: AnnotationUsage) => {
        const pos = new vscode.Position(usage.line, usage.column);
        const selection = new vscode.Range(pos, pos);
        await vscode.window.showTextDocument(usage.uri, {
          selection,
          preserveFocus: false,
        });
      },
    ),
    vscode.commands.registerCommand(
      'bock.annotations.showUsage',
      (node: AnnoNode | undefined) => {
        const name = node?.kind === 'group'
          ? node.name
          : node?.kind === 'usage'
            ? node.usage.name
            : undefined;
        if (!name) {
          void vscode.window.showInformationMessage(
            'Bock: select an annotation in the tree to show its usage.',
          );
          return;
        }
        const group = provider
          .getChildren()
          .find((n) => n.kind === 'group' && n.name === name) as
          | GroupNode
          | undefined;
        if (!group) return;
        openUsageWebview(webviews, vocab, group);
      },
    ),
  );
}

// ─── Usage analysis webview ─────────────────────────────────────────────────

function openUsageWebview(
  webviews: WebviewManager,
  vocab: VocabService,
  group: GroupNode,
): void {
  const ann = vocab.getAnnotation(group.name);
  const title = `Bock — @${group.name} usage`;
  const { body, script } = renderUsageHtml(group, ann);
  const handle = webviews.create(
    `bock.annotations.usage.${group.name}`,
    title,
    { body, scripts: [script] },
  );
  handle.panel.webview.onDidReceiveMessage((msg) => {
    if (msg?.type === 'openSpec' && typeof msg.ref === 'string') {
      void vscode.commands.executeCommand('bock.openSpecAt', msg.ref);
    } else if (msg?.type === 'reveal' && typeof msg.index === 'number') {
      const usage = group.usages[msg.index];
      if (usage) {
        void vscode.commands.executeCommand(
          'bock.annotations.revealUsage',
          usage,
        );
      }
    }
  });
}

function renderUsageHtml(
  group: GroupNode,
  ann: Annotation | undefined,
): { body: string; script: string } {
  const header = `<h1>@${escapeHtml(group.name)}</h1>`;
  const purpose = ann
    ? `<p>${escapeHtml(ann.purpose)}</p>`
    : `<p class="bock-missing">Not documented in vocabulary.</p>`;

  const paramsRow = ann?.params
    ? `<tr><th>Parameters</th><td><code>${escapeHtml(ann.params)}</code></td></tr>`
    : '';
  const specRow = ann?.spec_ref
    ? `<tr><th>Spec</th><td><a href="#" class="bock-spec-link" data-spec-ref="${escapeHtml(ann.spec_ref)}">${escapeHtml(ann.spec_ref)} →</a></td></tr>`
    : '';
  const influencesRow = `<tr><th>Influences</th><td>${escapeHtml(affectedSystems(group.name))}</td></tr>`;
  const countRow = `<tr><th>Occurrences</th><td>${group.usages.length}</td></tr>`;
  const meta = `<table>${paramsRow}${specRow}${influencesRow}${countRow}</table>`;

  const items = group.usages
    .map((u, i) => {
      const location = `${escapeHtml(u.uri.fsPath)}:${u.line + 1}`;
      const preview = u.params
        ? ` — <code>${escapeHtml(truncate(u.params, 80))}</code>`
        : '';
      return `<li><a href="#" class="bock-usage" data-index="${i}">${location}</a>${preview}</li>`;
    })
    .join('\n');

  const usageList = `<h2>Occurrences</h2><ul>${items || '<li class="bock-missing">None.</li>'}</ul>`;

  const script = `
const vscode = acquireVsCodeApi();
document.querySelectorAll('.bock-spec-link').forEach((el) => {
  el.addEventListener('click', (e) => {
    e.preventDefault();
    vscode.postMessage({ type: 'openSpec', ref: el.dataset.specRef });
  });
});
document.querySelectorAll('.bock-usage').forEach((el) => {
  el.addEventListener('click', (e) => {
    e.preventDefault();
    vscode.postMessage({ type: 'reveal', index: Number(el.dataset.index) });
  });
});`;
  return {
    body: `${header}${purpose}${meta}${usageList}`,
    script,
  };
}
