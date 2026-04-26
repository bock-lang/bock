// Decision manifest UI (F1.5.7).
//
// Surfaces build- and runtime-scope AI decisions from
// `.bock/decisions/{build,runtime}/**/*.json` as a tree grouped by
// source module, with inline pin/unpin/override/promote actions that
// shell out to the `bock` CLI. A detail webview renders the full
// decision record (reasoning, alternatives, pin metadata, confidence).
//
// The tree view exposes a scope toggle (Build ↔ Runtime ↔ All), a badge
// counting unpinned decisions, and a status-bar summary of the current
// pin state. A `FileSystemWatcher` on the decisions directory keeps the
// view in sync with external edits (e.g. a CI `bock pin --all-build`
// run).

import * as vscode from 'vscode';
import * as cp from 'child_process';
import * as fs from 'fs';
import * as path from 'path';
import { VocabService } from '../vocab';
import { WebviewManager, escapeHtml } from '../shared/webview';

// ─── Types mirroring crates/bock-ai/src/decision.rs ─────────────────────────

type DecisionScope = 'build' | 'runtime';
type ViewScope = DecisionScope | 'all';

type DecisionTypeTag =
  | 'codegen'
  | 'repair'
  | 'optimize'
  | 'rule_applied'
  | 'handler_choice'
  | 'adaptive_recovery';

interface DecisionRecord {
  id: string;
  module: string;
  target?: string | null;
  decision_type: DecisionTypeTag;
  choice: string;
  alternatives: string[];
  reasoning?: string | null;
  model_id: string;
  confidence: number;
  pinned: boolean;
  pin_reason?: string | null;
  pinned_at?: string | null;
  pinned_by?: string | null;
  superseded_by?: string | null;
  timestamp: string;
}

interface LoadedDecision {
  record: DecisionRecord;
  scope: DecisionScope;
  /** Source JSON file on disk, used for jump-to-source actions. */
  sourceFile: string;
}

// ─── Tree nodes ─────────────────────────────────────────────────────────────

type TreeNode = ModuleNode | DecisionNode | EmptyNode;

interface ModuleNode {
  kind: 'module';
  module: string;
  decisions: LoadedDecision[];
}

interface DecisionNode {
  kind: 'decision';
  decision: LoadedDecision;
}

interface EmptyNode {
  kind: 'empty';
  message: string;
}

// ─── TreeDataProvider ───────────────────────────────────────────────────────

class DecisionsTreeProvider implements vscode.TreeDataProvider<TreeNode> {
  private readonly emitter = new vscode.EventEmitter<TreeNode | undefined>();
  readonly onDidChangeTreeData = this.emitter.event;

  private decisions: LoadedDecision[] = [];
  private scope: ViewScope = 'build';

  setDecisions(decisions: LoadedDecision[]): void {
    this.decisions = decisions;
    this.emitter.fire(undefined);
  }

  setScope(scope: ViewScope): void {
    this.scope = scope;
    this.emitter.fire(undefined);
  }

  getScope(): ViewScope {
    return this.scope;
  }

  /** Every decision in the active scope (unfiltered by module). */
  filtered(): LoadedDecision[] {
    if (this.scope === 'all') return this.decisions;
    return this.decisions.filter((d) => d.scope === this.scope);
  }

  unpinnedCount(): number {
    return this.filtered().filter((d) => !d.record.pinned).length;
  }

  pinnedCount(): number {
    return this.filtered().filter((d) => d.record.pinned).length;
  }

