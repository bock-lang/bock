// Unit tests for the Bock quick-fix builders (src/features/quick-fixes-logic.ts)
// and the CodeActionProvider adapter (src/features/quick-fixes.ts — importable
// headlessly because it depends only on the stubbed `vscode` module, never on
// vscode-languageclient).
//
// Every diagnostic message used as a fixture below is the EXACT text emitted
// by the compiler, verified by running `bock check` against the matching
// source fixture. The format origins:
//
//   E4013 (general)  compiler/crates/bock-types/src/checker.rs:4333-4340
//                    "no method `{m}` on `{ty}`" + note "did you mean `{s}`?"
//   E4013 (Map/DQ22) compiler/crates/bock-types/src/checker.rs:2619-2630
//                    "`contains` is not a method on `Map`; did you mean
//                     `contains_key`?" (suggestion inline in the message)
//   E4014            compiler/crates/bock-types/src/checker.rs:1156-1172
//                    "`use {path}` is not a v1 import form: a `use` must name
//                     what it imports with a brace-list or a wildcard"
//   E5004            compiler/crates/bock-types/src/ownership.rs:257-274
//                    "cannot call `{m}` on `{recv}`: it mutates the list in
//                     place and requires a `mut` receiver"
//   W1001            compiler/crates/bock-air/src/resolve.rs:1370-1373
//                    "unused import `{local}`"
//
// Compiler notes reach the editor as DiagnosticRelatedInformation messages
// prefixed with "note: " (compiler/crates/bock-lsp/src/diagnostics.rs:68-76),
// which is why suggestion notes are passed via `relatedMessages`.

import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import {
  buildQuickFixes,
  buildQuickFixesForAll,
  normalizeDiagnosticCode,
  type QuickFixInput,
  type QuickFixRange,
} from '../src/features/quick-fixes-logic';
import { BockQuickFixProvider } from '../src/features/quick-fixes';
import {
  Position,
  Range,
  Uri,
  WorkspaceEdit as StubWorkspaceEdit,
} from './vscode-stub';

// ─── Fixture helpers ────────────────────────────────────────────────────────

function range(
  startLine: number,
  startChar: number,
  endLine: number,
  endChar: number,
): QuickFixRange {
  return { startLine, startChar, endLine, endChar };
}

/** Identity helper that gives fixture literals the QuickFixInput type. */
function input(i: QuickFixInput): QuickFixInput {
  return i;
}

// Mirrors /tmp fixture `e4013.bock`, where `bock check` reported the
// diagnostic at 5:14 (1-based) on the method-name span `lenth`.
const E4013_DOC = [
  'module e4013fix',
  '',
  'fn main() -> Int {',
  '  let xs = [1, 2, 3]',
  '  let n = xs.lenth()',
  '  n',
  '}',
  '',
].join('\n');
const E4013_MESSAGE = 'no method `lenth` on `List[Int]`';
const E4013_NOTE = 'note: did you mean `length`?';
const E4013_RANGE = range(4, 13, 4, 18);

// Mirrors /tmp fixture `e5004.bock`; `bock check` reported 5:3 on the
// receiver identifier span `xs`.
const E5004_DOC = [
  'module e5004fix',
  '',
  'fn main() -> Int {',
  '  let xs = [1, 2]',
  '  xs.push(3)',
  '  xs.length()',
  '}',
  '',
].join('\n');
const E5004_MESSAGE =
  'cannot call `push` on `xs`: it mutates the list in place and requires a `mut` receiver';
const E5004_RANGE = range(4, 2, 4, 4);

// Mirrors /tmp fixture `e4014.bock`; the diagnostic span covers the whole
// `use` declaration and runs through the trailing newline onto the next
// line (parser span merge, compiler/crates/bock-parser/src/lib.rs:407-414).
const E4014_DOC = [
  'module e4014fix',
  '',
  'use core.error',
  '',
  'fn main() -> Int {',
  '  1',
  '}',
  '',
].join('\n');
const E4014_MESSAGE =
  '`use core.error` is not a v1 import form: a `use` must name what it imports with a brace-list or a wildcard';
