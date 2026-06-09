// AIR tree viewer (`bock.showAir`): run `bock inspect air <file> --json` on
// the active `.bock` editor and render the lowered AIR tree in the
// `bock.airView` tree view (Bock activity-bar container). Clicking a node
// reveals and selects its source span; saving the shown document refreshes
// the tree (debounced).
//
// All JSON parsing, labelling, and span→editor coordinate reasoning lives in
// the pure, headless-testable `air-model.ts`; this module owns the
// editor/UI/process side. The `bock` binary is resolved exclusively via
// `findBockLspBinary` from `lsp.ts` (PATH or the machine-scoped
// `bock.lspPath` setting) — never from workspace contents (see the SECURITY
// note in `lsp.ts`). Every failure mode (missing binary, frontend error,
// crashed/incompatible CLI) renders as an informative node inside the view
// rather than a popup, so the extension degrades gracefully.

import * as vscode from 'vscode';
import * as cp from 'child_process';
import * as fs from 'fs';
import * as path from 'path';
import { findBockLspBinary } from '../lsp';
import { findProjectRoot } from './preview-paths';
import {
  AirDiagnostic,
  AirNode,
  AirSpan,
  childCount,
  nodeIconId,
  nodeLabel,
  nodeLocation,
  nodeTooltip,
  parseAirJson,
  spanStartPosition,
  utf16LengthForUtf8Bytes,
} from './air-model';

/** Depths 0 and 1 (module + top-level declarations) start expanded;
 *  everything deeper starts collapsed. */
const EXPANDED_BELOW_DEPTH = 2;

/** Debounce for the save-triggered refresh, matching the house ~300 ms
 *  pattern (see `effects.ts`). */
const SAVE_REFRESH_DELAY_MS = 300;

// ─── View state ─────────────────────────────────────────────────────────────

type ViewState =
  /** Nothing shown yet, or the command ran without a `.bock` editor. */
  | { mode: 'empty'; message: string }
  /** A non-compiler problem (missing binary, crashed CLI, bad output). */
  | { mode: 'info'; message: string; tooltip?: string }
  /** The frontend rejected the file: message plus clickable diagnostics. */
  | {
      mode: 'error';
      message: string;
      diagnostics: AirDiagnostic[];
      uri: vscode.Uri;
    }
  /** A lowered AIR tree for `uri`. */
  | { mode: 'tree'; root: AirNode; uri: vscode.Uri };

type AirElement =
  | { type: 'node'; node: AirNode; depth: number; uri: vscode.Uri }
  | { type: 'info'; message: string; tooltip?: string; iconId: string }
  | { type: 'errorRoot' }
  | { type: 'diag'; diag: AirDiagnostic; uri: vscode.Uri };

// ─── TreeDataProvider ───────────────────────────────────────────────────────

class AirTreeProvider implements vscode.TreeDataProvider<AirElement> {
  private readonly emitter = new vscode.EventEmitter<AirElement | undefined>();
  readonly onDidChangeTreeData = this.emitter.event;

  private state: ViewState = {
    mode: 'empty',
    message: 'Open a .bock file and run “Bock: Show AIR Tree”.',
  };

  getState(): ViewState {
    return this.state;
  }

  setState(state: ViewState): void {
    this.state = state;
    this.emitter.fire(undefined);
  }

  getChildren(element?: AirElement): AirElement[] {
    if (!element) return this.rootElements();
    if (element.type === 'node') {
      return element.node.children.map((child) => ({
        type: 'node',
        node: child,
        depth: element.depth + 1,
        uri: element.uri,
      }));
    }
    if (element.type === 'errorRoot' && this.state.mode === 'error') {
      const { diagnostics, uri } = this.state;
      return diagnostics.map((diag) => ({ type: 'diag', diag, uri }));
    }
    return [];
  }

  private rootElements(): AirElement[] {
    switch (this.state.mode) {
      case 'empty':
        return [
          { type: 'info', message: this.state.message, iconId: 'info' },
        ];
      case 'info':
        return [
          {
            type: 'info',
            message: this.state.message,
            tooltip: this.state.tooltip,
            iconId: 'warning',
          },
        ];
      case 'error':
        return [{ type: 'errorRoot' }];
      case 'tree':
        return [
          { type: 'node', node: this.state.root, depth: 0, uri: this.state.uri },
        ];
    }
  }

