// Strictness status-bar picker (`bock.setStrictness`).
//
// Shows the current §1.4 strictness level of the active project's
// `bock.project` in the status bar ("Bock: development"); clicking it (or
// running the command) opens a QuickPick over the sketch → development →
// production ladder and rewrites `bock.project` in place, preserving the
// file's formatting and comments via the pure line-level editor in
// `strictness-toml.ts`. A FileSystemWatcher keeps the item live when the
// file changes on disk; when no `bock.project` is visible the item hides.
//
// Which project? The one owning the active editor's file (nearest
// `bock.project` walking up) when there is one; otherwise the first
// `bock.project` discoverable in the workspace, so single-project windows
// show their level even with no editor open.

import * as vscode from 'vscode';
import * as fs from 'fs';
import * as path from 'path';
import { findProjectRoot } from './preview-paths';
import {
  STRICTNESS_LEVELS,
  StrictnessLevel,
  getStrictness,
  setStrictness,
} from './strictness-toml';

/** One-line descriptions of each level, condensed from spec §1.4. */
const LEVEL_DESCRIPTIONS: Record<StrictnessLevel, string> = {
  sketch:
    'Inferred wide types, minimal context, unrestricted mutation, AI decisions auto-resolved',
  development:
    'Inferred types with warnings, module-level context, warn on broad mutation, AI decisions logged',
  production:
    'Fully resolved types, full context, explicit mutation only, AI decisions must be pinned',
};

/** Registers the status-bar item, its watcher, and `bock.setStrictness`. */
export function registerStrictness(ctx: vscode.ExtensionContext): void {
  const item = vscode.window.createStatusBarItem(
    'bock.strictness',
    vscode.StatusBarAlignment.Left,
    0,
  );
  item.name = 'Bock Strictness';
  item.command = 'bock.setStrictness';
  ctx.subscriptions.push(item);

  const refresh = (): void => {
    void updateItem(item);
  };

  // Live updates: the manifest itself changing on disk (any project in the
  // workspace), and the active editor moving between projects.
  const watcher = vscode.workspace.createFileSystemWatcher('**/bock.project');
  watcher.onDidChange(refresh);
  watcher.onDidCreate(refresh);
  watcher.onDidDelete(refresh);
  ctx.subscriptions.push(
    watcher,
    vscode.window.onDidChangeActiveTextEditor(refresh),
    vscode.workspace.onDidChangeWorkspaceFolders(refresh),
    vscode.commands.registerCommand('bock.setStrictness', async () => {
      await pickStrictness(item);
    }),
  );

  refresh();
}

/**
 * The `bock.project` governing the active editor's file, falling back to the
 * first one visible in the workspace. `undefined` when neither exists.
 */
async function currentProjectFile(): Promise<string | undefined> {
  const active = vscode.window.activeTextEditor?.document;
  if (active && active.uri.scheme === 'file') {
    const root = findProjectRoot(
      path.dirname(active.uri.fsPath),
      (p) => fs.existsSync(p),
      path.sep,
    );
    if (root) return path.join(root, 'bock.project');
  }
  const found = await vscode.workspace.findFiles(
    '**/bock.project',
    '**/node_modules/**',
    1,
  );
  return found.length > 0 ? found[0].fsPath : undefined;
}

async function updateItem(item: vscode.StatusBarItem): Promise<void> {
  const projectFile = await currentProjectFile();
  if (!projectFile) {
    item.hide();
    return;
  }
  let level: StrictnessLevel;
  try {
    level = getStrictness(await fs.promises.readFile(projectFile, 'utf8'));
  } catch {
    // Manifest vanished between discovery and read (or is unreadable):
    // treat as "no project" rather than showing a stale level.
    item.hide();
    return;
  }
  item.text = `Bock: ${level}`;
  item.tooltip = new vscode.MarkdownString(
    `Bock strictness for \`${projectFile}\`\n\n` +
      `**${level}** — ${LEVEL_DESCRIPTIONS[level]}\n\n` +
      'Click to change (spec §1.4).',
  );
  item.show();
}

async function pickStrictness(item: vscode.StatusBarItem): Promise<void> {
  const projectFile = await currentProjectFile();
  if (!projectFile) {
    void vscode.window.showWarningMessage(
      'Bock: no `bock.project` found in this workspace — nothing to set strictness on.',
    );
    return;
  }

  let text: string;
  try {
    text = await fs.promises.readFile(projectFile, 'utf8');
  } catch (err) {
    void vscode.window.showErrorMessage(
      `Bock: could not read ${projectFile} — ${(err as Error).message}`,
    );
    return;
  }
  const current = getStrictness(text);

  const picked = await vscode.window.showQuickPick(
    STRICTNESS_LEVELS.map((level) => ({
      label: level,
      description: level === current ? '(current)' : '',
      detail: LEVEL_DESCRIPTIONS[level],
    })),
    {
      placeHolder: `Bock: set strictness for ${path.basename(
        path.dirname(projectFile),
      )} (currently ${current})`,
    },
  );
  if (!picked) return;
  const level = picked.label as StrictnessLevel;
  if (level === current) return;

  const updated = setStrictness(text, level);
  try {
    await fs.promises.writeFile(projectFile, updated, 'utf8');
  } catch (err) {
    void vscode.window.showErrorMessage(
      `Bock: could not write ${projectFile} — ${(err as Error).message}`,
    );
    return;
  }
  void vscode.window.showInformationMessage(
    `Bock: strictness set to ${level} in ${projectFile}.`,
  );
  await updateItem(item);
}
