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
//
// On top of scope, the view supports querying the decision database:
// a facet filter (decision type multi-select, pin state, minimum
// confidence) driven from the Filter command, five sort modes
// (default / confidence ↑↓ / newest / module), and a per-record
// "Jump to Source JSON" action that opens the backing manifest file
// at the record's `"id"` line. Filtering, sorting, and the source-line
// lookup are exported pure functions (`applyDecisionFilter`,
// `sortLoadedDecisions`, `findRecordLine`) covered by the headless
// unit suite.

import * as vscode from 'vscode';
import * as cp from 'child_process';
import * as fs from 'fs';
import * as path from 'path';
import { VocabService } from '../vocab';
import { WebviewManager, escapeHtml } from '../shared/webview';
import { truncate } from '../shared/strings';

// ─── Types mirroring crates/bock-ai/src/decision.rs ─────────────────────────

type DecisionScope = 'build' | 'runtime';
type ViewScope = DecisionScope | 'all';

export type DecisionTypeTag =
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

export interface LoadedDecision {
  record: DecisionRecord;
  scope: DecisionScope;
  /** Source JSON file on disk, used for jump-to-source actions. */
  sourceFile: string;
}

/**
 * Structural validation for a decision record loaded from disk. Only the
 * fields the tree / tooltip / detail webview dereference unconditionally are
 * required; everything optional in `DecisionRecord` stays optional here. A
 * record that fails this guard is dropped at load time (and counted) so a
 * single malformed file can't crash rendering with `record.id.slice` /
 * `confidence.toFixed` / `alternatives.length` on `undefined`.
 *
 * Exported for unit testing.
 */
export function isValidDecisionRecord(x: unknown): x is DecisionRecord {
  if (typeof x !== 'object' || x === null) return false;
  const r = x as Record<string, unknown>;
  return (
    typeof r.id === 'string' &&
    typeof r.module === 'string' &&
    typeof r.choice === 'string' &&
    typeof r.decision_type === 'string' &&
    typeof r.model_id === 'string' &&
    typeof r.confidence === 'number' &&
    Number.isFinite(r.confidence) &&
    Array.isArray(r.alternatives) &&
    typeof r.pinned === 'boolean' &&
    typeof r.timestamp === 'string'
  );
}

/** Outcome of loading decisions: the valid records plus a count of records
 *  that were dropped (malformed JSON file OR invalid record shape).
 *  Exported for unit testing of the drop-count threading. */
export interface LoadResult {
  decisions: LoadedDecision[];
  skipped: number;
}

// ─── Query helpers (filter / sort / source lookup) ──────────────────────────
//
// Pure functions over LoadedDecision[] — no `vscode` runtime dependency, so
// the headless unit suite exercises them directly.

/** Every decision type tag, in the order the filter QuickPick presents them.
 *  Mirrors the `DecisionTypeTag` union. */
export const DECISION_TYPE_TAGS: readonly DecisionTypeTag[] = [
  'codegen',
  'repair',
  'optimize',
  'rule_applied',
  'handler_choice',
  'adaptive_recovery',
];

/** Pin-state facet of a decision filter. */
export type PinFilter = 'all' | 'pinned' | 'unpinned';

/**
 * Facet filter over loaded decisions. Every facet is optional; an absent
 * facet imposes no constraint. The empty object `{}` matches everything.
 */
export interface DecisionFilter {
  /** Show only these decision types. `undefined` or `[]` = all types. */
  types?: DecisionTypeTag[];
  /** Show only pinned / only unpinned records. `undefined`/`'all'` = both. */
  pinned?: PinFilter;
  /** Show only records with `confidence >= minConfidence` (inclusive). */
  minConfidence?: number;
}

/** True when `filter` constrains the result set at all. */
export function isFilterActive(filter: DecisionFilter): boolean {
  return (
    (filter.types !== undefined && filter.types.length > 0) ||
    (filter.pinned !== undefined && filter.pinned !== 'all') ||
    filter.minConfidence !== undefined
  );
}

/**
 * Apply a `DecisionFilter` to a list of loaded decisions. Pure: returns a
 * new array, never mutates the input. Facets combine with AND; an empty
 * filter returns every element.
 */
export function applyDecisionFilter(
  list: LoadedDecision[],
  filter: DecisionFilter,
): LoadedDecision[] {
  return list.filter((d) => {
    if (
      filter.types !== undefined &&
      filter.types.length > 0 &&
      !filter.types.includes(d.record.decision_type)
    ) {
      return false;
    }
    if (filter.pinned === 'pinned' && !d.record.pinned) return false;
    if (filter.pinned === 'unpinned' && d.record.pinned) return false;
    if (
      filter.minConfidence !== undefined &&
      d.record.confidence < filter.minConfidence
    ) {
      return false;
    }
    return true;
  });
}

