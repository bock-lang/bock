// Pure semantic-token scanner for Bock documents.
//
// The TextMate grammar (syntaxes/bock.tmLanguage.json) already paints
// keywords, strings, comments, numbers, `fn`-declaration names, and — very
// broadly — every uppercase identifier as a generic "type". This scanner
// emits the *semantic* layer on top: the things TextMate cannot know.
//
// Token mapping (standard VS Code legend types/modifiers ONLY, so every
// theme renders them out of the box):
//
//   Bock construct                          → token type   + modifiers
//   ───────────────────────────────────────────────────────────────────────
//   `fn name(`  declaration (incl. effect
//   operations and impl/trait methods)      → function     + declaration
//   `record Name`                           → struct       + declaration
//   `class Name`                            → class        + declaration
//   `enum Name`                             → enum         + declaration
//   enum variant (in the enum body)         → enumMember   + declaration
//   `trait Name`                            → interface    + declaration
//   `effect Name` (block or `= A + B`)      → type         + declaration
//   reference to an in-file effect name
//   (`with E`, `handle E`, `handling (E`…)  → type
//   call of an in-file effect operation     → function
//   `@annotation`                           → decorator
//   `module a.b` path                       → namespace    + declaration
//   `use a.b.…` path                        → namespace
//   prelude/primitive type from vocab       → type         + defaultLibrary
//   prelude trait from vocab                → interface    + defaultLibrary
//   prelude function from vocab (called)    → function     + defaultLibrary
//   prelude constructor (Some/Ok/Less/…)    → enumMember   + defaultLibrary
//
// Mapping rationale: Bock records are nominal product types (→ `struct`),
// traits are behavioural contracts (→ `interface`), and effects are named
// capability *types* distinct from traits (→ `type`, so themes can keep
// them visually apart from `interface`-coloured traits).
//
// Design rules:
//   - Precision over recall. When a heuristic is unsure, emit nothing —
//     the TextMate layer still covers the base.
//   - Everything is scanned against a comment/string-masked view of each
//     line: `//` line comments, nested `/* … */` block comments, `"…"`
//     strings (with escapes), `'…'` char literals, and cross-line
//     `"""…"""` blocks never produce tokens.
//   - Effect names and operations are discovered with the exported
//     `extractEffects` parser from `effect-analyzer.ts` (not re-implemented
//     here), then validated against the masked text so an `effect` that
//     only "exists" inside a comment or string contributes nothing.
//
// This module is deliberately headless-testable: it imports `vscode` only
// for the `Uri` value handed to `extractEffects` (satisfied by
// `test/vscode-stub.ts`), and must never import `vscode-languageclient`.

import * as vscode from 'vscode';
import { extractEffects, type EffectDef } from './effect-analyzer';
import { Vocab } from '../shared/types';

// ─── Public types ────────────────────────────────────────────────────────────

/** Every token type this scanner can emit — all from the standard legend. */
export const SEMANTIC_TOKEN_TYPES = [
  'namespace',
  'type',
  'class',
  'enum',
  'interface',
  'struct',
  'function',
  'enumMember',
  'decorator',
] as const;

/** Every token modifier this scanner can emit — all from the standard legend. */
export const SEMANTIC_TOKEN_MODIFIERS = ['declaration', 'defaultLibrary'] as const;

export type SemanticTokenType = (typeof SEMANTIC_TOKEN_TYPES)[number];
export type SemanticTokenModifier = (typeof SEMANTIC_TOKEN_MODIFIERS)[number];

/** One semantic token, in zero-based line/character coordinates. */
export interface SemanticToken {
  line: number;
  char: number;
  length: number;
  tokenType: SemanticTokenType;
  tokenModifiers: SemanticTokenModifier[];
}

/**
 * The vocabulary slices the scanner consumes — plain string lists so tests
 * (and a degraded/empty vocab) can construct them trivially.
 */
export interface SemanticVocabInput {
  primitiveTypes: readonly string[];
  preludeTypes: readonly string[];
  preludeFunctions: readonly string[];
  preludeTraits: readonly string[];
  preludeConstructors: readonly string[];
}

/**
 * Project the full compiler vocabulary onto the scanner's input shape.
 *
 * Tolerates a structurally-incomplete vocab (the degraded path from a
 * corrupt `vocab.json`): every nested access is optional and non-string
 * entries are dropped, so an empty/partial vocab degrades to empty lists
 * rather than throwing.
 */
