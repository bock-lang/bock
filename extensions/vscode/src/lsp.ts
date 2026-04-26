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
  await client.start();
  return client;
}

function findBockLspBinary(): string | undefined {
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