  getTreeItem(element: AirElement): vscode.TreeItem {
    switch (element.type) {
      case 'info': {
        const item = new vscode.TreeItem(
          element.message,
          vscode.TreeItemCollapsibleState.None,
        );
        item.iconPath = new vscode.ThemeIcon(element.iconId);
        item.tooltip = element.tooltip ?? element.message;
        return item;
      }
      case 'errorRoot': {
        const state = this.state;
        const count = state.mode === 'error' ? state.diagnostics.length : 0;
        const item = new vscode.TreeItem(
          state.mode === 'error' ? state.message : 'frontend error',
          count > 0
            ? vscode.TreeItemCollapsibleState.Expanded
            : vscode.TreeItemCollapsibleState.None,
        );
        item.description =
          count > 0 ? `${count} ${count === 1 ? 'diagnostic' : 'diagnostics'}` : undefined;
        item.iconPath = new vscode.ThemeIcon(
          'error',
          new vscode.ThemeColor('errorForeground'),
        );
        item.tooltip =
          'The Bock frontend could not lower this file. Fix the errors below and save to refresh.';
        return item;
      }
      case 'diag': {
        const { diag } = element;
        const item = new vscode.TreeItem(
          diag.message,
          vscode.TreeItemCollapsibleState.None,
        );
        const loc = diag.span ? ` @${diag.span.line}:${diag.span.col}` : '';
        item.description = `${diag.code}${loc}`.trim();
        item.iconPath = new vscode.ThemeIcon(
          diag.severity === 'error' ? 'error' : 'warning',
        );
        item.tooltip = `${diag.severity || 'diagnostic'} ${diag.code}: ${diag.message}`;
        if (diag.span) {
          item.command = {
            command: 'bock.airView.revealSpan',
            title: 'Reveal in editor',
            arguments: [element.uri, diag.span],
          };
        }
        return item;
      }
      case 'node': {
        const { node, depth, uri } = element;
        const item = new vscode.TreeItem(
          nodeLabel(node),
          childCount(node) === 0
            ? vscode.TreeItemCollapsibleState.None
            : depth < EXPANDED_BELOW_DEPTH
              ? vscode.TreeItemCollapsibleState.Expanded
              : vscode.TreeItemCollapsibleState.Collapsed,
        );
        item.description = nodeLocation(node);
        item.tooltip = nodeTooltip(node);
        item.iconPath = new vscode.ThemeIcon(nodeIconId(node.kind));
        item.contextValue = 'airNode';
        item.command = {
          command: 'bock.airView.revealSpan',
          title: 'Reveal span in editor',
          arguments: [uri, node.span],
        };
        return item;
      }
    }
  }
}

// ─── Inspect process ────────────────────────────────────────────────────────

function execInspectAir(
  binary: string,
  cwd: string,
  file: string,
): Promise<{ stdout: string; stderr: string; failed: boolean }> {
  return new Promise((resolve) => {
    cp.execFile(
      binary,
      ['inspect', 'air', file, '--json'],
      // The JSON tree for a large file is sizeable; the frontend alone is
      // fast, so a wedged process should fail the view, not hang it.
      { cwd, maxBuffer: 64 * 1024 * 1024, timeout: 30_000 },
      (err, stdout, stderr) => {
        resolve({
          stdout: stdout ?? '',
          stderr: stderr ?? '',
          // Exit 1 with a JSON error object on stdout is an EXPECTED outcome
          // (frontend error); the parse result decides, not the exit code.
          failed: !!err,
        });
      },
    );
  });
}

// ─── Registration ───────────────────────────────────────────────────────────

/** Registers the `bock.airView` tree, `bock.showAir`, and the refresh and
 *  reveal commands. */
