// Pure quick-fix builders for Bock diagnostics.
//
// This module is provider-agnostic and headless-importable: it imports
// neither `vscode` nor `vscode-languageclient`, so the unit tests in
// `test/quick-fixes-logic.test.ts` can exercise it in plain Node (the
// `*-scan`/`*-flow`/`*-render` pattern). The CodeActionProvider wiring
// lives in `quick-fixes.ts`.
//
// Each builder is keyed on a diagnostic code published by `bock lsp`
// (string LSP `Diagnostic.code`, source `"bock"`) and derives a concrete
// text edit from the diagnostic *message* plus the *actual document
// text*. Two invariants keep the fixes safe under stale diagnostics
// (the user kept typing after the last publish):
//
//   1. never apply a blind template — every edit is re-derived from the
//      current document text, and
//   2. bail out (return `[]`) whenever the document text no longer
//      matches what the message describes.
//
// Message formats are verified against the compiler emission sites;
// each builder cites the `compiler/crates/...` file:line of the format
// it parses. The LSP bridge (`bock-lsp/src/diagnostics.rs:50-92`)
// publishes `diag.message` verbatim as `Diagnostic.message` and maps
// each compiler *note* to a `DiagnosticRelatedInformation` whose message
// is prefixed with `"note: "` — which is why suggestion notes are looked
// up in `relatedMessages` below.

/** A zero-based start/end position pair, mirroring an LSP range. */
export interface QuickFixRange {
  startLine: number;
  startChar: number;
  endLine: number;
  endChar: number;
}

/** A single provider-agnostic text edit: replace `range` with `newText`. */
export interface QuickFixEdit extends QuickFixRange {
  newText: string;
}

/** One quick-fix candidate for a diagnostic. */
export interface QuickFixSuggestion {
  /** Human-facing lightbulb title. */
  title: string;
  /** The edits to apply, all within the diagnosed document. */
  edits: QuickFixEdit[];
  /** Marks the fix VS Code should apply on "auto fix" (safe, unambiguous). */
  isPreferred?: boolean;
}

/** Everything a builder may inspect about one published diagnostic. */
export interface QuickFixInput {
  /** Normalized string diagnostic code, e.g. `"E4013"`. */
  code: string;
  /** The diagnostic message exactly as published. */
  message: string;
  /** The diagnostic range (zero-based, end-exclusive). */
  range: QuickFixRange;
  /** The current full text of the document. */
  documentText: string;
  /**
   * Messages of the diagnostic's `relatedInformation` entries. `bock lsp`
   * publishes compiler notes here as `"note: <text>"`
   * (compiler/crates/bock-lsp/src/diagnostics.rs:68-76).
   */
  relatedMessages?: string[];
}

/**
 * Normalize a raw `Diagnostic.code` (string | number | { value } | …)
 * to the plain string form the builders dispatch on.
 */
export function normalizeDiagnosticCode(raw: unknown): string | undefined {
  if (typeof raw === 'string') return raw;
  if (typeof raw === 'number') return String(raw);
  if (raw !== null && typeof raw === 'object' && 'value' in raw) {
    const v = (raw as { value: unknown }).value;
    if (typeof v === 'string') return v;
    if (typeof v === 'number') return String(v);
  }
  return undefined;
}

/**
 * Build the quick fixes for one diagnostic. Unknown codes and any
 * mismatch between message and document text yield `[]`.
 */
export function buildQuickFixes(input: QuickFixInput): QuickFixSuggestion[] {
  switch (input.code) {
    case 'E4013':
      return fixUnknownMethod(input);
    case 'E4014':
      return fixBareModuleImport(input);
    case 'E5004':
      return fixNonMutListReceiver(input);
    case 'W1001':
      return fixUnusedImport(input);
    default:
      return [];
  }
}

/**
 * Build quick fixes for a batch of diagnostics, preserving the
 * association between each input and its suggestions (index-aligned
 * with `inputs`). Used by the provider to attach the originating
 * diagnostic to each resulting code action.
 */
export function buildQuickFixesForAll(
  inputs: QuickFixInput[],
): QuickFixSuggestion[][] {
  return inputs.map((input) => buildQuickFixes(input));
}

// ─── Shared text helpers ────────────────────────────────────────────────────