  getChildren(element?: TreeNode): TreeNode[] {
    if (!element) {
      const active = this.filtered();
      if (active.length === 0) {
        const msg =
          this.decisions.length === 0
            ? 'No .bock/decisions/ found in this workspace.'
            : `No ${this.scope} decisions.`;
        return [{ kind: 'empty', message: msg }];
      }
      const byModule = new Map<string, LoadedDecision[]>();
      for (const d of active) {
        const arr = byModule.get(d.record.module);
        if (arr) arr.push(d);
        else byModule.set(d.record.module, [d]);
      }
      const modules: ModuleNode[] = Array.from(byModule.entries())
        .map(([module, ds]) => ({
          kind: 'module' as const,
          module,
          decisions: ds.sort(sortDecisions),
        }))
        .sort((a, b) => a.module.localeCompare(b.module));
      return modules;
    }
    if (element.kind === 'module') {
      return element.decisions.map((d) => ({ kind: 'decision', decision: d }));
    }
    return [];
  }

  getTreeItem(element: TreeNode): vscode.TreeItem {
    if (element.kind === 'empty') {
      const item = new vscode.TreeItem(
        element.message,
        vscode.TreeItemCollapsibleState.None,
      );
      item.iconPath = new vscode.ThemeIcon('info');
      item.contextValue = 'decisionsEmpty';
      return item;
    }
    if (element.kind === 'module') {
      const unpinned = element.decisions.filter((d) => !d.record.pinned).length;
      const label = `${element.module} (${element.decisions.length})`;
      const item = new vscode.TreeItem(
        label,
        vscode.TreeItemCollapsibleState.Expanded,
      );
      item.iconPath = new vscode.ThemeIcon('file-code');
      item.contextValue = 'decisionModule';
      item.tooltip = new vscode.MarkdownString(
        `**${element.module}**\n\n${element.decisions.length} decision(s), ${unpinned} unpinned`,
      );
      item.description =
        unpinned > 0 ? `${unpinned} unpinned` : undefined;
      return item;
    }

    const { record } = element.decision;
    const icon = record.pinned
      ? new vscode.ThemeIcon(
          'pinned',
          new vscode.ThemeColor('charts.green'),
        )
      : new vscode.ThemeIcon(
          'warning',
          new vscode.ThemeColor('charts.yellow'),
        );
    const short = record.id.length > 8 ? record.id.slice(0, 8) : record.id;
    const choicePreview = truncate(firstLine(record.choice), 50);
    const label = `${record.decision_type} #${short} — ${choicePreview}`;

    const item = new vscode.TreeItem(label, vscode.TreeItemCollapsibleState.None);
    item.iconPath = icon;
    item.tooltip = buildDecisionTooltip(element.decision);
    item.description = record.pinned
      ? `pinned${record.pinned_by ? ` · ${record.pinned_by}` : ''}`
      : 'unpinned';
    item.contextValue = decisionContextValue(element.decision);
    item.command = {
      command: 'bock.decisions.showDetail',
      title: 'Show Decision Details',
      arguments: [element.decision],
    };
    return item;
  }
}

function sortDecisions(a: LoadedDecision, b: LoadedDecision): number {
  // Unpinned first (so users see what still needs review), then by id.
  if (a.record.pinned !== b.record.pinned) return a.record.pinned ? 1 : -1;
  return a.record.id.localeCompare(b.record.id);
}

function decisionContextValue(d: LoadedDecision): string {
  const parts: string[] = [];
  parts.push(d.record.pinned ? 'pinnedDecision' : 'unpinnedDecision');
  if (d.scope === 'runtime') parts.push('runtimeDecision');
  else parts.push('buildDecision');
  return parts.join(' ');
}

function firstLine(s: string): string {
  const idx = s.indexOf('\n');
  return idx === -1 ? s : s.slice(0, idx);
}

function truncate(s: string, n: number): string {
  const trimmed = s.trim();
  return trimmed.length > n ? `${trimmed.slice(0, n - 1)}…` : trimmed;
}

