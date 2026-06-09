// Unit tests for the pure helpers in src/features/spec-panel.ts.
//
// spec-panel.ts powers the searchable spec side panel (F1.5.8). Its rendering
// pipeline relies on a handful of pure string helpers — heading parsing, the
// nav-tree builder, the `§`-ref normalizer/linkifier, the search-text stripper,
// and a small single-pass Bock tokenizer. None of these touch the live `vscode`
// API at call time, so they can be exercised headlessly. The module *imports*
// `vscode` and `marked` at the top level, but the ts-node CommonJS harness
// (test/tsconfig.json overrides module/moduleResolution to `commonjs`/`node`)
// loads both fine under Node's require(ESM) interop, so we can test in place
// without extracting a marked-free module.

import * as assert from 'node:assert/strict';
import {
  normalizeRef,
  buildNavTree,
  highlightBock,
  linkifySpecRefs,
  stripForSearch,
  parseSections,
  type SpecSection,
  type SpecIndex,
} from '../src/features/spec-panel';

// ─── Test helpers ────────────────────────────────────────────────────────────

/** Build a minimal SpecSection from just an id (the field the helpers read). */
function section(id: string): SpecSection {
  return {
    id,
    ref: `§${id}`,
    anchor: `section-${id.replace(/\./g, '-')}`,
    title: `Section ${id}`,
    level: id.includes('.') ? 3 : 2,
    html: '',
    text: '',
  };
}

/** Build a SpecIndex whose `sections` carry the given ids. */
function indexOf(...ids: string[]): SpecIndex {
  const sections = ids.map(section);
  return { sections, tree: [], sourcePath: '/spec.md' };
}

// ─── normalizeRef ────────────────────────────────────────────────────────────

describe('spec-panel.normalizeRef', () => {
  const index = indexOf('1', '6', '6.7', '17', '17.4');

  it('strips a leading § and returns the bare id', () => {
    assert.equal(normalizeRef('§6.7', index), '6.7');
  });

  it('accepts an id with no § prefix', () => {
    assert.equal(normalizeRef('17.4', index), '17.4');
  });

  it('trims trailing punctuation and whitespace', () => {
    // A ref pulled from prose often carries a trailing period/paren/comma.
    assert.equal(normalizeRef('§6.7).', index), '6.7');
    assert.equal(normalizeRef('§17 ', index), '17');
    assert.equal(normalizeRef('§6,', index), '6');
  });

  it('falls back to the nearest existing parent when the exact id is absent', () => {
    // §17.99 does not exist, but §17 does — the panel still navigates there.
    assert.equal(normalizeRef('§17.99', index), '17');
  });

  it('keeps stripping components until a hit is found', () => {
    // 6.7.3.1 -> 6.7.3 -> 6.7 (first existing ancestor).
    assert.equal(normalizeRef('§6.7.3.1', index), '6.7');
  });

  it('returns undefined when no ancestor exists at all', () => {
    assert.equal(normalizeRef('§99.1', index), undefined);
  });

  it('returns undefined for an empty / §-only ref', () => {
    assert.equal(normalizeRef('§', index), undefined);
    assert.equal(normalizeRef('   ', index), undefined);
  });
});

// ─── buildNavTree ────────────────────────────────────────────────────────────

describe('spec-panel.buildNavTree', () => {
  it('nests children under their numeric parent', () => {
    const tree = buildNavTree([
      section('1'),
      section('1.1'),
      section('1.2'),
      section('2'),
    ]);
    assert.equal(tree.length, 2);
    assert.equal(tree[0].id, '1');
    assert.deepEqual(
      tree[0].children.map((c) => c.id),
      ['1.1', '1.2'],
    );
    assert.equal(tree[1].id, '2');
    assert.equal(tree[1].children.length, 0);
  });

  it('nests grandchildren under children (multi-level)', () => {
    const tree = buildNavTree([
      section('3'),
      section('3.1'),
      section('3.1.1'),
    ]);
    assert.equal(tree.length, 1);
    assert.equal(tree[0].children.length, 1);
    assert.equal(tree[0].children[0].id, '3.1');
    assert.equal(tree[0].children[0].children[0].id, '3.1.1');
  });

  it('promotes an orphan (missing parent) to a root', () => {
    // §4.2 with no §4 present cannot be nested — it becomes a top-level node
    // rather than being dropped.
    const tree = buildNavTree([section('4.2'), section('5')]);
    assert.deepEqual(
      tree.map((n) => n.id),
      ['4.2', '5'],
    );
  });

  it('copies title/ref/anchor onto each node', () => {
    const [root] = buildNavTree([section('7')]);
    assert.equal(root.ref, '§7');
    assert.equal(root.anchor, 'section-7');
    assert.equal(root.title, 'Section 7');
  });
});

