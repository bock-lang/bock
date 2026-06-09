// Unit tests for the pure hover-rendering helpers in
// src/features/hover-render.ts.
//
// These are the dependency-free pieces of the hover provider: the LSP
// contents stringifier, the spec-link formatter, the vocab → cache builder,
// and the `render*` markdown builders. They are tested directly (not through
// `hover.ts`, which transitively imports `vscode-languageclient`, a package
// whose `exports` subpath the headless ts-node CommonJS resolver can't
// follow).

import * as assert from 'node:assert/strict';
import {
  stringifyHoverContents,
  specLink,
  buildCache,
  buildBuiltinMethodIndex,
  buildOperatorRegex,
  renderAnnotation,
  renderKeyword,
  renderOperator,
  renderPrimitive,
  renderPrelude,
  renderStdlibSymbol,
  renderBuiltinMethod,
  renderEffectUsage,
  renderEffectOperation,
} from '../src/features/hover-render';
import type {
  Vocab,
  Annotation,
  Keyword,
  Operator,
  PrimitiveType,
  Symbol as VocabSymbol,
} from '../src/shared/types';

// ─── A minimal but well-formed Vocab fixture ────────────────────────────────

function emptyVocab(): Vocab {
  return {
    version: 'test',
    language: {
      keywords: [],
      operators: [],
      annotations: [],
      strictness_levels: [],
      primitive_types: [],
      prelude_types: [],
      prelude_functions: [],
      prelude_traits: [],
      prelude_constructors: [],
    },
    stdlib: { modules: [], builtin_methods: [], builtin_globals: [] },
    diagnostics: { codes: [] },
    tooling: { targets: [], ai_providers: [], commands: [] },
  };
}

function sym(name: string, extra: Partial<VocabSymbol> = {}): VocabSymbol {
  return { name, kind: 'function', signature: '', ...extra };
}

function op(symbol: string, extra: Partial<Operator> = {}): Operator {
  return { symbol, associativity: 'none', kind: 'arithmetic', ...extra };
}

// ─── stringifyHoverContents ─────────────────────────────────────────────────

describe('hover-render.stringifyHoverContents', () => {
  it('returns undefined for a missing/empty payload', () => {
    assert.equal(stringifyHoverContents(undefined), undefined);
    // An empty string is falsy and therefore short-circuits to undefined.
    assert.equal(stringifyHoverContents(''), undefined);
  });

  it('passes a bare string through unchanged', () => {
    assert.equal(stringifyHoverContents('type Foo = Int'), 'type Foo = Int');
  });

  it('reads .value out of a single MarkedString object', () => {
    assert.equal(
      stringifyHoverContents({ language: 'bock', value: 'fn f() -> Int' }),
      'fn f() -> Int',
    );
  });

  it('joins an array of strings and MarkedString objects with blank lines', () => {
    const out = stringifyHoverContents([
      'first',
      { language: 'bock', value: 'second' },
      'third',
    ]);
    assert.equal(out, 'first\n\nsecond\n\nthird');
  });

  it('returns undefined for an unrecognised object shape', () => {
    // No `value` key — falls through to the final undefined.
    assert.equal(
      stringifyHoverContents({ language: 'bock' } as unknown as never),
      undefined,
    );
  });
});

// ─── specLink ───────────────────────────────────────────────────────────────

describe('hover-render.specLink', () => {
  it('URI-encodes the ref into the command argument', () => {
    const link = specLink('§8.2', true);
    assert.ok(link, 'expected a link');
    // The ref appears verbatim in the visible label…
    assert.ok(link.startsWith('[§8.2 →]('));
    // …and JSON-then-URI-encoded in the command argument.
    const expectedArg = encodeURIComponent(JSON.stringify(['§8.2']));
    assert.ok(
      link.includes(`command:bock.openSpecAt?${expectedArg}`),
      `link did not contain the encoded arg: ${link}`,
    );
    // The `§` must be percent-encoded in the argument, not raw.
    assert.ok(!link.includes(`?§`), 'the argument must be encoded');
  });

  it('short-circuits to undefined when links are disabled', () => {
    assert.equal(specLink('§8.2', false), undefined);
  });

  it('short-circuits to undefined for an empty ref', () => {
    assert.equal(specLink('', true), undefined);
  });
});

// ─── buildCache ─────────────────────────────────────────────────────────────