function buildDecisionTooltip(d: LoadedDecision): vscode.MarkdownString {
  const md = new vscode.MarkdownString(undefined, true);
  md.isTrusted = true;
  const r = d.record;
  md.appendMarkdown(`**${r.decision_type}** · \`${r.id}\`\n\n`);
  md.appendMarkdown(`_Module:_ \`${r.module}\`\n\n`);
  if (r.target) md.appendMarkdown(`_Target:_ \`${r.target}\`\n\n`);
  md.appendMarkdown(`_Model:_ \`${r.model_id}\`\n\n`);
  md.appendMarkdown(`_Confidence:_ ${r.confidence.toFixed(2)}\n\n`);
  md.appendMarkdown(`_Scope:_ ${d.scope}\n\n`);
  if (r.pinned) {
    const who = r.pinned_by ? ` by ${r.pinned_by}` : '';
    const when = r.pinned_at ? ` at ${r.pinned_at}` : '';
    md.appendMarkdown(`_Pinned:_ yes${who}${when}\n\n`);
  } else {
    md.appendMarkdown(`_Pinned:_ no\n\n`);
  }
  md.appendMarkdown(
    '_Click to open the decision detail panel._\n',
  );
  return md;
}

// ─── Manifest IO ────────────────────────────────────────────────────────────

function findProjectRoot(): string | undefined {
  const folders = vscode.workspace.workspaceFolders;
  if (!folders || folders.length === 0) return undefined;
  for (const folder of folders) {
    const candidate = path.join(folder.uri.fsPath, 'bock.project');
    if (fs.existsSync(candidate)) return folder.uri.fsPath;
  }
  // Fall back to the first folder even without an bock.project marker,
  // so the decisions tree still renders while the user scaffolds.
  return folders[0].uri.fsPath;
}

async function loadAllDecisions(root: string): Promise<LoadedDecision[]> {
  const decisionsRoot = path.join(root, '.bock', 'decisions');
  const out: LoadedDecision[] = [];
  for (const scope of ['build', 'runtime'] as const) {
    const scopeRoot = path.join(decisionsRoot, scope);
    if (!fs.existsSync(scopeRoot)) continue;
    await walkJson(scopeRoot, scope, out);
  }
  return out;
}

async function walkJson(
  dir: string,
  scope: DecisionScope,
  out: LoadedDecision[],
): Promise<void> {
  let entries: fs.Dirent[];
  try {
    entries = await fs.promises.readdir(dir, { withFileTypes: true });
  } catch {
    return;
  }
  for (const entry of entries) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      await walkJson(full, scope, out);
    } else if (entry.isFile() && entry.name.endsWith('.json')) {
      try {
        const text = await fs.promises.readFile(full, 'utf8');
        const parsed = JSON.parse(text);
        const list: DecisionRecord[] = Array.isArray(parsed) ? parsed : [parsed];
        for (const record of list) {
          out.push({ record, scope, sourceFile: full });
        }
      } catch {
        // Malformed JSON — silently skip so a single bad file doesn't
        // break the whole view. `bock check` will flag it on rebuild.
      }
    }
  }
}

// ─── CLI helpers ────────────────────────────────────────────────────────────

function findBockBinary(): string | undefined {
  const configured = vscode.workspace
    .getConfiguration('bock')
    .get<string>('lspPath', '')
    .trim();
  if (configured && fs.existsSync(configured)) return configured;

  const envPath = process.env.PATH ?? '';
  const isWin = process.platform === 'win32';
  const exe = isWin ? 'bock.exe' : 'bock';
  for (const dir of envPath.split(path.delimiter)) {
    if (!dir) continue;
    const candidate = path.join(dir, exe);
    if (fs.existsSync(candidate)) return candidate;
  }
  return undefined;
}

interface CliResult {
  ok: boolean;
  stdout: string;
  stderr: string;
}

async function runBock(args: string[], cwd: string): Promise<CliResult> {
  const binary = findBockBinary();
  if (!binary) {
    return {
      ok: false,
      stdout: '',
      stderr:
        'Could not locate `bock` binary. Set `bock.lspPath` or install the compiler.',
    };
  }
  return new Promise((resolve) => {
    cp.execFile(
      binary,
      args,
      { cwd, maxBuffer: 4 * 1024 * 1024 },
      (err, stdout, stderr) => {
        resolve({
          ok: !err,
          stdout: stdout?.toString() ?? '',
          stderr: stderr?.toString() ?? '',
        });
      },
    );
  });
}