// ─── highlightBock ───────────────────────────────────────────────────────────

describe('spec-panel.highlightBock', () => {
  it('wraps keywords in tok-keyword', () => {
    assert.equal(
      highlightBock('let'),
      '<span class="tok-keyword">let</span>',
    );
  });

  it('wraps known and capitalized types in tok-type', () => {
    // `String` is a known builtin; `Foo` is treated as a type by the
    // leading-uppercase heuristic.
    assert.equal(
      highlightBock('String'),
      '<span class="tok-type">String</span>',
    );
    assert.equal(highlightBock('Foo'), '<span class="tok-type">Foo</span>');
  });

  it('leaves plain lowercase identifiers unclassified (but escaped)', () => {
    assert.equal(highlightBock('x'), 'x');
  });

  it('wraps a double-quoted string in tok-string and escapes its quotes', () => {
    assert.equal(
      highlightBock('"hello"'),
      '<span class="tok-string">&quot;hello&quot;</span>',
    );
  });

  it('treats a ${…} interpolation as part of one string span (no sub-tokenizing)', () => {
    // The implementation comment mentions ${...} interpolation, but the
    // tokenizer consumes the whole `"…"` as a single string span — the
    // interpolation braces are NOT separately highlighted. Pin that behavior.
    assert.equal(
      highlightBock('"hi ${name}!"'),
      '<span class="tok-string">&quot;hi ${name}!&quot;</span>',
    );
  });

  it('wraps a char literal in tok-string', () => {
    assert.equal(
      highlightBock("'a'"),
      '<span class="tok-string">&#39;a&#39;</span>',
    );
  });

  it('handles an escaped char literal without terminating early', () => {
    assert.equal(
      highlightBock("'\\n'"),
      '<span class="tok-string">&#39;\\n&#39;</span>',
    );
  });

  it('wraps a // line comment in tok-comment up to the newline', () => {
    assert.equal(
      highlightBock('// note\nfn'),
      '<span class="tok-comment">// note</span>\n' +
        '<span class="tok-keyword">fn</span>',
    );
  });

  it('wraps a /* block comment */ in tok-comment', () => {
    assert.equal(
      highlightBock('/* c */'),
      '<span class="tok-comment">/* c */</span>',
    );
  });

  it('wraps a number (with underscores and exponent) in tok-number', () => {
    assert.equal(
      highlightBock('1_000.5e-3'),
      '<span class="tok-number">1_000.5e-3</span>',
    );
  });

  it('wraps an @annotation in tok-annotation', () => {
    assert.equal(
      highlightBock('@managed'),
      '<span class="tok-annotation">@managed</span>',
    );
  });

  it('consumes an UNTERMINATED string to end-of-input without crashing', () => {
    // A fenced block in the spec may show an intentionally-broken snippet.
    // The scanner must not loop forever or throw; it emits one string span.
    assert.equal(
      highlightBock('"oops no close'),
      '<span class="tok-string">&quot;oops no close</span>',
    );
  });

  it('consumes an UNTERMINATED block comment to end-of-input', () => {
    assert.equal(
      highlightBock('/* open'),
      '<span class="tok-comment">/* open</span>',
    );
  });

  it('HTML-escapes <, >, & punctuation passthrough', () => {
    assert.equal(highlightBock('a < b > c & d'), 'a &lt; b &gt; c &amp; d');
  });
});

// ─── linkifySpecRefs ─────────────────────────────────────────────────────────

describe('spec-panel.linkifySpecRefs', () => {
  it('linkifies a §ref appearing in prose', () => {
    assert.equal(
      linkifySpecRefs('See §6.7 here.'),
      'See <a class="bock-spec-ref" data-ref="§6.7" ' +
        'href="#section-6-7">§6.7</a> here.',
    );
  });

  it('does NOT linkify a §ref inside a <pre> block', () => {
    const input = '<pre>code §6.7 ref</pre>';
    assert.equal(linkifySpecRefs(input), input);
  });

  it('linkifies prose refs while leaving <pre>-block refs untouched (mixed)', () => {
    const out = linkifySpecRefs('§1 <pre>§2 in code</pre> §3.4');
    // §1 and §3.4 become anchors; the §2 inside <pre> stays literal.
    assert.ok(out.includes('data-ref="§1"'));
    assert.ok(out.includes('data-ref="§3.4"'));
    assert.ok(out.includes('<pre>§2 in code</pre>'));
    assert.ok(!out.includes('data-ref="§2"'));
  });
});

