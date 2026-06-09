// Unit tests for the pure parser helpers in src/features/effect-analyzer.ts.
//
// These cover the load-bearing string scanners that back function discovery,
// effect-block parsing, with-clause extraction, handler-binding splitting, and
// offset→location mapping. They run headlessly under Mocha + ts-node — no
// Extension Host — with `vscode` stubbed via test/register-vscode.ts. The
// helpers under test are pure (no live `vscode` runtime), apart from
// `offsetToLocation`/`extractEffects` which only touch the stubbed `Uri`.
//
// Sibling file test/effect-analyzer.test.ts covers extractEffects /
// parseProjectEffects; this file deliberately avoids re-covering those.

import * as assert from 'node:assert/strict';
import {
  matchDelimiter,
  findEnclosingFunction,
  splitBindings,
  parseWithClause,
  expandEffects,
  offsetToLocation,
  type EffectDef,
  type EnclosingFunction,
} from '../src/features/effect-analyzer';
import { Uri } from './vscode-stub';

// The functions that take `vscode.Uri` are structurally satisfied by the stub
// for the members they touch. Cast through `unknown` at the call site.
const uri = Uri.file('/ws/example.bock') as unknown as Parameters<
  typeof offsetToLocation
>[0];

// ─── matchDelimiter ─────────────────────────────────────────────────────────
// The balanced-delimiter scanner. Highest-value unit: it backs function
// discovery, effect blocks, and handler parsing.

describe('effect-analyzer.matchDelimiter', () => {
  it('matches a simple balanced pair', () => {
    const s = '{ a }';
    assert.equal(matchDelimiter(s, 0, '{', '}'), 4);
  });

  it('matches the outer pair across nesting, skipping inner pairs', () => {
    const s = '{ a { b } c }';
    // The closing brace is the final character, not the inner `}` at index 8.
    assert.equal(matchDelimiter(s, 0, '{', '}'), s.length - 1);
  });

  it('works for parentheses, not just braces', () => {
    const s = '(a (b) c)';
    assert.equal(matchDelimiter(s, 0, '(', ')'), s.length - 1);
  });

  it('ignores a close delimiter inside a double-quoted string literal', () => {
    const s = '{ "}" }';
    // The `}` at index 2 is inside the string and must be skipped; the real
    // closer is the trailing `}` at index 6.
    assert.equal(matchDelimiter(s, 0, '{', '}'), 6);
    assert.equal(s[6], '}');
  });

  it('ignores a close delimiter inside a single-quoted string literal', () => {
    const s = "{ '}' }";
    assert.equal(matchDelimiter(s, 0, '{', '}'), 6);
  });

  it('honours an escaped quote inside a string (the string does not end early)', () => {
    // `{ "a\"}" }` — the escaped `\"` keeps us in-string, so the `}` right
    // after it is still string content. The real closer is the final `}`.
    const s = '{ "a\\"}" }';
    const result = matchDelimiter(s, 0, '{', '}');
    assert.equal(result, s.length - 1);
    assert.equal(s[result], '}');
  });

  it('does not confuse the two quote styles (a single quote inside a double-quoted string is literal)', () => {
    // The lone `'` inside the double-quoted string must not open a new string;
    // the `}` inside the string is still skipped.
    const s = '{ "it\'s }" }';
    assert.equal(matchDelimiter(s, 0, '{', '}'), s.length - 1);
  });

  it('ignores a delimiter that appears in a // line comment', () => {
    const s = '{\n  // }\n}';
    // The `}` at index 6 is in a line comment; the closer is the final `}`.
    assert.equal(matchDelimiter(s, 0, '{', '}'), s.length - 1);
    assert.equal(s[s.length - 1], '}');
  });

  it('returns -1 when the delimiter is never balanced', () => {
    assert.equal(matchDelimiter('{ a b c ', 0, '{', '}'), -1);
  });

  it('returns -1 when an unterminated string swallows the close delimiter', () => {
    // The unterminated string runs to end-of-text, so depth never returns to 0.
    assert.equal(matchDelimiter('{ "no close }', 0, '{', '}'), -1);
  });
});