function scopedId(d: LoadedDecision): string {
  // The CLI accepts `build:<id>` / `runtime:<id>` prefixes; use them to
  // avoid ambiguous-id errors when the same hash lives in both scopes.
  return `${d.scope}:${d.record.id}`;
}

// ─── Detail webview ─────────────────────────────────────────────────────────

function openDetailWebview(
  webviews: WebviewManager,
  decision: LoadedDecision,
): void {
  const r = decision.record;
  const title = `Bock — Decision ${r.id.slice(0, 8)}`;
  const handle = webviews.create(
    `bock.decision.${r.id}`,
    title,
    renderDetailHtml(decision),
  );
  handle.panel.webview.onDidReceiveMessage((msg) => {
    if (!msg || typeof msg.type !== 'string') return;
    switch (msg.type) {
      case 'pin':
        void vscode.commands.executeCommand('bock.pinDecision', decision);
        break;
      case 'unpin':
        void vscode.commands.executeCommand('bock.unpinDecision', decision);
        break;
      case 'override':
        void vscode.commands.executeCommand('bock.overrideDecision', decision);
        break;
      case 'promote':
        void vscode.commands.executeCommand('bock.promoteDecision', decision);
        break;
      case 'openSpec':
        void vscode.commands.executeCommand('bock.openSpecAt', '§17.4');
        break;
      case 'openModule': {
        const root = findProjectRoot();
        if (root) {
          const uri = vscode.Uri.file(path.join(root, r.module));
          void vscode.window.showTextDocument(uri, { preview: false });
        }
        break;
      }
      default:
        break;
    }
  });
}