export function registerAirViewer(
  ctx: vscode.ExtensionContext,
  logChannel?: vscode.OutputChannel,
): void {
  const provider = new AirTreeProvider();
  const view = vscode.window.createTreeView('bock.airView', {
    treeDataProvider: provider,
    showCollapseAll: true,
  });
  ctx.subscriptions.push(view);

  /** The document the view currently reflects (tree or error state). */
  let shownUri: vscode.Uri | undefined;
  /** Monotonic run counter so a stale inspect result never clobbers a
   *  newer one (e.g. rapid save bursts or switching files mid-run). */
  let runSeq = 0;

  const log = (message: string): void => {
    logChannel?.appendLine(`[bock] air viewer: ${message}`);
  };

  async function runInspect(doc: vscode.TextDocument): Promise<void> {
    const seq = ++runSeq;

    const binary = findBockLspBinary();
    if (!binary) {
      shownUri = doc.uri;
      view.description = path.basename(doc.uri.fsPath);
      provider.setState({
        mode: 'info',
        message:
          'Could not find the `bock` binary on PATH — set `bock.lspPath` in your user settings or install the compiler.',
      });
      return;
    }

    // Inspect runs on the file on disk; flush the buffer first so the tree
    // matches what's on screen (mirrors the target-preview behaviour).
    if (doc.isDirty) await doc.save();

    const filePath = doc.uri.fsPath;
    const cwd =
      findProjectRoot(path.dirname(filePath), (p) => fs.existsSync(p), path.sep) ??
      path.dirname(filePath);

    const run = await execInspectAir(binary, cwd, filePath);
    if (seq !== runSeq) return; // a newer run superseded this one

    shownUri = doc.uri;
    view.description = path.basename(filePath);

    const result = parseAirJson(run.stdout);
    switch (result.kind) {
      case 'tree':
        provider.setState({ mode: 'tree', root: result.root, uri: doc.uri });
        return;
      case 'frontend-error':
        provider.setState({
          mode: 'error',
          message: result.message,
          diagnostics: result.diagnostics,
          uri: doc.uri,
        });
        return;
      case 'malformed': {
        // Spawn failure, timeout, or an incompatible `bock` without
        // `inspect air` — degrade to one informative node and log details.
        log(
          `\`bock inspect air\` produced unusable output for ${filePath} (${result.reason})`,
        );
        if (run.stderr.trim()) log(run.stderr.trim());
        provider.setState({
          mode: 'info',
          message: run.failed
            ? 'Running `bock inspect air` failed — is your `bock` binary up to date? See the Bock output channel.'
            : 'Unexpected `bock inspect air` output — see the Bock output channel.',
          tooltip: result.reason,
        });
        return;
      }
    }
  }

  // Debounced save-triggered refresh of whichever document the view shows.
  let saveTimer: NodeJS.Timeout | undefined;
  ctx.subscriptions.push(
    vscode.workspace.onDidSaveTextDocument((doc) => {
      if (!shownUri || doc.uri.toString() !== shownUri.toString()) return;
      if (saveTimer) clearTimeout(saveTimer);
      saveTimer = setTimeout(() => {
        saveTimer = undefined;
        void runInspect(doc);
      }, SAVE_REFRESH_DELAY_MS);
    }),
    {
      dispose: () => {
        if (saveTimer) clearTimeout(saveTimer);
      },
    },
  );

  ctx.subscriptions.push(
    vscode.commands.registerCommand('bock.showAir', async () => {
      const doc = vscode.window.activeTextEditor?.document;
      if (!doc || doc.uri.scheme !== 'file' || !doc.fileName.endsWith('.bock')) {
        shownUri = undefined;
        view.description = undefined;
        provider.setState({
          mode: 'empty',
          message: 'No active .bock editor — open a .bock file and rerun “Bock: Show AIR Tree”.',
        });
        await vscode.commands.executeCommand('bock.airView.focus');
        return;
      }
      await vscode.commands.executeCommand('bock.airView.focus');
      await runInspect(doc);
    }),

    vscode.commands.registerCommand('bock.airView.refresh', async () => {
      // Refresh the shown document if it's still open; otherwise behave
      // like `bock.showAir` on the active editor.
      const shown = shownUri?.toString();
      const doc = shown
        ? vscode.workspace.textDocuments.find((d) => d.uri.toString() === shown)
        : undefined;
      if (doc) {
        await runInspect(doc);
      } else {
        await vscode.commands.executeCommand('bock.showAir');
      }
    }),

    vscode.commands.registerCommand(
      'bock.airView.revealSpan',
      async (uri: vscode.Uri, span: AirSpan) => {
        const doc = await vscode.workspace.openTextDocument(uri);

        // Start from the 1-based line/col contract (NOT the byte offsets —
        // see air-model.ts); the line's own text converts the code-point
        // column to UTF-16 exactly.
        const rawLine = Math.max(0, span.line - 1);
        const line = Math.min(rawLine, Math.max(0, doc.lineCount - 1));
        const lineText = doc.lineAt(line).text;
        const startPos = spanStartPosition(
          { ...span, line: line + 1 },
          lineText,
        );
        const start = doc.validatePosition(
          new vscode.Position(startPos.line, startPos.character),
        );

        // Selection extent: measure the span's UTF-8 byte length forward
        // from the (correct) start, in UTF-16 units.
        const startOffset = doc.offsetAt(start);
        const length16 = utf16LengthForUtf8Bytes(
          doc.getText(),
          startOffset,
          Math.max(0, span.end - span.start),
        );
        const end = doc.positionAt(startOffset + length16);

        const editor = await vscode.window.showTextDocument(doc, {
          preserveFocus: false,
        });
        editor.selection = new vscode.Selection(start, end);
        editor.revealRange(
          new vscode.Range(start, end),
          vscode.TextEditorRevealType.InCenterIfOutsideViewport,
        );
      },
    ),
  );
}