const E4014_RANGE = range(2, 0, 3, 0);

// ─── E4013: unknown method with a near-miss suggestion ──────────────────────

describe('quick-fixes-logic.buildQuickFixes E4013', () => {
  it('renames the method to the suggestion carried in the LSP note', () => {
    const fixes = buildQuickFixes(
      input({
        code: 'E4013',
        message: E4013_MESSAGE,
        range: E4013_RANGE,
        documentText: E4013_DOC,
        relatedMessages: [E4013_NOTE],
      }),
    );
    assert.equal(fixes.length, 1);
    assert.equal(fixes[0].title, "Change 'lenth' to 'length'");
    assert.equal(fixes[0].isPreferred, true);
    assert.deepEqual(fixes[0].edits, [
      { ...range(4, 13, 4, 18), newText: 'length' },
    ]);
  });

  it('fixes the DQ22 Map `contains` case from the inline message suggestion', () => {
    // Mirrors /tmp fixture `e4013map.bock` (`bock check` reported 5:5).
    const doc = [
      'module e4013map',
      '',
      'fn main() -> Bool {',
      '  let m = {"a": 1}',
      '  m.contains("a")',
      '}',
      '',
    ].join('\n');
    const fixes = buildQuickFixes(
      input({
        code: 'E4013',
        message:
          '`contains` is not a method on `Map`; did you mean `contains_key`?',
        range: range(4, 4, 4, 12),
        documentText: doc,
        relatedMessages: [
          'note: use `contains_key(k)` to test for a key or `contains_value(v)` for a value; bare `contains` is a `Set` method',
        ],
      }),
    );
    assert.equal(fixes.length, 1);
    assert.deepEqual(fixes[0].edits, [
      { ...range(4, 4, 4, 12), newText: 'contains_key' },
    ]);
  });

  it('bails when the document text at the range no longer matches (stale)', () => {
    const edited = E4013_DOC.replace('xs.lenth()', 'xs.last()  ');
    const fixes = buildQuickFixes(
      input({
        code: 'E4013',
        message: E4013_MESSAGE,
        range: E4013_RANGE,
        documentText: edited,
        relatedMessages: [E4013_NOTE],
      }),
    );
    assert.deepEqual(fixes, []);
  });

  it('offers nothing when the compiler had no near-miss suggestion', () => {
    const fixes = buildQuickFixes(
      input({
        code: 'E4013',
        message: 'no method `frobnicate` on `List[Int]`',
        range: E4013_RANGE,
        documentText: E4013_DOC,
        relatedMessages: [],
      }),
    );
    assert.deepEqual(fixes, []);
  });
});

// ─── E4014: bare module-qualified import ────────────────────────────────────

