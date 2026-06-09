// Pure hover-rendering logic for the hover enrichment provider (F1.5.3).
//
// This module is deliberately free of the heavier extension dependencies
// (`vscode-languageclient`, the LSP client). It imports `vscode` only for
// type annotations, so it can be exercised by the headless Mocha + ts-node
// unit suite, whose CommonJS resolver can't follow the
// `vscode-languageclient/node` package `exports` subpath. `hover.ts`
// re-imports the cache builder, the LSP-contents stringifier, the spec-link
// formatter, and the `render*` markdown builders from here, keeping only the
// orchestration that touches the live `vscode` / LSP APIs.
//
// Every helper here is a pure `(...) => string | string[] | Cache` function:
// it consumes plain vocab data (and the resolved spec-link toggle) and
// produces markdown — no document I/O, no LSP round-trips. The pieces that
// genuinely need the live editor (word-range detection, handler scanning,
// `MarkdownString` assembly) stay in `hover.ts`.

import {
  Vocab,
  Annotation,
  Keyword,
  Operator,
  PrimitiveType,
  Symbol as VocabSymbol,
  Module,
  BuiltinMethodGroup,
} from '../shared/types';

// ─── Cache ──────────────────────────────────────────────────────────────────

/** Kind tag for a stdlib symbol, used to label the hover block. */
export type StdlibKind = 'function' | 'type' | 'trait' | 'effect';

/** A single stdlib symbol occurrence, carrying its owning module and kind. */
export interface StdlibHit {
  module: Module;
  symbol: VocabSymbol;
  kind: StdlibKind;
}

/**
 * Pre-built lookup tables over the vocab, keyed by the bare token a hover
 * would land on. Rebuilt whenever the VocabService signals a reload.
 */
export interface Cache {
  keywords: Map<string, Keyword>;
  operators: Map<string, Operator>;
  annotations: Map<string, Annotation>;
  primitives: Map<string, PrimitiveType>;
  preludeTypes: Map<string, VocabSymbol>;
  preludeFunctions: Map<string, VocabSymbol>;
  preludeTraits: Map<string, VocabSymbol>;
  preludeConstructors: Map<string, VocabSymbol>;
  stdlibSymbols: Map<string, StdlibHit[]>;
  effectNames: Set<string>;
  /** Built-in method name → receivers that expose it (e.g. `len` → List, String). */
  builtinMethods: Map<string, string[]>;
  /**
   * Word pattern matching exactly the vocab's operator symbols, for use as
   * the custom-regex fallback of `getWordRangeAtPosition` (operators are not
   * word characters, so the default word range misses them). `undefined`
   * when the vocab declares no operators.
   */
  operatorRegex: RegExp | undefined;
}

/** Build the hover lookup cache from a vocab snapshot. Pure. */
export function buildCache(vocab: Vocab): Cache {
  const keywords = new Map<string, Keyword>();
  for (const k of vocab.language.keywords) keywords.set(k.name, k);

  const operators = new Map<string, Operator>();
  for (const o of vocab.language.operators) operators.set(o.symbol, o);

  const annotations = new Map<string, Annotation>();
  for (const a of vocab.language.annotations) {
    const bare = a.name.startsWith('@') ? a.name.slice(1) : a.name;
    annotations.set(bare, a);
  }

  const primitives = new Map<string, PrimitiveType>();
  for (const p of vocab.language.primitive_types) primitives.set(p.name, p);

  const preludeTypes = indexByName(vocab.language.prelude_types);
  const preludeFunctions = indexByName(vocab.language.prelude_functions);
  const preludeTraits = indexByName(vocab.language.prelude_traits);
  const preludeConstructors = indexByName(vocab.language.prelude_constructors);

  const stdlibSymbols = new Map<string, StdlibHit[]>();
  const effectNames = new Set<string>();
  for (const mod of vocab.stdlib.modules) {
    pushStdlib(stdlibSymbols, mod, mod.functions, 'function');
    pushStdlib(stdlibSymbols, mod, mod.types, 'type');
    pushStdlib(stdlibSymbols, mod, mod.traits, 'trait');
    for (const s of mod.effects) effectNames.add(s.name);
    pushStdlib(stdlibSymbols, mod, mod.effects, 'effect');
  }

  const builtinMethods = buildBuiltinMethodIndex(vocab.stdlib.builtin_methods);
  const operatorRegex = buildOperatorRegex(
    vocab.language.operators.map((o) => o.symbol),
  );

  return {
    keywords,
    operators,
    annotations,
    primitives,
    preludeTypes,
    preludeFunctions,
    preludeTraits,
    preludeConstructors,
    stdlibSymbols,
    effectNames,
    builtinMethods,
    operatorRegex,
  };
}

/**
 * Index the vocab's builtin-method groups by method name, collecting every
 * receiver that exposes the method (e.g. `len` → `['List', 'String', …]`).
 * Receivers keep the vocab's group order and are deduplicated. Pure.
 */
