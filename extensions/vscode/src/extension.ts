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
import { VocabService, watchVocab } from './vocab';
import { startLspClient } from './lsp';
import { registerHover } from './features/hover';
import { registerErrors } from './features/errors';
import { registerAnnotations } from './features/annotations';
import { registerEffects } from './features/effects';
import { registerDecisions } from './features/decisions';
import { registerSpecPanel } from './features/spec-panel';

export async function activate(ctx: vscode.ExtensionContext): Promise<void> {
  const vocab = await VocabService.load(ctx);
  console.log(`[bock] vocabulary v${vocab.get().version} loaded`);

  const client = await startLspClient(ctx);

  registerHover(ctx, vocab, client);
  registerErrors(ctx, vocab, client);
  registerAnnotations(ctx, vocab, client);
  registerEffects(ctx, vocab, client);
  registerDecisions(ctx, vocab);
  registerSpecPanel(ctx, vocab);

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