describe('hover-render.buildCache', () => {
  it('strips the leading @ from annotation keys', () => {
    const v = emptyVocab();
    const managed: Annotation = {
      name: '@managed',
      params: '',
      purpose: 'managed by the AI',
    };
    const bare: Annotation = {
      name: 'context',
      params: 'text',
      purpose: 'context block',
    };
    v.language.annotations = [managed, bare];
    const cache = buildCache(v);
    // Keyed by the bare name, regardless of the source `@` prefix.
    assert.equal(cache.annotations.get('managed'), managed);
    assert.equal(cache.annotations.get('context'), bare);
    assert.equal(cache.annotations.has('@managed'), false);
  });

  it('accumulates multiple stdlib hits for the same symbol name', () => {
    const v = emptyVocab();
    v.stdlib.modules = [
      {
        path: 'std.io',
        types: [],
        functions: [sym('read')],
        effects: [],
        traits: [],
      },
      {
        path: 'std.net',
        types: [],
        functions: [sym('read')],
        effects: [],
        traits: [],
      },
    ];
    const cache = buildCache(v);
    const hits = cache.stdlibSymbols.get('read');
    assert.ok(hits, 'expected hits for "read"');
    assert.equal(hits.length, 2);
    assert.deepEqual(
      hits.map((h) => h.module.path).sort(),
      ['std.io', 'std.net'],
    );
    assert.deepEqual(
      hits.map((h) => h.kind),
      ['function', 'function'],
    );
  });

  it('records effect names and tags effect symbols as a stdlib hit', () => {
    const v = emptyVocab();
    v.stdlib.modules = [
      {
        path: 'std.async',
        types: [],
        functions: [],
        effects: [sym('Async', { kind: 'effect' })],
        traits: [],
      },
    ];
    const cache = buildCache(v);
    assert.ok(cache.effectNames.has('Async'));
    const hits = cache.stdlibSymbols.get('Async');
    assert.ok(hits);
    assert.equal(hits[0].kind, 'effect');
  });

  it('indexes prelude buckets by name', () => {
    const v = emptyVocab();
    v.language.prelude_types = [sym('Option', { kind: 'type' })];
    v.language.prelude_functions = [sym('print')];
    const cache = buildCache(v);
    assert.equal(cache.preludeTypes.get('Option')?.name, 'Option');
    assert.equal(cache.preludeFunctions.get('print')?.name, 'print');
  });

  it('indexes builtin methods by name → receiver list', () => {
    const v = emptyVocab();
    v.stdlib.builtin_methods = [
      { receiver: 'List', methods: ['len', 'map'] },
      { receiver: 'String', methods: ['len'] },
    ];
    const cache = buildCache(v);
    assert.deepEqual(cache.builtinMethods.get('len'), ['List', 'String']);
    assert.deepEqual(cache.builtinMethods.get('map'), ['List']);
    assert.equal(cache.builtinMethods.get('nope'), undefined);
  });

  it('builds an operator word pattern from the vocab symbols', () => {
    const v = emptyVocab();
    v.language.operators = [op('+'), op('+=')];
    const cache = buildCache(v);
    assert.ok(cache.operatorRegex, 'expected a regex for a non-empty operator list');
    assert.equal(cache.operatorRegex.exec('+=')?.[0], '+=');
  });

  it('leaves operatorRegex undefined on an operator-free vocab', () => {
    assert.equal(buildCache(emptyVocab()).operatorRegex, undefined);
  });
});

// ─── buildBuiltinMethodIndex ────────────────────────────────────────────────

describe('hover-render.buildBuiltinMethodIndex', () => {
  it('collects every receiver exposing a shared method, in group order', () => {
    const index = buildBuiltinMethodIndex([
      { receiver: 'List', methods: ['len', 'is_empty'] },
      { receiver: 'Map', methods: ['len'] },
      { receiver: 'String', methods: ['len'] },
    ]);
    assert.deepEqual(index.get('len'), ['List', 'Map', 'String']);
    assert.deepEqual(index.get('is_empty'), ['List']);
  });

  it('returns an empty map for no groups', () => {
    assert.equal(buildBuiltinMethodIndex([]).size, 0);
  });

  it('has no entry for a method no receiver exposes', () => {
    const index = buildBuiltinMethodIndex([{ receiver: 'List', methods: ['len'] }]);
    assert.equal(index.get('regex_find'), undefined);
  });

  it('dedups a receiver that lists the same method twice', () => {
    const index = buildBuiltinMethodIndex([
      { receiver: 'List', methods: ['len', 'len'] },
    ]);
    assert.deepEqual(index.get('len'), ['List']);
  });
});

// ─── buildOperatorRegex ─────────────────────────────────────────────────────

