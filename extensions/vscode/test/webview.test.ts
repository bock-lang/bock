// Unit tests for escapeHtml in src/shared/webview.ts.
//
// escapeHtml guards every webview that interpolates vocab-derived text
// (diagnostic labels, spec refs, decision titles) into HTML, so getting the
// entity escaping right is a real correctness/security concern, not a toy.

import * as assert from 'node:assert/strict';
import { escapeHtml } from '../src/shared/webview';

describe('webview.escapeHtml', () => {
  it('escapes all five HTML-significant characters', () => {
    const input = `<a href="x" title='y'>Tom & Jerry</a>`;
    assert.equal(
      escapeHtml(input),
      '&lt;a href=&quot;x&quot; title=&#39;y&#39;&gt;Tom &amp; Jerry&lt;/a&gt;',
    );
  });

  it('escapes ampersands before introduced entities (no double-encoding bug)', () => {
    // The `&` rule must run first; otherwise the `&` it introduces for `<`
    // would itself get re-escaped. `<&` is the canonical regression input.
    assert.equal(escapeHtml('<&'), '&lt;&amp;');
  });

  it('leaves text with no special characters untouched', () => {
    assert.equal(escapeHtml('plain ASCII text 123'), 'plain ASCII text 123');
  });
});
