// Searchable spec side panel (F1.5.8).
//
// Loads `assets/spec/bock-spec.md` (or a user-configured override), parses its
// heading structure into a navigation tree, renders each section as HTML with
// Bock-aware code highlighting, and serves the whole thing to a webview that
// owns navigation and back/forward client-side. Search is split: the webview
// posts each query to the extension, which ranks sections with the exported
// (and unit-tested) `rankSpecSections` and posts rendered result rows back.
//
// Every other feature's spec link (hover, errors, decisions, annotations,
// effects) ultimately funnels into `bock.openSpecAt §X.Y`, so this panel is
// the foundation that those links open into.

import * as vscode from 'vscode';
import { marked, Renderer, Tokens } from 'marked';
import { VocabService } from '../vocab';
import { escapeHtml, nonce } from '../shared/webview';

// ─── Types ──────────────────────────────────────────────────────────────────

export interface SpecSection {
  id: string; // "1", "1.1", "17.4"
  ref: string; // "§1", "§1.1"
  anchor: string; // "section-1-1"
  title: string;
  level: number; // 2 or 3
  html: string; // rendered body HTML (excludes the heading itself)
  text: string; // plain text for search
}

export interface NavNode {
  id: string;
  ref: string;
  anchor: string;
  title: string;
  children: NavNode[];
}

export interface SpecIndex {
  sections: SpecSection[];
  tree: NavNode[];
  sourcePath: string;
}

// ─── Public entry point ─────────────────────────────────────────────────────

export function registerSpecPanel(
  ctx: vscode.ExtensionContext,
  _vocab: VocabService,
): void {
  const controller = new SpecPanelController(ctx);

  ctx.subscriptions.push(
    vscode.commands.registerCommand('bock.showSpec', () =>
      controller.open(undefined),
    ),
    vscode.commands.registerCommand(
      'bock.openSpecAt',
      async (ref?: string) => {
        const target =
          ref ??
          (await vscode.window.showInputBox({
            prompt: 'Spec section (e.g. §6.7 or §17.4)',
            value: '§',
          }));
        if (target) await controller.open(target);
      },
    ),
  );

  controller.registerWatcher();
}

// ─── Controller ─────────────────────────────────────────────────────────────

class SpecPanelController {
  private panel?: vscode.WebviewPanel;
  private index?: SpecIndex;
  private watcher?: vscode.FileSystemWatcher;
  private reloadTimer?: NodeJS.Timeout;

  constructor(private readonly ctx: vscode.ExtensionContext) {}

  async open(ref: string | undefined): Promise<void> {
    const index = await this.loadIndex();
    if (!index) {
      void vscode.window.showErrorMessage(
        'Bock: could not load spec — check `bock.specPath` setting.',
      );
      return;
    }
    this.ensurePanel();
    if (!this.panel) return;
    this.panel.webview.html = renderHtml(index);
    this.panel.reveal(vscode.ViewColumn.Beside, false);
    if (ref) {
      const id = normalizeRef(ref, index);
      if (id) this.postNavigate(id);
    }
  }

  registerWatcher(): void {
    // Dev-mode live reload: rebuild the index and refresh the panel whenever
    // any bundled spec markdown changes. The override path (bock.specPath)
    // is handled separately by listening for config changes.
    const pattern = new vscode.RelativePattern(
      vscode.Uri.joinPath(this.ctx.extensionUri, 'assets', 'spec'),
      '*.md',
    );
    this.watcher = vscode.workspace.createFileSystemWatcher(pattern);
    const onChange = () => this.scheduleReload();
    this.ctx.subscriptions.push(
      this.watcher,
      this.watcher.onDidChange(onChange),
      this.watcher.onDidCreate(onChange),
      this.watcher.onDidDelete(onChange),
      vscode.workspace.onDidChangeConfiguration((e) => {
        if (e.affectsConfiguration('bock.specPath')) this.scheduleReload();
      }),
    );
  }

  private scheduleReload(): void {
    if (this.reloadTimer) clearTimeout(this.reloadTimer);
    this.reloadTimer = setTimeout(() => {
      this.reloadTimer = undefined;
      this.index = undefined;
      if (this.panel) void this.open(undefined);
    }, 150);
  }

  private ensurePanel(): void {
    if (this.panel) return;
    this.panel = vscode.window.createWebviewPanel(
      'bock.specView',
      'Bock Specification',
      { viewColumn: vscode.ViewColumn.Beside, preserveFocus: false },
      {
        enableScripts: true,
        retainContextWhenHidden: true,
        localResourceRoots: [
          vscode.Uri.joinPath(this.ctx.extensionUri, 'assets'),
          vscode.Uri.joinPath(this.ctx.extensionUri, 'out'),
        ],
      },
    );
    this.panel.onDidDispose(() => {
      this.panel = undefined;
    });
    this.panel.webview.onDidReceiveMessage((msg) => this.onMessage(msg));
  }

  private postNavigate(id: string): void {
    if (!this.panel) return;
    void this.panel.webview.postMessage({ type: 'navigate', id });
  }

  private onMessage(msg: unknown): void {
    if (!msg || typeof msg !== 'object') return;
    const m = msg as Record<string, unknown>;
    if (m.type === 'openRef' && typeof m.ref === 'string') {
      void vscode.commands.executeCommand('bock.openSpecAt', m.ref);
    } else if (m.type === 'search' && typeof m.query === 'string') {
      const seq = typeof m.seq === 'number' ? m.seq : 0;
      void this.runSearch(m.query, seq);
    }
  }

  /**
   * Rank sections against `query` and post pre-rendered result rows back to
   * the webview. The `seq` value is echoed verbatim so the webview can drop
   * responses that a newer query has superseded.
   */
  private async runSearch(query: string, seq: number): Promise<void> {
    const index = await this.loadIndex();
    if (!index || !this.panel) return;
    const hits = rankSpecSections(index.sections, query, SEARCH_RESULT_LIMIT);
    void this.panel.webview.postMessage({
      type: 'searchResults',
      seq,
      query,
      hits: hits.map((h) => ({
        id: h.id,
        ref: h.ref,
        // HTML is escaped here (renderHighlighted) so the webview can inject
        // it directly; only <mark> tags are introduced.
        titleHtml: renderHighlighted(h.title, h.titleMatches),
        snippetHtml:
          (h.snippetEllipsisStart ? '…' : '') +
          renderHighlighted(h.snippet, h.snippetMatches) +
          (h.snippetEllipsisEnd ? '…' : ''),
      })),
    });
  }

