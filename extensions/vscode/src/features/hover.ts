// Hover enrichment (F1.5.3).
//
// Wraps the LSP server's `textDocument/hover` response with vocab-derived
// context: keyword/operator purpose, annotation explanations, primitive and
// prelude spec links, stdlib module provenance, and effect handler hints.
//
// The base type information still comes from the LSP. This layer only adds
// signposts — spec section links (command:bock.openSpecAt) and inline
// examples. When `bock.hover.showSpecLinks` is false, the spec links are
// suppressed; other enrichment (purpose strings, examples) remains.
//
// All vocab lookups go through a pre-built cache that is rebuilt whenever
// the VocabService signals a reload.
//
// The sectioning pattern per symbol:
//   1. LSP hover body (type / signature)
//   2. Zero or more enrichment blocks (annotation, keyword, prelude, …)
// Each block is a small markdown fragment separated by a horizontal rule so
// the user can scan the tooltip top-to-bottom.

import * as vscode from 'vscode';
import type { LanguageClient } from 'vscode-languageclient/node';
import { VocabService } from '../vocab';
import {
  Vocab,
  Annotation,
  Keyword,
  Operator,
  PrimitiveType,
  Symbol as VocabSymbol,
  Module,
} from '../shared/types';

type StdlibKind = 'function' | 'type' | 'trait' | 'effect';
interface StdlibHit {
  module: Module;
  symbol: VocabSymbol;
  kind: StdlibKind;
}

interface Cache {
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
}

function buildCache(vocab: Vocab): Cache {
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
  };
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

export function registerHover(
  ctx: vscode.ExtensionContext,
  vocab: VocabService,
  client: LanguageClient | undefined,
): void {
  let cache = buildCache(vocab.get());
  ctx.subscriptions.push(
    vocab.onDidChange((v) => {
      cache = buildCache(v);
    }),
  );

  const provider: vscode.HoverProvider = {
    async provideHover(doc, pos, token) {
      const showSpecLinks = vscode.workspace
        .getConfiguration('bock')
        .get<boolean>('hover.showSpecLinks', true);

      const [lspBody, enrichments] = await Promise.all([
        fetchLspHover(client, doc, pos, token),
        Promise.resolve(collectEnrichments(doc, pos, cache, showSpecLinks)),
      ]);

      if (token.isCancellationRequested) return undefined;
      if (!lspBody && enrichments.length === 0) return undefined;

      const md = new vscode.MarkdownString();
      md.isTrusted = true;
      md.supportHtml = false;

      if (lspBody) md.appendMarkdown(lspBody.trim());

      for (const section of enrichments) {
        if (md.value.length > 0) md.appendMarkdown('\n\n---\n\n');
        md.appendMarkdown(section);
      }
      return new vscode.Hover(md);
    },
  };

  ctx.subscriptions.push(
    vscode.languages.registerHoverProvider({ scheme: 'file', language: 'bock' }, provider),
  );
}

// ─── LSP passthrough ────────────────────────────────────────────────────────

async function fetchLspHover(
  client: LanguageClient | undefined,
  doc: vscode.TextDocument,
  pos: vscode.Position,
  token: vscode.CancellationToken,
): Promise<string | undefined> {
  if (!client) return undefined;
  try {
    const result = await client.sendRequest<LspHoverResponse | null>(
      'textDocument/hover',
      {
        textDocument: { uri: doc.uri.toString() },
        position: { line: pos.line, character: pos.character },
      },
      token,
    );
    return stringifyHoverContents(result?.contents);
  } catch {
    return undefined;
  }
}

interface LspHoverResponse {
  contents?: string | MarkedString | MarkedString[];
}
type MarkedString = string | { language?: string; value: string; kind?: string };