describe('quick-fixes-logic.buildQuickFixes E4014', () => {
  it('offers braced and wildcard rewrites for a lowercase module path', () => {
    const fixes = buildQuickFixes(
      input({
        code: 'E4014',
        message: E4014_MESSAGE,
        range: E4014_RANGE,
        documentText: E4014_DOC,
      }),
    );
    assert.equal(fixes.length, 2);
    assert.equal(
      fixes[0].title,
      "Replace with 'use core.error.{ }' (then add the names you need)",
    );
    // Both forms verified to parse and `bock check` cleanly.
    assert.deepEqual(fixes[0].edits, [
      { ...range(2, 14, 2, 14), newText: '.{ }' },
    ]);
    assert.equal(
      fixes[1].title,
      "Replace with 'use core.error.*' (wildcard import)",
    );
    assert.deepEqual(fixes[1].edits, [
      { ...range(2, 14, 2, 14), newText: '.*' },
    ]);
  });

  it('moves a capitalized trailing symbol into the braced form', () => {
    // The parser greedily folds trailing TypeIdents into the module path
    // (compiler/crates/bock-parser/src/lib.rs:424-452), so
    // `use core.error.SimpleError2` is also a bare import — verified to
    // produce E4014 with the full dotted path in the message.
    const doc = E4014_DOC.replace('use core.error', 'use core.error.SimpleError2');
    const fixes = buildQuickFixes(
      input({
        code: 'E4014',
        message:
          '`use core.error.SimpleError2` is not a v1 import form: a `use` must name what it imports with a brace-list or a wildcard',
        range: range(2, 0, 3, 0),
        documentText: doc,
      }),
    );
    assert.equal(fixes.length, 1);
    assert.equal(fixes[0].isPreferred, true);
    assert.equal(
      fixes[0].title,
      "Replace with 'use core.error.{ SimpleError2 }'",
    );
    // Replaces the path portion `core.error.SimpleError2` (23 chars, cols 4..27).
    assert.deepEqual(fixes[0].edits, [
      { ...range(2, 4, 2, 27), newText: 'core.error.{ SimpleError2 }' },
    ]);
  });

  it('bails when the line was already fixed (stale diagnostic)', () => {
    const fixedDoc = E4014_DOC.replace(
      'use core.error',
      'use core.error.{ Error }',
    );
    const fixes = buildQuickFixes(
      input({
        code: 'E4014',
        message: E4014_MESSAGE,
        range: E4014_RANGE,
        documentText: fixedDoc,
      }),
    );
    assert.deepEqual(fixes, []);
  });

  it('bails when the path in the message does not match the line', () => {
    const otherDoc = E4014_DOC.replace('use core.error', 'use std.fs');
    const fixes = buildQuickFixes(
      input({
        code: 'E4014',
        message: E4014_MESSAGE,
        range: E4014_RANGE,
        documentText: otherDoc,
      }),
    );
    assert.deepEqual(fixes, []);
  });
});

// ─── E5004: in-place List mutator on a non-mut binding ──────────────────────

describe('quick-fixes-logic.buildQuickFixes E5004', () => {
  it("inserts `mut ` into the receiver's `let` declaration", () => {
    const fixes = buildQuickFixes(
      input({
        code: 'E5004',
        message: E5004_MESSAGE,
        range: E5004_RANGE,
        documentText: E5004_DOC,
        relatedMessages: [
          'note: declare the list with `let mut`, or use `+` / `concat` to build a new list without mutation',
        ],
      }),
    );
    assert.equal(fixes.length, 1);
    assert.equal(fixes[0].title, "Declare 'xs' as 'let mut'");
    assert.equal(fixes[0].isPreferred, true);
    // Inserting at line 3 col 6 turns `  let xs = [1, 2]` into
    // `  let mut xs = [1, 2]` — verified to check cleanly.
    assert.deepEqual(fixes[0].edits, [
      { ...range(3, 6, 3, 6), newText: 'mut ' },
    ]);
  });

  it('bails when the binding is already `let mut` (stale diagnostic)', () => {
    const fixedDoc = E5004_DOC.replace('let xs', 'let mut xs');
    const fixes = buildQuickFixes(
      input({
        code: 'E5004',
        message: E5004_MESSAGE,
        // `xs` shifted right by 4 in the fixed doc; range no longer matches
        // either way, but even with a corrected range the `mut` is detected.
        range: range(4, 2, 4, 4),
        documentText: fixedDoc,
      }),
    );
    assert.deepEqual(fixes, []);
  });

  it('bails when no `let` declaration for the receiver exists (parameter)', () => {
    const doc = [
      'module p',
      '',
      'fn add(xs: List[Int]) -> Int {',
      '  xs.push(3)',
      '  xs.length()',
      '}',
      '',
    ].join('\n');
    const fixes = buildQuickFixes(
      input({
        code: 'E5004',
        message: E5004_MESSAGE,
        range: range(3, 2, 3, 4),
        documentText: doc,
      }),
    );
    assert.deepEqual(fixes, []);
  });

  it('bails on the non-identifier receiver wording ("the receiver")', () => {
    const fixes = buildQuickFixes(
      input({
        code: 'E5004',
        message:
          'cannot call `push` on the receiver: it mutates the list in place and requires a `mut` receiver',
        range: E5004_RANGE,
        documentText: E5004_DOC,
      }),
    );
    assert.deepEqual(fixes, []);
  });
});

