// Searchable spec side panel (F1.5.8).
//
// Loads `assets/spec/bock-spec.md` (or a user-configured override), parses its
// heading structure into a navigation tree, renders each section as HTML with
// Bock-aware code highlighting, and serves the whole thing to a webview that
// owns search, navigation, and back/forward entirely client-side.
//
// Every other feature's spec link (hover, errors, decisions, annotations,
// effects) ultimately funnels into `bock.openSpecAt §X.Y`, so this panel is
// the foundation that those links open into.

import * as vscode from 'vscode';
import { marked, Renderer } from 'marked';
import { VocabService } from '../vocab';
import { escapeHtml } from '../shared/webview';

// ─── Types ──────────────────────────────────────────────────────────────────

interface SpecSection {
  id: string; // "1", "1.1", "17.4"
  ref: string; // "§1", "§1.1"
  anchor: string; // "section-1-1"
  title: string;
  level: number; // 2 or 3
  html: string; // rendered body HTML (excludes the heading itself)
  text: string; // plain text for search
}

interface NavNode {
  id: string;
  ref: string;
  anchor: string;
  title: string;
  children: NavNode[];
}

interface SpecIndex {
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
    }
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

function parseSections(source: string): SpecSection[] {
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

function buildNavTree(sections: SpecSection[]): NavNode[] {
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

function normalizeRef(ref: string, index: SpecIndex): string | undefined {
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
  const baseCode = renderer.code.bind(renderer);
  renderer.code = (code: string, infostring?: string) => {
    const lang = (infostring ?? '').split(/\s+/)[0];
    if (lang === 'bock') {
      return `<pre class="bock-code"><code class="lang-bock">${highlightBock(code)}</code></pre>`;
    }
    return baseCode(code, infostring, false);
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

function highlightBock(src: string): string {
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

function linkifySpecRefs(html: string): string {
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

function stripForSearch(md: string): string {
  return md
    .replace(/```[\s\S]*?```/g, ' ')
    .replace(/`[^`]*`/g, ' ')
    .replace(/!\[[^\]]*\]\([^)]*\)/g, ' ')
    .replace(/\[([^\]]+)\]\([^)]*\)/g, '$1')
    .replace(/[*_#>|]/g, ' ')
    .replace(/\s+/g, ' ')
    .trim();
}

// ─── HTML rendering ─────────────────────────────────────────────────────────

function renderHtml(index: SpecIndex): string {
  const nonce = randomNonce();
  const csp = [
    "default-src 'none'",
    `script-src 'nonce-${nonce}'`,
    `style-src 'unsafe-inline'`,
    `font-src 'self'`,
  ].join('; ');

  const navHtml = renderNav(index.tree);
  const contentHtml = index.sections
    .map((s) => renderSectionHtml(s, index))
    .join('\n');
  const searchIndex = index.sections.map((s) => ({
    id: s.id,
    ref: s.ref,
    anchor: s.anchor,
    title: s.title,
    text: s.text,
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
      <nav>${navHtml}</nav>
      <div class="bock-results" id="bock-results" hidden></div>
    </aside>
    <main class="bock-content" id="bock-content">
      ${contentHtml}
    </main>
  </div>
  <script nonce="${nonce}">
    (function () {
      const vscode = acquireVsCodeApi();
      const sections = ${JSON.stringify(searchIndex)};
      const byId = new Map(sections.map((s) => [s.id, s]));
      const navEl = document.getElementById('bock-nav');
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

      // Search
      function renderResults(query) {
        if (!query) {
          resultsEl.hidden = true;
          resultsEl.innerHTML = '';
          return;
        }
        const q = query.toLowerCase();
        const hits = [];
        for (const s of sections) {
          const hay = (s.title + ' ' + s.text).toLowerCase();
          const idx = hay.indexOf(q);
          if (idx === -1) continue;
          const snippetSource = s.text;
          const snipIdx = snippetSource.toLowerCase().indexOf(q);
          let snippet = '';
          if (snipIdx !== -1) {
            const start = Math.max(0, snipIdx - 40);
            const end = Math.min(snippetSource.length, snipIdx + q.length + 60);
            snippet = (start > 0 ? '…' : '') +
              snippetSource.slice(start, end) +
              (end < snippetSource.length ? '…' : '');
          } else {
            snippet = s.title;
          }
          hits.push({ id: s.id, ref: s.ref, title: s.title, snippet });
          if (hits.length >= 40) break;
        }
        if (hits.length === 0) {
          resultsEl.innerHTML = '<p class="bock-noresults">No matches for "' +
            escapeHtmlClient(query) + '"</p>';
        } else {
          resultsEl.innerHTML = hits.map((h) => (
            '<a href="#" class="bock-result" data-id="' + h.id + '">' +
            '<span class="bock-result-ref">' + escapeHtmlClient(h.ref) + '</span>' +
            '<span class="bock-result-title">' + escapeHtmlClient(h.title) + '</span>' +
            '<span class="bock-result-snippet">' +
              highlightQuery(h.snippet, query) + '</span></a>'
          )).join('');
        }
        resultsEl.hidden = false;
      }

      function highlightQuery(text, q) {
        const escQ = q.replace(/[.*+?^$\\{}()|[\\]]/g, '\\\\$&');
        const safe = escapeHtmlClient(text);
        try {
          return safe.replace(new RegExp(escQ, 'gi'), (m) =>
            '<mark>' + m + '</mark>'
          );
        } catch (_) {
          return safe;
        }
      }

      function escapeHtmlClient(s) {
        return String(s)
          .replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
          .replace(/"/g, '&quot;').replace(/'/g, '&#39;');
      }

      let searchTimer = null;
      searchEl.addEventListener('input', () => {
        if (searchTimer) clearTimeout(searchTimer);
        searchTimer = setTimeout(() => renderResults(searchEl.value.trim()), 80);
      });

      resultsEl.addEventListener('click', (e) => {
        const a = e.target.closest('.bock-result');
        if (!a) return;
        e.preventDefault();
        const id = a.getAttribute('data-id');
        if (id) {
          scrollToSection(id, true);
          searchEl.value = '';
          resultsEl.hidden = true;
          resultsEl.innerHTML = '';
        }
      });

      // Ctrl/Cmd+F focuses the search box
      window.addEventListener('keydown', (e) => {
        if ((e.ctrlKey || e.metaKey) && e.key === 'f') {
          e.preventDefault();
          searchEl.focus();
          searchEl.select();
        } else if (e.key === 'Escape' && document.activeElement === searchEl) {
          searchEl.value = '';
          renderResults('');
          searchEl.blur();
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

function randomNonce(): string {
  const chars =
    'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789';
  let out = '';
  for (let i = 0; i < 32; i++) {
    out += chars.charAt(Math.floor(Math.random() * chars.length));
  }
  return out;
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