function renderDetailHtml(decision: LoadedDecision): {
  body: string;
  scripts: string[];
} {
  const r = decision.record;
  const pinBadge = r.pinned
    ? `<span class="bock-badge bock-pinned">pinned</span>`
    : `<span class="bock-badge bock-unpinned">unpinned</span>`;

  const metaRows: string[] = [];
  metaRows.push(
    `<tr><th>Type</th><td><code>${escapeHtml(r.decision_type)}</code></td></tr>`,
  );
  metaRows.push(
    `<tr><th>Scope</th><td><code>${escapeHtml(decision.scope)}</code></td></tr>`,
  );
  metaRows.push(
    `<tr><th>Module</th><td><a href="#" class="bock-open-module"><code>${escapeHtml(r.module)}</code></a></td></tr>`,
  );
  if (r.target) {
    metaRows.push(
      `<tr><th>Target</th><td><code>${escapeHtml(r.target)}</code></td></tr>`,
    );
  }
  metaRows.push(
    `<tr><th>Model</th><td><code>${escapeHtml(r.model_id)}</code></td></tr>`,
  );
  metaRows.push(
    `<tr><th>Confidence</th><td>${r.confidence.toFixed(2)}</td></tr>`,
  );
  if (r.pinned) {
    const who = r.pinned_by ? escapeHtml(r.pinned_by) : 'unknown';
    const when = r.pinned_at
      ? ` on ${escapeHtml(r.pinned_at)}`
      : '';
    metaRows.push(`<tr><th>Pinned</th><td>Yes (by ${who}${when})</td></tr>`);
    if (r.pin_reason) {
      metaRows.push(
        `<tr><th>Reason</th><td>${escapeHtml(r.pin_reason)}</td></tr>`,
      );
    }
  }
  if (r.superseded_by) {
    metaRows.push(
      `<tr><th>Superseded by</th><td><code>${escapeHtml(r.superseded_by)}</code></td></tr>`,
    );
  }
  metaRows.push(
    `<tr><th>Recorded</th><td>${escapeHtml(r.timestamp)}</td></tr>`,
  );

  const alternatives = r.alternatives.length
    ? `<ol>${r.alternatives
        .map((alt) => `<li><code>${escapeHtml(alt)}</code></li>`)
        .join('')}</ol>`
    : `<p class="bock-missing">None recorded.</p>`;

  const reasoning = r.reasoning
    ? `<p>${escapeHtml(r.reasoning)}</p>`
    : `<p class="bock-missing">No reasoning recorded.</p>`;

  const actions: string[] = [];
  if (r.pinned) {
    actions.push(
      `<button class="bock-action" data-action="unpin">Unpin</button>`,
    );
  } else {
    actions.push(
      `<button class="bock-action" data-action="pin">Pin</button>`,
    );
  }
  actions.push(
    `<button class="bock-action" data-action="override">Override</button>`,
  );
  if (decision.scope === 'runtime') {
    actions.push(
      `<button class="bock-action" data-action="promote">Promote to Build</button>`,
    );
  }
  actions.push(
    `<button class="bock-action" data-action="openSpec">View Spec §17.4</button>`,
  );

  const body = `
    <style>
      table { border-collapse: collapse; margin: 1em 0; }
      th, td { text-align: left; padding: 0.25em 0.75em 0.25em 0; vertical-align: top; }
      th { color: var(--vscode-descriptionForeground); font-weight: normal; }
      .bock-pinned { background: var(--vscode-testing-iconPassed, #2d7d2d); color: #fff; }
      .bock-unpinned { background: var(--vscode-editorWarning-foreground, #b08d00); color: #000; }
      .bock-actions { display: flex; gap: 0.5em; flex-wrap: wrap; margin-top: 1em; }
      .bock-action {
        padding: 0.35em 0.9em;
        border: 1px solid var(--vscode-button-border, transparent);
        background: var(--vscode-button-background);
        color: var(--vscode-button-foreground);
        cursor: pointer;
        border-radius: 3px;
      }
      .bock-action:hover { background: var(--vscode-button-hoverBackground); }
    </style>
    <h1>Decision <code>${escapeHtml(r.id.slice(0, 12))}</code> ${pinBadge}</h1>
    <table>${metaRows.join('')}</table>
    <h2>Choice</h2>
    <pre><code>${escapeHtml(r.choice)}</code></pre>
    <h2>Alternatives considered</h2>
    ${alternatives}
    <h2>Reasoning</h2>
    ${reasoning}
    <h2>Actions</h2>
    <div class="bock-actions">${actions.join('')}</div>
  `;

  const script = `
const vscode = acquireVsCodeApi();
document.querySelectorAll('.bock-action').forEach((el) => {
  el.addEventListener('click', () => {
    vscode.postMessage({ type: el.dataset.action });
  });
});
document.querySelectorAll('.bock-open-module').forEach((el) => {
  el.addEventListener('click', (e) => {
    e.preventDefault();
    vscode.postMessage({ type: 'openModule' });
  });
});`;
  return { body, scripts: [script] };
}

// ─── Registration ───────────────────────────────────────────────────────────

