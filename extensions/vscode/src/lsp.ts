// Starts the Bock language server as a child process over stdio, wires
// it to the VS Code language client, and returns the started client so
// feature modules can add request/notification handlers.

import * as vscode from 'vscode';
import * as fs from 'fs';
import * as path from 'path';
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
} from 'vscode-languageclient/node';

const CHANNEL = 'Bock Language Server';

export async function startLspClient(
  ctx: vscode.ExtensionContext,
): Promise<LanguageClient | undefined> {
  const serverPath = findBockLspBinary();
  if (!serverPath) {
    vscode.window.showWarningMessage(
      'Bock: could not find `bock` binary on PATH. Set `bock.lspPath` or install the compiler to enable language features.',
    );
    return undefined;
  }

  const serverOptions: ServerOptions = {
    run: { command: serverPath, args: ['lsp'], transport: TransportKind.stdio },
    debug: {
      command: serverPath,
      args: ['lsp'],
      transport: TransportKind.stdio,
    },
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: 'file', language: 'bock' }],
    synchronize: {
      fileEvents: vscode.workspace.createFileSystemWatcher('**/*.bock'),
    },
    outputChannelName: CHANNEL,
  };

  const client = new LanguageClient(
    'bock-lsp',
    'Bock Language Server',
    serverOptions,
    clientOptions,
  );

  ctx.subscriptions.push({ dispose: () => client.stop() });

  try {
    await client.start();
  } catch (err) {
    // A present-but-broken `bock` binary (wrong version, missing `lsp`
    // subcommand, immediate crash) rejects here. Swallow it so the rest of
    // the extension — commands, panels, syntax highlighting — still activates.
    // Best-effort stop to release any half-spawned process; ignore failures.
    try {
      await client.stop();
    } catch {
      // The client never fully started; stop() may itself reject. Nothing to do.
    }
    vscode.window.showWarningMessage(
      `Bock: language server failed to start — ${(err as Error).message}. ` +
        'Language features are disabled; syntax highlighting and panels still work.',
    );
    return undefined;
  }

  return client;
}

function findBockLspBinary(): string | undefined {
  const configured = vscode.workspace
    .getConfiguration('bock')
    .get<string>('lspPath', '')
    .trim();
  if (configured) {
    if (fs.existsSync(configured)) return configured;
    // An explicit override that points nowhere is almost always a typo or a
    // stale path; name it so the user can fix it, then fall through to the
    // PATH search rather than silently ignoring the misconfiguration.
    vscode.window.showWarningMessage(
      `Bock: configured \`bock.lspPath\` does not exist: ${configured}. ` +
        'Falling back to searching PATH for `bock`.',
    );
  }

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
