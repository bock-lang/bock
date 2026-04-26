// Best-effort effect flow analysis for F1.5.6.
//
// The LSP does not (yet) expose an `bock/effectFlow` custom request, so this
// module parses the .bock text directly to answer three questions:
//
//   1. What function encloses the cursor, and what effects does it declare
//      via its `with` clause?
//   2. Which effect operations does the body call, and which effects do
//      those operations belong to?
//   3. Where do those effects get handled — in a local `handling` block,
//      at module level via `handle E with H`, or in the project's
//      `bock.project [effects]` section?
//
// The parsing is regex-based and handles the common idiomatic shapes in
// `examples/spec-exercisers/effect-showcase`. It is not a full parser.
// When the LSP grows `bock/effectFlow`, the call sites in `effects.ts`
// should switch over and treat this module as a fallback.
//
// Coordinates returned in `Location` are zero-based (VS Code convention).
// The `column` for operation call sites points at the operation name, not
// the opening paren.

import * as path from 'path';
import * as vscode from 'vscode';

// ─── Types ──────────────────────────────────────────────────────────────────

export interface Location {
  uri: vscode.Uri;
  line: number;
  column: number;
}

export interface EffectDef {
  name: string;
  /** Operation names declared in the effect body (empty for composites). */
  operations: string[];
  /** For `effect X = A + B`, the component names. */
  components: string[];
  defined?: Location;
}

export interface EffectOpCall {
  /** Operation name (e.g. `log`, `now`). */
  operation: string;
  /** Effect the operation belongs to, if we could resolve it. */
  effect?: string;
  /** Where the call appears, relative to the document. */
  location: Location;
}

export type HandlerLayer = 'local' | 'module' | 'project';

export interface HandlerBinding {
  /** Effect being handled. */
  effect: string;
  /** Handler name — record type for `E with H {}`, `native` for project layer. */
  handler: string;
  layer: HandlerLayer;
  /** Not set for project-layer bindings without a concrete source line. */
  location?: Location;
}

export interface EffectFlow {
  /** Name of the function that encloses the cursor. */
  functionName: string;
  /** URI of the document containing the function. */
  documentUri: vscode.Uri;
  /** Full range of the function definition (signature + body). */
  functionRange: vscode.Range;
  /** Effects declared in the `with` clause, expanded through composites. */
  effects: string[];
  /** Every effect definition reachable from the with-clause. */
  effectDefs: EffectDef[];
  /** Calls to effect operations found inside the function body. */
  callees: EffectOpCall[];
  /** Resolved handlers across the three layers. */
  handlers: HandlerBinding[];
}

// ─── Entry point ────────────────────────────────────────────────────────────

export async function analyzeEffectFlow(
  document: vscode.TextDocument,
  position: vscode.Position,
): Promise<EffectFlow | undefined> {
  const text = document.getText();
  const enclosing = findEnclosingFunction(text, document.offsetAt(position));
  if (!enclosing) return undefined;

  const effectRegistry = await collectWorkspaceEffects();
  const effects = expandEffects(enclosing.withClause, effectRegistry);
  const effectDefs = effects
    .map((name) => effectRegistry.get(name))
    .filter((e): e is EffectDef => Boolean(e));

  const operationToEffect = new Map<string, string>();
  for (const def of effectDefs) {
    for (const op of def.operations) operationToEffect.set(op, def.name);
  }

  const bodyStartOffset = enclosing.bodyRange.start;
  const bodyEndOffset = enclosing.bodyRange.end;
  const callees = collectCallees(
    document,
    text,
    bodyStartOffset,
    bodyEndOffset,
    operationToEffect,
  );

  const moduleHandlers = collectModuleHandlers(document, text);
  const localHandlers = collectLocalHandlers(
    document,
    text,
    bodyStartOffset,
    bodyEndOffset,
  );
  const projectHandlers = await collectProjectHandlers(document.uri);

  const handlers = [
    ...localHandlers,
    ...moduleHandlers,
    ...projectHandlers,
  ].filter((h) => effects.includes(h.effect));

  return {
    functionName: enclosing.name,
    documentUri: document.uri,
    functionRange: new vscode.Range(
      document.positionAt(enclosing.signatureStart),
      document.positionAt(bodyEndOffset + 1),
    ),
    effects,
    effectDefs,
    callees,
    handlers,
  };
}