// ─── W1001: unused import ───────────────────────────────────────────────────

describe('quick-fixes-logic.buildQuickFixes W1001', () => {
  function importDoc(useLine: string): string {
    return ['module w', '', useLine, '', 'fn main() -> Int {', '  1', '}', ''].join(
      '\n',
    );
  }

  it('removes a whole bare-module import line', () => {
    // For module imports the W1001 span is the whole declaration
    // (compiler/crates/bock-air/src/resolve.rs:426-439) and the local
    // name is the last path segment — verified: `use core.error` warns
    // "unused import `error`".
    const fixes = buildQuickFixes(
      input({
        code: 'W1001',
        message: 'unused import `error`',
        range: range(2, 0, 3, 0),
        documentText: importDoc('use core.error'),
      }),
    );
    assert.equal(fixes.length, 1);
    assert.equal(fixes[0].title, "Remove unused import 'error'");
    assert.deepEqual(fixes[0].edits, [{ ...range(2, 0, 3, 0), newText: '' }]);
  });

  it('removes the line for a sole braced import', () => {
    // Named-entry spans point at the name token (resolve.rs:501-514);
    // verified: span 3:18 inside `use core.error.{ Error }`.
    const fixes = buildQuickFixes(
      input({
        code: 'W1001',
        message: 'unused import `Error`',
        range: range(2, 17, 2, 22),
        documentText: importDoc('use core.error.{ Error }'),
      }),
    );
    assert.equal(fixes.length, 1);
    assert.deepEqual(fixes[0].edits, [{ ...range(2, 0, 3, 0), newText: '' }]);
  });

  it('removes one aliased entry from a braced list, keeping the rest', () => {
    // Verified: `use core.error.{ Error as E, SimpleError }` warns
    // "unused import `E`" with the span covering `Error as E`.
    const fixes = buildQuickFixes(
      input({
        code: 'W1001',
        message: 'unused import `E`',
        range: range(2, 17, 2, 27),
        documentText: importDoc('use core.error.{ Error as E, SimpleError }'),
      }),
    );
    assert.equal(fixes.length, 1);
    assert.deepEqual(fixes[0].edits, [
      {
        ...range(2, 0, 2, 'use core.error.{ Error as E, SimpleError }'.length),
        newText: 'use core.error.{ SimpleError }',
      },
    ]);
  });

  it('removes an entry line from a multi-line braced list', () => {
    // Multi-line lists and trailing commas parse fine
    // (compiler/crates/bock-parser/src/lib.rs:499-548) — verified that
    // entry spans land on the entry lines.
    const doc = [
      'module w',
      '',
      'use core.error.{',
      '  Error,',
      '  SimpleError,',
      '}',
      '',
      'fn main() -> Int {',
      '  1',
      '}',
      '',
    ].join('\n');
    const fixes = buildQuickFixes(
      input({
        code: 'W1001',
        message: 'unused import `Error`',
        range: range(3, 2, 3, 7),
        documentText: doc,
      }),
    );
    assert.equal(fixes.length, 1);
    assert.deepEqual(fixes[0].edits, [{ ...range(3, 0, 4, 0), newText: '' }]);
  });

  it('does not treat an arbitrary identifier line as an import entry', () => {
    // Same entry-shaped line, but not inside a `use ….{ … }` block.
    const doc = [
      'module w',
      '',
      'fn main() -> Int {',
      '  Error',
      '}',
      '',
    ].join('\n');
    const fixes = buildQuickFixes(
      input({
        code: 'W1001',
        message: 'unused import `Error`',
        range: range(3, 2, 3, 7),
        documentText: doc,
      }),
    );
    assert.deepEqual(fixes, []);
  });

  it('bails when the named entry is gone from the list (stale)', () => {
    const fixes = buildQuickFixes(
      input({
        code: 'W1001',
        message: 'unused import `Error`',
        range: range(2, 17, 2, 22),
        documentText: importDoc('use core.error.{ SimpleError }'),
      }),
    );
    assert.deepEqual(fixes, []);
  });

  it('bails on glob-import per-symbol warnings', () => {
    // Verified: `use core.error.*` warns once per unused exported symbol
    // (e.g. "unused import `SimpleError`") with the whole-decl span; the
    // line shape matches no removable form.
    const fixes = buildQuickFixes(
      input({
        code: 'W1001',
        message: 'unused import `SimpleError`',
        range: range(2, 0, 3, 0),
        documentText: importDoc('use core.error.*'),
      }),
    );
    assert.deepEqual(fixes, []);
  });
});

