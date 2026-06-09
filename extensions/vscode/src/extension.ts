// Entry point for the Bock VS Code extension.
//
// Responsibilities:
//   1. Load the compiler-emitted vocabulary (assets/vocab.json).
//   2. Start the LSP client (spawns `bock lsp` over stdio).
//   3. Register every feature module — hover, errors, annotations,
//      effects, decisions, spec panel — plus the `bock.refreshVocab`
//      command, the vocab watcher, and the `workspaceHasBockFiles`
//      context key.
//
// Activation degrades gracefully: neither `VocabService.load` nor
// `startLspClient` rejects, so a missing/corrupt `vocab.json` or a
// broken `bock` binary leaves activation intact — features still
// register (over an empty vocab and/or an `undefined` client), and
// syntax highlighting and panels keep working. This is the behaviour
// the README promises.

import * as vscode from 'vscode';
import { VocabService, watchVocab, setVocabLogChannel } from './vocab';
import { LspController } from './lsp';
import { registerHover } from './features/hover';
import { registerErrors } from './features/errors';
import { registerAnnotations } from './features/annotations';
import { registerEffects } from './features/effects';
import { registerDecisions } from './features/decisions';
import { registerSpecPanel } from './features/spec-panel';
import { registerTargetPreview } from './features/target-preview';
import { registerStrictness } from './features/strictness';
import { registerQuickFixes } from './features/quick-fixes';
import { registerSemanticTokens } from './features/semantic-tokens';
import { registerAirViewer } from './features/air-viewer';

// The extension's own diagnostics go to a "Bock" output channel, created in
// `activate` and shared with `vocab.ts` via `setVocabLogChannel`. It is
// distinct from the LSP server's "Bock Language Server" channel.
let logChannel: vscode.OutputChannel | undefined;

/** Appends a timestamped `[bock]`-prefixed line to the Bock output channel. */
function log(message: string): void {
  logChannel?.appendLine(`[bock] ${message}`);
}

export async function activate(ctx: vscode.ExtensionContext): Promise<void> {
  logChannel = vscode.window.createOutputChannel('Bock');
  ctx.subscriptions.push(logChannel);
  setVocabLogChannel(logChannel);

  const vocab = await VocabService.load(ctx);
  log(`vocabulary v${vocab.get().version} loaded`);

  const { controller, client } = await LspController.create(ctx);

  registerHover(ctx, vocab, client);
  registerErrors(ctx, vocab, client);
  registerAnnotations(ctx, vocab, client);
  registerEffects(ctx, vocab, client);
  registerDecisions(ctx, vocab);
  registerSpecPanel(ctx, vocab);
  registerTargetPreview(ctx, logChannel);
  registerStrictness(ctx);
  registerQuickFixes(ctx);
  registerSemanticTokens(ctx, vocab);
  registerAirViewer(ctx, logChannel);

  ctx.subscriptions.push(
    vscode.commands.registerCommand('bock.refreshVocab', async () => {
      try {
        await vocab.reload();
        vscode.window.showInformationMessage(
          `Bock: vocabulary reloaded (v${vocab.get().version}).`,
        );
      } catch (err) {
        vscode.window.showErrorMessage(
          `Bock: vocab reload failed — ${(err as Error).message}`,
        );
      }
    }),
    vscode.commands.registerCommand('bock.restartLsp', async () => {
      log('restarting language server…');
      const restarted = await controller.restart();
      if (restarted) {
        vscode.window.showInformationMessage(
          'Bock: language server restarted.',
        );
        log('language server restarted');
      } else {
        vscode.window.showWarningMessage(
          'Bock: language server did not restart — see the Bock output channel for details.',
        );
        log('language server restart failed');
      }
    }),
  );

  watchVocab(ctx, vocab);

  await vscode.commands.executeCommand(
    'setContext',
    'workspaceHasBockFiles',
    true,
  );
}

export function deactivate(): void {
  // LSP client shutdown is registered as a disposable in startLspClient.
}