/**
 * Sort modes for the decisions view.
 *
 * - `default` — unpinned first (needs-review on top), then id
 * - `confidence-asc` / `confidence-desc` — by `confidence`, ties by id
 * - `newest` — by `timestamp` descending (ISO-8601 strings compare
 *   lexicographically; non-ISO timestamps degrade to string order), ties by id
 * - `module` — by `module` ascending, then the default order within a module
 */
export type DecisionSortMode =
  | 'default'
  | 'confidence-asc'
  | 'confidence-desc'
  | 'newest'
  | 'module';

/**
 * Return a sorted copy of `list` according to `mode`. Pure: the input array
 * is never mutated.
 */
export function sortLoadedDecisions(
  list: LoadedDecision[],
  mode: DecisionSortMode,
): LoadedDecision[] {
  const copy = [...list];
  const byId = (a: LoadedDecision, b: LoadedDecision): number =>
    a.record.id.localeCompare(b.record.id);
  switch (mode) {
    case 'default':
      return copy.sort(sortDecisions);
    case 'confidence-asc':
      return copy.sort(
        (a, b) => a.record.confidence - b.record.confidence || byId(a, b),
      );
    case 'confidence-desc':
      return copy.sort(
        (a, b) => b.record.confidence - a.record.confidence || byId(a, b),
      );
    case 'newest':
      return copy.sort(
        (a, b) =>
          b.record.timestamp.localeCompare(a.record.timestamp) || byId(a, b),
      );
    case 'module':
      return copy.sort(
        (a, b) =>
          a.record.module.localeCompare(b.record.module) || sortDecisions(a, b),
      );
  }
}

/** Compact label for a sort mode, used in the view description. */
const SORT_MODE_SHORT: Record<DecisionSortMode, string> = {
  default: 'default',
  'confidence-asc': 'conf↑',
  'confidence-desc': 'conf↓',
  newest: 'newest',
  module: 'module',
};

/**
 * Compact summary of the active filter + sort for the tree view's
 * description (e.g. `type:codegen · conf≥0.8 · sort:newest`). Returns
 * `undefined` when nothing diverges from the defaults, so the view shows
 * no description at all.
 */
export function describeDecisionView(
  filter: DecisionFilter,
  mode: DecisionSortMode,
): string | undefined {
  const parts: string[] = [];
  if (filter.types !== undefined && filter.types.length > 0) {
    parts.push(`type:${filter.types.join(',')}`);
  }
  if (filter.pinned === 'pinned' || filter.pinned === 'unpinned') {
    parts.push(filter.pinned);
  }
  if (filter.minConfidence !== undefined) {
    parts.push(`conf≥${filter.minConfidence}`);
  }
  if (mode !== 'default') {
    parts.push(`sort:${SORT_MODE_SHORT[mode]}`);
  }
  return parts.length > 0 ? parts.join(' · ') : undefined;
}

/**
 * Parse a user-typed minimum-confidence value. Accepts a finite number in
 * `[0, 1]` (e.g. `0.85`); returns `undefined` for anything else (empty
 * input, non-numeric text, out-of-range values).
 */
export function parseConfidenceInput(raw: string): number | undefined {
  const trimmed = raw.trim();
  if (trimmed.length === 0) return undefined;
  const n = Number(trimmed);
  if (!Number.isFinite(n) || n < 0 || n > 1) return undefined;
  return n;
}

/** Escape a string for literal use inside a `RegExp`. */
function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/**
 * Find the 0-based line in `jsonText` that contains the `"id"` key whose
 * value is exactly `id`. Handles both pretty-printed (`"id": "abc"`) and
 * compact (`{"id":"abc",...}`) JSON, and does not false-positive on other
 * keys that merely end in `id` (`"model_id"`) or on the same hash stored
 * under a different key (`"superseded_by": "abc"`). Returns `undefined`
 * when the record is not present.
 *
 * Used by the "Jump to Source JSON" action; exported for unit testing.
 */