export function registerDecisions(
  ctx: vscode.ExtensionContext,
  _vocab: VocabService,
): void {
  const provider = new DecisionsTreeProvider();
  const webviews = new WebviewManager(ctx);

  const view = vscode.window.createTreeView('bock.decisions', {
    treeDataProvider: provider,
    showCollapseAll: true,
  });
  ctx.subscriptions.push(view);

  const statusBar = vscode.window.createStatusBarItem(
    vscode.StatusBarAlignment.Right,
    99,
  );
  statusBar.command = 'bock.showDecisions';
  ctx.subscriptions.push(statusBar);

  let pending: NodeJS.Timeout | undefined;
  const refresh = async (): Promise<void> => {
    const root = findProjectRoot();
    if (!root) {
      provider.setDecisions([]);
      updateIndicators(view, statusBar, provider);
      return;
    }
    const decisions = await loadAllDecisions(root);
    provider.setDecisions(decisions);
    updateIndicators(view, statusBar, provider);
  };
  const scheduleRefresh = (): void => {
    if (pending) clearTimeout(pending);
    pending = setTimeout(() => {
      pending = undefined;
      void refresh();
    }, 150);
  };

  void refresh();

  // Keep the title scope indicator in sync when the caller cycles scopes.
  const setScope = (scope: ViewScope): void => {
    provider.setScope(scope);
    void vscode.commands.executeCommand(
      'setContext',
      'bock.decisions.scope',
      scope,
    );
    view.title = `Bock Decisions — ${scopeLabel(scope)}`;
    updateIndicators(view, statusBar, provider);
  };
  setScope('build');

  const watcher = vscode.workspace.createFileSystemWatcher(
    '**/.bock/decisions/**/*.json',
  );
  watcher.onDidChange(scheduleRefresh);
  watcher.onDidCreate(scheduleRefresh);
  watcher.onDidDelete(scheduleRefresh);
  ctx.subscriptions.push(watcher);

  ctx.subscriptions.push(
    vscode.commands.registerCommand('bock.showDecisions', async () => {
      await view.reveal(undefined as unknown as TreeNode, {
        focus: true,
      }).then(undefined, () => undefined);
      await vscode.commands.executeCommand('bock.decisions.focus');
    }),
    vscode.commands.registerCommand('bock.decisions.refresh', () => {
      void refresh();
    }),
    vscode.commands.registerCommand('bock.decisions.filter', async () => {
      const pick = await vscode.window.showQuickPick(
        [
          { label: 'Build', description: 'Compile-time decisions', scope: 'build' as const },
          { label: 'Runtime', description: 'Adaptive-recovery decisions', scope: 'runtime' as const },
          { label: 'All', description: 'Both scopes', scope: 'all' as const },
        ],
        { placeHolder: 'Filter decisions by scope' },
      );
      if (pick) setScope(pick.scope);
    }),
    vscode.commands.registerCommand('bock.decisions.scopeBuild', () =>
      setScope('build'),
    ),
    vscode.commands.registerCommand('bock.decisions.scopeRuntime', () =>
      setScope('runtime'),
    ),
    vscode.commands.registerCommand('bock.decisions.scopeAll', () =>
      setScope('all'),
    ),
    vscode.commands.registerCommand(
      'bock.decisions.showDetail',
      (node: LoadedDecision | DecisionNode | undefined) => {
        const decision = resolveDecision(node);
        if (!decision) return;
        openDetailWebview(webviews, decision);
      },
    ),
    vscode.commands.registerCommand(
      'bock.pinDecision',
      async (node: DecisionNode | LoadedDecision | undefined) => {
        const decision = resolveDecision(node);
        if (!decision) return;
        await runDecisionCliAction(
          ['pin', scopedId(decision)],
          `Pinning ${decision.scope} decision ${decision.record.id.slice(0, 8)}…`,
          refresh,
        );
      },
    ),
    vscode.commands.registerCommand(
      'bock.unpinDecision',
      async (node: DecisionNode | LoadedDecision | undefined) => {
        const decision = resolveDecision(node);
        if (!decision) return;
        await runDecisionCliAction(
          ['unpin', scopedId(decision)],
          `Unpinning ${decision.scope} decision ${decision.record.id.slice(0, 8)}…`,
          refresh,
        );
      },
    ),
    vscode.commands.registerCommand(
      'bock.overrideDecision',
      async (node: DecisionNode | LoadedDecision | undefined) => {
        const decision = resolveDecision(node);
        if (!decision) return;
        const current = decision.record.choice;
        const newChoice = await vscode.window.showInputBox({
          prompt: `New choice for ${decision.record.decision_type} decision ${decision.record.id.slice(0, 8)}`,
          value: current,
          valueSelection: [0, current.length],
        });
        if (newChoice === undefined) return;
        await runDecisionCliAction(
          ['override', scopedId(decision), newChoice],
          `Overriding ${decision.scope} decision…`,
          refresh,
        );
      },
    ),
    vscode.commands.registerCommand(
      'bock.promoteDecision',
      async (node: DecisionNode | LoadedDecision | undefined) => {
        const decision = resolveDecision(node);
        if (!decision) return;
        if (decision.scope !== 'runtime') {
          void vscode.window.showWarningMessage(
            'Bock: only runtime decisions can be promoted to the build manifest.',
          );
          return;
        }
        if (!decision.record.pinned) {
          void vscode.window.showWarningMessage(
            'Bock: promote requires a pinned runtime decision. Pin it first.',
          );
          return;
        }
        await runDecisionCliAction(
          ['override', '--promote', scopedId(decision)],
          `Promoting runtime decision ${decision.record.id.slice(0, 8)} to build…`,
          refresh,
        );
      },
    ),
    vscode.commands.registerCommand('bock.decisions.pinAllBuild', async () => {
      await runDecisionCliAction(
        ['pin', '--all-build'],
        'Pinning all build decisions…',
        refresh,
      );
    }),
  );

  async function runDecisionCliAction(
    args: string[],
    progressMessage: string,
    after: () => Promise<void>,
  ): Promise<void> {
    const root = findProjectRoot();
    if (!root) {
      void vscode.window.showErrorMessage(
        'Bock: no workspace folder — open an Bock project to manage decisions.',
      );
      return;
    }
    const result = await vscode.window.withProgress(
      { location: vscode.ProgressLocation.Notification, title: progressMessage },
      () => runBock(args, root),
    );
    if (!result.ok) {
      const msg = result.stderr.trim() || result.stdout.trim() || 'command failed';
      void vscode.window.showErrorMessage(`Bock: ${msg}`);
    } else {
      const summary = result.stdout.trim().split('\n')[0] || 'done';
      void vscode.window.setStatusBarMessage(`Bock: ${summary}`, 4000);
    }
    await after();
  }
}

