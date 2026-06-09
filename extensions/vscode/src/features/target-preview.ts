// Target preview (`bock.showTargetPreview`): build the active `.bock` file's
// project with `bock build -t <target> --source-only` and open the emitted
// file for the active module beside the editor, as a plain text document —
// VS Code's native syntax highlighting for the target language applies.
//
// All path reasoning (project-root discovery, source → emitted-file mapping)
// lives in the pure, headless-testable `preview-paths.ts`; this module owns
// the editor/UI/process side. The `bock` binary is resolved exclusively via
// `findBockLspBinary` from `lsp.ts` (PATH or the machine-scoped
// `bock.lspPath` setting) — never from workspace contents (see the SECURITY
// note in `lsp.ts`).

import * as vscode from 'vscode';
import * as cp from 'child_process';
import * as fs from 'fs';
import * as path from 'path';
import { findBockLspBinary } from '../lsp';
import {
  TARGETS,
  Target,
  buildDirFor,
  findProjectRoot,
  parseModuleDecl,
  resolveEmittedFile,
} from './preview-paths';

/** Human-readable QuickPick descriptions per target. */
const TARGET_DESCRIPTIONS: Record<Target, string> = {
  js: 'JavaScript (ESM)',
  ts: 'TypeScript',
  python: 'Python',
  rust: 'Rust',
  go: 'Go',
};

/** Sentinel QuickPick entry that previews every target in turn. */
const ALL_TARGETS_LABEL = 'All targets';

interface BuildOutcome {
  target: Target;
  ok: boolean;
  /** Combined stdout + stderr, for the output channel on failure. */
  output: string;
  /** Absolute path of the emitted file for the active module, when found. */
  emittedFile?: string;
}

/** Registers the `bock.showTargetPreview` command. */
export function registerTargetPreview(
  ctx: vscode.ExtensionContext,
  logChannel?: vscode.OutputChannel,
): void {
  ctx.subscriptions.push(
    vscode.commands.registerCommand('bock.showTargetPreview', async () => {
      await showTargetPreview(logChannel);
    }),
  );
}

async function showTargetPreview(
  logChannel?: vscode.OutputChannel,
): Promise<void> {
  const editor = vscode.window.activeTextEditor;
  const doc = editor?.document;
  if (!doc || doc.uri.scheme !== 'file' || !doc.fileName.endsWith('.bock')) {
    void vscode.window.showWarningMessage(
      'Bock: open a .bock file to preview its transpiled output.',
    );
    return;
  }

  const projectRoot = findProjectRoot(
    path.dirname(doc.uri.fsPath),
    (p) => fs.existsSync(p),
    path.sep,
  );
  if (!projectRoot) {
    void vscode.window.showErrorMessage(
      'Bock: no `bock.project` found in this file’s directory or any parent — the target preview needs a project root to build from.',
    );
    return;
  }

  const picked = await vscode.window.showQuickPick(
    [
      ...TARGETS.map((t) => ({
        label: t,
        description: TARGET_DESCRIPTIONS[t],
      })),
      { label: ALL_TARGETS_LABEL, description: 'Preview every target in turn' },
    ],
    { placeHolder: 'Bock: preview transpiled output for which target?' },
  );
  if (!picked) return;
  const targets: readonly Target[] =
    picked.label === ALL_TARGETS_LABEL ? TARGETS : [picked.label as Target];

  const binary = findBockLspBinary();
  if (!binary) {
    void vscode.window.showErrorMessage(
      'Bock: could not find the `bock` binary on PATH. Set `bock.lspPath` in your user settings or install the compiler.',
    );
    return;
  }

  // Save the active document first so the preview reflects what's on screen.
  if (doc.isDirty) await doc.save();

  const relSource = path.relative(projectRoot, doc.uri.fsPath);
  const modulePath = parseModuleDecl(doc.getText());

  const outcomes = await vscode.window.withProgress(
    {
      location: vscode.ProgressLocation.Notification,
      title: 'Bock: building target preview',
      cancellable: false,
    },
    async (progress) => {
      const results: BuildOutcome[] = [];
      for (const target of targets) {
        progress.report({ message: `${target}…` });
        // Always rebuild: an emitted tree left by an earlier build may be
        // stale relative to the current source, so it is never trusted.
        results.push(
          await buildOne(binary, projectRoot, target, relSource, modulePath),
        );
      }
      return results;
    },
  );

  const failures = outcomes.filter((o) => !o.ok);
  for (const f of failures) {
    logChannel?.appendLine(
      `[bock] target preview: \`bock build -t ${f.target} --source-only\` failed in ${projectRoot}:`,
    );
    logChannel?.appendLine(f.output.trimEnd());
  }

  const missing = outcomes.filter((o) => o.ok && !o.emittedFile);
  for (const m of missing) {
    logChannel?.appendLine(
      `[bock] target preview: build succeeded for ${m.target} but no emitted file matched ${relSource}` +
        (modulePath ? ` (module ${modulePath})` : '') +
        ` under ${buildDirFor(m.target)}/`,
    );
  }

  // Open every preview we did get, beside the source editor.
  for (const o of outcomes) {
    if (!o.emittedFile) continue;
    const opened = await vscode.workspace.openTextDocument(
      vscode.Uri.file(o.emittedFile),
    );
    await vscode.window.showTextDocument(opened, {
      viewColumn: vscode.ViewColumn.Beside,
      preview: false,
      preserveFocus: targets.length > 1,
    });
  }

  if (failures.length > 0) {
    const detail = failures.map((f) => f.target).join(', ');
    const choice = await vscode.window.showErrorMessage(
      `Bock: build failed for ${detail} — see the Bock output channel for the compiler output.`,
      'Show Output',
    );
    if (choice === 'Show Output') logChannel?.show(true);
  } else if (missing.length > 0) {
    const detail = missing.map((m) => m.target).join(', ');
    void vscode.window.showWarningMessage(
      `Bock: build succeeded for ${detail} but no emitted file was found for ${relSource}. ` +
        'Is this file part of the project build? See the Bock output channel for the paths that were checked.',
    );
  }
}

/** Run one `bock build -t <target> --source-only` and map the active file. */
async function buildOne(
  binary: string,
  projectRoot: string,
  target: Target,
  relSource: string,
  modulePath: string | undefined,
): Promise<BuildOutcome> {
  const run = await execBuild(binary, projectRoot, target);
  if (!run.ok) {
    return { target, ok: false, output: run.output };
  }

  const buildDir = path.join(projectRoot, ...buildDirFor(target).split('/'));
  let listing: string[];
  try {
    listing = (
      await fs.promises.readdir(buildDir, { recursive: true })
    ).map(String);
  } catch {
    // A clean build with no emitted dir would be a compiler bug; degrade to
    // "no emitted file found" rather than crash the command.
    return { target, ok: true, output: run.output };
  }

  const rel = resolveEmittedFile(relSource, target, listing, modulePath);
  return {
    target,
    ok: true,
    output: run.output,
    emittedFile: rel === undefined ? undefined : path.join(buildDir, rel),
  };
}

function execBuild(
  binary: string,
  cwd: string,
  target: Target,
): Promise<{ ok: boolean; output: string }> {
  return new Promise((resolve) => {
    cp.execFile(
      binary,
      ['build', '-t', target, '--source-only'],
      // Generous but bounded: a wedged compiler should fail the preview,
      // not pin a progress notification open forever.
      { cwd, maxBuffer: 16 * 1024 * 1024, timeout: 120_000 },
      (err, stdout, stderr) => {
        const output = `${stdout ?? ''}${stderr ?? ''}`;
        resolve({ ok: !err, output });
      },
    );
  });
}
