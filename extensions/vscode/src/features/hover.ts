// Hover enrichment (F1.5.3).
//
// Wraps the LSP server's `textDocument/hover` response with vocab-derived
// context: keyword/operator purpose, annotation explanations, primitive and
// prelude spec links, stdlib module provenance, built-in method receivers,
// effect handler hints, and effect-operation provenance (which effect
// declared in the current document owns the hovered operation).
//
// Operators are not word characters, so the default
// `getWordRangeAtPosition` misses them; a vocab-derived regex (escaped
// symbols, longest-first) is used as the fallback word pattern.
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
//
// The pure markdown builders and the cache constructor live in
// `hover-render.ts` (free of the `vscode-languageclient` dependency) so they
// can be unit-tested headlessly. This module keeps only the orchestration
// that touches the live `vscode` / LSP APIs.

import * as vscode from 'vscode';
import type { LanguageClient } from 'vscode-languageclient/node';
import { VocabService } from '../vocab';
import {
  Cache,
  LspHoverResponse,
  buildCache,
  stringifyHoverContents,
  renderAnnotation,
  renderKeyword,
  renderOperator,
  renderPrimitive,
  renderPrelude,
  renderStdlibSymbol,
  renderBuiltinMethod,
  renderEffectUsage,
  renderEffectOperation,
} from './hover-render';
import {
  extractEffects,
  buildOperationToEffectMap,
  type EffectDef,
} from './effect-analyzer';

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
      // The hover appends verbatim LSP content, so a fully-trusted markdown
      // string would let a malicious/buggy server smuggle arbitrary
      // `command:` links. Trust exactly the one command this layer emits
      // (the `§…→` spec links) and nothing else.
      md.isTrusted = { enabledCommands: ['bock.openSpecAt'] };
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

// ─── Enrichment ─────────────────────────────────────────────────────────────

function collectEnrichments(
  doc: vscode.TextDocument,
  pos: vscode.Position,
  cache: Cache,
  showSpecLinks: boolean,
): string[] {
  const sections: string[] = [];

  const wordRange = doc.getWordRangeAtPosition(pos);
  if (!wordRange) {
    // Operator symbols are not word characters, so the default word range
    // misses them. Retry with the vocab-derived operator pattern (escaped,
    // longest-first) and render the operator block on a hit.
    if (cache.operatorRegex) {
      const opRange = doc.getWordRangeAtPosition(pos, cache.operatorRegex);
      if (opRange) {
        const op = cache.operators.get(doc.getText(opRange));
        if (op) sections.push(renderOperator(op, showSpecLinks));
      }
    }
    return sections;
  }
  const word = doc.getText(wordRange);

  if (isAnnotationPrefix(doc, wordRange)) {
    const ann = cache.annotations.get(word);
    if (ann) sections.push(renderAnnotation(ann, showSpecLinks));
    return sections;
  }

  const kw = cache.keywords.get(word);
  if (kw) sections.push(renderKeyword(kw, showSpecLinks));

  // `_` (wildcard) is the one operator made of word characters, so it
  // arrives through the default word range rather than the operator-regex
  // fallback above.
  const wordOp = cache.operators.get(word);
  if (wordOp) sections.push(renderOperator(wordOp, showSpecLinks));

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

  if (isPrecededByDot(doc, wordRange)) {
    const receivers = cache.builtinMethods.get(word);
    if (receivers && receivers.length > 0) {
      sections.push(renderBuiltinMethod(word, receivers, showSpecLinks));
    }
  }

  if (isInEffectContext(doc, wordRange) && !cache.keywords.has(word)) {
    const handlerLine = findHandlerInFile(doc, word);
    sections.push(renderEffectUsage(word, handlerLine, showSpecLinks));
  }

  if (!kw && !prim) {
    const owningEffect = findDocumentEffectOperation(doc, word);
    if (owningEffect) {
      sections.push(
        renderEffectOperation(
          word,
          owningEffect.name,
          owningEffect.operations,
          owningEffect.defined?.line,
          showSpecLinks,
        ),
      );
    }
  }

  return sections;
}

/**
 * Resolve `word` as an operation of an effect declared in this document.
 *
 * Re-parses the document text with the analyzer's effect extractor (two
 * regex passes — cheap enough per hover) and maps operations to their owning
 * effect via the same helper `analyzeEffectFlow` uses. Only effects declared
 * in the hovered document participate, so the "declared in this file" line
 * hint in the rendered block is always accurate.
 */
function findDocumentEffectOperation(
  doc: vscode.TextDocument,
  word: string,
): EffectDef | undefined {
  const defs = new Map<string, EffectDef>();
  extractEffects(doc.uri, doc.getText(), defs);
  const owner = buildOperationToEffectMap([...defs.values()]).get(word);
  return owner === undefined ? undefined : defs.get(owner);
}

function isAnnotationPrefix(doc: vscode.TextDocument, range: vscode.Range): boolean {
  if (range.start.character === 0) return false;
  const before = new vscode.Range(
    range.start.translate(0, -1),
    range.start,
  );
  return doc.getText(before) === '@';
}

/** True when the hovered word is directly preceded by `.` (method position). */
function isPrecededByDot(doc: vscode.TextDocument, range: vscode.Range): boolean {
  if (range.start.character === 0) return false;
  const before = new vscode.Range(
    range.start.translate(0, -1),
    range.start,
  );
  return doc.getText(before) === '.';
}

function isInEffectContext(doc: vscode.TextDocument, range: vscode.Range): boolean {
  const line = doc.lineAt(range.start.line).text;
  const prefix = line.slice(0, range.start.character);
  return /\b(with|handle|handling)\b[\s,A-Za-z0-9_]*$/.test(prefix);
}

function findHandlerInFile(doc: vscode.TextDocument, name: string): number | undefined {
  const needle = new RegExp(`\\bhandle\\s+${name}\\b`);
  for (let i = 0; i < doc.lineCount; i++) {
    if (needle.test(doc.lineAt(i).text)) return i;
  }
  return undefined;
}
