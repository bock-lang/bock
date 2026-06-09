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
  rankSpecSections,
  renderHighlighted,
  SEARCH_RESULT_LIMIT,
  type SpecSection,
  type SpecIndex,
  type SpecSearchHit,
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

// ─── rankSpecSections ────────────────────────────────────────────────────────

/**
 * Build a SpecSection with searchable content. Using the full SpecSection
 * shape (rather than a bare SearchableSection literal) also pins that
 * SpecSection stays structurally assignable to the ranking input.
 */
function searchable(
  id: string,
  title: string,
  text: string,
  level = 2,
): SpecSection {
  return {
    id,
    ref: `§${id}`,
    anchor: `section-${id.replace(/\./g, '-')}`,
    title,
    level,
    html: '',
    text,
  };
}

/** Shorthand: rank and return just the hit ids, in order. */
function rankIds(
  sections: SpecSection[],
  query: string,
  limit?: number,
): string[] {
  return rankSpecSections(sections, query, limit).map((h) => h.id);
}

describe('spec-panel.rankSpecSections', () => {
  it('returns [] for an empty or whitespace-only query', () => {
    const sections = [searchable('1', 'Effects', 'effect rows')];
    assert.deepEqual(rankSpecSections(sections, ''), []);
    assert.deepEqual(rankSpecSections(sections, '   \t '), []);
  });

  it('returns [] when no section matches', () => {
    const sections = [searchable('1', 'Effects', 'effect rows')];
    assert.deepEqual(rankSpecSections(sections, 'zebra'), []);
  });

  it('matches case-insensitively', () => {
    const sections = [searchable('1', 'Effects', 'about effect rows')];
    assert.deepEqual(rankIds(sections, 'EFFECT'), ['1']);
    assert.deepEqual(rankIds([searchable('2', 'UPPER TITLE', '')], 'upper'), [
      '2',
    ]);
  });

  it('ranks a title match above a body match', () => {
    const sections = [
      // §1 mentions the term only in its body — at position 0 with an exact
      // word boundary, i.e. the strongest possible body hit.
      searchable('1', 'Introduction', 'handlers are described here'),
      searchable('2', 'Handlers', 'something else entirely'),
    ];
    assert.deepEqual(rankIds(sections, 'handlers'), ['2', '1']);
  });

  it('ranks an exact word-boundary title match above a substring title match', () => {
    const sections = [
      searchable('1', 'Cranky parser notes', ''), // "rank" mid-word substring
      searchable('2', 'Rank rules', ''), // exact word
    ];
    assert.deepEqual(rankIds(sections, 'rank'), ['2', '1']);
  });

  it('ranks an exact word above a word-prefix above a substring (title)', () => {
    const sections = [
      searchable('1', 'Cranky', ''), // substring
      searchable('2', 'Effective ranking', ''), // "rank" is a word prefix of "ranking"
      searchable('3', 'Rank', ''), // exact word
    ];
    assert.deepEqual(rankIds(sections, 'rank'), ['3', '2', '1']);
  });

  it('ranks an exact word above a substring in the body too', () => {
    const sections = [
      searchable('1', 'Alpha', 'the cranky tokenizer'),
      searchable('2', 'Beta', 'the rank computation'),
    ];
    assert.deepEqual(rankIds(sections, 'rank'), ['2', '1']);
  });

  it('applies AND semantics across whitespace-separated terms', () => {
    const sections = [
      searchable('1', 'Effects', 'no handlers mentioned... wait, yes: handler'),
      searchable('2', 'Effects', 'plain body text'),
      searchable('3', 'Types', 'unrelated'),
    ];
    // §2 has "effects" but no "handler"; §3 has neither.
    assert.deepEqual(rankIds(sections, 'effect handler'), ['1']);
  });

  it('lets different terms match in title vs body (AND still satisfied)', () => {
    const sections = [
      searchable('1', 'Effect rows', 'a handler resumes the computation'),
    ];
    assert.deepEqual(rankIds(sections, 'effect handler'), ['1']);
  });

  it('gives earlier body matches a position bonus', () => {
    const filler = 'lorem ipsum dolor sit amet '.repeat(40); // > 1000 chars, no term
    const sections = [
      searchable('1', 'Alpha', filler + 'resume appears late'),
      searchable('2', 'Beta', 'resume appears immediately'),
    ];
    assert.deepEqual(rankIds(sections, 'resume'), ['2', '1']);
  });

  it('gives shallower headings a bonus (level 2 over level 3 over level 4)', () => {
    const sections = [
      searchable('1.1.1', 'Guards', 'guard clauses', 4),
      searchable('1.1', 'Guards', 'guard clauses', 3),
      searchable('1', 'Guards', 'guard clauses', 2),
    ];
    assert.deepEqual(rankIds(sections, 'guards'), ['1', '1.1', '1.1.1']);
  });

  it('breaks score ties by document order (deterministic, stable)', () => {
    const sections = [
      searchable('7', 'Match', 'same body', 2),
      searchable('3', 'Match', 'same body', 2),
      searchable('9', 'Match', 'same body', 2),
    ];
    // Identical scores — input (document) order must be preserved.
    assert.deepEqual(rankIds(sections, 'match'), ['7', '3', '9']);
  });

  it('respects the limit parameter and the default cap', () => {
    const many = Array.from({ length: 50 }, (_, i) =>
      searchable(String(i + 1), 'Match', 'match body'),
    );
    assert.equal(rankSpecSections(many, 'match', 5).length, 5);
    assert.equal(rankSpecSections(many, 'match').length, SEARCH_RESULT_LIMIT);
    assert.deepEqual(rankSpecSections(many, 'match', 0), []);
  });

  it('builds a snippet window around the first body match with ellipsis flags', () => {
    const before = 'b'.repeat(100);
    const after = 'a'.repeat(100);
    const sections = [searchable('1', 'Alpha', `${before} resume ${after}`)];
    const [hit] = rankSpecSections(sections, 'resume');
    assert.ok(hit.snippet.includes('resume'));
    assert.equal(hit.snippetEllipsisStart, true);
    assert.equal(hit.snippetEllipsisEnd, true);
    // Window: 40 chars before the match, 60 after it.
    assert.ok(hit.snippet.length <= 40 + 'resume'.length + 60);
  });

  it('reports snippet match offsets that index into the returned snippet', () => {
    const sections = [
      searchable('1', 'Alpha', 'x'.repeat(80) + ' the resume keyword resumes'),
    ];
    const [hit] = rankSpecSections(sections, 'resume');
    assert.ok(hit.snippetMatches.length >= 1);
    for (const [start, end] of hit.snippetMatches) {
      assert.equal(hit.snippet.slice(start, end).toLowerCase(), 'resume');
    }
  });

  it('reports title match offsets that index into the title', () => {
    const sections = [searchable('1', 'Effect handlers and effects', '')];
    const [hit] = rankSpecSections(sections, 'effect');
    assert.ok(hit.titleMatches.length >= 2);
    for (const [start, end] of hit.titleMatches) {
      assert.equal(hit.title.slice(start, end).toLowerCase(), 'effect');
    }
  });

  it('merges overlapping term spans into one highlight range', () => {
    // "foo" and "oo" overlap inside "foo" — a single merged span [0, 3).
    const sections = [searchable('1', 'foo bar', 'foo here')];
    const [hit] = rankSpecSections(sections, 'foo oo');
    assert.deepEqual(hit.titleMatches[0], [0, 3]);
  });

  it('falls back to the opening of the body for a title-only match', () => {
    const long = 'opening words of the section body. ' + 'z'.repeat(200);
    const sections = [searchable('1', 'Pattern matching', long)];
    const [hit] = rankSpecSections(sections, 'pattern');
    assert.ok(hit.snippet.startsWith('opening words'));
    assert.equal(hit.snippetEllipsisStart, false);
    assert.equal(hit.snippetEllipsisEnd, true);
    assert.deepEqual(hit.snippetMatches, []);
  });

  it('returns an empty snippet when the matched section has no body text', () => {
    const sections = [searchable('1', 'Pattern matching', '')];
    const [hit]: SpecSearchHit[] = rankSpecSections(sections, 'pattern');
    assert.equal(hit.snippet, '');
    assert.equal(hit.snippetEllipsisStart, false);
    assert.equal(hit.snippetEllipsisEnd, false);
    assert.deepEqual(hit.snippetMatches, []);
  });
});

