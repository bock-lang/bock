// Unit tests for the pure bock.project strictness editor in
// src/features/strictness-toml.ts: read (getStrictness), line-level rewrite
// (setStrictness) with formatting/comment preservation, missing-key and
// missing-section insertion, and idempotence. The edit semantics mirror the
// compiler's own rewriter (bock-cli/src/promote.rs); strictness-toml.ts is
// vscode-free, so these run headlessly under Mocha + ts-node.

import * as assert from 'node:assert/strict';
import {
  STRICTNESS_LEVELS,
  getStrictness,
  isStrictnessLevel,
  setStrictness,
} from '../src/features/strictness-toml';

// The exact shape the scaffolder writes (examples/*/bock.project).
const SCAFFOLDED = `[project]
name = "expense-tracker"
version = "0.1.0"

[strictness]
default = "sketch"
`;

describe('strictness-toml.isStrictnessLevel', () => {
  it('accepts exactly the three ladder levels', () => {
    for (const level of STRICTNESS_LEVELS) assert.ok(isStrictnessLevel(level));
    assert.ok(!isStrictnessLevel('strict'));
    assert.ok(!isStrictnessLevel('Production'));
    assert.ok(!isStrictnessLevel(''));
  });
});

describe('strictness-toml.getStrictness', () => {
  it('reads the scaffolded shape', () => {
    assert.equal(getStrictness(SCAFFOLDED), 'sketch');
  });

  it('reads each level', () => {
    for (const level of STRICTNESS_LEVELS) {
      assert.equal(
        getStrictness(`[strictness]\ndefault = "${level}"\n`),
        level,
      );
    }
  });

  it('defaults to sketch when the section is missing', () => {
    assert.equal(getStrictness('[project]\nname = "x"\n'), 'sketch');
    assert.equal(getStrictness(''), 'sketch');
  });

  it('defaults to sketch when the key is missing or unrecognized', () => {
    assert.equal(getStrictness('[strictness]\n# nothing here\n'), 'sketch');
    assert.equal(
      getStrictness('[strictness]\ndefault = "paranoid"\n'),
      'sketch',
    );
  });

  it('ignores a default key in a different section', () => {
    assert.equal(
      getStrictness('[ai]\ndefault = "production"\n\n[strictness]\ndefault = "development"\n'),
      'development',
    );
    assert.equal(getStrictness('[ai]\ndefault = "production"\n'), 'sketch');
  });

  it('tolerates indentation, spacing, and trailing comments', () => {
    assert.equal(
      getStrictness('[strictness]\n  default="production"   # ship it\n'),
      'production',
    );
  });

  it('tolerates a comment after the section header', () => {
    assert.equal(
      getStrictness('[strictness] # the §1.4 ladder\ndefault = "development"\n'),
      'development',
    );
  });

  it('reads CRLF files', () => {
    assert.equal(
      getStrictness('[strictness]\r\ndefault = "production"\r\n'),
      'production',
    );
  });
});

describe('strictness-toml.setStrictness', () => {
  it('rewrites the key in place on the scaffolded shape', () => {
    const out = setStrictness(SCAFFOLDED, 'production');
    assert.equal(
      out,
      SCAFFOLDED.replace('default = "sketch"', 'default = "production"'),
    );
    assert.equal(getStrictness(out), 'production');
  });

  it('round-trips every level through get(set(...))', () => {
    for (const level of STRICTNESS_LEVELS) {
      assert.equal(getStrictness(setStrictness(SCAFFOLDED, level)), level);
    }
  });

  it('preserves comments, indentation, spacing, and trailing comments', () => {
    const input = `# top note
[project]
name = "x"     # keep me

[strictness]   # ladder
  default="sketch"  # promoted by hand
# trailing note
`;
    const out = setStrictness(input, 'development');
    assert.equal(
      out,
      `# top note
[project]
name = "x"     # keep me

[strictness]   # ladder
  default="development"  # promoted by hand
# trailing note
`,
    );
  });

  it('only touches the strictness section, not same-named keys elsewhere', () => {
    const input = `[ai]
default = "claude"

[strictness]
default = "sketch"
`;
    const out = setStrictness(input, 'production');
    assert.ok(out.includes('default = "claude"'));
    assert.ok(out.includes('default = "production"'));
    assert.ok(!out.includes('default = "sketch"'));
  });

  it('inserts the key right after the header when it is absent', () => {
    const input = `[strictness]
# decided later

[targets.js]
runtime = "node"
`;
    const out = setStrictness(input, 'development');
    assert.equal(
      out,
      `[strictness]
default = "development"
# decided later

[targets.js]
runtime = "node"
`,
    );
  });

  it('appends the section when it is absent, blank-line separated', () => {
    const input = '[project]\nname = "x"\nversion = "0.1.0"\n';
    const out = setStrictness(input, 'production');
    assert.equal(
      out,
      '[project]\nname = "x"\nversion = "0.1.0"\n\n[strictness]\ndefault = "production"\n',
    );
  });

  it('appends without doubling an existing trailing blank line', () => {
    const input = '[project]\nname = "x"\n\n';
    const out = setStrictness(input, 'sketch');
    assert.equal(out, '[project]\nname = "x"\n\n[strictness]\ndefault = "sketch"\n');
  });

  it('handles an empty file', () => {
    const out = setStrictness('', 'development');
    assert.equal(out, '[strictness]\ndefault = "development"');
    assert.equal(getStrictness(out), 'development');
  });

  it('preserves the absence of a trailing newline', () => {
    const input = '[strictness]\ndefault = "sketch"';
    assert.equal(
      setStrictness(input, 'production'),
      '[strictness]\ndefault = "production"',
    );
  });

  it('preserves CRLF line endings', () => {
    const input = '[project]\r\nname = "x"\r\n\r\n[strictness]\r\ndefault = "sketch"\r\n';
    const out = setStrictness(input, 'development');
    assert.equal(
      out,
      '[project]\r\nname = "x"\r\n\r\n[strictness]\r\ndefault = "development"\r\n',
    );
  });

  it('is idempotent', () => {
    for (const input of [
      SCAFFOLDED,
      '[project]\nname = "x"\n',
      '[strictness]\n# empty\n',
      '',
    ]) {
      const once = setStrictness(input, 'production');
      assert.equal(setStrictness(once, 'production'), once);
    }
  });

  it('setting the current level back is a no-op on text', () => {
    assert.equal(setStrictness(SCAFFOLDED, 'sketch'), SCAFFOLDED);
  });
});