// ─── Function discovery ─────────────────────────────────────────────────────

interface EnclosingFunction {
  name: string;
  signatureStart: number;
  /** Inclusive open brace offset. */
  bodyRange: { start: number; end: number };
  withClause: string[];
}

const FN_SIG_RE = /\bfn\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(/g;

function findEnclosingFunction(
  text: string,
  cursorOffset: number,
): EnclosingFunction | undefined {
  FN_SIG_RE.lastIndex = 0;
  let best: EnclosingFunction | undefined;
  let match: RegExpExecArray | null;
  while ((match = FN_SIG_RE.exec(text)) !== null) {
    const name = match[1];
    const openParen = match.index + match[0].length - 1;
    const closeParen = matchDelimiter(text, openParen, '(', ')');
    if (closeParen === -1) continue;
    const openBrace = text.indexOf('{', closeParen);
    if (openBrace === -1) continue;
    const signatureBetween = text.slice(closeParen + 1, openBrace);
    // Skip constructs that reuse `fn` syntax but are not bodies — e.g. the
    // `fn log(...) -> Void` lines inside `effect Logger { ... }`. Those are
    // followed by `}` (closing the effect block) before any `{` body, so a
    // stray `}` in the between-text means we've crossed a boundary.
    if (/[=;}]/.test(signatureBetween)) continue;
    const closeBrace = matchDelimiter(text, openBrace, '{', '}');
    if (closeBrace === -1) continue;
    if (cursorOffset < match.index || cursorOffset > closeBrace) continue;

    const withClause = parseWithClause(signatureBetween);
    const candidate: EnclosingFunction = {
      name,
      signatureStart: match.index,
      bodyRange: { start: openBrace, end: closeBrace },
      withClause,
    };
    // Prefer the innermost enclosing function.
    if (!best || candidate.signatureStart > best.signatureStart) {
      best = candidate;
    }
  }
  return best;
}

/** Match a balanced delimiter pair starting at `openIdx`. Returns the index
 *  of the matching closing delimiter or -1 if unbalanced. Ignores `{`/`(`
 *  that appear inside double-quoted strings. */
function matchDelimiter(
  text: string,
  openIdx: number,
  open: string,
  close: string,
): number {
  let depth = 0;
  let inString = false;
  let stringChar: '"' | "'" | null = null;
  for (let i = openIdx; i < text.length; i++) {
    const c = text[i];
    if (inString) {
      if (c === '\\') {
        i++;
        continue;
      }
      if (c === stringChar) {
        inString = false;
        stringChar = null;
      }
      continue;
    }
    if (c === '"' || c === "'") {
      inString = true;
      stringChar = c;
      continue;
    }
    if (c === '/' && text[i + 1] === '/') {
      const nl = text.indexOf('\n', i);
      i = nl === -1 ? text.length : nl;
      continue;
    }
    if (c === open) depth++;
    else if (c === close) {
      depth--;
      if (depth === 0) return i;
    }
  }
  return -1;
}

