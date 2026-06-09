// Pure model logic for the AIR tree viewer (`bock.showAir`).
//
// This module is deliberately free of any `vscode` import so the headless
// Mocha + ts-node unit suite can exercise it directly. `air-viewer.ts` owns
// the editor/UI/process side and delegates all JSON parsing, labelling, and
// coordinate reasoning here.
//
// ── Input contract (verified against the real compiler) ────────────────────
//
// `bock inspect air <file.bock> --json` runs the frontend (lex, parse, name
// resolution, AIR lowering — no type check) and prints a single JSON object
// to stdout. On success it is the root `Module` node; every node carries
// exactly four fields (key order is not significant, evolution is additive):
//
//   { "kind": "FnDecl", "name": "add",
//     "span": { "start": 13, "end": 52, "line": 3, "col": 1 },
//     "children": [...] }
//
// `span.start`/`span.end` are BYTE offsets into the UTF-8 source (`end`
// exclusive); `span.line`/`span.col` are the 1-based line and column
// (column counted in characters, i.e. Unicode code points) of `start`.
// Compiler-synthesized nodes report `0..0`.
//
// On any frontend failure the command exits 1 and stdout carries an error
// object instead of a (partial) tree, distinguished by the top-level `error`
// key:
//
//   { "error": { "message": "parsing failed", "diagnostics": [
//       { "severity": "error", "code": "E2000",
//         "message": "expected `)`, found `{`",
//         "span": { "start": 3, "end": 4, "line": 1, "col": 4 } } ] } }
//
// The full contract lives in `docs/src/reference/cli.md` (§ `bock inspect
// air`) and is pinned end-to-end by
// `compiler/crates/bock-cli/tests/inspect_air_command.rs`.

/** Source span of an AIR node: byte offsets plus the 1-based start line/col. */
export interface AirSpan {
  /** Byte offset of the span start in the UTF-8 source. */
  start: number;
  /** Byte offset one past the span end (exclusive). */
  end: number;
  /** 1-based line of the span start. */
  line: number;
  /** 1-based column (in Unicode code points) of the span start. */
  col: number;
}

/** One validated AIR tree node. */
export interface AirNode {
  /** AIR node kind, e.g. `"Module"`, `"FnDecl"`, `"BinaryOp"`. */
  kind: string;
  /** Source-level name when the node has one, otherwise `null`. */
  name: string | null;
  /** Source span of the node. */
  span: AirSpan;
  /** AIR children in traversal order (may be empty). */
  children: AirNode[];
}

/** One frontend diagnostic from an `inspect air` error object. */
export interface AirDiagnostic {
  /** Severity string, e.g. `"error"` (empty when absent). */
  severity: string;
  /** Diagnostic code, e.g. `"E2000"` (empty when absent). */
  code: string;
  /** Human-readable message. */
  message: string;
  /** Source span of the diagnostic, when present and well-formed. */
  span?: AirSpan;
}

/** Outcome of parsing `bock inspect air --json` stdout. */
export type AirParseResult =
  /** A well-formed AIR tree. */
  | { kind: 'tree'; root: AirNode }
  /** The compiler reported a frontend error (exit 1 + `error` object). */
  | { kind: 'frontend-error'; message: string; diagnostics: AirDiagnostic[] }
  /** Output that matches neither contract — wrong tool version, crash, etc. */
  | { kind: 'malformed'; reason: string };