export function buildBuiltinMethodIndex(
  groups: BuiltinMethodGroup[],
): Map<string, string[]> {
  const map = new Map<string, string[]>();
  for (const group of groups) {
    for (const method of group.methods) {
      const receivers = map.get(method);
      if (!receivers) map.set(method, [group.receiver]);
      else if (!receivers.includes(group.receiver)) receivers.push(group.receiver);
    }
  }
  return map;
}

/**
 * Build a word-pattern RegExp that matches exactly the given operator
 * symbols, for `getWordRangeAtPosition`'s custom-regex fallback.
 *
 * Alternatives are sorted longest-first (ties lexicographic) so maximal
 * munch wins at any scan position — `..=` before `..` before `.`, `+=`
 * before `+` — since JS alternation is first-match-wins. Every symbol is
 * regex-escaped. Returns `undefined` for an empty symbol list (a regex that
 * matches nothing is not expressible as a useful word pattern). Pure.
 */
export function buildOperatorRegex(symbols: string[]): RegExp | undefined {
  const unique = [...new Set(symbols)].filter((s) => s.length > 0);
  if (unique.length === 0) return undefined;
  // Code-unit comparison for the tie-break (not localeCompare) so the
  // pattern is byte-for-byte deterministic across runtime locales.
  unique.sort((a, b) => b.length - a.length || (a < b ? -1 : a > b ? 1 : 0));
  return new RegExp(unique.map(escapeRegExp).join('|'));
}

/** Escape a literal string for safe embedding in a RegExp pattern. */
function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

function indexByName(symbols: VocabSymbol[]): Map<string, VocabSymbol> {
  const map = new Map<string, VocabSymbol>();
  for (const s of symbols) map.set(s.name, s);
  return map;
}

function pushStdlib(
  map: Map<string, StdlibHit[]>,
  module: Module,
  symbols: VocabSymbol[],
  kind: StdlibKind,
): void {
  for (const symbol of symbols) {
    const hit = { module, symbol, kind };
    const arr = map.get(symbol.name);
    if (arr) arr.push(hit);
    else map.set(symbol.name, [hit]);
  }
}

// ─── LSP contents → string ──────────────────────────────────────────────────

/** Shape of the LSP `textDocument/hover` response we care about. */
export interface LspHoverResponse {
  contents?: string | MarkedString | MarkedString[];
}

/** An LSP `MarkedString` — a bare string or a `{ value, language? }` record. */
export type MarkedString =
  | string
  | { language?: string; value: string; kind?: string };

/**
 * Flatten an LSP hover `contents` payload into a single markdown string.
 *
 * Handles every shape the protocol allows: a bare string, a single
 * `MarkedString` object, an array of either, or a missing/`undefined`
 * payload (which yields `undefined` so the caller can skip the LSP block).
 */
export function stringifyHoverContents(
  contents: LspHoverResponse['contents'],
): string | undefined {
  if (!contents) return undefined;
  if (typeof contents === 'string') return contents;
  if (Array.isArray(contents)) {
    const parts = contents.map((c) => (typeof c === 'string' ? c : c.value));
    return parts.join('\n\n');
  }
  if (typeof contents === 'object' && 'value' in contents) return contents.value;
  return undefined;
}

// ─── Rendering ──────────────────────────────────────────────────────────────

/**
 * Build the markdown spec-link (`[§… →](command:bock.openSpecAt?…)`) for a
 * spec reference, or `undefined` when links are disabled or the ref is empty.
 */
export function specLink(ref: string, enabled: boolean): string | undefined {
  if (!enabled || !ref) return undefined;
  const uri = `command:bock.openSpecAt?${encodeURIComponent(JSON.stringify([ref]))}`;
  return `[${ref} →](${uri})`;
}

/** Render the hover block for an annotation. Pure markdown. */
export function renderAnnotation(a: Annotation, showSpecLinks: boolean): string {
  const name = a.name.startsWith('@') ? a.name : `@${a.name}`;
  const lines = [`**${name}** — annotation`, '', a.purpose];
  if (a.params) lines.push('', `Params: \`${a.params}\``);
  const example = a.params ? `${name}(${a.params})` : name;
  lines.push('', '_Example:_', '```bock', example, '```');
  const link = specLink(a.spec_ref ?? '', showSpecLinks);
  if (link) lines.push('', link);
  return lines.join('\n');
}

/** Render the hover block for a keyword. Pure markdown. */
export function renderKeyword(k: Keyword, showSpecLinks: boolean): string {
  const lines = [`**\`${k.name}\`** — ${k.category} keyword`];
  const link = specLink(k.spec_ref ?? '', showSpecLinks);
  if (link) lines.push('', link);
  return lines.join('\n');
}

/** Render the hover block for a primitive type. Pure markdown. */
export function renderPrimitive(p: PrimitiveType, showSpecLinks: boolean): string {
  const lines = [`**${p.name}** — primitive type`];
  const link = specLink(p.spec_ref ?? '', showSpecLinks);
  if (link) lines.push('', link);
  return lines.join('\n');
}