describe('hover-render.buildOperatorRegex', () => {
  it('returns undefined for an empty / all-empty symbol list', () => {
    assert.equal(buildOperatorRegex([]), undefined);
    assert.equal(buildOperatorRegex(['']), undefined);
  });

  it('escapes regex metacharacters in operator symbols', () => {
    const re = buildOperatorRegex(['**']);
    assert.ok(re);
    assert.equal(re.source, '\\*\\*');
    assert.equal(re.exec('a ** b')?.[0], '**');
  });

  it('orders alternatives longest-first so maximal munch wins', () => {
    // `+` listed before `+=`: without the length sort, JS alternation would
    // stop at `+` when scanning `+=`.
    const re = buildOperatorRegex(['+', '+=']);
    assert.ok(re);
    assert.equal(re.exec('+=')?.[0], '+=');
    // Same for the three-way range family.
    const range = buildOperatorRegex(['.', '..', '..=']);
    assert.ok(range);
    assert.equal(range.exec('..=')?.[0], '..=');
    assert.equal(range.exec('..')?.[0], '..');
    assert.equal(range.exec('.')?.[0], '.');
  });

  it('dedups repeated symbols and breaks length ties deterministically', () => {
    const re = buildOperatorRegex(['>', '<', '>']);
    assert.ok(re);
    // Lexicographic tie-break among equal lengths: `<` before `>`.
    assert.equal(re.source, '<|>');
  });

  it('matches every symbol of a realistic Bock operator set exactly', () => {
    const symbols = [
      '=', '+=', '-=', '*=', '/=', '%=', '|>', '>>', '..', '..=', '||', '&&',
      '==', '!=', '<', '>', '<=', '>=', '|', '^', '&', '+', '-', '*', '/',
      '%', '**', '!', '~', '?', '.', '=>', '->', '<<', ':', ';', ',', '@',
      '#', '_',
    ];
    const re = buildOperatorRegex(symbols);
    assert.ok(re);
    for (const s of symbols) {
      // Each symbol, scanned from its own start, must match itself in full —
      // i.e. no shorter alternative shadows it and the escaping is sound.
      assert.equal(re.exec(s)?.[0], s, `expected ${JSON.stringify(s)} to self-match`);
    }
  });
});

// ─── render* (markdown snapshots) ───────────────────────────────────────────

