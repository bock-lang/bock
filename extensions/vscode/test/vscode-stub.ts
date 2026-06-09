// Minimal stand-in for the `vscode` module so pure-logic extension code can
// be unit-tested in plain Node, without launching an Extension Host.
//
// The real `vscode` module is only resolvable inside the VS Code extension
// host. The unit tests here exercise pure parsers (`extractEffects`,
// `parseProjectEffects`) and helpers (`escapeHtml`) that import the source
// modules — those modules carry a top-level `import * as vscode from 'vscode'`
// purely for type annotations, but a handful of runtime constructors
// (`vscode.Position`, `vscode.Uri.*`) are referenced inside the functions
// under test. This stub supplies exactly those, and nothing more.
//
// Wired in via `test/register-vscode.ts`, which intercepts `require('vscode')`
// at module-resolution time and returns this object.

/** Mirror of `vscode.Position`: a zero-based (line, character) coordinate. */
export class Position {
  constructor(
    public readonly line: number,
    public readonly character: number,
  ) {}
}

/** Mirror of `vscode.Range`: an inclusive start / exclusive end pair. */
export class Range {
  constructor(
    public readonly start: Position,
    public readonly end: Position,
  ) {}
}

/**
 * Minimal `vscode.Uri` stand-in. Only the members touched by the
 * functions under test are modelled: `fsPath`, `toString()`, and the
 * static `file` / `joinPath` constructors.
 */
export class Uri {
  private constructor(
    public readonly scheme: string,
    public readonly fsPath: string,
  ) {}

  static file(fsPath: string): Uri {
    return new Uri('file', fsPath);
  }

  static joinPath(base: Uri, ...segments: string[]): Uri {
    const joined = [base.fsPath.replace(/\/+$/, ''), ...segments].join('/');
    return new Uri(base.scheme, joined);
  }

  toString(): string {
    return `${this.scheme}://${this.fsPath}`;
  }
}

/**
 * Minimal `vscode.window` stand-in. `VocabService.load` calls
 * `showErrorMessage` on its degrade-to-empty path, so the headless vocab
 * test needs the call to be a harmless no-op rather than `undefined`.
 * Returns a resolved promise to mirror the real (Thenable) signature.
 */
export const window = {
  showErrorMessage(_message: string): Promise<undefined> {
    return Promise.resolve(undefined);
  },
  showWarningMessage(_message: string): Promise<undefined> {
    return Promise.resolve(undefined);
  },
  showInformationMessage(_message: string): Promise<undefined> {
    return Promise.resolve(undefined);
  },
};

/**
 * Mirror of `vscode.CodeActionKind` — only the `QuickFix` constant plus
 * the `value` field the real class exposes.
 */
export class CodeActionKind {
  static readonly QuickFix = new CodeActionKind('quickfix');
  private constructor(public readonly value: string) {}
}

/**
 * Mirror of `vscode.CodeAction`: title/kind plus the mutable fields the
 * quick-fix provider assigns (`edit`, `diagnostics`, `isPreferred`).
 */
export class CodeAction {
  edit?: WorkspaceEdit;
  diagnostics?: unknown[];
  isPreferred?: boolean;
  command?: unknown;
  constructor(
    public readonly title: string,
    public readonly kind?: CodeActionKind,
  ) {}
}

/**
 * Minimal `vscode.WorkspaceEdit` stand-in. Only `replace` is modelled;
 * the recorded operations are exposed via `replacements` so headless
 * tests can assert exactly what an action would change.
 */
export class WorkspaceEdit {
  readonly replacements: Array<{
    uri: Uri;
    range: Range;
    newText: string;
  }> = [];

  replace(uri: Uri, range: Range, newText: string): void {
    this.replacements.push({ uri, range, newText });
  }
}

/**
 * Minimal `vscode.languages` stand-in: provider registration is a no-op
 * disposable, which is all `register*` wiring functions need headlessly.
 */
export const languages = {
  registerCodeActionsProvider(
    _selector: unknown,
    _provider: unknown,
    _metadata?: unknown,
  ): { dispose(): void } {
    return { dispose() {} };
  },
};
