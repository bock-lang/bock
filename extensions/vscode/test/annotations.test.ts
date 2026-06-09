// Unit tests for scanText in src/features/annotations-scan.ts.
//
// scanText is the pure parser that turns a single `.bock` file's text into
// top-level annotation usages for the insight tree. Its cross-line
// triple-quote tracking decides which lines are "inside a `@context("""…""")`
// body" and therefore suppressed. A naive `"""`-counting implementation
// flips that state on a stray `"""` inside a `//` comment or an ordinary
// `"…"` string, which suppresses every annotation on the following lines —
// a false-negative. These tests pin the correct behaviour.

import * as assert from 'node:assert/strict';
// Import from the pure scanner module directly. `annotations.ts` re-exports
// `scanText`, but pulling it in transitively imports `vscode-languageclient`,
// whose package `exports` subpath the headless ts-node CommonJS resolver
// can't follow — so the tests target the dependency-free module.
import {
  scanText,
  aggregateByFile,
  summarizeParams,
  type AnnotationUsage,
} from '../src/features/annotations-scan';
import { Uri } from './vscode-stub';

// scanText takes a `vscode.Uri`; the stub is structurally compatible for the
// members it touches (`toString`, `fsPath`). Cast through `unknown`.
const uri = Uri.file('/ws/example.bock') as unknown as Parameters<
  typeof scanText
>[0];

function names(text: string): string[] {
  return scanText(uri, text).map((u) => u.name);
}

describe('annotations.scanText', () => {
  it('finds an indented annotation', () => {
    const usages = scanText(uri, '    @managed\nfn f() {}');
    assert.equal(usages.length, 1);
    assert.equal(usages[0].name, 'managed');
    // Column points at the `@`, not the line start.
    assert.equal(usages[0].column, 4);
    assert.equal(usages[0].line, 0);
  });

  it('captures nested-paren params up to the unnested close', () => {
    const usages = scanText(uri, '@requires(a, (b))');
    assert.equal(usages.length, 1);
    assert.equal(usages[0].name, 'requires');
    assert.equal(usages[0].params, 'a, (b)');
  });

  it('suppresses annotation-like tokens inside a multi-line @context("""…""")', () => {
    const text = [
      '@context("""',
      '  @intent: this is documentation, not a real annotation',
      '  more body text',
      '""")',
      '@managed',
    ].join('\n');
    // The `@intent` line is inside the triple-quoted body and must be
    // suppressed; the `@managed` after the closing `"""` must be found.
    assert.deepEqual(names(text), ['context', 'managed']);
  });

  it('handles CRLF line endings', () => {
    const text = ['@context("""', '  @intent: doc', '""")', '@test'].join(
      '\r\n',
    );
    assert.deepEqual(names(text), ['context', 'test']);
  });

  it('does NOT enter triple-string state for a `"""` inside a // comment', () => {
    // Regression: a line-granular `"""` count flips inTripleString here,
    // suppressing every later annotation. The scanner must ignore `"""`
    // that appears after `//`.
    const text = [
      '// example shows a """ token in prose',
      '@managed',
      '@performance("hot")',
    ].join('\n');
    assert.deepEqual(names(text), ['managed', 'performance']);
  });

  it('does NOT enter triple-string state for a `"""` inside a normal "…" string', () => {
    // A `"""` can appear inside an ordinary single-line string (an empty
    // string immediately followed by an opening quote: `"" + "`). The
    // scanner must treat the line's quotes as a balanced single-line
    // string and NOT toggle the cross-line triple-quote state.
    const text = [
      'let s = "a literal triple """ inside a string"',
      '@managed',
      '@security("pii")',
    ].join('\n');
    assert.deepEqual(names(text), ['managed', 'security']);
  });

  it('still suppresses inside a genuine triple string opened mid-line', () => {
    const text = [
      'let doc = """',
      '@intent: still documentation',
      '"""',
      '@invariant(x > 0)',
    ].join('\n');
    assert.deepEqual(names(text), ['invariant']);
  });

  it('handles a single-line triple string without leaking state', () => {
    // Open and close on the same line: subsequent annotations are live.
    const text = ['let d = """inline doc"""', '@managed'].join('\n');
    assert.deepEqual(names(text), ['managed']);
  });
});

// ─── Aggregation helpers (per-file tree depth + usage webview) ──────────────

/** Build an AnnotationUsage at a given file path / location. */
function usage(
  fsPath: string,
  name: string,
  params = '',
  line = 0,
  column = 0,
): AnnotationUsage {
  return {
    name,
    params,
    uri: Uri.file(fsPath) as unknown as AnnotationUsage['uri'],
    line,
    column,
  };
}

