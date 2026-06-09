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
import { truncate } from '../shared/strings';
import { scanText, type AnnotationUsage } from './annotations-scan';

// Re-export the pure scanner so existing importers (and tests) can reach it
// through this module too. The scanning logic itself lives in
// `annotations-scan.ts` so it can be unit-tested without pulling in the
// `vscode-languageclient` / webview dependency chain.
export { scanText, type AnnotationUsage } from './annotations-scan';

// ─── Types ──────────────────────────────────────────────────────────────────

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
//
// The per-line `scanText` parser (and its triple-quote state machine) lives
// in `annotations-scan.ts` so it can be unit-tested in isolation. The IO
// orchestration below stays here because it touches the live `vscode`
// workspace API.

/** Read and scan a single `.bock` file. Returns `undefined` if the file is
 *  unreadable (deleted, permissions) so the caller can drop its entry. */
async function scanFile(uri: vscode.Uri): Promise<AnnotationUsage[] | undefined> {
  try {
    const bytes = await vscode.workspace.fs.readFile(uri);
    const text = Buffer.from(bytes).toString('utf8');
    return scanText(uri, text);
  } catch {
    // Unreadable (e.g. deleted between watcher event and read).
    return undefined;
  }
}

/** Full workspace scan, keyed by file-URI string. Used for the initial load
 *  and the explicit `bock.annotations.refresh` command only; incremental
 *  watcher events re-scan just the changed file. */
async function scanWorkspace(): Promise<Map<string, AnnotationUsage[]>> {
  const files = await vscode.workspace.findFiles(
    '**/*.bock',
    '**/{node_modules,target,dist,out,.git}/**',
  );
  const byFile = new Map<string, AnnotationUsage[]>();
  for (const uri of files) {
    const usages = await scanFile(uri);
    if (usages) byFile.set(uri.toString(), usages);
  }
  return byFile;
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

  // Per-file usage store keyed by `uri.toString()`. The tree is rebuilt by
  // flattening every file's usages, so a single-file save only has to
  // re-scan and replace that one file's entry instead of re-reading the
  // whole workspace.
  const byFile = new Map<string, AnnotationUsage[]>();

  const rebuildTree = (): void => {
    const all: AnnotationUsage[] = [];
    for (const usages of byFile.values()) all.push(...usages);
    provider.setUsages(all);
  };

  // Full rescan: initial load and the explicit refresh command only.
  const fullRefresh = async (): Promise<void> => {
    const scanned = await scanWorkspace();
    byFile.clear();
    for (const [key, usages] of scanned) byFile.set(key, usages);
    rebuildTree();
  };

  // Incremental: re-scan exactly one changed/created file and swap its entry.
  const refreshFile = async (uri: vscode.Uri): Promise<void> => {
    const usages = await scanFile(uri);
    if (usages === undefined) byFile.delete(uri.toString());
    else byFile.set(uri.toString(), usages);
    rebuildTree();
  };

  const removeFile = (uri: vscode.Uri): void => {
    if (byFile.delete(uri.toString())) rebuildTree();
  };

  // Initial scan — run asynchronously so activation isn't blocked on IO.
  void fullRefresh();

  const watcher = vscode.workspace.createFileSystemWatcher('**/*.bock');
  watcher.onDidChange((uri) => void refreshFile(uri));
  watcher.onDidCreate((uri) => void refreshFile(uri));
  watcher.onDidDelete((uri) => removeFile(uri));
  ctx.subscriptions.push(watcher);

  ctx.subscriptions.push(
    vscode.commands.registerCommand('bock.annotations.refresh', () => {
      void fullRefresh();
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