  private async loadIndex(): Promise<SpecIndex | undefined> {
    if (this.index) return this.index;
    const source = await readSpec(this.ctx);
    if (!source) return undefined;
    const sections = parseSections(source.text);
    const tree = buildNavTree(sections);
    this.index = { sections, tree, sourcePath: source.path };
    return this.index;
  }
}

// ─── Reading the spec source ────────────────────────────────────────────────

async function readSpec(
  ctx: vscode.ExtensionContext,
): Promise<{ text: string; path: string } | undefined> {
  const cfg = vscode.workspace.getConfiguration('bock');
  const override = cfg.get<string>('specPath', '');
  const candidates: vscode.Uri[] = [];
  if (override) {
    const uri = override.endsWith('.md')
      ? vscode.Uri.file(override)
      : vscode.Uri.file(`${override.replace(/\/$/, '')}/bock-spec.md`);
    candidates.push(uri);
  }
  candidates.push(
    vscode.Uri.joinPath(ctx.extensionUri, 'assets', 'spec', 'bock-spec.md'),
  );
  for (const uri of candidates) {
    try {
      const bytes = await vscode.workspace.fs.readFile(uri);
      return { text: new TextDecoder('utf-8').decode(bytes), path: uri.fsPath };
    } catch {
      /* try next */
    }
  }
  return undefined;
}

// ─── Section parsing ────────────────────────────────────────────────────────

const HEADING_RE = /^(##+)\s+(\d+(?:\.\d+)*)\.?\s*(?:[—–-]\s*)?(.*)$/;

export function parseSections(source: string): SpecSection[] {
  const lines = source.split('\n');
  type Pending = {
    id: string;
    title: string;
    level: number;
    bodyLines: string[];
  };
  const pending: Pending[] = [];
  let current: Pending | undefined;

  for (const line of lines) {
    const m = line.match(HEADING_RE);
    if (m && (m[1] === '##' || m[1] === '###' || m[1] === '####')) {
      if (current) pending.push(current);
      current = {
        id: m[2],
        title: m[3].trim() || `Section ${m[2]}`,
        level: m[1].length,
        bodyLines: [],
      };
    } else if (current) {
      current.bodyLines.push(line);
    }
  }
  if (current) pending.push(current);

  const renderer = buildMarkedRenderer();
  return pending.map((p) => {
    const body = p.bodyLines.join('\n').trim();
    const rawHtml = body
      ? (marked.parse(body, { async: false, renderer }) as string)
      : '';
    const html = linkifySpecRefs(rawHtml);
    return {
      id: p.id,
      ref: `§${p.id}`,
      anchor: `section-${p.id.replace(/\./g, '-')}`,
      title: p.title,
      level: p.level,
      html,
      text: stripForSearch(body),
    };
  });
}

export function buildNavTree(sections: SpecSection[]): NavNode[] {
  const roots: NavNode[] = [];
  const byId = new Map<string, NavNode>();
  for (const s of sections) {
    const node: NavNode = {
      id: s.id,
      ref: s.ref,
      anchor: s.anchor,
      title: s.title,
      children: [],
    };
    byId.set(s.id, node);
    const parts = s.id.split('.');
    if (parts.length === 1) {
      roots.push(node);
    } else {
      const parentId = parts.slice(0, -1).join('.');
      const parent = byId.get(parentId);
      if (parent) parent.children.push(node);
      else roots.push(node);
    }
  }
  return roots;
}

export function normalizeRef(ref: string, index: SpecIndex): string | undefined {
  const cleaned = ref.trim().replace(/^§/, '').replace(/[.,;:)\]\s]+$/, '');
  if (!cleaned) return undefined;
  if (index.sections.some((s) => s.id === cleaned)) return cleaned;
  // Graceful fallback: strip trailing components until we find a hit.
  const parts = cleaned.split('.');
  while (parts.length > 0) {
    const id = parts.join('.');
    if (index.sections.some((s) => s.id === id)) return id;
    parts.pop();
  }
  return undefined;
}

// ─── Markdown rendering with Bock highlighter ───────────────────────────────

function buildMarkedRenderer(): Renderer {
  const renderer = new Renderer();
  // marked v5+ passes a Tokens.Code object to renderer.code instead of
  // positional (code, infostring, escaped) arguments.
  const baseCode = renderer.code.bind(renderer);
  renderer.code = (token: Tokens.Code) => {
    const lang = (token.lang ?? '').split(/\s+/)[0];
    if (lang === 'bock') {
      return `<pre class="bock-code"><code class="lang-bock">${highlightBock(token.text)}</code></pre>`;
    }
    return baseCode(token);
  };
  return renderer;
}

// ─── Bock tokenizer ─────────────────────────────────────────────────────────
//
// Minimal single-pass tokenizer used only for rendering `bock` fenced code
// blocks in the spec panel. Intentionally simple — the TextMate grammar in
// syntaxes/bock.tmLanguage.json is the authoritative source for editor
// highlighting; this pass just produces something readable in HTML.

const BOCK_KEYWORDS = new Set([
  'fn', 'let', 'mut', 'if', 'else', 'match', 'for', 'while', 'loop',
  'return', 'break', 'continue', 'use', 'module', 'public', 'private',
  'record', 'enum', 'trait', 'impl', 'type', 'const', 'guard', 'where',
  'with', 'handle', 'handling', 'do', 'in', 'effect', 'context', 'by',
  'resume', 'async', 'await', 'true', 'false', 'null', 'self', 'Self',
  'as', 'is', 'and', 'or', 'not', 'derive', 'extends',
]);