describe('annotations.aggregateByFile', () => {
  it('returns an empty array for no usages', () => {
    assert.deepEqual(aggregateByFile([]), []);
  });

  it('groups usages by file and sorts files by path', () => {
    const files = aggregateByFile([
      usage('/ws/zeta.bock', 'managed', '', 3),
      usage('/ws/alpha.bock', 'managed', '', 9),
      usage('/ws/zeta.bock', 'managed', '', 1),
      usage('/ws/midway.bock', 'managed', '', 0),
    ]);
    assert.deepEqual(
      files.map((f) => f.fsPath),
      ['/ws/alpha.bock', '/ws/midway.bock', '/ws/zeta.bock'],
    );
    assert.deepEqual(
      files.map((f) => f.usages.length),
      [1, 1, 2],
    );
  });

  it('uses the URI string as the stable grouping key', () => {
    const [file] = aggregateByFile([usage('/ws/a.bock', 'test')]);
    assert.equal(file.key, 'file:///ws/a.bock');
    assert.equal(file.uri.fsPath, '/ws/a.bock');
  });

  it('sorts usages within a file by line, then column', () => {
    const [file] = aggregateByFile([
      usage('/ws/a.bock', 'context', '', 5, 4),
      usage('/ws/a.bock', 'context', '', 2, 0),
      usage('/ws/a.bock', 'context', '', 5, 0),
    ]);
    assert.deepEqual(
      file.usages.map((u) => [u.line, u.column]),
      [
        [2, 0],
        [5, 0],
        [5, 4],
      ],
    );
  });

  it('handles many files without merging distinct paths', () => {
    const input: AnnotationUsage[] = [];
    for (let i = 0; i < 25; i++) {
      input.push(usage(`/ws/f${String(i).padStart(2, '0')}.bock`, 'managed'));
    }
    const files = aggregateByFile(input);
    assert.equal(files.length, 25);
    assert.ok(files.every((f) => f.usages.length === 1));
  });
});

describe('annotations.summarizeParams', () => {
  it('returns an empty array for no usages', () => {
    assert.deepEqual(summarizeParams([]), []);
  });

  it('counts usages without arguments under the empty string', () => {
    const patterns = summarizeParams([
      usage('/ws/a.bock', 'managed'),
      usage('/ws/b.bock', 'managed'),
      usage('/ws/c.bock', 'managed', '"hot"'),
    ]);
    assert.deepEqual(patterns, [
      { params: '', count: 2 },
      { params: '"hot"', count: 1 },
    ]);
  });

  it('counts duplicate parameter strings across files', () => {
    const patterns = summarizeParams([
      usage('/ws/a.bock', 'security', '"pii"'),
      usage('/ws/b.bock', 'security', '"pii"'),
      usage('/ws/c.bock', 'security', '"pii"'),
      usage('/ws/c.bock', 'security', '"audit"'),
    ]);
    assert.deepEqual(patterns, [
      { params: '"pii"', count: 3 },
      { params: '"audit"', count: 1 },
    ]);
  });

  it('breaks count ties by ascending parameter text', () => {
    const patterns = summarizeParams([
      usage('/ws/a.bock', 'requires', 'net'),
      usage('/ws/a.bock', 'requires', 'fs'),
    ]);
    assert.deepEqual(
      patterns.map((p) => p.params),
      ['fs', 'net'],
    );
  });

  it('truncates to the given limit, keeping the most frequent', () => {
    const input: AnnotationUsage[] = [];
    // 12 distinct patterns; pattern i occurs (i + 1) times.
    for (let i = 0; i < 12; i++) {
      for (let n = 0; n <= i; n++) {
        input.push(usage('/ws/a.bock', 'performance', `p${i}`));
      }
    }
    const top = summarizeParams(input, 10);
    assert.equal(top.length, 10);
    assert.equal(top[0].params, 'p11');
    assert.equal(top[0].count, 12);
    // The two least-frequent patterns (p0, p1) fall off the end.
    assert.ok(top.every((p) => p.params !== 'p0' && p.params !== 'p1'));
  });

  it('returns all patterns when no limit is given', () => {
    const input: AnnotationUsage[] = [];
    for (let i = 0; i < 12; i++) {
      input.push(usage('/ws/a.bock', 'performance', `p${i}`));
    }
    assert.equal(summarizeParams(input).length, 12);
  });
});