export function semanticVocabInput(vocab: Vocab): SemanticVocabInput {
  const lang = vocab?.language;
  return {
    primitiveTypes: names(lang?.primitive_types),
    preludeTypes: names(lang?.prelude_types),
    preludeFunctions: names(lang?.prelude_functions),
    preludeTraits: names(lang?.prelude_traits),
    preludeConstructors: names(lang?.prelude_constructors),
  };
}

function names(entries: ReadonlyArray<{ name: string }> | undefined): string[] {
  return (entries ?? [])
    .map((e) => e?.name)
    .filter((n): n is string => typeof n === 'string');
}

// ─── Line masking ────────────────────────────────────────────────────────────

/**
 * Cross-line lexical state for `maskLine`: inside nothing, inside a
 * `"""…"""` block (triple-quoted or raw-triple — the closer is the same),
 * or inside a (possibly nested) block comment.
 */
export type MaskState =
  | { readonly kind: 'code' }
  | { readonly kind: 'triple' }
  | { readonly kind: 'block'; readonly depth: number };

/** The state a document starts in: plain code. */
export const initialMaskState: MaskState = { kind: 'code' };

/**
 * Replace every comment/string character on `line` with a space, preserving
 * indices, and advance the cross-line state. Ordinary `"…"` strings and
 * `'…'` char literals are treated as single-line (unterminated ones mask to
 * the end of the line); `"""` blocks and block comments carry their state
 * across lines.
 */
export function maskLine(
  line: string,
  state: MaskState,
): { masked: string; state: MaskState } {
  const out: string[] = [];
  let mode = state;
  let i = 0;
  const n = line.length;

  while (i < n) {
    if (mode.kind === 'triple') {
      const close = line.indexOf('"""', i);
      const stop = close === -1 ? n : close + 3;
      while (i < stop) {
        out.push(' ');
        i++;
      }
      if (close !== -1) mode = initialMaskState;
      continue;
    }

    if (mode.kind === 'block') {
      let depth = mode.depth;
      while (i < n && depth > 0) {
        if (line.startsWith('/*', i)) {
          depth++;
          out.push(' ', ' ');
          i += 2;
        } else if (line.startsWith('*/', i)) {
          depth--;
          out.push(' ', ' ');
          i += 2;
        } else {
          out.push(' ');
          i++;
        }
      }
      mode = depth > 0 ? { kind: 'block', depth } : initialMaskState;
      continue;
    }

    // mode.kind === 'code'
    const c = line[i];
    if (c === '/' && line[i + 1] === '/') {
      while (i < n) {
        out.push(' ');
        i++;
      }
      break;
    }
    if (c === '/' && line[i + 1] === '*') {
      out.push(' ', ' ');
      i += 2;
      mode = { kind: 'block', depth: 1 };
      continue;
    }
    if (c === '"') {
      if (line.startsWith('"""', i)) {
        out.push(' ', ' ', ' ');
        i += 3;
        mode = { kind: 'triple' };
        continue;
      }
      i = maskSingleLineLiteral(line, i, '"', out);
      continue;
    }
    if (c === "'") {
      i = maskSingleLineLiteral(line, i, "'", out);
      continue;
    }
    out.push(c);
    i++;
  }

  return { masked: out.join(''), state: mode };
}

/** Mask a `"…"` / `'…'` literal starting at `i`; returns the index after it. */
function maskSingleLineLiteral(
  line: string,
  i: number,
  quote: '"' | "'",
  out: string[],
): number {
  out.push(' ');
  i++;
  while (i < line.length) {
    if (line[i] === '\\') {
      out.push(' ');
      i++;
      if (i < line.length) {
        out.push(' ');
        i++;
      }
      continue;
    }
    if (line[i] === quote) {
      out.push(' ');
      return i + 1;
    }
    out.push(' ');
    i++;
  }
  return i;
}

// ─── Scanner ─────────────────────────────────────────────────────────────────

