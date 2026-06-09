// Starts the Bock language server as a child process over stdio, wires
// it to the VS Code language client, and returns the started client so
// feature modules can add request/notification handlers.
//
// The `LspController` owns the *current* client so the `bock.restartLsp`
// command can tear it down and spin up a fresh one (e.g. after rebuilding
// the compiler) without reloading the whole window.

import * as vscode from 'vscode';
import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
} from 'vscode-languageclient/node';

const CHANNEL = 'Bock Language Server';

/**
 * Owns the lifecycle of the single live {@link LanguageClient}. Feature
 * modules capture the client returned by {@link start} at activation, but the
 * controller can transparently {@link restart} the underlying server process
 * — handy when the contributor has just rebuilt the `bock` compiler.
 */
export class LspController {
  private current: LanguageClient | undefined;

  private constructor() {}

  /**
   * Creates the controller and performs the initial start. Registers a single
   * disposable on the extension context that always stops whichever client is
   * current at shutdown time, so a post-restart client is cleaned up too.
   */
  static async create(
    ctx: vscode.ExtensionContext,
  ): Promise<{ controller: LspController; client: LanguageClient | undefined }> {
    const controller = new LspController();
    ctx.subscriptions.push({
      dispose: () => {
        void controller.current?.stop();
      },
    });
    const client = await controller.start();
    return { controller, client };
  }

  /** The currently running client, or `undefined` if none is active. */
  get client(): LanguageClient | undefined {
    return this.current;
  }

  /**
   * Stops the current client (if any) and starts a fresh one. Returns the new
   * client, or `undefined` if the binary could not be found or failed to start.
   */
  async restart(): Promise<LanguageClient | undefined> {
    if (this.current) {
      try {
        await this.current.stop();
      } catch {
        // A wedged client may reject on stop(); proceed to start a new one
        // regardless rather than leaving the user with no server.
      }
      this.current = undefined;
    }
    return this.start();
  }

  private async start(): Promise<LanguageClient | undefined> {
    this.current = await startLspClient();
    return this.current;
  }
}

/**
 * Builds, starts, and returns a single Bock {@link LanguageClient}. Resolves to
 * `undefined` (after surfacing a warning) when the binary is missing or the
 * server fails to start, so callers can degrade gracefully.
 */
export async function startLspClient(): Promise<LanguageClient | undefined> {
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
    const resolved = resolveConfiguredLspPath(configured);
    if (fs.existsSync(resolved)) return resolved;
    // An explicit override that points nowhere is almost always a typo or a
    // stale path; name it so the user can fix it, then fall through to the
    // PATH search rather than silently ignoring the misconfiguration.
    vscode.window.showWarningMessage(
      `Bock: configured \`bock.lspPath\` does not exist: ${resolved}. ` +
        'Falling back to searching PATH for `bock`.',
    );
  }

  const isWin = process.platform === 'win32';
  const exe = isWin ? 'bock.exe' : 'bock';

  const envPath = process.env.PATH ?? '';
  for (const dir of envPath.split(path.delimiter)) {
    if (!dir) continue;
    const candidate = path.join(dir, exe);
    if (fs.existsSync(candidate)) return candidate;
  }

  // SECURITY: the extension deliberately does NOT auto-discover a `bock` binary
  // inside opened workspace folders (e.g. `target/{release,debug}/bock`).
  // Spawning an executable found in workspace *content* is an arbitrary-code-
  // execution vector — opening/cloning a hostile repo would silently run its
  // bundled binary. The server binary is resolved only from `PATH` or the
  // machine-scoped, user-controlled `bock.lspPath` setting (which a malicious
  // workspace cannot set — see `"scope": "machine"` in package.json). A
  // contributor building from source sets `bock.lspPath` explicitly in their
  // *user* settings; `${workspaceFolder}` and `~` are expanded for them.
  return undefined;
}

/**
 * Resolve the user-controlled `bock.lspPath` setting, expanding a leading `~`
 * to the home directory and the `${workspaceFolder}` variable to the first
 * workspace folder. Only this explicitly-configured setting is expanded — the
 * extension never derives a server binary from workspace *content*, so a hostile
 * workspace cannot inject a path here (the setting is machine-scoped).
 */
function resolveConfiguredLspPath(raw: string): string {
  let p = raw;
  const folder = vscode.workspace.workspaceFolders?.[0]?.uri.fsPath;
  if (folder) {
    p = p.replace(/\$\{workspaceFolder\}/g, folder);
  }
  if (p === '~' || p.startsWith('~/') || p.startsWith('~\\')) {
    p = path.join(os.homedir(), p.slice(1));
  }
  return p;
}