const BOCK_TYPES = new Set([
  'Int', 'Float', 'Bool', 'String', 'Char', 'Unit', 'Never',
  'List', 'Map', 'Set', 'Tuple', 'Optional', 'Option', 'Result',
  'Ok', 'Err', 'Some', 'None', 'Duration', 'Instant',
]);

export function highlightBock(src: string): string {
  const out: string[] = [];
  const n = src.length;
  let i = 0;

  const isIdStart = (c: string) => /[A-Za-z_]/.test(c);
  const isIdCont = (c: string) => /[A-Za-z0-9_]/.test(c);

  while (i < n) {
    const c = src[i];

    // Line comment
    if (c === '/' && src[i + 1] === '/') {
      let j = i;
      while (j < n && src[j] !== '\n') j++;
      out.push(`<span class="tok-comment">${escapeHtml(src.slice(i, j))}</span>`);
      i = j;
      continue;
    }

    // Block comment
    if (c === '/' && src[i + 1] === '*') {
      let j = i + 2;
      while (j < n && !(src[j] === '*' && src[j + 1] === '/')) j++;
      j = Math.min(n, j + 2);
      out.push(`<span class="tok-comment">${escapeHtml(src.slice(i, j))}</span>`);
      i = j;
      continue;
    }

    // String literal (double-quoted), supports escapes and ${...} interp.
    if (c === '"') {
      let j = i + 1;
      while (j < n) {
        if (src[j] === '\\' && j + 1 < n) {
          j += 2;
          continue;
        }
        if (src[j] === '"') {
          j++;
          break;
        }
        j++;
      }
      out.push(`<span class="tok-string">${escapeHtml(src.slice(i, j))}</span>`);
      i = j;
      continue;
    }

    // Char literal
    if (c === "'") {
      let j = i + 1;
      while (j < n && src[j] !== "'") {
        if (src[j] === '\\' && j + 1 < n) j++;
        j++;
      }
      if (j < n) j++;
      out.push(`<span class="tok-string">${escapeHtml(src.slice(i, j))}</span>`);
      i = j;
      continue;
    }

    // Number
    if (/[0-9]/.test(c)) {
      let j = i;
      while (j < n && /[0-9_.]/.test(src[j])) j++;
      // optional exponent
      if (j < n && (src[j] === 'e' || src[j] === 'E')) {
        j++;
        if (src[j] === '+' || src[j] === '-') j++;
        while (j < n && /[0-9]/.test(src[j])) j++;
      }
      out.push(`<span class="tok-number">${escapeHtml(src.slice(i, j))}</span>`);
      i = j;
      continue;
    }

    // Annotation
    if (c === '@') {
      let j = i + 1;
      while (j < n && isIdCont(src[j])) j++;
      out.push(`<span class="tok-annotation">${escapeHtml(src.slice(i, j))}</span>`);
      i = j;
      continue;
    }

    // Identifier / keyword / type
    if (isIdStart(c)) {
      let j = i;
      while (j < n && isIdCont(src[j])) j++;
      const tok = src.slice(i, j);
      let cls: string | undefined;
      if (BOCK_KEYWORDS.has(tok)) cls = 'tok-keyword';
      else if (BOCK_TYPES.has(tok)) cls = 'tok-type';
      else if (/^[A-Z]/.test(tok)) cls = 'tok-type';
      if (cls) {
        out.push(`<span class="${cls}">${escapeHtml(tok)}</span>`);
      } else {
        out.push(escapeHtml(tok));
      }
      i = j;
      continue;
    }

    // Punctuation passthrough (HTML-escape `<`, `>`, `&`)
    out.push(escapeHtml(c));
    i++;
  }
  return out.join('');
}

// ─── Inline §-ref linkification ─────────────────────────────────────────────
//
// Rewrites occurrences of `§N` / `§N.N` / `§N.N.N` in rendered HTML into
// clickable anchors that post back to the extension (so clicks land at the
// correct section). Skips content inside `<pre>` blocks.

export function linkifySpecRefs(html: string): string {
  const parts = html.split(/(<pre[\s\S]*?<\/pre>)/g);
  const refRe = /§(\d+(?:\.\d+)*)/g;
  for (let i = 0; i < parts.length; i++) {
    if (i % 2 === 1) continue; // odd indices are <pre> blocks
    parts[i] = parts[i].replace(
      refRe,
      (_m, id: string) =>
        `<a class="bock-spec-ref" data-ref="§${id}" href="#section-${id.replace(
          /\./g,
          '-',
        )}">§${id}</a>`,
    );
  }
  return parts.join('');
}

// ─── Plain-text extraction for search index ─────────────────────────────────

