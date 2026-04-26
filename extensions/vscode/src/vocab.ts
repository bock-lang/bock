// Loads and exposes the compiler-emitted vocabulary (assets/vocab.json).
// Features query this service for keyword docs, diagnostic descriptions,
// annotation purpose strings, stdlib symbol signatures, and spec references
// — giving the extension a single source of truth that mirrors the compiler.

import * as vscode from 'vscode';
import * as fs from 'fs/promises';
import * as path from 'path';
import {
  Vocab,
  Annotation,
  DiagnosticCode,
  Symbol,
  Keyword,
  Operator,
} from './shared/types';

type ChangeHandler = (vocab: Vocab) => void;

export class VocabService {
  private vocab: Vocab;
  private readonly handlers: ChangeHandler[] = [];
  private readonly vocabPath: string;

  private constructor(vocab: Vocab, vocabPath: string) {
    this.vocab = vocab;
    this.vocabPath = vocabPath;
  }

  static async load(ctx: vscode.ExtensionContext): Promise<VocabService> {
    const vocabPath = path.join(ctx.extensionPath, 'assets', 'vocab.json');
    const vocab = await readVocab(vocabPath);
    return new VocabService(vocab, vocabPath);
  }

  get(): Vocab {
    return this.vocab;
  }

  get path(): string {
    return this.vocabPath;
  }

  async reload(): Promise<void> {
    this.vocab = await readVocab(this.vocabPath);
    for (const handler of this.handlers) {
      handler(this.vocab);
    }
  }

  onDidChange(fn: ChangeHandler): vscode.Disposable {
    this.handlers.push(fn);
    return {
      dispose: () => {
        const i = this.handlers.indexOf(fn);
        if (i >= 0) this.handlers.splice(i, 1);
      },
    };
  }

  getDiagnostic(code: string): DiagnosticCode | undefined {
    return this.vocab.diagnostics.codes.find((d) => d.code === code);
  }

  getAnnotation(name: string): Annotation | undefined {
    const bare = name.startsWith('@') ? name.slice(1) : name;
    return this.vocab.language.annotations.find(
      (a) => a.name === bare || a.name === `@${bare}`,
    );
  }

  getKeyword(name: string): Keyword | undefined {
    return this.vocab.language.keywords.find((k) => k.name === name);
  }

  getOperator(symbol: string): Operator | undefined {
    return this.vocab.language.operators.find((o) => o.symbol === symbol);
  }

  getSpecRef(symbol: string): string | undefined {
    const { language } = this.vocab;
    const kw = language.keywords.find((k) => k.name === symbol);
    if (kw?.spec_ref) return kw.spec_ref;
    const op = language.operators.find((o) => o.symbol === symbol);
    if (op?.spec_ref) return op.spec_ref;
    const ann = this.getAnnotation(symbol);
    if (ann?.spec_ref) return ann.spec_ref;
    const prim = language.primitive_types.find((p) => p.name === symbol);
    if (prim?.spec_ref) return prim.spec_ref;
    return undefined;
  }

  getStdlibSymbol(module: string, name: string): Symbol | undefined {
    const mod = this.vocab.stdlib.modules.find((m) => m.path === module);
    if (!mod) return undefined;
    return (
      mod.functions.find((s) => s.name === name) ??
      mod.types.find((s) => s.name === name) ??
      mod.traits.find((s) => s.name === name) ??
      mod.effects.find((s) => s.name === name)
    );
  }

  getBuiltinMethods(receiver: string): string[] {
    const group = this.vocab.stdlib.builtin_methods.find(
      (g) => g.receiver === receiver,
    );
    return group?.methods ?? [];
  }
}

async function readVocab(vocabPath: string): Promise<Vocab> {
  try {
    const raw = await fs.readFile(vocabPath, 'utf8');
    return JSON.parse(raw) as Vocab;
  } catch (err) {
    throw new Error(
      `Failed to load Bock vocabulary from ${vocabPath}: ${(err as Error).message}. ` +
        `Run scripts/sync-vscode-assets.sh to regenerate.`,
    );
  }
}

export function watchVocab(
  ctx: vscode.ExtensionContext,
  vocab: VocabService,
): void {
  const pattern = new vscode.RelativePattern(
    path.dirname(vocab.path),
    'vocab.json',
  );
  const watcher = vscode.workspace.createFileSystemWatcher(pattern);
  const reload = async () => {
    try {
      await vocab.reload();
      console.log('[bock] vocab reloaded');
    } catch (err) {
      console.error('[bock] vocab reload failed:', err);
    }
  };
  watcher.onDidChange(reload);
  watcher.onDidCreate(reload);
  ctx.subscriptions.push(watcher);
}