/** The `\r`-stripped text of line `line`, or undefined when out of range. */
function lineAt(documentText: string, line: number): string | undefined {
  const lines = documentText.split('\n');
  if (line < 0 || line >= lines.length) return undefined;
  return lines[line].replace(/\r$/, '');
}

/**
 * The document text inside a single-line range, or undefined when the
 * range is multi-line or out of bounds. Used to confirm the document
 * still contains what the message says it does.
 */
function textAtRange(
  documentText: string,
  range: QuickFixRange,
): string | undefined {
  if (range.startLine !== range.endLine) return undefined;
  const line = lineAt(documentText, range.startLine);
  if (line === undefined) return undefined;
  if (range.startChar > range.endChar || range.endChar > line.length) {
    return undefined;
  }
  return line.slice(range.startChar, range.endChar);
}

const IDENT = String.raw`[A-Za-z_]\w*`;
const PATH = String.raw`[A-Za-z_][\w.]*`;

// ─── E4013 — unknown method, with a "did you mean" suggestion ───────────────

// Message formats (both verified by running `bock check`):
//   compiler/crates/bock-types/src/checker.rs:4333-4340
//     "no method `{method}` on `{receiver}`"
//     + note "did you mean `{suggestion}`?"  (when a near-miss exists)
//   compiler/crates/bock-types/src/checker.rs:2619-2630 (DQ22 Map special case)
//     "`contains` is not a method on `Map`; did you mean `contains_key`?"
//     (suggestion inline in the message itself)
// The diagnostic range is the method-name span in both cases.
function fixUnknownMethod(input: QuickFixInput): QuickFixSuggestion[] {
  const general = new RegExp(`^no method \`(${IDENT})\` on \``).exec(
    input.message,
  );
  const mapCase = new RegExp(`^\`(${IDENT})\` is not a method on \`Map\``).exec(
    input.message,
  );
  const method = general?.[1] ?? mapCase?.[1];
  if (!method) return [];

  // The suggestion lives inline in the message (Map case) or in a note
  // relayed via relatedInformation (general case).
  const didYouMean = new RegExp(`did you mean \`(${IDENT})\`\\?`);
  let suggestion = didYouMean.exec(input.message)?.[1];
  if (!suggestion) {
    for (const related of input.relatedMessages ?? []) {
      suggestion = didYouMean.exec(related)?.[1];
      if (suggestion) break;
    }
  }
  if (!suggestion || suggestion === method) return [];

  // Stale-diagnostic guard: the range must still contain the method name.
  if (textAtRange(input.documentText, input.range) !== method) return [];

  return [
    {
      title: `Change '${method}' to '${suggestion}'`,
      edits: [{ ...input.range, newText: suggestion }],
      isPreferred: true,
    },
  ];
}

// ─── E4014 — bare module-qualified import ───────────────────────────────────