function malformed(reason: string): AirParseResult {
  return { kind: 'malformed', reason };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/**
 * Parse the stdout of `bock inspect air <file> --json` into a validated
 * model. Distinguishes the three possible shapes:
 *
 * - a node tree (`kind`/`name`/`span`/`children` on every node),
 * - a frontend error object (top-level `error` key), or
 * - anything else → `malformed`, with a reason naming the first offending
 *   path so the failure is debuggable from the output channel.
 *
 * Validation is defensive but additive-tolerant: unknown extra fields on
 * nodes, spans, or the error object are ignored (the CLI contract only ever
 * changes additively), while missing or mistyped required fields reject the
 * whole payload rather than yielding a half-usable tree.
 */
export function parseAirJson(stdout: string): AirParseResult {
  const trimmed = stdout.trim();
  if (trimmed === '') return malformed('empty output');

  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch (err) {
    return malformed(`invalid JSON: ${(err as Error).message}`);
  }
  if (!isRecord(parsed)) {
    return malformed('top-level JSON value is not an object');
  }
  if ('error' in parsed) return parseErrorObject(parsed.error);

  const root = parseNode(parsed, 'root');
  if (typeof root === 'string') return malformed(root);
  return { kind: 'tree', root };
}

/** Validate one node (and, recursively, its children). Returns the parsed
 *  node, or a string describing the first contract violation and where. */
function parseNode(value: unknown, path: string): AirNode | string {
  if (!isRecord(value)) return `${path}: node is not an object`;
  const { kind, name, span, children } = value;
  if (typeof kind !== 'string' || kind === '') {
    return `${path}: missing or non-string \`kind\``;
  }
  if (name !== null && typeof name !== 'string') {
    return `${path}: \`name\` must be a string or null`;
  }
  const parsedSpan = parseSpan(span);
  if (parsedSpan === undefined) {
    return `${path}: missing or malformed \`span\``;
  }
  if (!Array.isArray(children)) {
    return `${path}: \`children\` must be an array`;
  }
  const kids: AirNode[] = [];
  for (let i = 0; i < children.length; i++) {
    const kid = parseNode(children[i], `${path}.children[${i}]`);
    if (typeof kid === 'string') return kid;
    kids.push(kid);
  }
  return { kind, name, span: parsedSpan, children: kids };
}

/** Validate a span object: four non-negative finite numbers. Extra fields
 *  are dropped; anything missing/mistyped rejects the span. */
function parseSpan(value: unknown): AirSpan | undefined {
  if (!isRecord(value)) return undefined;
  const fields: Array<keyof AirSpan> = ['start', 'end', 'line', 'col'];
  const out: Partial<Record<keyof AirSpan, number>> = {};
  for (const field of fields) {
    const v = value[field];
    if (typeof v !== 'number' || !Number.isFinite(v) || v < 0) {
      return undefined;
    }
    out[field] = v;
  }
  return out as AirSpan;
}

/** Interpret a top-level `error` value. A non-object `error` is treated as
 *  malformed output (the contract pins an object), but inside the object we
 *  degrade gracefully: a missing message gets a fallback, diagnostics that
 *  fail validation are skipped rather than discarding the whole error. */
function parseErrorObject(error: unknown): AirParseResult {
  if (!isRecord(error)) {
    return malformed('`error` is not an object');
  }
  const message =
    typeof error.message === 'string' && error.message !== ''
      ? error.message
      : 'frontend error';
  const diagnostics: AirDiagnostic[] = [];
  if (Array.isArray(error.diagnostics)) {
    for (const raw of error.diagnostics) {
      if (!isRecord(raw) || typeof raw.message !== 'string') continue;
      diagnostics.push({
        severity: typeof raw.severity === 'string' ? raw.severity : '',
        code: typeof raw.code === 'string' ? raw.code : '',
        message: raw.message,
        span: parseSpan(raw.span),
      });
    }
  }
  return { kind: 'frontend-error', message, diagnostics };
}

// ─── Presentation helpers ───────────────────────────────────────────────────

/** Tree label for a node: the kind, plus the name when it has one
 *  (`FnDecl add`, `Literal 3`, bare `Block`). Mirrors the CLI's human view. */
export function nodeLabel(node: AirNode): string {
  return node.name === null || node.name === ''
    ? node.kind
    : `${node.kind} ${node.name}`;
}

/** Dimmed location suffix for a node row: `@line:col`, as in the CLI view. */
export function nodeLocation(node: AirNode): string {
  return `@${node.span.line}:${node.span.col}`;
}

/** Number of direct children of a node. */
export function childCount(node: AirNode): number {
  return node.children.length;
}

/** Multi-line tooltip: kind/name, start line/col, byte range, child count. */
export function nodeTooltip(node: AirNode): string {
  const n = childCount(node);
  return [
    node.name === null || node.name === ''
      ? node.kind
      : `${node.kind} \`${node.name}\``,
    `line ${node.span.line}, col ${node.span.col} — bytes ${node.span.start}..${node.span.end}`,
    `${n} ${n === 1 ? 'child' : 'children'}`,
  ].join('\n');
}

/** Codicon ids for the more recognizable AIR kinds. */
const ICON_BY_KIND: Readonly<Record<string, string>> = {
  Module: 'symbol-namespace',
  ImportDecl: 'package',
  FnDecl: 'symbol-function',
  Param: 'symbol-parameter',
  ConstDecl: 'symbol-constant',
  LetBinding: 'symbol-variable',
  BindPat: 'symbol-variable',
  Identifier: 'symbol-variable',
  Literal: 'symbol-number',
  TypeNamed: 'symbol-class',
  RecordDecl: 'symbol-struct',
  EnumDecl: 'symbol-enum',
  Block: 'bracket',
  Call: 'symbol-method',
  BinaryOp: 'symbol-operator',
  UnaryOp: 'symbol-operator',
};

/** Codicon id used to render a node of the given kind (defaults to
 *  `symbol-misc` so unknown future kinds still render sensibly). */
export function nodeIconId(kind: string): string {
  return ICON_BY_KIND[kind] ?? 'symbol-misc';
}

// ─── Span → editor coordinates ──────────────────────────────────────────────
//
// Positioning NEVER uses `span.start`/`span.end` directly: those are UTF-8
// byte offsets, which diverge from VS Code's UTF-16 code-unit positions on
// any non-ASCII source. The node's start comes from the 1-based `line`/`col`
// (converted code points → UTF-16 below); the selection *extent* is derived
// by measuring `end - start` UTF-8 bytes forward from that already-correct
// start, in UTF-16 units.

/** A 0-based editor coordinate (mirror of `vscode.Position`'s fields). */
export interface ZeroBasedPosition {
  line: number;
  character: number;
}

/**
 * Convert a span's 1-based start `line`/`col` to a 0-based editor position.
 *
 * `col` counts Unicode code points, but `vscode.Position.character` counts
 * UTF-16 code units — pass the text of the target line as `lineText` to
 * convert exactly (an astral character earlier on the line occupies one code
 * point but two UTF-16 units). Without `lineText` the column is `col - 1`,
 * which is exact whenever the line's prefix is within the BMP.
 */
export function spanStartPosition(
  span: AirSpan,
  lineText?: string,
): ZeroBasedPosition {
  const line = Math.max(0, span.line - 1);
  if (lineText === undefined) {
    return { line, character: Math.max(0, span.col - 1) };
  }
  let remaining = Math.max(0, span.col - 1);
  let u = 0;
  while (remaining > 0 && u < lineText.length) {
    const cp = lineText.codePointAt(u) as number;
    u += cp > 0xffff ? 2 : 1;
    remaining--;
  }
  return { line, character: u };
}

/**
 * Measure how many UTF-16 code units of `text`, starting at UTF-16 offset
 * `startUtf16`, cover `byteLength` UTF-8 bytes. Used to turn a node's byte
 * length (`span.end - span.start`) into a selection end once the start
 * position is known. Clamps at the end of `text`; a `byteLength` that ends
 * mid-code-point includes that whole code point.
 */
export function utf16LengthForUtf8Bytes(
  text: string,
  startUtf16: number,
  byteLength: number,
): number {
  const start = Math.max(0, startUtf16);
  let u = start;
  let bytes = 0;
  while (u < text.length && bytes < byteLength) {
    const cp = text.codePointAt(u) as number;
    bytes += cp <= 0x7f ? 1 : cp <= 0x7ff ? 2 : cp <= 0xffff ? 3 : 4;
    u += cp > 0xffff ? 2 : 1;
  }
  return u - start;
}