// ─── Dispatch, batching, code normalization ─────────────────────────────────

describe('quick-fixes-logic dispatch', () => {
  it('returns no fixes for unknown diagnostic codes', () => {
    const fixes = buildQuickFixes(
      input({
        code: 'E9999',
        message: 'no method `lenth` on `List[Int]`',
        range: E4013_RANGE,
        documentText: E4013_DOC,
        relatedMessages: [E4013_NOTE],
      }),
    );
    assert.deepEqual(fixes, []);
  });

  it('keeps batch results index-aligned with their inputs', () => {
    const results = buildQuickFixesForAll([
      input({
        code: 'E4013',
        message: E4013_MESSAGE,
        range: E4013_RANGE,
        documentText: E4013_DOC,
        relatedMessages: [E4013_NOTE],
      }),
      input({
        code: 'E9999',
        message: 'something unrecognized',
        range: range(0, 0, 0, 1),
        documentText: E4013_DOC,
      }),
      input({
        code: 'E5004',
        message: E5004_MESSAGE,
        range: E5004_RANGE,
        documentText: E5004_DOC,
      }),
    ]);
    assert.equal(results.length, 3);
    assert.equal(results[0].length, 1);
    assert.equal(results[0][0].title, "Change 'lenth' to 'length'");
    assert.deepEqual(results[1], []);
    assert.equal(results[2].length, 1);
    assert.equal(results[2][0].title, "Declare 'xs' as 'let mut'");
  });

  it('normalizes string, number and { value } diagnostic codes', () => {
    assert.equal(normalizeDiagnosticCode('E4013'), 'E4013');
    assert.equal(normalizeDiagnosticCode(4013), '4013');
    assert.equal(normalizeDiagnosticCode({ value: 'W1001', target: {} }), 'W1001');
    assert.equal(normalizeDiagnosticCode({ value: 7 }), '7');
    assert.equal(normalizeDiagnosticCode(undefined), undefined);
    assert.equal(normalizeDiagnosticCode(null), undefined);
  });
});

// ─── Provider adapter (BockQuickFixProvider over the vscode stub) ───────────