function resolveDecision(
  node: DecisionNode | LoadedDecision | undefined,
): LoadedDecision | undefined {
  if (!node) return undefined;
  if ('kind' in node && node.kind === 'decision') return node.decision;
  if ('record' in node && 'scope' in node) return node as LoadedDecision;
  return undefined;
}

function scopeLabel(scope: ViewScope): string {
  switch (scope) {
    case 'build':
      return 'Build';
    case 'runtime':
      return 'Runtime';
    case 'all':
      return 'All';
  }
}

function updateIndicators(
  view: vscode.TreeView<TreeNode>,
  statusBar: vscode.StatusBarItem,
  provider: DecisionsTreeProvider,
): void {
  const unpinned = provider.unpinnedCount();
  const pinned = provider.pinnedCount();

  const badgeEnabled = vscode.workspace
    .getConfiguration('bock')
    .get<boolean>('decisions.showUnpinnedBadge', true);
  if (badgeEnabled && unpinned > 0) {
    view.badge = {
      value: unpinned,
      tooltip: `${unpinned} unpinned ${provider.getScope()} decision(s)`,
    };
  } else {
    view.badge = undefined;
  }

  statusBar.text = `$(pinned) Bock: ${pinned} pinned, ${unpinned} unpinned`;
  statusBar.tooltip = new vscode.MarkdownString(
    `Scope: **${scopeLabel(provider.getScope())}**\n\nClick to open the Decisions view.`,
  );
  if (unpinned > 0) {
    statusBar.backgroundColor = new vscode.ThemeColor(
      'statusBarItem.warningBackground',
    );
  } else {
    statusBar.backgroundColor = undefined;
  }
  if (pinned + unpinned > 0) statusBar.show();
  else statusBar.hide();
}