describe('hover-render.render*', () => {
  it('renders an annotation with params, example, and spec link', () => {
    const a: Annotation = {
      name: 'performance',
      params: 'hot',
      purpose: 'marks a hot path',
      spec_ref: '§9.1',
    };
    const md = renderAnnotation(a, true);
    assert.equal(
      md,
      [
        '**@performance** — annotation',
        '',
        'marks a hot path',
        '',
        'Params: `hot`',
        '',
        '_Example:_',
        '```bock',
        '@performance(hot)',
        '```',
        '',
        specLink('§9.1', true),
      ].join('\n'),
    );
  });

  it('omits the spec link from an annotation when links are disabled', () => {
    const a: Annotation = {
      name: '@managed',
      params: '',
      purpose: 'AI-managed',
      spec_ref: '§9.2',
    };
    const md = renderAnnotation(a, false);
    assert.ok(!md.includes('command:bock.openSpecAt'));
    // A bare-name (no params) annotation uses the name itself as the example.
    assert.ok(md.includes('@managed\n```'));
    // The `@` prefix is preserved when already present.
    assert.ok(md.startsWith('**@managed** — annotation'));
  });

  it('renders a keyword with its category', () => {
    const k: Keyword = { name: 'match', category: 'control-flow', spec_ref: '§5' };
    assert.equal(
      renderKeyword(k, true),
      ['**`match`** — control-flow keyword', '', specLink('§5', true)].join('\n'),
    );
  });

  it('renders a primitive type', () => {
    const p: PrimitiveType = { name: 'Int', spec_ref: '§3.1' };
    assert.equal(
      renderPrimitive(p, true),
      ['**Int** — primitive type', '', specLink('§3.1', true)].join('\n'),
    );
  });

  it('renders a prelude symbol with signature and doc', () => {
    const s = sym('Option', {
      kind: 'type',
      signature: 'enum Option<T>',
      doc: 'an optional value',
      spec_ref: '§4',
    });
    assert.equal(
      renderPrelude('prelude type', s, true),
      [
        '**Option** — prelude type',
        '',
        '```bock',
        'enum Option<T>',
        '```',
        '',
        'an optional value',
        '',
        specLink('§4', true),
      ].join('\n'),
    );
  });

  it('renders a stdlib symbol, falling back to the module spec_ref', () => {
    const hit = {
      module: {
        path: 'std.io',
        types: [],
        functions: [],
        effects: [],
        traits: [],
        spec_ref: '§12',
      },
      symbol: sym('read', { signature: 'fn read() -> String', since: '0.2' }),
      kind: 'function' as const,
    };
    const md = renderStdlibSymbol(hit, true);
    assert.ok(md.startsWith('**read** — function in `std.io`'));
    assert.ok(md.includes('```bock\nfn read() -> String\n```'));
    assert.ok(md.includes('_Since: 0.2_'));
    // No symbol spec_ref → the module's §12 is used for the link.
    assert.ok(md.includes(specLink('§12', true) as string));
  });

  it('renders the effect block with a resolved handler line (1-based)', () => {
    // handlerLine 4 (0-based) renders as "line 5".
    const md = renderEffectUsage('Async', 4, true);
    assert.ok(md.startsWith('**Async** — effect'));
    assert.ok(md.includes('Handler in this file: line 5.'));
    assert.ok(md.includes('handle Async { ... }'));
    assert.ok(md.includes(specLink('§8', true) as string));
  });

  it('renders the effect block "no handler" message for undefined / line 0', () => {
    const none = renderEffectUsage('Async', undefined, false);
    assert.ok(none.includes('No `handle Async` found in this file'));
    assert.ok(!none.includes('command:bock.openSpecAt'));
    // A falsy line index (0 — the first line) keeps the original behaviour of
    // taking the "no handler" branch.
    const lineZero = renderEffectUsage('Async', 0, true);
    assert.ok(lineZero.includes('No `handle Async` found in this file'));
  });

  it('renders a binary operator with precedence, associativity, and spec link', () => {
    const plus = op('+', {
      precedence: 11,
      associativity: 'left',
      kind: 'arithmetic',
      spec_ref: '§5.1',
    });
    assert.equal(
      renderOperator(plus, true),
      [
        '**`+`** — arithmetic operator',
        '',
        'Precedence 11, left-associative.',
        '',
        specLink('§5.1', true),
      ].join('\n'),
    );
  });

  it('renders a right-associative operator', () => {
    const pow = op('**', { precedence: 13, associativity: 'right' });
    const md = renderOperator(pow, false);
    assert.ok(md.includes('Precedence 13, right-associative.'));
  });

  it('labels a none-associativity operator with a precedence as non-associative', () => {
    const eq = op('==', { precedence: 7, associativity: 'none', kind: 'comparison' });
    const md = renderOperator(eq, true);
    assert.ok(md.startsWith('**`==`** — comparison operator'));
    assert.ok(md.includes('Precedence 7, non-associative.'));
  });

  it('renders bare punctuation (no precedence, none associativity) without a details line', () => {
    const arrow = op('=>', { kind: 'punctuation' });
    assert.equal(renderOperator(arrow, true), '**`=>`** — punctuation operator');
  });

  it('omits the operator spec link when links are disabled', () => {
    const pipe = op('|>', { precedence: 2, associativity: 'left', kind: 'pipe', spec_ref: '§5.1' });
    const md = renderOperator(pipe, false);
    assert.ok(!md.includes('command:bock.openSpecAt'));
    assert.ok(md.includes('Precedence 2, left-associative.'));
  });

  it('renders a builtin method with every receiver and the §18.3 link', () => {
    assert.equal(
      renderBuiltinMethod('len', ['List', 'String'], true),
      [
        '**`len`** — built-in method on `List`, `String`',
        '',
        specLink('§18.3', true),
      ].join('\n'),
    );
  });

  it('renders a single-receiver builtin method without a link when disabled', () => {
    const md = renderBuiltinMethod('push', ['List'], false);
    assert.equal(md, '**`push`** — built-in method on `List`');
  });

  it('renders an effect operation with owner, operation list, line hint, and §8 link', () => {
    const md = renderEffectOperation('log', 'Logger', ['log', 'warn'], 2, true);
    assert.equal(
      md,
      [
        '**`log`** — operation of effect `Logger`',
        '',
        '`Logger` operations: `log`, `warn`',
        '',
        'Declared in this file: `effect Logger` at line 3.',
        '',
        specLink('§8', true),
      ].join('\n'),
    );
  });

  it('treats declaration line 0 as a real location (line 1), unlike the legacy falsy-check', () => {
    const md = renderEffectOperation('now', 'Clock', ['now'], 0, false);
    assert.ok(md.includes('Declared in this file: `effect Clock` at line 1.'));
  });

  it('omits the line hint when the declaration line is unknown', () => {
    const md = renderEffectOperation('now', 'Clock', ['now'], undefined, false);
    assert.ok(!md.includes('Declared in this file'));
    assert.ok(md.includes('`Clock` operations: `now`'));
  });

  it('omits the operation list when the owning effect has none recorded', () => {
    const md = renderEffectOperation('now', 'Clock', [], undefined, false);
    assert.equal(md, '**`now`** — operation of effect `Clock`');
  });
});