// ─── findEnclosingFunction ──────────────────────────────────────────────────
// Pure (text, offset) → innermost enclosing function (or undefined).

describe('effect-analyzer.findEnclosingFunction', () => {
  function at(text: string, marker: string): number {
    const idx = text.indexOf(marker);
    assert.notEqual(idx, -1, `marker ${marker} should be present`);
    return idx;
  }

  it('finds the function whose body contains the cursor', () => {
    const text = ['fn greet() {', '  CURSOR', '}'].join('\n');
    const fn = findEnclosingFunction(text, at(text, 'CURSOR'));
    assert.ok(fn, 'cursor inside greet should resolve');
    assert.equal(fn.name, 'greet');
  });

  it('returns undefined when the cursor is outside every function', () => {
    const text = ['fn greet() {', '  body', '}', '', 'OUTSIDE'].join('\n');
    assert.equal(findEnclosingFunction(text, at(text, 'OUTSIDE')), undefined);
  });

  it('picks the correct sibling when the cursor is in the first of two', () => {
    const text = [
      'fn first() {',
      '  CURSOR',
      '}',
      '',
      'fn second() {',
      '  other',
      '}',
    ].join('\n');
    const fn = findEnclosingFunction(text, at(text, 'CURSOR'));
    assert.ok(fn);
    assert.equal(fn.name, 'first');
  });

  it('picks the second sibling when the cursor is in it', () => {
    const text = [
      'fn first() {',
      '  body',
      '}',
      '',
      'fn second() {',
      '  CURSOR',
      '}',
    ].join('\n');
    const fn = findEnclosingFunction(text, at(text, 'CURSOR'));
    assert.ok(fn);
    assert.equal(fn.name, 'second');
  });

  it('selects the INNERMOST function for a nested closure (cursor inside the inner fn)', () => {
    // A closure `inner` defined inside `outer`; the cursor sits in `inner`.
    // The correct innermost-enclosing pick is `inner`, not the surrounding
    // `outer`. (For balanced-brace nesting the maximal-signatureStart pick and
    // the smallest-span pick coincide, so this asserts the correct semantics.)
    const text = [
      'fn outer() with Logger {',
      '  let handler = fn inner() {',
      '    CURSOR',
      '  }',
      '}',
    ].join('\n');
    const fn = findEnclosingFunction(text, at(text, 'CURSOR'));
    assert.ok(fn);
    assert.equal(fn.name, 'inner');
  });

  it('falls back to the OUTER function when the cursor is in the outer body, before the inner fn', () => {
    const text = [
      'fn outer() with Logger {',
      '  CURSOR',
      '  let handler = fn inner() {',
      '    body',
      '  }',
      '}',
    ].join('\n');
    const fn = findEnclosingFunction(text, at(text, 'CURSOR'));
    assert.ok(fn);
    assert.equal(fn.name, 'outer');
  });

  it('reports the with-clause on the enclosing function (multi-line shape)', () => {
    // The with-clause lives between `)` and `{`; on its own line it parses.
    const text = [
      'fn run()',
      '  with Logger, Clock',
      '{',
      '  CURSOR',
      '}',
    ].join('\n');
    const fn = findEnclosingFunction(text, at(text, 'CURSOR'));
    assert.ok(fn);
    assert.deepEqual(fn.withClause, ['Logger', 'Clock']);
  });

  it('skips effect-block fn signatures (no `{` body) and finds the real enclosing fn', () => {
    // The `fn log(...)` lines inside `effect Logger { ... }` are declarations,
    // not bodies; they must not be mistaken for the enclosing function.
    const text = [
      'effect Logger {',
      '  fn log(message: String) -> Void',
      '}',
      '',
      'fn main() {',
      '  CURSOR',
      '}',
    ].join('\n');
    const fn = findEnclosingFunction(text, at(text, 'CURSOR'));
    assert.ok(fn);
    assert.equal(fn.name, 'main');
  });
});