/** Render the hover block for a prelude symbol (type/function/trait/ctor). */
export function renderPrelude(
  label: string,
  s: VocabSymbol,
  showSpecLinks: boolean,
): string {
  const lines = [`**${s.name}** — ${label}`];
  if (s.signature) lines.push('', '```bock', s.signature, '```');
  if (s.doc) lines.push('', s.doc);
  const link = specLink(s.spec_ref ?? '', showSpecLinks);
  if (link) lines.push('', link);
  return lines.join('\n');
}

/** Render the hover block for a stdlib symbol hit. Pure markdown. */
export function renderStdlibSymbol(hit: StdlibHit, showSpecLinks: boolean): string {
  const { symbol: s, module, kind } = hit;
  const lines = [`**${s.name}** — ${kind} in \`${module.path}\``];
  if (s.signature) lines.push('', '```bock', s.signature, '```');
  if (s.doc) lines.push('', s.doc);
  if (s.since) lines.push('', `_Since: ${s.since}_`);
  const ref = s.spec_ref ?? module.spec_ref ?? '';
  const link = specLink(ref, showSpecLinks);
  if (link) lines.push('', link);
  return lines.join('\n');
}

/**
 * Render the hover block for an effect-context token.
 *
 * The handler line (if any) is resolved by the caller from the live document
 * and passed in as a zero-based line number, keeping this builder pure. The
 * caller's lookup returns the line of the first matching `handle <name>` (or
 * `undefined` when none is found); a falsy line index (`0`, the very first
 * line) takes the "no handler" message, matching the original behaviour.
 */
export function renderEffectUsage(
  name: string,
  handlerLine: number | undefined,
  showSpecLinks: boolean,
): string {
  const lines = [`**${name}** — effect`];
  lines.push(
    '',
    handlerLine
      ? `Handler in this file: line ${handlerLine + 1}.`
      : `No \`handle ${name}\` found in this file — the handler is in scope at the call site (enclosing \`with\` / \`handling\` block) or provided by the runtime.`,
  );
  lines.push('', '_Example handler:_', '```bock', `handle ${name} { ... }`, '```');
  const link = specLink('§8', showSpecLinks);
  if (link) lines.push('', link);
  return lines.join('\n');
}

/** Render the hover block for an operator. Pure markdown. */
export function renderOperator(o: Operator, showSpecLinks: boolean): string {
  const lines = [`**\`${o.symbol}\`** — ${o.kind} operator`];
  const details: string[] = [];
  if (o.precedence !== undefined) details.push(`precedence ${o.precedence}`);
  if (o.associativity === 'left' || o.associativity === 'right') {
    details.push(`${o.associativity}-associative`);
  } else if (o.associativity === 'none' && o.precedence !== undefined) {
    // Only meaningful next to a precedence; bare punctuation (`=>`, `,`, …)
    // carries neither and gets no details line at all.
    details.push('non-associative');
  }
  if (details.length > 0) {
    const sentence = details.join(', ');
    lines.push('', `${sentence.charAt(0).toUpperCase()}${sentence.slice(1)}.`);
  }
  const link = specLink(o.spec_ref ?? '', showSpecLinks);
  if (link) lines.push('', link);
  return lines.join('\n');
}

/**
 * Spec section governing the built-in receiver-method surface (§18.3 — Core
 * Modules: `List`/`Map`/`Set`/`String` methods lower to native target ops).
 */
const BUILTIN_METHOD_SPEC_REF = '§18.3';

/**
 * Render the hover block for a built-in method (`.len`, `.map`, …), listing
 * every receiver type that exposes it. Pure markdown. The caller resolves
 * `receivers` from the cache's builtin-method index and only calls this with
 * a non-empty list.
 */
export function renderBuiltinMethod(
  method: string,
  receivers: string[],
  showSpecLinks: boolean,
): string {
  const on = receivers.map((r) => `\`${r}\``).join(', ');
  const lines = [`**\`${method}\`** — built-in method on ${on}`];
  const link = specLink(BUILTIN_METHOD_SPEC_REF, showSpecLinks);
  if (link) lines.push('', link);
  return lines.join('\n');
}

/**
 * Render the hover block for an operation of an effect declared in the
 * current document: the owning effect, the effect's full operation list, and
 * a go-to hint for the declaration line.
 *
 * `declaredLine` is the zero-based line of the `effect` declaration (from
 * the analyzer's `EffectDef.defined`), rendered 1-based. Unlike
 * `renderEffectUsage`'s legacy falsy-check, line `0` is a valid location
 * here (an effect declared on the document's first line); only `undefined`
 * omits the hint. Pure markdown.
 */
export function renderEffectOperation(
  operation: string,
  effectName: string,
  operations: string[],
  declaredLine: number | undefined,
  showSpecLinks: boolean,
): string {
  const lines = [`**\`${operation}\`** — operation of effect \`${effectName}\``];
  if (operations.length > 0) {
    const ops = operations.map((op) => `\`${op}\``).join(', ');
    lines.push('', `\`${effectName}\` operations: ${ops}`);
  }
  if (declaredLine !== undefined) {
    lines.push(
      '',
      `Declared in this file: \`effect ${effectName}\` at line ${declaredLine + 1}.`,
    );
  }
  const link = specLink('§8', showSpecLinks);
  if (link) lines.push('', link);
  return lines.join('\n');
}
