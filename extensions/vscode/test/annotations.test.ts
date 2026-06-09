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
import { scanText } from '../src/features/annotations-scan';
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