// Message format (compiler/crates/bock-types/src/checker.rs:1156-1172,
// verified by running `bock check`):
//   "`use {path}` is not a v1 import form: a `use` must name what it
//    imports with a brace-list or a wildcard"
// The range covers the whole `use` declaration and (because the parser
// merges the span through the trailing newline,
// compiler/crates/bock-parser/src/lib.rs:407-414) may extend onto the
// following line — so the edit is derived from the start line's text,
// never from the range end.
//
// The parser greedily consumes *every* trailing identifier segment into
// the module path (compiler/crates/bock-parser/src/lib.rs:424-452), so
// `use core.error.SimpleError2` is also a bare import whose "path" ends
// in a symbol name. Bock's lexer makes the case distinction structural
// (lowercase `Ident` segments are modules, capitalized `TypeIdent` names
// are types), so:
//   - capitalized last segment → rewrite to the braced form the compiler
//     note recommends: `use core.error.{ SimpleError2 }` (preferred);
//   - otherwise → offer the wildcard (`use core.error.*`) and an empty
//     brace-list to fill in (`use core.error.{ }`); both forms verified
//     to parse and check cleanly.
function fixBareModuleImport(input: QuickFixInput): QuickFixSuggestion[] {
  const m = new RegExp(`^\`use (${PATH})\` is not a v1 import form`).exec(
    input.message,
  );
  if (!m) return [];
  const path = m[1];

  const line = lineAt(input.documentText, input.range.startLine);
  if (line === undefined) return [];

  // Stale-diagnostic guard: the line must still be exactly this bare
  // import (optionally `public`), with nothing else on it.
  const lineMatch = new RegExp(
    `^\\s*(?:public\\s+)?use\\s+(${PATH})\\s*$`,
  ).exec(line);
  if (!lineMatch || lineMatch[1] !== path) return [];

  const contentEnd = line.replace(/\s+$/, '').length;
  const pathStart = contentEnd - path.length;
  const lineNo = input.range.startLine;

  const segments = path.split('.');
  const last = segments[segments.length - 1];

  if (segments.length >= 2 && /^[A-Z]/.test(last)) {
    const parent = segments.slice(0, -1).join('.');
    const replacement = `${parent}.{ ${last} }`;
    return [
      {
        title: `Replace with 'use ${replacement}'`,
        edits: [
          {
            startLine: lineNo,
            startChar: pathStart,
            endLine: lineNo,
            endChar: contentEnd,
            newText: replacement,
          },
        ],
        isPreferred: true,
      },
    ];
  }

  const insertAt = {
    startLine: lineNo,
    startChar: contentEnd,
    endLine: lineNo,
    endChar: contentEnd,
  };
  return [
    {
      title: `Replace with 'use ${path}.{ }' (then add the names you need)`,
      edits: [{ ...insertAt, newText: '.{ }' }],
    },
    {
      title: `Replace with 'use ${path}.*' (wildcard import)`,
      edits: [{ ...insertAt, newText: '.*' }],
    },
  ];
}

// ─── E5004 — in-place List mutator on a non-mut binding ─────────────────────

// Message format (compiler/crates/bock-types/src/ownership.rs:257-274,
// verified by running `bock check`):
//   "cannot call `{method}` on `{name}`: it mutates the list in place
//    and requires a `mut` receiver"
// where `{name}` is backtick-quoted only for identifier receivers (other
// receivers render as the unquoted "the receiver", which this builder
// deliberately does not match — there is no single binding to fix). The
// range is the receiver identifier's span.
function fixNonMutListReceiver(input: QuickFixInput): QuickFixSuggestion[] {
  const m = new RegExp(
    `^cannot call \`(${IDENT})\` on \`(${IDENT})\`: it mutates the list ` +
      'in place and requires a `mut` receiver$',
  ).exec(input.message);
  if (!m) return [];
  const name = m[2];

  // Stale-diagnostic guard: the range must still contain the receiver.
  if (textAtRange(input.documentText, input.range) !== name) return [];

  // Find the innermost preceding `let {name}` declaration. If it is
  // already `let mut`, the diagnostic is stale — bail. If no `let` is
  // found (e.g. the receiver is a parameter), there is no single edit
  // this builder can trust — bail.
  const declRe = new RegExp(`^(\\s*)let\\s+(mut\\s+)?${name}\\b`);
  for (let i = input.range.startLine - 1; i >= 0; i--) {
    const line = lineAt(input.documentText, i);
    if (line === undefined) continue;
    const decl = declRe.exec(line);
    if (!decl) continue;
    if (decl[2]) return []; // already `let mut` — stale diagnostic
    const insertAt = decl[0].length - name.length;
    return [
      {
        title: `Declare '${name}' as 'let mut'`,
        edits: [
          {
            startLine: i,
            startChar: insertAt,
            endLine: i,
            endChar: insertAt,
            newText: 'mut ',
          },
        ],
        isPreferred: true,
      },
    ];
  }
  return [];
}

// ─── W1001 — unused import ──────────────────────────────────────────────────