// ─── splitBindings ──────────────────────────────────────────────────────────
// Top-level comma split, brace-/paren-aware. Known weakness: not string-aware.

describe('effect-analyzer.splitBindings', () => {
  it('splits a flat binding list on top-level commas', () => {
    assert.deepEqual(splitBindings('A with H1, B with H2'), [
      'A with H1',
      'B with H2',
    ]);
  });

  it('does not split commas nested inside record-literal braces', () => {
    assert.deepEqual(splitBindings('A with H { x: 1, y: 2 }, B with C'), [
      'A with H { x: 1, y: 2 }',
      'B with C',
    ]);
  });

  it('does not split commas nested inside parentheses', () => {
    assert.deepEqual(splitBindings('A with H1 (x, y), B with H2'), [
      'A with H1 (x, y)',
      'B with H2',
    ]);
  });

  it('trims whitespace and drops empty segments', () => {
    assert.deepEqual(splitBindings('  A with H1 ,  , B with H2 ,'), [
      'A with H1',
      'B with H2',
    ]);
  });

  it('KNOWN WEAKNESS: a top-level comma inside a string literal is (wrongly) treated as a separator', () => {
    // splitBindings is brace-/paren-aware but NOT string-aware, so a comma in
    // an unbraced string literal splits the binding. Asserting current
    // behaviour to pin the limitation; see report.
    assert.deepEqual(splitBindings('A with "x,y", B with C'), [
      'A with "x',
      'y"',
      'B with C',
    ]);
  });
});

// ─── parseWithClause ────────────────────────────────────────────────────────
// Strips a return type, then comma-splits the `with` list.

describe('effect-analyzer.parseWithClause', () => {
  it('extracts effects from a bare with-clause', () => {
    assert.deepEqual(parseWithClause(' with Logger '), ['Logger']);
  });

  it('comma-splits multiple effects in a with-clause', () => {
    assert.deepEqual(parseWithClause(' with Logger, Clock, Storage '), [
      'Logger',
      'Clock',
      'Storage',
    ]);
  });

  it('returns no effects when there is no with-clause', () => {
    assert.deepEqual(parseWithClause(' -> Void '), []);
    assert.deepEqual(parseWithClause('   '), []);
  });

  it('strips a return type that sits on a separate line from the with-clause', () => {
    // Multi-line shape: `-> Void` is consumed up to the newline, leaving the
    // with-clause on the next line intact.
    assert.deepEqual(parseWithClause('-> Void\n  with Logger, Storage'), [
      'Logger',
      'Storage',
    ]);
  });

  it('KNOWN BUG: a same-line `-> Type with Effects` loses its effects (greedy return-type strip)', () => {
    // The idiomatic Bock signature `fn f() -> Void with Logger {` puts the
    // return type and with-clause on ONE line. The strip regex `/->\s*[^\n{]*/`
    // is greedy to the newline, so it consumes ` with Logger` too and the
    // with-clause comes back EMPTY. Asserting current (buggy) behaviour to pin
    // it; this is the dominant signature shape across examples/. See report.
    assert.deepEqual(parseWithClause(' -> Void with Logger, Storage '), []);
  });
});

// ─── expandEffects ──────────────────────────────────────────────────────────
// Composite expansion through a registry Map, with diamond dedup and lenient
// handling of missing components.