// ─── renderHighlighted ───────────────────────────────────────────────────────

describe('spec-panel.renderHighlighted', () => {
  it('returns plain escaped text when there are no spans', () => {
    assert.equal(renderHighlighted('a < b & c', []), 'a &lt; b &amp; c');
  });

  it('wraps a span in <mark>', () => {
    assert.equal(
      renderHighlighted('the resume keyword', [[4, 10]]),
      'the <mark>resume</mark> keyword',
    );
  });

  it('handles spans at the very start and end of the text', () => {
    assert.equal(
      renderHighlighted('resume it', [[0, 6]]),
      '<mark>resume</mark> it',
    );
    assert.equal(
      renderHighlighted('to resume', [[3, 9]]),
      'to <mark>resume</mark>',
    );
  });

  it('renders multiple spans in order', () => {
    assert.equal(
      renderHighlighted('aa bb cc', [
        [0, 2],
        [6, 8],
      ]),
      '<mark>aa</mark> bb <mark>cc</mark>',
    );
  });

  it('escapes HTML both inside and outside marked spans (no injection)', () => {
    // The span covers the <script> tag itself — it must come out escaped.
    const out = renderHighlighted('x <script>alert(1)</script> y', [[2, 27]]);
    assert.ok(!out.includes('<script>'));
    assert.equal(
      out,
      'x <mark>&lt;script&gt;alert(1)&lt;/script&gt;</mark> y',
    );
  });

  it('skips malformed spans (inverted, overlapping, out of range) defensively', () => {
    assert.equal(renderHighlighted('abcdef', [[4, 2]]), 'abcdef');
    assert.equal(renderHighlighted('abcdef', [[2, 99]]), 'abcdef');
    assert.equal(
      renderHighlighted('abcdef', [
        [0, 3],
        [2, 4], // overlaps the previous span — skipped
      ]),
      '<mark>abc</mark>def',
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