// Message format (compiler/crates/bock-air/src/resolve.rs:1370-1373,
// verified by running `bock check`):
//   "unused import `{localName}`"
// The range is the imported name's span for brace-list entries
// (resolve.rs:501-514, covering `Name` or `Name as Alias`), and the
// whole declaration for bare module imports (resolve.rs:426-439). Glob
// imports warn once per unused *exported symbol* whose name appears
// nowhere in the document text — those bail at the shape checks below.
function fixUnusedImport(input: QuickFixInput): QuickFixSuggestion[] {
  const m = new RegExp(`^unused import \`(${IDENT})\`$`).exec(input.message);
  if (!m) return [];
  const name = m[1];

  const lineNo = input.range.startLine;
  const line = lineAt(input.documentText, lineNo);
  if (line === undefined) return [];

  const title = `Remove unused import '${name}'`;
  const deleteLine: QuickFixEdit = {
    startLine: lineNo,
    startChar: 0,
    endLine: lineNo + 1,
    endChar: 0,
    newText: '',
  };

  // Case A — the whole line is a path-only import (bare module form or
  // greedy single trailing name, e.g. `use core.error` /
  // `use core.error.Error`): the unused local is the last path segment.
  const bare = new RegExp(`^\\s*(?:public\\s+)?use\\s+(${PATH})\\s*$`).exec(
    line,
  );
  if (bare) {
    const segments = bare[1].split('.');
    if (segments[segments.length - 1] !== name) return [];
    return [{ title, edits: [deleteLine] }];
  }

  // Case B — a single-line brace list: `use a.b.{ X, Y as Z }`.
  const braced = new RegExp(
    `^(\\s*(?:public\\s+)?use\\s+${PATH}\\.)\\{([^}]*)\\}\\s*$`,
  ).exec(line);
  if (braced) {
    const entries = braced[2]
      .split(',')
      .map((e) => e.trim())
      .filter((e) => e.length > 0);
    const entryRe = new RegExp(`^(${IDENT})(?:\\s+as\\s+(${IDENT}))?$`);
    const locals: string[] = [];
    for (const entry of entries) {
      const em = entryRe.exec(entry);
      if (!em) return []; // unrecognized entry shape — don't touch the line
      locals.push(em[2] ?? em[1]);
    }
    const index = locals.indexOf(name);
    if (index < 0) return []; // stale: the name is no longer in the list
    if (entries.length === 1) {
      return [{ title, edits: [deleteLine] }];
    }
    const remaining = entries.filter((_, i) => i !== index);
    const rebuilt = `${braced[1]}{ ${remaining.join(', ')} }`;
    return [
      {
        title,
        edits: [
          {
            startLine: lineNo,
            startChar: 0,
            endLine: lineNo,
            endChar: line.length,
            newText: rebuilt,
          },
        ],
      },
    ];
  }

  // Case C — one entry line of a multi-line brace list (the parser
  // accepts newline-separated entries and trailing commas,
  // compiler/crates/bock-parser/src/lib.rs:499-548). Only fires when the
  // line is exactly one entry and the surrounding lines confirm we are
  // inside `use ….{` … `}`.
  const entryLine = new RegExp(
    `^\\s*(${IDENT})(?:\\s+as\\s+(${IDENT}))?\\s*,?\\s*$`,
  ).exec(line);
  if (entryLine) {
    const local = entryLine[2] ?? entryLine[1];
    if (local !== name) return [];
    if (!insideMultilineImportList(input.documentText, lineNo)) return [];
    return [{ title, edits: [deleteLine] }];
  }

  return [];
}

/**
 * True when line `lineNo` sits strictly between a `use ….{` opener above
 * and its closing `}` below, with nothing but import entries in between.
 */
function insideMultilineImportList(
  documentText: string,
  lineNo: number,
): boolean {
  const entryShape = new RegExp(
    `^\\s*(?:${IDENT}(?:\\s+as\\s+${IDENT})?\\s*,?)?\\s*$`,
  );
  const opener = new RegExp(`^\\s*(?:public\\s+)?use\\s+${PATH}\\.\\{[^}]*$`);
  const closer = new RegExp(
    `^\\s*(?:${IDENT}(?:\\s+as\\s+${IDENT})?\\s*,?\\s*)?\\}\\s*$`,
  );

  let openerFound = false;
  for (let i = lineNo - 1; i >= 0; i--) {
    const line = lineAt(documentText, i);
    if (line === undefined) return false;
    if (opener.test(line)) {
      openerFound = true;
      break;
    }
    if (!entryShape.test(line)) return false;
  }
  if (!openerFound) return false;

  const lines = documentText.split('\n');
  for (let i = lineNo + 1; i < lines.length; i++) {
    const line = lines[i].replace(/\r$/, '');
    if (closer.test(line)) return true;
    if (!entryShape.test(line)) return false;
  }
  return false;
}