export function findRecordLine(
  jsonText: string,
  id: string,
): number | undefined {
  if (typeof jsonText !== 'string' || typeof id !== 'string' || id.length === 0) {
    return undefined;
  }
  // JSON.stringify yields the quoted, escaped form a serializer writes for
  // the id; escape that literal for use in the key:value pattern.
  const needle = new RegExp(
    `"id"\\s*:\\s*${escapeRegExp(JSON.stringify(id))}`,
  );
  const match = needle.exec(jsonText);
  if (!match) return undefined;
  let line = 0;
  for (let i = 0; i < match.index; i++) {
    if (jsonText.charCodeAt(i) === 10 /* \n */) line++;
  }
  return line;
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
  private filter: DecisionFilter = {};
  private sortMode: DecisionSortMode = 'default';

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

  setFilter(filter: DecisionFilter): void {
    this.filter = filter;
    this.emitter.fire(undefined);
  }

  getFilter(): DecisionFilter {
    return this.filter;
  }

  setSortMode(mode: DecisionSortMode): void {
    this.sortMode = mode;
    this.emitter.fire(undefined);
  }

  getSortMode(): DecisionSortMode {
    return this.sortMode;
  }

  /** Every decision in the active scope, before facet filtering. */
  private scoped(): LoadedDecision[] {
    if (this.scope === 'all') return this.decisions;
    return this.decisions.filter((d) => d.scope === this.scope);
  }

  /** Every decision in the active scope that passes the active filter. */
  filtered(): LoadedDecision[] {
    return applyDecisionFilter(this.scoped(), this.filter);
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
            : this.scoped().length === 0
              ? `No ${this.scope} decisions.`
              : 'No decisions match the active filters.';
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
          decisions: sortLoadedDecisions(ds, this.sortMode),
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
    const id = record.id ?? '';
    const short = id.length > 8 ? id.slice(0, 8) : id || '(no id)';
    const choicePreview = truncate(firstLine(record.choice ?? ''), 50);
    const label = `${record.decision_type ?? 'decision'} #${short} — ${choicePreview}`;

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
  if (typeof s !== 'string') return '';
  const idx = s.indexOf('\n');
  return idx === -1 ? s : s.slice(0, idx);
}

/** Format a confidence value defensively — fall back to `n/a` if the field
 *  isn't a finite number (e.g. a record that slipped past validation). */
function formatConfidence(value: number): string {
  return typeof value === 'number' && Number.isFinite(value)
    ? value.toFixed(2)
    : 'n/a';
}

/** Truncate an id defensively for display — tolerates a missing/non-string
 *  id on a record that slipped past validation. */
function shortId(id: string, n: number): string {
  if (typeof id !== 'string' || id.length === 0) return '(no id)';
  return id.length > n ? id.slice(0, n) : id;
}

function buildDecisionTooltip(d: LoadedDecision): vscode.MarkdownString {
  const md = new vscode.MarkdownString(undefined, true);
  md.isTrusted = true;
  const r = d.record;
  md.appendMarkdown(`**${r.decision_type}** · \`${r.id}\`\n\n`);
  md.appendMarkdown(`_Module:_ \`${r.module}\`\n\n`);
  if (r.target) md.appendMarkdown(`_Target:_ \`${r.target}\`\n\n`);
  md.appendMarkdown(`_Model:_ \`${r.model_id}\`\n\n`);
  md.appendMarkdown(`_Confidence:_ ${formatConfidence(r.confidence)}\n\n`);
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

/** Load every decision JSON under `<root>/.bock/decisions/{build,runtime}`,
 *  returning the valid records and a count of those dropped (malformed JSON
 *  or invalid shape). Exported for unit testing. */
export async function loadAllDecisions(root: string): Promise<LoadResult> {
  const decisionsRoot = path.join(root, '.bock', 'decisions');
  const result: LoadResult = { decisions: [], skipped: 0 };
  for (const scope of ['build', 'runtime'] as const) {
    const scopeRoot = path.join(decisionsRoot, scope);
    if (!fs.existsSync(scopeRoot)) continue;
    await walkJson(scopeRoot, scope, result);
  }
  return result;
}

async function walkJson(
  dir: string,
  scope: DecisionScope,
  result: LoadResult,
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
      await walkJson(full, scope, result);
    } else if (entry.isFile() && entry.name.endsWith('.json')) {
      let parsed: unknown;
      try {
        const text = await fs.promises.readFile(full, 'utf8');
        parsed = JSON.parse(text);
      } catch {
        // Malformed JSON — count and skip so a single bad file doesn't
        // break the whole view. `bock check` will flag it on rebuild.
        result.skipped++;
        continue;
      }
      const list: unknown[] = Array.isArray(parsed) ? parsed : [parsed];
      for (const candidate of list) {
        if (isValidDecisionRecord(candidate)) {
          result.decisions.push({ record: candidate, scope, sourceFile: full });
        } else {
          // Valid JSON but the record is missing fields the tree / detail
          // view dereferences unconditionally. Drop it (counted) rather
          // than crash rendering.
          result.skipped++;
        }
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
  const title = `Bock — Decision ${shortId(r.id, 8)}`;
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
    `<tr><th>Confidence</th><td>${formatConfidence(r.confidence)}</td></tr>`,
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

  const alts = Array.isArray(r.alternatives) ? r.alternatives : [];
  const alternatives = alts.length
    ? `<ol>${alts
        .map((alt) => `<li><code>${escapeHtml(String(alt))}</code></li>`)
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
    <h1>Decision <code>${escapeHtml(shortId(r.id, 12))}</code> ${pinBadge}</h1>
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
    const { decisions, skipped } = await loadAllDecisions(root);
    provider.setDecisions(decisions);
    updateIndicators(view, statusBar, provider);
    if (skipped > 0) {
      void vscode.window.showWarningMessage(
        `Bock: skipped ${skipped} malformed decision record(s).`,
      );
    }
  };
  const scheduleRefresh = (): void => {
    if (pending) clearTimeout(pending);
    pending = setTimeout(() => {
      pending = undefined;
      void refresh();
    }, 150);
  };

  void refresh();

  // Re-derive everything that depends on the filter/sort state: the
  // compact description next to the view title, the unpinned badge, and
  // the status-bar summary (all of which respect the active filter).
  const refreshViewState = (): void => {
    view.description = describeDecisionView(
      provider.getFilter(),
      provider.getSortMode(),
    );
    updateIndicators(view, statusBar, provider);
  };

  // Keep the title scope indicator in sync when the caller cycles scopes.
  const setScope = (scope: ViewScope): void => {
    provider.setScope(scope);
    void vscode.commands.executeCommand(
      'setContext',
      'bock.decisions.scope',
      scope,
    );
    view.title = `Bock Decisions — ${scopeLabel(scope)}`;
    refreshViewState();
  };
  setScope('build');

  const setFilter = (filter: DecisionFilter): void => {
    provider.setFilter(filter);
    refreshViewState();
  };

  // ── Filter facet pickers ──────────────────────────────────────────────

  const pickScope = async (): Promise<void> => {
    const pick = await vscode.window.showQuickPick(
      [
        { label: 'Build', description: 'Compile-time decisions', scope: 'build' as const },
        { label: 'Runtime', description: 'Adaptive-recovery decisions', scope: 'runtime' as const },
        { label: 'All', description: 'Both scopes', scope: 'all' as const },
      ],
      { placeHolder: 'Filter decisions by scope' },
    );
    if (pick) setScope(pick.scope);
  };

  const pickTypeFilter = async (): Promise<void> => {
    const current = provider.getFilter().types;
    const items = DECISION_TYPE_TAGS.map((tag) => ({
      label: tag,
      picked: current === undefined || current.includes(tag),
    }));
    const picks = await vscode.window.showQuickPick(items, {
      canPickMany: true,
      placeHolder:
        'Show only these decision types (all or none checked = no type filter)',
    });
    if (picks === undefined) return; // cancelled
    const chosen = picks.map((p) => p.label as DecisionTypeTag);
    const types =
      chosen.length === 0 || chosen.length === DECISION_TYPE_TAGS.length
        ? undefined
        : chosen;
    setFilter({ ...provider.getFilter(), types });
  };

  const pickPinFilter = async (): Promise<void> => {
    const pick = await vscode.window.showQuickPick(
      [
        { label: 'All', description: 'Pinned and unpinned', value: 'all' as const },
        { label: 'Pinned only', value: 'pinned' as const },
        { label: 'Unpinned only', description: 'Still needs review', value: 'unpinned' as const },
      ],
      { placeHolder: 'Filter decisions by pin state' },
    );
    if (pick === undefined) return;
    setFilter({
      ...provider.getFilter(),
      pinned: pick.value === 'all' ? undefined : pick.value,
    });
  };

  const pickConfidenceFilter = async (): Promise<void> => {
    const pick = await vscode.window.showQuickPick(
      [
        { label: 'No minimum', value: 'none' as const },
        { label: '≥ 0.5', value: 0.5 },
        { label: '≥ 0.7', value: 0.7 },
        { label: '≥ 0.9', value: 0.9 },
        { label: 'Custom…', description: 'Enter a value in [0, 1]', value: 'custom' as const },
      ],
      { placeHolder: 'Show only decisions at or above this confidence' },
    );
    if (pick === undefined) return;
    let min: number | undefined;
    if (pick.value === 'none') {
      min = undefined;
    } else if (pick.value === 'custom') {
      const raw = await vscode.window.showInputBox({
        prompt: 'Minimum confidence (0 to 1, e.g. 0.85)',
        validateInput: (v) =>
          parseConfidenceInput(v) === undefined
            ? 'Enter a number between 0 and 1.'
            : undefined,
      });
      if (raw === undefined) return; // cancelled
      min = parseConfidenceInput(raw);
      if (min === undefined) return;
    } else {
      min = pick.value;
    }
    setFilter({ ...provider.getFilter(), minConfidence: min });
  };

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
      const f = provider.getFilter();
      const pick = await vscode.window.showQuickPick(
        [
          {
            label: '$(target) Scope',
            description: scopeLabel(provider.getScope()),
            action: 'scope' as const,
          },
          {
            label: '$(symbol-enum) Decision type',
            description: f.types !== undefined && f.types.length > 0 ? f.types.join(', ') : 'all',
            action: 'type' as const,
          },
          {
            label: '$(pin) Pin state',
            description: f.pinned ?? 'all',
            action: 'pinned' as const,
          },
          {
            label: '$(dashboard) Minimum confidence',
            description: f.minConfidence !== undefined ? `≥ ${f.minConfidence}` : 'none',
            action: 'confidence' as const,
          },
          {
            label: '$(clear-all) Clear filters',
            description: isFilterActive(f) ? 'remove all active facets' : 'nothing active',
            action: 'clear' as const,
          },
        ],
        { placeHolder: 'Filter Bock decisions' },
      );
      if (pick === undefined) return;
      switch (pick.action) {
        case 'scope':
          await pickScope();
          break;
        case 'type':
          await pickTypeFilter();
          break;
        case 'pinned':
          await pickPinFilter();
          break;
        case 'confidence':
          await pickConfidenceFilter();
          break;
        case 'clear':
          setFilter({});
          break;
      }
    }),
    vscode.commands.registerCommand('bock.decisions.clearFilters', () => {
      setFilter({});
    }),
    vscode.commands.registerCommand('bock.decisions.sort', async () => {
      const current = provider.getSortMode();
      const modes: { label: string; description?: string; mode: DecisionSortMode }[] = [
        { label: 'Default', description: 'unpinned first, then id', mode: 'default' },
        { label: 'Confidence — low to high', mode: 'confidence-asc' },
        { label: 'Confidence — high to low', mode: 'confidence-desc' },
        { label: 'Newest first', description: 'by timestamp', mode: 'newest' },
        { label: 'Module', description: 'by source module', mode: 'module' },
      ];
      const pick = await vscode.window.showQuickPick(
        modes.map((m) => ({
          ...m,
          description:
            m.mode === current
              ? `${m.description ? `${m.description} · ` : ''}current`
              : m.description,
        })),
        { placeHolder: 'Sort Bock decisions' },
      );
      if (pick === undefined) return;
      provider.setSortMode(pick.mode);
      refreshViewState();
    }),
    vscode.commands.registerCommand(
      'bock.decisions.jumpToSource',
      async (node: DecisionNode | LoadedDecision | undefined) => {
        const decision = resolveDecision(node);
        if (!decision) return;
        let doc: vscode.TextDocument;
        try {
          doc = await vscode.workspace.openTextDocument(
            vscode.Uri.file(decision.sourceFile),
          );
        } catch {
          void vscode.window.showErrorMessage(
            `Bock: could not open ${decision.sourceFile}.`,
          );
          return;
        }
        const editor = await vscode.window.showTextDocument(doc, {
          preview: false,
        });
        const line = findRecordLine(doc.getText(), decision.record.id);
        if (line === undefined) {
          void vscode.window.showWarningMessage(
            `Bock: record ${shortId(decision.record.id, 8)} not found in ${path.basename(decision.sourceFile)} — the manifest may have changed on disk.`,
          );
          return;
        }
        const range = doc.lineAt(line).range;
        editor.selection = new vscode.Selection(range.start, range.end);
        editor.revealRange(
          range,
          vscode.TextEditorRevealType.InCenterIfOutsideViewport,
        );
      },
    ),
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
  const filterSuffix = isFilterActive(provider.getFilter())
    ? ' (filtered)'
    : '';
  if (badgeEnabled && unpinned > 0) {
    view.badge = {
      value: unpinned,
      tooltip: `${unpinned} unpinned ${provider.getScope()} decision(s)${filterSuffix}`,
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