describe('quick-fixes.BockQuickFixProvider', () => {
  const uri = Uri.file('/proj/src/main.bock');

  function fakeDocument(text: string): vscode.TextDocument {
    return {
      uri,
      languageId: 'bock',
      getText: () => text,
    } as unknown as vscode.TextDocument;
  }

  function fakeDiagnostic(opts: {
    message: string;
    range: QuickFixRange;
    code?: unknown;
    source?: string;
    relatedMessages?: string[];
  }): vscode.Diagnostic {
    return {
      message: opts.message,
      range: new Range(
        new Position(opts.range.startLine, opts.range.startChar),
        new Position(opts.range.endLine, opts.range.endChar),
      ),
      code: opts.code,
      source: opts.source ?? 'bock',
      severity: 0,
      relatedInformation: opts.relatedMessages?.map((message) => ({
        message,
        location: { uri, range: new Range(new Position(0, 0), new Position(0, 0)) },
      })),
    } as unknown as vscode.Diagnostic;
  }

  function provide(
    doc: vscode.TextDocument,
    diagnostics: vscode.Diagnostic[],
  ): vscode.CodeAction[] {
    const provider = new BockQuickFixProvider();
    const context = {
      diagnostics,
      only: undefined,
      triggerKind: 1,
    } as unknown as vscode.CodeActionContext;
    const anyRange = new Range(new Position(0, 0), new Position(0, 0));
    return provider.provideCodeActions(
      doc,
      anyRange as unknown as vscode.Range,
      context,
    );
  }

  it('maps a diagnostic to a CodeAction with a WorkspaceEdit and back-link', () => {
    const diag = fakeDiagnostic({
      message: E4013_MESSAGE,
      range: E4013_RANGE,
      code: 'E4013',
      relatedMessages: [E4013_NOTE],
    });
    const actions = provide(fakeDocument(E4013_DOC), [diag]);
    assert.equal(actions.length, 1);
    const action = actions[0];
    assert.equal(action.title, "Change 'lenth' to 'length'");
    assert.equal(action.isPreferred, true);
    assert.deepEqual(action.diagnostics, [diag]);

    const edit = action.edit as unknown as StubWorkspaceEdit;
    assert.equal(edit.replacements.length, 1);
    const op = edit.replacements[0];
    assert.equal(op.uri, uri);
    assert.equal(op.newText, 'length');
    assert.equal(op.range.start.line, 4);
    assert.equal(op.range.start.character, 13);
    assert.equal(op.range.end.line, 4);
    assert.equal(op.range.end.character, 18);
  });

  it('ignores diagnostics from other sources and unknown codes', () => {
    const foreign = fakeDiagnostic({
      message: E4013_MESSAGE,
      range: E4013_RANGE,
      code: 'E4013',
      source: 'eslint',
      relatedMessages: [E4013_NOTE],
    });
    const unknownCode = fakeDiagnostic({
      message: 'something else entirely',
      range: range(0, 0, 0, 3),
      code: 'E1234',
    });
    const codeless = fakeDiagnostic({
      message: E4013_MESSAGE,
      range: E4013_RANGE,
      code: undefined,
      relatedMessages: [E4013_NOTE],
    });
    const actions = provide(fakeDocument(E4013_DOC), [
      foreign,
      unknownCode,
      codeless,
    ]);
    assert.deepEqual(actions, []);
  });

  it('batches several diagnostics, attaching each action to its own diagnostic', () => {
    // One document containing both an unknown-method typo and an unused
    // import, diagnosed together.
    const doc = [
      'module both',
      '',
      'use core.error.{ Error }',
      '',
      'fn main() -> Int {',
      '  let xs = [1, 2, 3]',
      '  xs.lenth()',
      '}',
      '',
    ].join('\n');
    const unused = fakeDiagnostic({
      message: 'unused import `Error`',
      range: range(2, 17, 2, 22),
      code: 'W1001',
    });
    const typo = fakeDiagnostic({
      message: E4013_MESSAGE,
      range: range(6, 5, 6, 10),
      code: 'E4013',
      relatedMessages: [E4013_NOTE],
    });
    const actions = provide(fakeDocument(doc), [unused, typo]);
    assert.equal(actions.length, 2);
    assert.equal(actions[0].title, "Remove unused import 'Error'");
    assert.deepEqual(actions[0].diagnostics, [unused]);
    assert.equal(actions[1].title, "Change 'lenth' to 'length'");
    assert.deepEqual(actions[1].diagnostics, [typo]);
  });
});