function stringifyHoverContents(
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

// ─── Enrichment ─────────────────────────────────────────────────────────────

function collectEnrichments(
  doc: vscode.TextDocument,
  pos: vscode.Position,
  cache: Cache,
  showSpecLinks: boolean,
): string[] {
  const sections: string[] = [];

  const wordRange = doc.getWordRangeAtPosition(pos);
  if (!wordRange) return sections;
  const word = doc.getText(wordRange);

  if (isAnnotationPrefix(doc, wordRange)) {
    const ann = cache.annotations.get(word);
    if (ann) sections.push(renderAnnotation(ann, showSpecLinks));
    return sections;
  }

  const kw = cache.keywords.get(word);
  if (kw) sections.push(renderKeyword(kw, showSpecLinks));

  const prim = cache.primitives.get(word);
  if (prim) sections.push(renderPrimitive(prim, showSpecLinks));

  if (!prim) {
    const preludeHit =
      cache.preludeTypes.get(word) ??
      cache.preludeFunctions.get(word) ??
      cache.preludeTraits.get(word) ??
      cache.preludeConstructors.get(word);
    if (preludeHit) {
      const label = cache.preludeTypes.has(word)
        ? 'prelude type'
        : cache.preludeFunctions.has(word)
          ? 'prelude function'
          : cache.preludeTraits.has(word)
            ? 'prelude trait'
            : 'prelude constructor';
      sections.push(renderPrelude(label, preludeHit, showSpecLinks));
    }
  }

  const stdlibHits = cache.stdlibSymbols.get(word);
  if (stdlibHits) {
    for (const hit of stdlibHits) sections.push(renderStdlibSymbol(hit, showSpecLinks));
  }

  if (isInEffectContext(doc, wordRange) && !cache.keywords.has(word)) {
    sections.push(renderEffectUsage(word, doc, showSpecLinks));
  }

  return sections;
}

function isAnnotationPrefix(doc: vscode.TextDocument, range: vscode.Range): boolean {
  if (range.start.character === 0) return false;
  const before = new vscode.Range(
    range.start.translate(0, -1),
    range.start,
  );
  return doc.getText(before) === '@';
}

function isInEffectContext(doc: vscode.TextDocument, range: vscode.Range): boolean {
  const line = doc.lineAt(range.start.line).text;
  const prefix = line.slice(0, range.start.character);
  return /\b(with|handle|handling)\b[\s,A-Za-z0-9_]*$/.test(prefix);
}

// ─── Rendering ──────────────────────────────────────────────────────────────

function specLink(ref: string, enabled: boolean): string | undefined {
  if (!enabled || !ref) return undefined;
  const uri = `command:bock.openSpecAt?${encodeURIComponent(JSON.stringify([ref]))}`;
  return `[${ref} →](${uri})`;
}

function renderAnnotation(a: Annotation, showSpecLinks: boolean): string {
  const name = a.name.startsWith('@') ? a.name : `@${a.name}`;
  const lines = [`**${name}** — annotation`, '', a.purpose];
  if (a.params) lines.push('', `Params: \`${a.params}\``);
  const example = a.params ? `${name}(${a.params})` : name;
  lines.push('', '_Example:_', '```bock', example, '```');
  const link = specLink(a.spec_ref ?? '', showSpecLinks);
  if (link) lines.push('', link);
  return lines.join('\n');
}

function renderKeyword(k: Keyword, showSpecLinks: boolean): string {
  const lines = [`**\`${k.name}\`** — ${k.category} keyword`];
  const link = specLink(k.spec_ref ?? '', showSpecLinks);
  if (link) lines.push('', link);
  return lines.join('\n');
}

function renderPrimitive(p: PrimitiveType, showSpecLinks: boolean): string {
  const lines = [`**${p.name}** — primitive type`];
  const link = specLink(p.spec_ref ?? '', showSpecLinks);
  if (link) lines.push('', link);
  return lines.join('\n');
}

function renderPrelude(
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

function renderStdlibSymbol(hit: StdlibHit, showSpecLinks: boolean): string {
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

function renderEffectUsage(
  name: string,
  doc: vscode.TextDocument,
  showSpecLinks: boolean,
): string {
  const lines = [`**${name}** — effect`];
  const handler = findHandlerInFile(doc, name);
  lines.push(
    '',
    handler
      ? `Handler in this file: line ${handler + 1}.`
      : `No \`handle ${name}\` found in this file — the handler is in scope at the call site (enclosing \`with\` / \`handling\` block) or provided by the runtime.`,
  );
  lines.push('', '_Example handler:_', '```bock', `handle ${name} { ... }`, '```');
  const link = specLink('§8', showSpecLinks);
  if (link) lines.push('', link);
  return lines.join('\n');
}

function findHandlerInFile(doc: vscode.TextDocument, name: string): number | undefined {
  const needle = new RegExp(`\\bhandle\\s+${name}\\b`);
  for (let i = 0; i < doc.lineCount; i++) {
    if (needle.test(doc.lineAt(i).text)) return i;
  }
  return undefined;
}