describe('effect-analyzer.expandEffects', () => {
  function reg(defs: Array<[string, Partial<EffectDef>]>): Map<string, EffectDef> {
    const m = new Map<string, EffectDef>();
    for (const [name, d] of defs) {
      m.set(name, { name, operations: [], components: [], ...d });
    }
    return m;
  }

  it('returns leaf effects unchanged', () => {
    const r = reg([
      ['Logger', {}],
      ['Clock', {}],
    ]);
    assert.deepEqual(expandEffects(['Logger', 'Clock'], r), ['Logger', 'Clock']);
  });

  it('expands a composite into the composite plus its components', () => {
    const r = reg([
      ['App', { components: ['Logger', 'Clock'] }],
      ['Logger', {}],
      ['Clock', {}],
    ]);
    // The composite itself is retained, followed by its components.
    assert.deepEqual(expandEffects(['App'], r), ['App', 'Logger', 'Clock']);
  });

  it('expands transitively (composite of composites)', () => {
    const r = reg([
      ['Outer', { components: ['Inner'] }],
      ['Inner', { components: ['Leaf'] }],
      ['Leaf', {}],
    ]);
    assert.deepEqual(expandEffects(['Outer'], r), ['Outer', 'Inner', 'Leaf']);
  });

  it('dedups a diamond (a component reached via two paths appears once)', () => {
    // App = Read + Write; Read = Base; Write = Base. Base must appear once.
    const r = reg([
      ['App', { components: ['Read', 'Write'] }],
      ['Read', { components: ['Base'] }],
      ['Write', { components: ['Base'] }],
      ['Base', {}],
    ]);
    assert.deepEqual(expandEffects(['App'], r), [
      'App',
      'Read',
      'Base',
      'Write',
    ]);
  });

  it('is lenient about a component missing from the registry (keeps the name, stops descending)', () => {
    const r = reg([['App', { components: ['Logger', 'Ghost'] }]]);
    // Logger and Ghost are both unknown leaves here, but the names survive.
    assert.deepEqual(expandEffects(['App'], r), ['App', 'Logger', 'Ghost']);
  });

  it('tolerates a root name absent from the registry', () => {
    assert.deepEqual(expandEffects(['Unknown'], new Map()), ['Unknown']);
  });

  it('does not loop forever on a self-referential / cyclic composite', () => {
    const r = reg([
      ['A', { components: ['B'] }],
      ['B', { components: ['A'] }],
    ]);
    assert.deepEqual(expandEffects(['A'], r), ['A', 'B']);
  });
});

// ─── offsetToLocation ───────────────────────────────────────────────────────
// Zero-based (line, column) for a byte offset into multi-line text.

describe('effect-analyzer.offsetToLocation', () => {
  it('maps offset 0 to line 0, column 0', () => {
    const loc = offsetToLocation(uri, 'abc', 0);
    assert.equal(loc.line, 0);
    assert.equal(loc.column, 0);
  });

  it('computes the column within the first line', () => {
    const loc = offsetToLocation(uri, 'hello world', 6);
    assert.equal(loc.line, 0);
    assert.equal(loc.column, 6);
  });

  it('counts lines and resets the column after each newline', () => {
    const text = 'aa\nbbb\ncccc';
    // Offset of the first `c` on line 2: after "aa\n" (3) + "bbb\n" (4) = 7.
    const loc = offsetToLocation(uri, text, 7);
    assert.equal(loc.line, 2);
    assert.equal(loc.column, 0);
  });

  it('gives column 0 for the character immediately after a newline', () => {
    const text = 'x\ny';
    // Offset 2 is the `y` at the start of line 1.
    const loc = offsetToLocation(uri, text, 2);
    assert.equal(loc.line, 1);
    assert.equal(loc.column, 0);
  });

  it('computes a non-zero column in the middle of a later line', () => {
    const text = 'first\nsecond';
    // Offset of the `c` in "second": 6 (after "first\n") + 3 = 9.
    const loc = offsetToLocation(uri, text, 9);
    assert.equal(loc.line, 1);
    assert.equal(loc.column, 3);
  });

  it('points the offset at the newline itself to the column past the previous line', () => {
    const text = 'ab\ncd';
    // Offset 2 is the `\n`; it belongs to line 0 at column 2 (one past `b`).
    const loc = offsetToLocation(uri, text, 2);
    assert.equal(loc.line, 0);
    assert.equal(loc.column, 2);
  });

  // Use the EnclosingFunction type so the import is exercised structurally.
  it('exposes the EnclosingFunction shape for downstream consumers', () => {
    const stub: EnclosingFunction = {
      name: 'f',
      signatureStart: 0,
      bodyRange: { start: 5, end: 9 },
      withClause: [],
    };
    assert.equal(stub.bodyRange.end - stub.bodyRange.start, 4);
  });
});