// ─── stripForSearch ──────────────────────────────────────────────────────────

describe('spec-panel.stripForSearch', () => {
  it('drops fenced code blocks entirely', () => {
    const md = 'before\n```bock\nlet x = 1\n```\nafter';
    assert.equal(stripForSearch(md), 'before after');
  });

  it('drops inline `code` spans', () => {
    assert.equal(stripForSearch('use the `foo` function'), 'use the function');
  });

  it('keeps link text but drops the URL', () => {
    assert.equal(
      stripForSearch('see [the spec](https://x/y) now'),
      'see the spec now',
    );
  });

  it('drops image markup', () => {
    assert.equal(stripForSearch('a ![alt](img.png) b'), 'a b');
  });

  it('strips markdown punctuation and collapses whitespace', () => {
    assert.equal(
      stripForSearch('# Heading\n\n*bold*  and   _em_'),
      'Heading bold and em',
    );
  });
});

// ─── parseSections (uses marked) ─────────────────────────────────────────────

describe('spec-panel.parseSections', () => {
  it('parses a normal numbered ## heading into a section', () => {
    const out = parseSections('## 1. Introduction\n\nHello world.\n');
    assert.equal(out.length, 1);
    const s = out[0];
    assert.equal(s.id, '1');
    assert.equal(s.ref, '§1');
    assert.equal(s.anchor, 'section-1');
    assert.equal(s.title, 'Introduction');
    assert.equal(s.level, 2);
    assert.ok(s.html.includes('Hello world'));
    assert.equal(s.text, 'Hello world.');
  });

  it('parses nested ### / #### headings with dotted ids and correct levels', () => {
    const md = ['## 2. Top', '### 2.1 Mid', '#### 2.1.1 Deep'].join('\n');
    const out = parseSections(md);
    assert.deepEqual(
      out.map((s) => [s.id, s.level]),
      [
        ['2', 2],
        ['2.1', 3],
        ['2.1.1', 4],
      ],
    );
  });

  it('falls back to "Section N" when a numbered heading has no title', () => {
    // `## 3` matches HEADING_RE with an empty title group.
    const out = parseSections('## 3\n\nbody');
    assert.equal(out.length, 1);
    assert.equal(out[0].id, '3');
    assert.equal(out[0].title, 'Section 3');
  });

  it('does NOT treat an UNnumbered ## heading as a section (HEADING_RE requires a number)', () => {
    // `## Appendix` has no leading number, so HEADING_RE fails to match it.
    // It is therefore absorbed into the preceding section's body rather than
    // starting a new section. (If it appears before any numbered heading it
    // is dropped — see the next test.)
    const md = '## 1. Intro\n\nIntro body.\n\n## Appendix\n\nAppendix body.';
    const out = parseSections(md);
    assert.equal(out.length, 1);
    assert.equal(out[0].id, '1');
    // The unnumbered "Appendix" text lands inside §1's rendered body.
    assert.ok(out[0].html.includes('Appendix'));
    assert.ok(out[0].text.includes('Appendix'));
  });

  it('SILENTLY DROPS content appearing before the first numbered heading', () => {
    // DOCUMENTING KNOWN BEHAVIOR (not a fix): parseSections only accumulates
    // body lines once `current` is set, i.e. after the first numbered heading.
    // Any preamble before that heading is discarded — it appears in no
    // section. This is acceptable for the bundled spec (which opens with a
    // numbered heading) but is a latent surprise for arbitrary markdown.
    const md = [
      'Preamble paragraph that precedes any heading.',
      '',
      '## 1. Intro',
      '',
      'Real body.',
    ].join('\n');
    const out = parseSections(md);
    assert.equal(out.length, 1);
    assert.equal(out[0].id, '1');
    // The preamble is nowhere to be found.
    assert.ok(!out[0].text.includes('Preamble'));
    assert.ok(!out[0].html.includes('Preamble'));
  });

  it('linkifies §refs in rendered prose but not inside fenced code', () => {
    const md = [
      '## 4. Refs',
      '',
      'See §6.7 for details.',
      '',
      '```bock',
      'let x = "see §9.9"',
      '```',
    ].join('\n');
    const [s] = parseSections(md);
    // The prose §6.7 became an anchor...
    assert.ok(s.html.includes('data-ref="§6.7"'));
    // ...while the §9.9 inside the highlighted <pre> block did not.
    assert.ok(!s.html.includes('data-ref="§9.9"'));
  });
});