export function stripForSearch(md: string): string {
  return md
    .replace(/```[\s\S]*?```/g, ' ')
    .replace(/`[^`]*`/g, ' ')
    .replace(/!\[[^\]]*\]\([^)]*\)/g, ' ')
    .replace(/\[([^\]]+)\]\([^)]*\)/g, '$1')
    .replace(/[*_#>|]/g, ' ')
    .replace(/\s+/g, ' ')
    .trim();
}

// ─── Search ranking ─────────────────────────────────────────────────────────
//
// The webview posts each (debounced) query to the extension; the extension
// ranks sections here and posts pre-rendered result rows back. Keeping the
// ranking on this side means the exported function below *is* the production
// search path, and the headless unit tests exercise the real code.

/** The subset of {@link SpecSection} the ranking function reads. */
export interface SearchableSection {
  id: string;
  ref: string;
  title: string;
  /** Plain text of the section body (see {@link stripForSearch}). */
  text: string;
  /** Heading depth (2–4); shallower sections get a small rank bonus. */
  level: number;
}

/** A `[start, end)` character span inside a title or snippet. */
export type MatchSpan = [number, number];

/** One ranked search result, with everything needed to render + highlight. */
export interface SpecSearchHit {
  id: string;
  ref: string;
  title: string;
  /** Total relevance score (higher is better). Deterministic for a given input. */
  score: number;
  /** Plain-text window from the section body around the best match ('' if the body is empty). */
  snippet: string;
  /** True when the snippet was cut from a longer text on the left/right. */
  snippetEllipsisStart: boolean;
  snippetEllipsisEnd: boolean;
  /** Term-occurrence spans inside `title` (sorted, non-overlapping). */
  titleMatches: MatchSpan[];
  /** Term-occurrence spans inside `snippet` (sorted, non-overlapping). */
  snippetMatches: MatchSpan[];
}

/** Maximum number of results the panel shows for one query. */
export const SEARCH_RESULT_LIMIT = 40;

// Per-term weights. Any title hit must outrank any body hit, and within each
// haystack an exact word-boundary hit outranks a word-prefix hit, which
// outranks a bare substring hit. The body position bonus (max 8) is sized so
// it can never lift a body hit over a title hit.
const TITLE_WORD_SCORE = 100;
const TITLE_PREFIX_SCORE = 80;
const TITLE_SUBSTRING_SCORE = 60;
const BODY_WORD_SCORE = 30;
const BODY_PREFIX_SCORE = 22;
const BODY_SUBSTRING_SCORE = 15;

const SNIPPET_CHARS_BEFORE = 40;
const SNIPPET_CHARS_AFTER = 60;

/** Match quality: 2 = exact word (bounded both sides), 1 = word prefix, 0 = substring. */
type MatchQuality = 0 | 1 | 2;

function isWordChar(c: string | undefined): boolean {
  return c !== undefined && /[A-Za-z0-9_]/.test(c);
}

/**
 * Find every occurrence of `term` in `hayLower` and report the first
 * occurrence index plus the best word-boundary quality seen. Returns
 * undefined when the term does not occur at all.
 */
function scanTerm(
  hayLower: string,
  term: string,
): { first: number; quality: MatchQuality } | undefined {
  let from = 0;
  let first = -1;
  let quality: MatchQuality = 0;
  for (;;) {
    const i = hayLower.indexOf(term, from);
    if (i === -1) break;
    if (first === -1) first = i;
    const startOk = !isWordChar(hayLower[i - 1]);
    const endOk = !isWordChar(hayLower[i + term.length]);
    const q: MatchQuality = startOk && endOk ? 2 : startOk ? 1 : 0;
    if (q > quality) quality = q;
    if (quality === 2) break; // can't improve further
    from = i + 1;
  }
  return first === -1 ? undefined : { first, quality };
}

/** Earlier body matches score higher; the bonus decays in coarse bands. */
function positionBonus(first: number): number {
  if (first === 0) return 8;
  if (first < 50) return 6;
  if (first < 200) return 4;
  if (first < 1000) return 2;
  return 0;
}

/**
 * Collect the spans of every occurrence of every term in `hayLower`,
 * merging overlaps so the result is sorted and non-overlapping (safe to
 * feed to {@link renderHighlighted}).
 */
function collectSpans(hayLower: string, terms: readonly string[]): MatchSpan[] {
  const raw: MatchSpan[] = [];
  for (const term of terms) {
    let from = 0;
    for (;;) {
      const i = hayLower.indexOf(term, from);
      if (i === -1) break;
      raw.push([i, i + term.length]);
      from = i + 1;
    }
  }
  raw.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
  const merged: MatchSpan[] = [];
  for (const span of raw) {
    const last = merged[merged.length - 1];
    if (last && span[0] <= last[1]) last[1] = Math.max(last[1], span[1]);
    else merged.push([span[0], span[1]]);
  }
  return merged;
}

function buildSnippet(
  text: string,
  textLower: string,
  terms: readonly string[],
  earliestBodyMatch: number,
): { text: string; leading: boolean; trailing: boolean } {
  if (text.length === 0) return { text: '', leading: false, trailing: false };
  if (earliestBodyMatch === -1) {
    // Title-only match: show the opening of the section body for context.
    const end = Math.min(text.length, SNIPPET_CHARS_BEFORE + SNIPPET_CHARS_AFTER);
    return { text: text.slice(0, end), leading: false, trailing: end < text.length };
  }
  // Size the window from the longest term that matches at the earliest spot.
  let matchLen = 0;
  for (const term of terms) {
    if (textLower.startsWith(term, earliestBodyMatch)) {
      matchLen = Math.max(matchLen, term.length);
    }
  }
  const start = Math.max(0, earliestBodyMatch - SNIPPET_CHARS_BEFORE);
  const end = Math.min(text.length, earliestBodyMatch + matchLen + SNIPPET_CHARS_AFTER);
  return { text: text.slice(start, end), leading: start > 0, trailing: end < text.length };
}

/**
 * Rank spec sections against a whitespace-separated query.
 *
 * Semantics:
 * - **AND across terms** — every term must occur (case-insensitively) in the
 *   section's title or body, or the section is excluded.
 * - **Weighting** — title hits outrank body hits; exact word-boundary hits
 *   outrank word-prefix hits, which outrank bare substring hits; earlier body
 *   matches and shallower headings earn small bonuses.
 * - **Determinism** — equal scores tie-break on document order, so the result
 *   ordering is stable for a given input.
 *
 * Offsets in the returned hits index into `title`/`snippet` as returned
 * (matching is done on a lowercased copy; for the ASCII spec text this
 * preserves offsets).
 */
export function rankSpecSections(
  sections: readonly SearchableSection[],
  query: string,
  limit: number = SEARCH_RESULT_LIMIT,
): SpecSearchHit[] {
  const terms = query
    .toLowerCase()
    .split(/\s+/)
    .filter((t) => t.length > 0);
  if (terms.length === 0 || limit <= 0) return [];

  const scored: { hit: SpecSearchHit; order: number }[] = [];
  sections.forEach((s, order) => {
    const titleLower = s.title.toLowerCase();
    const textLower = s.text.toLowerCase();
    let score = 0;
    let earliestBodyMatch = -1;
    for (const term of terms) {
      const inTitle = scanTerm(titleLower, term);
      const inBody = scanTerm(textLower, term);
      if (!inTitle && !inBody) return; // AND semantics: every term must hit
      if (inTitle) {
        score +=
          inTitle.quality === 2
            ? TITLE_WORD_SCORE
            : inTitle.quality === 1
              ? TITLE_PREFIX_SCORE
              : TITLE_SUBSTRING_SCORE;
      }
      if (inBody) {
        score +=
          (inBody.quality === 2
            ? BODY_WORD_SCORE
            : inBody.quality === 1
              ? BODY_PREFIX_SCORE
              : BODY_SUBSTRING_SCORE) + positionBonus(inBody.first);
        if (earliestBodyMatch === -1 || inBody.first < earliestBodyMatch) {
          earliestBodyMatch = inBody.first;
        }
      }
    }
    // Shallower headings (## over ###/####) get a small structural bonus.
    score += (4 - Math.min(4, Math.max(2, s.level))) * 3;

    const snip = buildSnippet(s.text, textLower, terms, earliestBodyMatch);
    scored.push({
      hit: {
        id: s.id,
        ref: s.ref,
        title: s.title,
        score,
        snippet: snip.text,
        snippetEllipsisStart: snip.leading,
        snippetEllipsisEnd: snip.trailing,
        titleMatches: collectSpans(titleLower, terms),
        snippetMatches: collectSpans(snip.text.toLowerCase(), terms),
      },
      order,
    });
  });

  scored.sort((a, b) => b.hit.score - a.hit.score || a.order - b.order);
  return scored.slice(0, limit).map((e) => e.hit);
}

/**
 * Render `text` as HTML with the given (sorted, non-overlapping) spans
 * wrapped in `<mark>`. Both the marked and unmarked segments are passed
 * through {@link escapeHtml}, so the output is safe to inject. Malformed
 * spans (overlapping, inverted, or out of range) are skipped defensively.
 */
export function renderHighlighted(
  text: string,
  spans: readonly (readonly [number, number])[],
): string {
  if (spans.length === 0) return escapeHtml(text);
  const out: string[] = [];
  let pos = 0;
  for (const [start, end] of spans) {
    if (start < pos || end <= start || end > text.length) continue;
    out.push(escapeHtml(text.slice(pos, start)));
    out.push(`<mark>${escapeHtml(text.slice(start, end))}</mark>`);
    pos = end;
  }
  out.push(escapeHtml(text.slice(pos)));
  return out.join('');
}

// ─── HTML rendering ─────────────────────────────────────────────────────────

function renderHtml(index: SpecIndex): string {
  const pageNonce = nonce();
  const csp = [
    "default-src 'none'",
    `script-src 'nonce-${pageNonce}'`,
    `style-src 'unsafe-inline'`,
    `font-src 'self'`,
  ].join('; ');

  const navHtml = renderNav(index.tree);
  const contentHtml = index.sections
    .map((s) => renderSectionHtml(s, index))
    .join('\n');
  // Metadata the client script needs for navigation/crumbs. Search itself
  // runs extension-side (rankSpecSections), so body text is not shipped here.
  const searchIndex = index.sections.map((s) => ({
    id: s.id,
    ref: s.ref,
    anchor: s.anchor,
    title: s.title,
  }));

  return `<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8" />
  <meta http-equiv="Content-Security-Policy" content="${csp}" />
  <title>Bock Specification</title>
  <style>${styles()}</style>
</head>
<body>
  <header class="bock-header">
    <div class="bock-header-left">
      <button class="bock-iconbtn" id="bock-back" title="Back" disabled>←</button>
      <button class="bock-iconbtn" id="bock-forward" title="Forward" disabled>→</button>
    </div>
    <input
      type="search"
      id="bock-search"
      class="bock-search"
      placeholder="Search spec (Ctrl/Cmd+F)…"
      autocomplete="off"
      spellcheck="false"
    />
    <div class="bock-header-right" id="bock-crumb"></div>
  </header>
  <div class="bock-layout">
    <aside class="bock-nav" id="bock-nav">
      <nav id="bock-nav-tree">${navHtml}</nav>
      <div class="bock-results" id="bock-results" hidden></div>
    </aside>
    <main class="bock-content" id="bock-content">
      ${contentHtml}
    </main>
  </div>
  <script nonce="${pageNonce}">
    (function () {
      const vscode = acquireVsCodeApi();
      const sections = ${JSON.stringify(searchIndex)};
      const byId = new Map(sections.map((s) => [s.id, s]));
      const navEl = document.getElementById('bock-nav');
      const navTreeEl = document.getElementById('bock-nav-tree');
      const contentEl = document.getElementById('bock-content');
      const searchEl = document.getElementById('bock-search');
      const resultsEl = document.getElementById('bock-results');
      const crumbEl = document.getElementById('bock-crumb');
      const backBtn = document.getElementById('bock-back');
      const forwardBtn = document.getElementById('bock-forward');

      const history = [];
      let cursor = -1;
      let suppressPush = false;

      function updateNavButtons() {
        backBtn.disabled = cursor <= 0;
        forwardBtn.disabled = cursor >= history.length - 1;
      }

      function updateCrumb(id) {
        const s = byId.get(id);
        crumbEl.textContent = s ? s.ref + ' — ' + s.title : '';
      }

      function highlightNav(id) {
        navEl.querySelectorAll('.bock-nav-link').forEach((el) => {
          el.classList.toggle('active', el.getAttribute('data-id') === id);
        });
      }

      function scrollToSection(id, push) {
        const s = byId.get(id);
        if (!s) return false;
        const el = document.getElementById(s.anchor);
        if (!el) return false;
        el.scrollIntoView({ behavior: 'smooth', block: 'start' });
        el.classList.remove('bock-flash');
        void el.offsetWidth;
        el.classList.add('bock-flash');
        setTimeout(() => el.classList.remove('bock-flash'), 1200);
        highlightNav(id);
        updateCrumb(id);
        if (push && !suppressPush) {
          history.splice(cursor + 1);
          history.push(id);
          cursor = history.length - 1;
          updateNavButtons();
        }
        return true;
      }

      // Nav tree click
      navEl.addEventListener('click', (e) => {
        const a = e.target.closest('.bock-nav-link');
        if (!a) return;
        e.preventDefault();
        const id = a.getAttribute('data-id');
        if (id) scrollToSection(id, true);
      });

      // Inline §-ref click (delegated)
      contentEl.addEventListener('click', (e) => {
        const a = e.target.closest('.bock-spec-ref');
        if (!a) return;
        e.preventDefault();
        const ref = a.getAttribute('data-ref') || '';
        const id = ref.replace(/^§/, '');
        if (byId.has(id)) {
          scrollToSection(id, true);
        } else {
          vscode.postMessage({ type: 'openRef', ref: ref });
        }
      });

      // Prev/next inside section footer
      contentEl.addEventListener('click', (e) => {
        const a = e.target.closest('.bock-section-nav a');
        if (!a) return;
        e.preventDefault();
        const id = a.getAttribute('data-id');
        if (id) scrollToSection(id, true);
      });

      // Back / forward
      backBtn.addEventListener('click', () => {
        if (cursor <= 0) return;
        cursor--;
        suppressPush = true;
        scrollToSection(history[cursor], false);
        suppressPush = false;
        updateNavButtons();
      });
      forwardBtn.addEventListener('click', () => {
        if (cursor >= history.length - 1) return;
        cursor++;
        suppressPush = true;
        scrollToSection(history[cursor], false);
        suppressPush = false;
        updateNavButtons();
      });

      // Search — each (debounced) query is posted to the extension, which
      // ranks sections with rankSpecSections() and posts pre-rendered,
      // pre-escaped result rows back. The seq counter drops stale responses.
      let searchSeq = 0;
      let searchTimer = null;
      let activeIdx = -1;
      let resultIds = [];

      function hideResults() {
        resultsEl.hidden = true;
        resultsEl.innerHTML = '';
        navTreeEl.hidden = false;
        activeIdx = -1;
        resultIds = [];
      }

      function clearSearch() {
        searchEl.value = '';
        hideResults();
      }

      function setActive(idx) {
        activeIdx = idx;
        const els = resultsEl.querySelectorAll('.bock-result');
        els.forEach((el, i) => {
          el.classList.toggle('active', i === idx);
        });
        if (idx >= 0 && els[idx]) {
          els[idx].scrollIntoView({ block: 'nearest' });
        }
      }

      function moveActive(delta) {
        const n = resultIds.length;
        if (n === 0) return;
        const next = activeIdx < 0
          ? (delta > 0 ? 0 : n - 1)
          : (activeIdx + delta + n) % n;
        setActive(next);
      }

      function openActive() {
        if (activeIdx < 0 || activeIdx >= resultIds.length) return;
        const id = resultIds[activeIdx];
        clearSearch();
        scrollToSection(id, true);
      }

      function renderResults(query, hits) {
        resultsEl.innerHTML = '';
        resultIds = hits.map((h) => h.id);
        if (hits.length === 0) {
          const p = document.createElement('p');
          p.className = 'bock-noresults';
          p.textContent = 'No matches for "' + query + '"';
          resultsEl.appendChild(p);
        } else {
          for (const h of hits) {
            const a = document.createElement('a');
            a.href = '#';
            a.className = 'bock-result';
            a.setAttribute('data-id', h.id);
            const ref = document.createElement('span');
            ref.className = 'bock-result-ref';
            ref.textContent = h.ref;
            const title = document.createElement('span');
            title.className = 'bock-result-title';
            // Escaped extension-side (renderHighlighted); only <mark> tags.
            title.innerHTML = h.titleHtml;
            const snippet = document.createElement('span');
            snippet.className = 'bock-result-snippet';
            snippet.innerHTML = h.snippetHtml;
            a.appendChild(ref);
            a.appendChild(title);
            a.appendChild(snippet);
            resultsEl.appendChild(a);
          }
        }
        navTreeEl.hidden = true;
        resultsEl.hidden = false;
        setActive(hits.length > 0 ? 0 : -1);
      }

      searchEl.addEventListener('input', () => {
        if (searchTimer) clearTimeout(searchTimer);
        searchTimer = setTimeout(() => {
          const q = searchEl.value.trim();
          if (!q) {
            hideResults();
            return;
          }
          searchSeq++;
          vscode.postMessage({ type: 'search', query: q, seq: searchSeq });
        }, 80);
      });

      // Keyboard navigation while the search box has focus: arrows move the
      // active-result cursor, Enter opens it. Escape is handled at window
      // level so it also works when focus is elsewhere.
      searchEl.addEventListener('keydown', (e) => {
        if (e.key === 'ArrowDown') {
          e.preventDefault();
          moveActive(1);
        } else if (e.key === 'ArrowUp') {
          e.preventDefault();
          moveActive(-1);
        } else if (e.key === 'Enter') {
          e.preventDefault();
          openActive();
        }
      });

      resultsEl.addEventListener('click', (e) => {
        const a = e.target.closest('.bock-result');
        if (!a) return;
        e.preventDefault();
        const id = a.getAttribute('data-id');
        if (id) {
          clearSearch();
          scrollToSection(id, true);
        }
      });

      // Ctrl/Cmd+F focuses the search box; Escape clears the query and
      // restores the nav tree.
      window.addEventListener('keydown', (e) => {
        if ((e.ctrlKey || e.metaKey) && e.key === 'f') {
          e.preventDefault();
          searchEl.focus();
          searchEl.select();
        } else if (e.key === 'Escape' && (searchEl.value || !resultsEl.hidden)) {
          e.preventDefault();
          clearSearch();
          if (document.activeElement === searchEl) searchEl.blur();
        }
      });

      // IntersectionObserver updates the active nav link while scrolling
      const sectionEls = Array.from(
        contentEl.querySelectorAll('[data-section-id]'),
      );
      const io = new IntersectionObserver(
        (entries) => {
          for (const entry of entries) {
            if (entry.isIntersecting) {
              const id = entry.target.getAttribute('data-section-id');
              if (id) {
                highlightNav(id);
                updateCrumb(id);
              }
              break;
            }
          }
        },
        { rootMargin: '-10% 0px -70% 0px', threshold: 0 },
      );
      sectionEls.forEach((el) => io.observe(el));

      // Messages from extension
      window.addEventListener('message', (event) => {
        const msg = event.data;
        if (!msg || typeof msg !== 'object') return;
        if (msg.type === 'navigate' && typeof msg.id === 'string') {
          scrollToSection(msg.id, true);
        } else if (msg.type === 'searchResults') {
          if (msg.seq !== searchSeq) return; // superseded by a newer query
          if (!searchEl.value.trim()) return; // cleared while ranking
          renderResults(
            typeof msg.query === 'string' ? msg.query : '',
            Array.isArray(msg.hits) ? msg.hits : [],
          );
        }
      });

      // Initial state: show §1 if nothing else
      if (sections.length > 0) {
        highlightNav(sections[0].id);
        updateCrumb(sections[0].id);
      }
    })();
  </script>
</body>
</html>`;
}

function renderNav(tree: NavNode[]): string {
  const render = (nodes: NavNode[], depth: number): string => {
    if (nodes.length === 0) return '';
    const items = nodes
      .map((n) => {
        const childHtml = render(n.children, depth + 1);
        return `<li class="bock-nav-item depth-${depth}">
          <a href="#${n.anchor}" class="bock-nav-link" data-id="${n.id}">
            <span class="bock-nav-num">${escapeHtml(n.id)}</span>
            <span class="bock-nav-title">${escapeHtml(n.title)}</span>
          </a>
          ${childHtml}
        </li>`;
      })
      .join('');
    return `<ul class="bock-nav-list depth-${depth}">${items}</ul>`;
  };
  return render(tree, 0);
}

function renderSectionHtml(section: SpecSection, index: SpecIndex): string {
  const { prev, next } = findNeighbors(section, index);
  const footer = `<div class="bock-section-nav">
    ${prev ? `<a href="#${prev.anchor}" data-id="${prev.id}">← ${escapeHtml(prev.ref)} ${escapeHtml(prev.title)}</a>` : '<span></span>'}
    ${next ? `<a href="#${next.anchor}" data-id="${next.id}">${escapeHtml(next.ref)} ${escapeHtml(next.title)} →</a>` : '<span></span>'}
  </div>`;
  const tag = section.level === 2 ? 'h2' : section.level === 3 ? 'h3' : 'h4';
  return `<section class="bock-section bock-section-level-${section.level}"
      id="${section.anchor}" data-section-id="${section.id}">
    <${tag} class="bock-section-heading">
      <span class="bock-section-num">${escapeHtml(section.ref)}</span>
      ${escapeHtml(section.title)}
    </${tag}>
    ${section.html}
    ${footer}
  </section>`;
}

function findNeighbors(
  section: SpecSection,
  index: SpecIndex,
): { prev?: SpecSection; next?: SpecSection } {
  const flat = index.sections;
  const i = flat.findIndex((s) => s.id === section.id);
  return {
    prev: i > 0 ? flat[i - 1] : undefined,
    next: i >= 0 && i < flat.length - 1 ? flat[i + 1] : undefined,
  };
}

// ─── Styles ─────────────────────────────────────────────────────────────────

function styles(): string {
  return `
    :root {
      --bock-nav-width: 260px;
      --bock-header-h: 44px;
    }
    html, body {
      margin: 0;
      padding: 0;
      height: 100%;
      color: var(--vscode-foreground);
      background: var(--vscode-editor-background);
      font-family: var(--vscode-font-family);
      font-size: var(--vscode-font-size);
      line-height: 1.55;
    }
    .bock-header {
      display: flex;
      align-items: center;
      gap: 0.5rem;
      height: var(--bock-header-h);
      padding: 0 0.75rem;
      border-bottom: 1px solid var(--vscode-panel-border);
      background: var(--vscode-sideBar-background);
      position: sticky;
      top: 0;
      z-index: 5;
    }
    .bock-header-left, .bock-header-right {
      display: flex;
      align-items: center;
      gap: 0.25rem;
    }
    .bock-header-right {
      margin-left: auto;
      color: var(--vscode-descriptionForeground);
      font-size: 0.9em;
      max-width: 40%;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .bock-iconbtn {
      background: transparent;
      color: var(--vscode-foreground);
      border: 1px solid transparent;
      border-radius: 3px;
      padding: 0.1em 0.5em;
      font-size: 1em;
      cursor: pointer;
    }
    .bock-iconbtn:hover:not(:disabled) {
      background: var(--vscode-toolbar-hoverBackground);
    }
    .bock-iconbtn:disabled {
      color: var(--vscode-disabledForeground);
      cursor: not-allowed;
    }
    .bock-search {
      flex: 1;
      max-width: 360px;
      padding: 0.25em 0.6em;
      background: var(--vscode-input-background);
      color: var(--vscode-input-foreground);
      border: 1px solid var(--vscode-input-border, transparent);
      border-radius: 3px;
      font-family: inherit;
      font-size: inherit;
    }
    .bock-search:focus {
      outline: 1px solid var(--vscode-focusBorder);
    }
    .bock-layout {
      display: grid;
      grid-template-columns: var(--bock-nav-width) 1fr;
      height: calc(100vh - var(--bock-header-h));
    }
    .bock-nav {
      border-right: 1px solid var(--vscode-panel-border);
      overflow-y: auto;
      background: var(--vscode-sideBar-background);
      padding: 0.5rem 0;
    }
    .bock-nav nav ul {
      list-style: none;
      padding-left: 0;
      margin: 0;
    }
    .bock-nav-list.depth-1 { padding-left: 0.75rem; }
    .bock-nav-list.depth-2 { padding-left: 0.75rem; }
    .bock-nav-item { margin: 0; }
    .bock-nav-link {
      display: flex;
      align-items: baseline;
      gap: 0.45em;
      padding: 0.2em 0.85em;
      color: var(--vscode-foreground);
      text-decoration: none;
      border-left: 2px solid transparent;
      font-size: 0.92em;
    }
    .bock-nav-link:hover {
      background: var(--vscode-list-hoverBackground);
    }
    .bock-nav-link.active {
      background: var(--vscode-list-activeSelectionBackground);
      color: var(--vscode-list-activeSelectionForeground);
      border-left-color: var(--vscode-focusBorder);
    }
    .bock-nav-num {
      color: var(--vscode-descriptionForeground);
      font-variant-numeric: tabular-nums;
      min-width: 2.5em;
      font-size: 0.85em;
    }
    .bock-nav-link.active .bock-nav-num {
      color: inherit;
    }
    .bock-nav-title { flex: 1; }
    .bock-nav-item.depth-0 > .bock-nav-link {
      font-weight: 600;
      margin-top: 0.25em;
    }
    .bock-results {
      border-top: 1px solid var(--vscode-panel-border);
      padding: 0.5rem 0;
      margin-top: 0.5rem;
    }
    .bock-result {
      display: block;
      padding: 0.4em 0.85em;
      color: inherit;
      text-decoration: none;
      border-left: 2px solid transparent;
    }
    .bock-result:hover {
      background: var(--vscode-list-hoverBackground);
    }
    .bock-result.active {
      background: var(--vscode-list-activeSelectionBackground);
      color: var(--vscode-list-activeSelectionForeground);
      border-left-color: var(--vscode-focusBorder);
    }
    .bock-result.active .bock-result-ref,
    .bock-result.active .bock-result-snippet {
      color: inherit;
    }
    .bock-result-title mark {
      background: var(--vscode-editor-findMatchHighlightBackground);
      color: inherit;
    }
    .bock-result-ref {
      color: var(--vscode-descriptionForeground);
      font-size: 0.85em;
      font-variant-numeric: tabular-nums;
      margin-right: 0.4em;
    }
    .bock-result-title {
      font-weight: 600;
      font-size: 0.9em;
    }
    .bock-result-snippet {
      display: block;
      color: var(--vscode-descriptionForeground);
      font-size: 0.85em;
      margin-top: 0.15em;
      overflow: hidden;
      text-overflow: ellipsis;
      white-space: nowrap;
    }
    .bock-result-snippet mark {
      background: var(--vscode-editor-findMatchHighlightBackground);
      color: inherit;
    }
    .bock-noresults {
      padding: 0.5em 0.85em;
      color: var(--vscode-descriptionForeground);
      font-style: italic;
    }
    .bock-content {
      overflow-y: auto;
      padding: 1rem 2rem 6rem;
      max-width: 960px;
    }
    .bock-section { margin-bottom: 2rem; scroll-margin-top: var(--bock-header-h); }
    .bock-section-level-2 { border-top: 1px solid var(--vscode-panel-border); padding-top: 1rem; }
    .bock-section-heading {
      display: flex;
      align-items: baseline;
      gap: 0.5em;
      color: var(--vscode-editor-foreground);
      margin-top: 0.5em;
    }
    .bock-section-level-2 .bock-section-heading { font-size: 1.5em; }
    .bock-section-level-3 .bock-section-heading { font-size: 1.2em; }
    .bock-section-level-4 .bock-section-heading { font-size: 1.05em; }
    .bock-section-num {
      color: var(--vscode-descriptionForeground);
      font-variant-numeric: tabular-nums;
    }
    .bock-section.bock-flash {
      animation: bock-flash 1.1s ease-out;
    }
    @keyframes bock-flash {
      0%   { background: var(--vscode-editor-findMatchHighlightBackground); }
      100% { background: transparent; }
    }
    .bock-content h4 { color: var(--vscode-editor-foreground); margin-top: 1em; }
    .bock-content p { margin: 0.5em 0; }
    .bock-content a, .bock-spec-ref {
      color: var(--vscode-textLink-foreground);
      text-decoration: none;
      cursor: pointer;
    }
    .bock-content a:hover, .bock-spec-ref:hover {
      color: var(--vscode-textLink-activeForeground);
      text-decoration: underline;
    }
    .bock-content code {
      font-family: var(--vscode-editor-font-family);
      background: var(--vscode-textCodeBlock-background);
      padding: 0.1em 0.35em;
      border-radius: 3px;
      font-size: 0.92em;
    }
    .bock-content pre {
      background: var(--vscode-textCodeBlock-background);
      padding: 0.85em 1em;
      border-radius: 4px;
      overflow-x: auto;
      font-family: var(--vscode-editor-font-family);
      font-size: 0.92em;
    }
    .bock-content pre code {
      background: transparent;
      padding: 0;
      font-size: inherit;
    }
    .bock-content table {
      border-collapse: collapse;
      margin: 0.75em 0;
    }
    .bock-content th, .bock-content td {
      border: 1px solid var(--vscode-panel-border);
      padding: 0.35em 0.7em;
      text-align: left;
    }
    .bock-content blockquote {
      border-left: 3px solid var(--vscode-panel-border);
      margin: 0.75em 0;
      padding: 0.1em 1em;
      color: var(--vscode-descriptionForeground);
    }
    .bock-section-nav {
      display: flex;
      justify-content: space-between;
      gap: 1em;
      margin-top: 1.5em;
      padding-top: 0.6em;
      border-top: 1px dashed var(--vscode-panel-border);
      font-size: 0.9em;
    }
    .bock-section-nav a { color: var(--vscode-textLink-foreground); text-decoration: none; }
    .bock-section-nav a:hover { text-decoration: underline; }

    /* Bock token highlighting */
    .tok-keyword { color: var(--vscode-symbolIcon-keywordForeground, #c586c0); }
    .tok-type { color: var(--vscode-symbolIcon-classForeground, #4ec9b0); }
    .tok-string { color: var(--vscode-symbolIcon-stringForeground, #ce9178); }
    .tok-number { color: var(--vscode-symbolIcon-numberForeground, #b5cea8); }
    .tok-comment { color: var(--vscode-symbolIcon-textForeground, #6a9955); font-style: italic; }
    .tok-annotation { color: var(--vscode-symbolIcon-constantForeground, #dcdcaa); }
  `;
}