const MODULE_RE =
  /^[\t ]*module[\t ]+([A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*)/;
const USE_RE =
  /^[\t ]*use[\t ]+([A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*)/;
const ANNOTATION_RE = /^[\t ]*@([A-Za-z_][A-Za-z0-9_]*)\b/;
const DECL_RE = /\b(fn|record|class|enum|trait|effect)\s+([A-Za-z_][A-Za-z0-9_]*)/g;
const IDENT_RE = /[A-Za-z_][A-Za-z0-9_]*/g;

const DECL_TOKEN_TYPE: Record<string, SemanticTokenType> = {
  fn: 'function',
  record: 'struct',
  class: 'class',
  enum: 'enum',
  trait: 'interface',
  effect: 'type',
};

/** Brace-depth tracker for a multi-line enum body (1 = directly in the body). */
interface EnumBodyState {
  braceDepth: number;
}

/**
 * Scan a Bock document and produce semantic tokens.
 *
 * Pure with respect to the inputs: `text` is the full document text and
 * `vocab` carries the prelude/primitive name lists (empty lists are fine —
 * structural tokens are still emitted). Output is sorted by line, then
 * start character, and tokens never overlap.
 */
export function scanSemanticTokens(
  text: string,
  vocab: SemanticVocabInput,
): SemanticToken[] {
  const tokens: SemanticToken[] = [];
  const claimed = new Map<number, Array<[number, number]>>();

  const overlaps = (line: number, start: number, end: number): boolean => {
    const spans = claimed.get(line);
    return spans !== undefined && spans.some(([s, e]) => start < e && end > s);
  };
  const emit = (
    line: number,
    char: number,
    length: number,
    tokenType: SemanticTokenType,
    tokenModifiers: SemanticTokenModifier[],
  ): void => {
    if (length <= 0 || overlaps(line, char, char + length)) return;
    const spans = claimed.get(line) ?? [];
    spans.push([char, char + length]);
    claimed.set(line, spans);
    tokens.push({ line, char, length, tokenType, tokenModifiers });
  };

  // Pass 0: mask comments/strings, preserving character positions.
  const rawLines = text.split(/\r?\n/);
  const masked: string[] = [];
  let maskState: MaskState = initialMaskState;
  for (const raw of rawLines) {
    const r = maskLine(raw, maskState);
    masked.push(r.masked);
    maskState = r.state;
  }

  // Effect discovery — reuse the effect-analyzer parser on the raw text.
  // The Uri only rides along inside `defined` locations we never read, so a
  // fixed placeholder is fine for both the live extension and the test stub.
  const registry = new Map<string, EffectDef>();
  extractEffects(vscode.Uri.file('/semantic-scan.bock'), text, registry);

  // Pass 1: structural tokens (declarations, annotations, module/use paths).
  // Also collects the in-file `fn` and `effect` names that pass 2 resolves
  // references against.
  const fnNames = new Set<string>();
  const effectNames = new Set<string>();
  let enumBody: EnumBodyState | undefined;

  for (let lineNo = 0; lineNo < masked.length; lineNo++) {
    const line = masked[lineNo];

    if (enumBody) {
      if (scanEnumVariants(line, 0, enumBody, lineNo, emit)) {
        enumBody = undefined;
      }
    }

    const mod = MODULE_RE.exec(line);
    if (mod) {
      emit(lineNo, mod[0].length - mod[1].length, mod[1].length, 'namespace', [
        'declaration',
      ]);
    }
    const use = USE_RE.exec(line);
    if (use) {
      emit(lineNo, use[0].length - use[1].length, use[1].length, 'namespace', []);
    }
    const ann = ANNOTATION_RE.exec(line);
    if (ann) {
      // Cover `@name` including the `@`.
      emit(lineNo, ann[0].length - ann[1].length - 1, ann[1].length + 1, 'decorator', []);
    }

    DECL_RE.lastIndex = 0;
    let decl: RegExpExecArray | null;
    while ((decl = DECL_RE.exec(line)) !== null) {
      const keyword = decl[1];
      const name = decl[2];
      const nameStart = decl.index + decl[0].length - name.length;
      emit(lineNo, nameStart, name.length, DECL_TOKEN_TYPE[keyword], ['declaration']);
      if (keyword === 'fn') fnNames.add(name);
      if (keyword === 'effect') effectNames.add(name);
      if (keyword === 'enum') {
        // Variant tracking starts at the body's `{` — required on the same
        // line as the declaration (the idiomatic shape). Without it we skip
        // variant tokens for this enum: precision over recall.
        const braceIdx = line.indexOf('{', nameStart + name.length);
        if (braceIdx !== -1) {
          enumBody = { braceDepth: 0 };
          if (scanEnumVariants(line, braceIdx, enumBody, lineNo, emit)) {
            enumBody = undefined;
          }
        }
      }
    }
  }

  // In-file effect operations: intersect what `extractEffects` reports with
  // the `fn` declarations seen in the masked text, so an op that only
  // "exists" inside a comment never produces call-site tokens. Same for the
  // effect names themselves (masked declaration required above).
  const effectOps = new Set<string>();
  for (const def of registry.values()) {
    if (!effectNames.has(def.name)) continue;
    for (const op of def.operations) {
      if (fnNames.has(op)) effectOps.add(op);
    }
  }

  // Pass 2: identifier references — in-file effect names, effect-operation
  // call sites, and the vocab-known prelude/primitive names.
  const typeNames = new Set([...vocab.primitiveTypes, ...vocab.preludeTypes]);
  const traitNames = new Set(vocab.preludeTraits);
  const ctorNames = new Set(vocab.preludeConstructors);
  const fnVocab = new Set(vocab.preludeFunctions);

  for (let lineNo = 0; lineNo < masked.length; lineNo++) {
    const line = masked[lineNo];
    IDENT_RE.lastIndex = 0;
    let id: RegExpExecArray | null;
    while ((id = IDENT_RE.exec(line)) !== null) {
      const name = id[0];
      const start = id.index;
      if (overlaps(lineNo, start, start + name.length)) continue;
      if (effectNames.has(name)) {
        emit(lineNo, start, name.length, 'type', []);
        continue;
      }
      const called = isCallPosition(line, start, start + name.length);
      if (called && effectOps.has(name)) {
        emit(lineNo, start, name.length, 'function', []);
        continue;
      }
      if (typeNames.has(name)) {
        emit(lineNo, start, name.length, 'type', ['defaultLibrary']);
        continue;
      }
      if (traitNames.has(name)) {
        emit(lineNo, start, name.length, 'interface', ['defaultLibrary']);
        continue;
      }
      if (ctorNames.has(name)) {
        emit(lineNo, start, name.length, 'enumMember', ['defaultLibrary']);
        continue;
      }
      if (called && fnVocab.has(name)) {
        emit(lineNo, start, name.length, 'function', ['defaultLibrary']);
      }
    }
  }

  tokens.sort((a, b) => a.line - b.line || a.char - b.char);
  return tokens;
}

/**
 * Emit `enumMember`+`declaration` tokens for variant names on one (masked)
 * line of an enum body and advance the body's brace depth.
 *
 * A variant is an uppercase-initial identifier sitting directly at body
 * level — brace depth 1 with no surrounding `(`/`[` nesting — which excludes
 * payload fields and types (`Priced { total: Float }` tags only `Priced`).
 * Returns true when the body's closing `}` was consumed on this line.
 */
function scanEnumVariants(
  line: string,
  from: number,
  state: EnumBodyState,
  lineNo: number,
  emit: (
    line: number,
    char: number,
    length: number,
    tokenType: SemanticTokenType,
    tokenModifiers: SemanticTokenModifier[],
  ) => void,
): boolean {
  let brace = state.braceDepth;
  let nested = 0; // (), [] nesting within the line
  let i = from;
  while (i < line.length) {
    const c = line[i];
    if (c === '{') {
      brace++;
      i++;
      continue;
    }
    if (c === '}') {
      brace--;
      i++;
      if (brace <= 0) {
        state.braceDepth = 0;
        return true;
      }
      continue;
    }
    if (c === '(' || c === '[') {
      nested++;
      i++;
      continue;
    }
    if (c === ')' || c === ']') {
      if (nested > 0) nested--;
      i++;
      continue;
    }
    if (/[A-Za-z_]/.test(c)) {
      let j = i + 1;
      while (j < line.length && /[A-Za-z0-9_]/.test(line[j])) j++;
      if (brace === 1 && nested === 0 && /[A-Z]/.test(c)) {
        emit(lineNo, i, j - i, 'enumMember', ['declaration']);
      }
      i = j;
      continue;
    }
    i++;
  }
  state.braceDepth = brace;
  return false;
}

/**
 * True when the identifier spanning [`start`, `end`) on the (masked) line
 * looks like a free function call: the next non-blank character is `(` and
 * the previous non-blank character is not `.` (method calls like `x.log(…)`
 * are someone else's `log`, not the bare effect operation).
 */
function isCallPosition(line: string, start: number, end: number): boolean {
  let j = end;
  while (j < line.length && (line[j] === ' ' || line[j] === '\t')) j++;
  if (line[j] !== '(') return false;
  let k = start - 1;
  while (k >= 0 && (line[k] === ' ' || line[k] === '\t')) k--;
  return k < 0 || line[k] !== '.';
}