function parseWithClause(betweenCloseParenAndBrace: string): string[] {
  const stripped = betweenCloseParenAndBrace
    .replace(/->\s*[^\n{]*/, '')
    .trim();
  const match = /\bwith\b\s+([^\n{]+)/.exec(stripped);
  if (!match) return [];
  return match[1]
    .split(',')
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}

// ─── Workspace effect registry ──────────────────────────────────────────────

async function collectWorkspaceEffects(): Promise<Map<string, EffectDef>> {
  const registry = new Map<string, EffectDef>();
  const files = await vscode.workspace.findFiles(
    '**/*.bock',
    '**/{node_modules,target,dist,out,.git}/**',
  );
  for (const uri of files) {
    try {
      const bytes = await vscode.workspace.fs.readFile(uri);
      const text = Buffer.from(bytes).toString('utf8');
      extractEffects(uri, text, registry);
    } catch {
      // Skip unreadable files.
    }
  }
  return registry;
}

const EFFECT_BLOCK_RE =
  /(?:public\s+)?effect\s+([A-Za-z_][A-Za-z0-9_]*)\s*\{/g;
const EFFECT_ALIAS_RE =
  /(?:public\s+)?effect\s+([A-Za-z_][A-Za-z0-9_]*)\s*=\s*([^\n]+)/g;
const EFFECT_OP_RE = /\bfn\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(/g;

export function extractEffects(
  uri: vscode.Uri,
  text: string,
  out: Map<string, EffectDef>,
): void {
  EFFECT_BLOCK_RE.lastIndex = 0;
  let m: RegExpExecArray | null;
  while ((m = EFFECT_BLOCK_RE.exec(text)) !== null) {
    const name = m[1];
    const braceOpen = text.indexOf('{', m.index + m[0].length - 1);
    if (braceOpen === -1) continue;
    const braceClose = matchDelimiter(text, braceOpen, '{', '}');
    if (braceClose === -1) continue;
    const body = text.slice(braceOpen + 1, braceClose);
    const operations: string[] = [];
    let op: RegExpExecArray | null;
    EFFECT_OP_RE.lastIndex = 0;
    while ((op = EFFECT_OP_RE.exec(body)) !== null) {
      operations.push(op[1]);
    }
    const loc = offsetToLocation(uri, text, m.index);
    out.set(name, {
      name,
      operations,
      components: [],
      defined: loc,
    });
  }

  EFFECT_ALIAS_RE.lastIndex = 0;
  while ((m = EFFECT_ALIAS_RE.exec(text)) !== null) {
    const name = m[1];
    if (out.has(name)) continue;
    const components = m[2]
      .split('+')
      .map((s) => s.trim())
      .filter((s) => s.length > 0 && /^[A-Za-z_][A-Za-z0-9_]*$/.test(s));
    const loc = offsetToLocation(uri, text, m.index);
    out.set(name, {
      name,
      operations: [],
      components,
      defined: loc,
    });
  }
}

function offsetToLocation(
  uri: vscode.Uri,
  text: string,
  offset: number,
): Location {
  let line = 0;
  let lastNewline = -1;
  for (let i = 0; i < offset; i++) {
    if (text[i] === '\n') {
      line++;
      lastNewline = i;
    }
  }
  return { uri, line, column: offset - lastNewline - 1 };
}

function expandEffects(
  root: string[],
  registry: Map<string, EffectDef>,
): string[] {
  const out = new Set<string>();
  const visit = (name: string) => {
    if (out.has(name)) return;
    out.add(name);
    const def = registry.get(name);
    if (!def) return;
    for (const c of def.components) visit(c);
  };
  for (const name of root) visit(name);
  return Array.from(out);
}

// ─── Callee collection ──────────────────────────────────────────────────────

function collectCallees(
  document: vscode.TextDocument,
  text: string,
  bodyStart: number,
  bodyEnd: number,
  operationToEffect: Map<string, string>,
): EffectOpCall[] {
  const body = text.slice(bodyStart, bodyEnd + 1);
  const seen = new Set<string>();
  const out: EffectOpCall[] = [];
  for (const [op, effect] of operationToEffect) {
    const re = new RegExp(`\\b${escapeRegex(op)}\\s*\\(`, 'g');
    let m: RegExpExecArray | null;
    while ((m = re.exec(body)) !== null) {
      const absOffset = bodyStart + m.index;
      const key = `${op}@${absOffset}`;
      if (seen.has(key)) continue;
      seen.add(key);
      const pos = document.positionAt(absOffset);
      out.push({
        operation: op,
        effect,
        location: { uri: document.uri, line: pos.line, column: pos.character },
      });
    }
  }
  out.sort((a, b) => a.location.line - b.location.line);
  return out;
}

function escapeRegex(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

// ─── Handler collection ─────────────────────────────────────────────────────

const MODULE_HANDLE_RE =
  /^[\t ]*(?:public\s+)?handle\s+([A-Za-z_][A-Za-z0-9_]*)\s+with\s+([A-Za-z_][A-Za-z0-9_]*)\b/gm;

function collectModuleHandlers(
  document: vscode.TextDocument,
  text: string,
): HandlerBinding[] {
  MODULE_HANDLE_RE.lastIndex = 0;
  const out: HandlerBinding[] = [];
  let m: RegExpExecArray | null;
  while ((m = MODULE_HANDLE_RE.exec(text)) !== null) {
    const pos = document.positionAt(m.index);
    out.push({
      effect: m[1],
      handler: m[2],
      layer: 'module',
      location: { uri: document.uri, line: pos.line, column: pos.character },
    });
  }
  return out;
}

// Local handling blocks: `handling (E with H {}, E2 with H2 {})`.
// We capture the parenthesised binding list and split it into `E with H`
// pairs, tolerating record literals and extra whitespace.
const LOCAL_HANDLING_RE = /\bhandling\s*\(/g;

function collectLocalHandlers(
  document: vscode.TextDocument,
  text: string,
  bodyStart: number,
  bodyEnd: number,
): HandlerBinding[] {
  const body = text.slice(bodyStart, bodyEnd + 1);
  LOCAL_HANDLING_RE.lastIndex = 0;
  const out: HandlerBinding[] = [];
  let m: RegExpExecArray | null;
  while ((m = LOCAL_HANDLING_RE.exec(body)) !== null) {
    const openParen = bodyStart + m.index + m[0].length - 1;
    const closeParen = matchDelimiter(text, openParen, '(', ')');
    if (closeParen === -1) continue;
    const inner = text.slice(openParen + 1, closeParen);
    for (const binding of splitBindings(inner)) {
      const pair =
        /([A-Za-z_][A-Za-z0-9_]*)\s+with\s+([A-Za-z_][A-Za-z0-9_]*)/.exec(
          binding,
        );
      if (!pair) continue;
      const pos = document.positionAt(openParen + 1);
      out.push({
        effect: pair[1],
        handler: pair[2],
        layer: 'local',
        location: { uri: document.uri, line: pos.line, column: pos.character },
      });
    }
  }
  return out;
}

/** Split a handling-block binding list on top-level commas. Brace- and
 *  paren-aware so record literals like `{}` don't get split. */
function splitBindings(inner: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let start = 0;
  for (let i = 0; i < inner.length; i++) {
    const c = inner[i];
    if (c === '{' || c === '(') depth++;
    else if (c === '}' || c === ')') depth--;
    else if (c === ',' && depth === 0) {
      parts.push(inner.slice(start, i));
      start = i + 1;
    }
  }
  parts.push(inner.slice(start));
  return parts.map((s) => s.trim()).filter((s) => s.length > 0);
}

// ─── Project layer: bock.project [effects] ──────────────────────────────────

async function collectProjectHandlers(
  docUri: vscode.Uri,
): Promise<HandlerBinding[]> {
  const folder = vscode.workspace.getWorkspaceFolder(docUri);
  if (!folder) return [];
  const projectUri = vscode.Uri.joinPath(folder.uri, 'bock.project');
  try {
    const bytes = await vscode.workspace.fs.readFile(projectUri);
    const text = Buffer.from(bytes).toString('utf8');
    return parseProjectEffects(projectUri, text);
  } catch {
    return [];
  }
}

export function parseProjectEffects(
  uri: vscode.Uri,
  text: string,
): HandlerBinding[] {
  const lines = text.split(/\r?\n/);
  const out: HandlerBinding[] = [];
  let inEffects = false;
  for (let i = 0; i < lines.length; i++) {
    const raw = lines[i];
    const trimmed = raw.trim();
    if (trimmed.startsWith('#') || trimmed.length === 0) continue;
    if (/^\[[^\]]+\]$/.test(trimmed)) {
      inEffects = trimmed === '[effects]';
      continue;
    }
    if (!inEffects) continue;
    const eq = raw.indexOf('=');
    if (eq === -1) continue;
    const effect = raw.slice(0, eq).trim();
    const rhs = raw.slice(eq + 1).trim().replace(/^["']|["']$/g, '');
    if (!effect || !rhs) continue;
    const pos = new vscode.Position(i, 0);
    out.push({
      effect,
      handler: rhs,
      layer: 'project',
      location: { uri, line: pos.line, column: pos.character },
    });
  }
  return out;
}

// ─── Convenience for the UI ─────────────────────────────────────────────────

export function displayPath(
  uri: vscode.Uri,
  folder?: vscode.WorkspaceFolder,
): string {
  if (folder) return path.relative(folder.uri.fsPath, uri.fsPath);
  return uri.fsPath;
}
