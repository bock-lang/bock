// Unit tests for the pure AIR-viewer model in src/features/air-model.ts:
// defensive parsing of `bock inspect air --json` stdout, presentation
// helpers, and the span → editor coordinate conversions.
//
// The fixtures are not guessed: the success tree and the error object mirror
// real output captured from the worktree's own `bock inspect air --json`
// (examples/fundamentals/fizzbuzz and a deliberately broken file), whose
// shape is pinned end-to-end by
// compiler/crates/bock-cli/tests/inspect_air_command.rs and documented in
// docs/src/reference/cli.md. air-model.ts is vscode-free, so these run
// headlessly under Mocha + ts-node.

import * as assert from 'node:assert/strict';
import {
  AirNode,
  childCount,
  nodeIconId,
  nodeLabel,
  nodeLocation,
  nodeTooltip,
  parseAirJson,
  spanStartPosition,
  utf16LengthForUtf8Bytes,
} from '../src/features/air-model';

// ─── Fixtures ───────────────────────────────────────────────────────────────

/** Captured (and trimmed) from `bock inspect air src/main.bock --json` on
 *  examples/fundamentals/fizzbuzz: alphabetical key order, a const decl, and
 *  a fn with params reaching expression depth. */
const REAL_TREE_STDOUT = `{
  "children": [
    {
      "children": [
        {
          "children": [],
          "kind": "TypeNamed",
          "name": "Int",
          "span": { "col": 17, "end": 208, "line": 7, "start": 205 }
        },
        {
          "children": [],
          "kind": "Literal",
          "name": "3",
          "span": { "col": 23, "end": 212, "line": 7, "start": 211 }
        }
      ],
      "kind": "ConstDecl",
      "name": "FIZZ_NUM",
      "span": { "col": 1, "end": 212, "line": 7, "start": 189 }
    },
    {
      "children": [
        {
          "children": [
            {
              "children": [],
              "kind": "BindPat",
              "name": "n",
              "span": { "col": 13, "end": 327, "line": 13, "start": 326 }
            },
            {
              "children": [],
              "kind": "TypeNamed",
              "name": "Int",
              "span": { "col": 16, "end": 332, "line": 13, "start": 329 }
            }
          ],
          "kind": "Param",
          "name": null,
          "span": { "col": 13, "end": 333, "line": 13, "start": 326 }
        },
        {
          "children": [],
          "kind": "TypeNamed",
          "name": "String",
          "span": { "col": 24, "end": 343, "line": 13, "start": 337 }
        },
        {
          "children": [
            {
              "children": [
                {
                  "children": [],
                  "kind": "Identifier",
                  "name": "n",
                  "span": { "col": 7, "end": 351, "line": 14, "start": 350 }
                }
              ],
              "kind": "BinaryOp",
              "name": null,
              "span": { "col": 7, "end": 360, "line": 14, "start": 350 }
            }
          ],
          "kind": "Block",
          "name": null,
          "span": { "col": 31, "end": 420, "line": 13, "start": 344 }
        }
      ],
      "kind": "FnDecl",
      "name": "fizzbuzz",
      "span": { "col": 1, "end": 420, "line": 13, "start": 314 }
    }
  ],
  "kind": "Module",
  "name": null,
  "span": { "col": 1, "end": 604, "line": 1, "start": 0 }
}`;

/** Captured from `bock inspect air --json` on a file containing
 *  `fn { broken` — exit 1, error object on stdout. */
const REAL_ERROR_STDOUT = `{
  "error": {
    "diagnostics": [
      {
        "code": "E2030",
        "message": "expected function name, found \`{\`",
        "severity": "error",
        "span": { "col": 4, "end": 4, "line": 1, "start": 3 }
      },
      {
        "code": "E2000",
        "message": "expected \`}\`, found \`<eof>\`",
        "severity": "error",
        "span": { "col": 1, "end": 12, "line": 2, "start": 12 }
      }
    ],
    "message": "parsing failed"
  }
}`;

function parseTree(stdout: string): AirNode {
  const result = parseAirJson(stdout);
  assert.equal(result.kind, 'tree', JSON.stringify(result));
  if (result.kind !== 'tree') throw new Error('unreachable');
  return result.root;
}

/** Depth-first search by kind (and name, when given). */
function findNode(
  node: AirNode,
  kind: string,
  name?: string,
): AirNode | undefined {
  if (node.kind === kind && (name === undefined || node.name === name)) {
    return node;
  }
  for (const child of node.children) {
    const hit = findNode(child, kind, name);
    if (hit) return hit;
  }
  return undefined;
}

// ─── parseAirJson: success tree ─────────────────────────────────────────────

describe('air-model.parseAirJson (tree)', () => {
  it('parses the real multi-node CLI output into a tree', () => {
    const root = parseTree(REAL_TREE_STDOUT);
    assert.equal(root.kind, 'Module');
    assert.equal(root.name, null);
    assert.deepEqual(root.span, { start: 0, end: 604, line: 1, col: 1 });
    assert.equal(root.children.length, 2);
  });

  it('reaches declaration and expression depth', () => {
    const root = parseTree(REAL_TREE_STDOUT);
    const constDecl = findNode(root, 'ConstDecl', 'FIZZ_NUM');
    assert.ok(constDecl, 'ConstDecl FIZZ_NUM present');
    assert.equal(constDecl.children.length, 2);

    const fn = findNode(root, 'FnDecl', 'fizzbuzz');
    assert.ok(fn, 'FnDecl fizzbuzz present');
    assert.deepEqual(fn.span, { start: 314, end: 420, line: 13, col: 1 });

    const param = findNode(fn, 'Param');
    assert.ok(param, 'Param present');
    assert.equal(param.name, null, 'unnamed nodes carry null');
    assert.ok(findNode(param, 'BindPat', 'n'), 'pattern under param');
    assert.ok(findNode(fn, 'Identifier', 'n'), 'expression depth reached');
  });

  it('tolerates unknown extra fields on nodes and spans (additive contract)', () => {
    const stdout = JSON.stringify({
      kind: 'Module',
      name: null,
      span: { start: 0, end: 1, line: 1, col: 1, file: 'x.bock' },
      children: [
        {
          kind: 'FnDecl',
          name: 'f',
          span: { start: 0, end: 1, line: 1, col: 1 },
          children: [],
          typeInfo: { resolved: 'Int' },
        },
      ],
      version: 2,
    });
    const root = parseTree(stdout);
    assert.equal(root.children[0].kind, 'FnDecl');
    // Extra fields are dropped, not propagated.
    assert.deepEqual(Object.keys(root.span).sort(), [
      'col',
      'end',
      'line',
      'start',
    ]);
  });

  it('accepts an empty-children leaf-only module', () => {
    const root = parseTree(
      '{"kind":"Module","name":"Demo","span":{"start":0,"end":11,"line":1,"col":1},"children":[]}',
    );
    assert.equal(root.name, 'Demo');
    assert.equal(childCount(root), 0);
  });
});

// ─── parseAirJson: frontend error ───────────────────────────────────────────

describe('air-model.parseAirJson (frontend error)', () => {
  it('parses the real error-object stdout', () => {
    const result = parseAirJson(REAL_ERROR_STDOUT);
    assert.equal(result.kind, 'frontend-error');
    if (result.kind !== 'frontend-error') throw new Error('unreachable');
    assert.equal(result.message, 'parsing failed');
    assert.equal(result.diagnostics.length, 2);
    assert.equal(result.diagnostics[0].code, 'E2030');
    assert.equal(result.diagnostics[0].severity, 'error');
    assert.deepEqual(result.diagnostics[1].span, {
      start: 12,
      end: 12,
      line: 2,
      col: 1,
    });
  });

  it('parses an I/O error with empty diagnostics (missing file)', () => {
    const result = parseAirJson(
      '{"error":{"message":"could not read `/x/y.bock`","diagnostics":[]}}',
    );
    assert.equal(result.kind, 'frontend-error');
    if (result.kind !== 'frontend-error') throw new Error('unreachable');
    assert.match(result.message, /could not read/);
    assert.deepEqual(result.diagnostics, []);
  });

  it('skips malformed diagnostics but keeps the rest', () => {
    const result = parseAirJson(
      JSON.stringify({
        error: {
          message: 'parsing failed',
          diagnostics: [
            'not an object',
            { severity: 'error', code: 'E1', span: null }, // no message
            { message: 'kept', extra: true }, // tolerated, defaults filled
          ],
        },
      }),
    );
    assert.equal(result.kind, 'frontend-error');
    if (result.kind !== 'frontend-error') throw new Error('unreachable');
    assert.equal(result.diagnostics.length, 1);
    assert.deepEqual(result.diagnostics[0], {
      severity: '',
      code: '',
      message: 'kept',
      span: undefined,
    });
  });

  it('falls back to a generic message when error.message is missing', () => {
    const result = parseAirJson('{"error":{"diagnostics":[]}}');
    assert.equal(result.kind, 'frontend-error');
    if (result.kind !== 'frontend-error') throw new Error('unreachable');
    assert.equal(result.message, 'frontend error');
  });

  it('tolerates unknown extra fields inside the error object', () => {
    const result = parseAirJson(
      '{"error":{"message":"m","diagnostics":[],"stage":"parse"}}',
    );
    assert.equal(result.kind, 'frontend-error');
  });
});

// ─── parseAirJson: malformed output ─────────────────────────────────────────

describe('air-model.parseAirJson (malformed)', () => {
  function expectMalformed(stdout: string, reasonPattern: RegExp): void {
    const result = parseAirJson(stdout);
    assert.equal(result.kind, 'malformed', JSON.stringify(result));
    if (result.kind !== 'malformed') throw new Error('unreachable');
    assert.match(result.reason, reasonPattern);
  }

  it('rejects empty and whitespace-only stdout', () => {
    expectMalformed('', /empty output/);
    expectMalformed('  \n ', /empty output/);
  });

  it('rejects non-JSON stdout', () => {
    expectMalformed('error: unrecognized subcommand', /invalid JSON/);
  });

  it('rejects non-object top-level values', () => {
    expectMalformed('[]', /not an object/);
    expectMalformed('null', /not an object/);
    expectMalformed('"Module"', /not an object/);
    expectMalformed('42', /not an object/);
  });

  it('rejects a non-object `error` value', () => {
    expectMalformed('{"error":"boom"}', /`error` is not an object/);
  });

  it('rejects a node missing `kind`', () => {
    expectMalformed(
      '{"name":null,"span":{"start":0,"end":0,"line":1,"col":1},"children":[]}',
      /root: missing or non-string `kind`/,
    );
  });

  it('rejects a node missing `name` (contract pins all four fields)', () => {
    expectMalformed(
      '{"kind":"Module","span":{"start":0,"end":0,"line":1,"col":1},"children":[]}',
      /root: `name` must be a string or null/,
    );
  });

  it('rejects a missing or malformed span, naming the offending node', () => {
    expectMalformed(
      JSON.stringify({
        kind: 'Module',
        name: null,
        span: { start: 0, end: 0, line: 1, col: 1 },
        children: [
          {
            kind: 'FnDecl',
            name: 'f',
            span: { start: 0, end: 0, line: 1 }, // col missing
            children: [],
          },
        ],
      }),
      /root\.children\[0\]: missing or malformed `span`/,
    );
  });

  it('rejects non-numeric and negative span fields', () => {
    const mk = (span: unknown): string =>
      JSON.stringify({ kind: 'Module', name: null, span, children: [] });
    expectMalformed(mk({ start: '0', end: 0, line: 1, col: 1 }), /span/);
    expectMalformed(mk({ start: -1, end: 0, line: 1, col: 1 }), /span/);
    expectMalformed(mk(null), /span/);
  });

  it('rejects non-array children and malformed nested children', () => {
    expectMalformed(
      '{"kind":"Module","name":null,"span":{"start":0,"end":0,"line":1,"col":1},"children":{}}',
      /root: `children` must be an array/,
    );
    expectMalformed(
      JSON.stringify({
        kind: 'Module',
        name: null,
        span: { start: 0, end: 0, line: 1, col: 1 },
        children: [
          {
            kind: 'FnDecl',
            name: 'f',
            span: { start: 0, end: 0, line: 1, col: 1 },
            children: ['leaf'],
          },
        ],
      }),
      /root\.children\[0\]\.children\[0\]: node is not an object/,
    );
  });

  it('rejects a non-string/non-null name', () => {
    expectMalformed(
      '{"kind":"Module","name":7,"span":{"start":0,"end":0,"line":1,"col":1},"children":[]}',
      /`name` must be a string or null/,
    );
  });
});

// ─── Presentation helpers ───────────────────────────────────────────────────

describe('air-model presentation helpers', () => {
  const named: AirNode = {
    kind: 'FnDecl',
    name: 'add',
    span: { start: 13, end: 52, line: 3, col: 1 },
    children: [
      {
        kind: 'Param',
        name: null,
        span: { start: 20, end: 27, line: 3, col: 8 },
        children: [],
      },
    ],
  };
  const unnamed: AirNode = {
    kind: 'Block',
    name: null,
    span: { start: 43, end: 52, line: 3, col: 31 },
    children: [],
  };

  it('nodeLabel combines kind and name; bare kind when unnamed', () => {
    assert.equal(nodeLabel(named), 'FnDecl add');
    assert.equal(nodeLabel(unnamed), 'Block');
    assert.equal(nodeLabel({ ...named, name: '' }), 'FnDecl');
  });

  it('nodeLocation renders the 1-based @line:col like the CLI view', () => {
    assert.equal(nodeLocation(named), '@3:1');
    assert.equal(nodeLocation(unnamed), '@3:31');
  });

  it('childCount counts direct children only', () => {
    assert.equal(childCount(named), 1);
    assert.equal(childCount(unnamed), 0);
  });

  it('nodeTooltip carries kind, name, span, and child count', () => {
    const tip = nodeTooltip(named);
    assert.match(tip, /FnDecl `add`/);
    assert.match(tip, /line 3, col 1/);
    assert.match(tip, /bytes 13\.\.52/);
    assert.match(tip, /1 child$/m);
    assert.match(nodeTooltip(unnamed), /0 children/);
    assert.doesNotMatch(nodeTooltip(unnamed), /`/);
  });

  it('nodeIconId maps known kinds and defaults for unknown ones', () => {
    assert.equal(nodeIconId('FnDecl'), 'symbol-function');
    assert.equal(nodeIconId('Module'), 'symbol-namespace');
    assert.equal(nodeIconId('SomeFutureKind'), 'symbol-misc');
  });
});

// ─── Span → editor coordinates ──────────────────────────────────────────────

describe('air-model.spanStartPosition', () => {
  const span = (line: number, col: number) => ({
    start: 0,
    end: 0,
    line,
    col,
  });

  it('converts 1-based line/col to 0-based positions', () => {
    assert.deepEqual(spanStartPosition(span(3, 5)), { line: 2, character: 4 });
    assert.deepEqual(spanStartPosition(span(1, 1)), { line: 0, character: 0 });
  });

  it('clamps degenerate 0 values (synthesized nodes) to 0', () => {
    assert.deepEqual(spanStartPosition(span(0, 0)), { line: 0, character: 0 });
  });

  it('is unchanged by ASCII line text', () => {
    assert.deepEqual(spanStartPosition(span(1, 9), 'fn add(a: Int)'), {
      line: 0,
      character: 8,
    });
  });

  it('widens the column for astral characters earlier on the line', () => {
    // '😀' is one code point (one compiler "character") but two UTF-16
    // units: skipping `"`,`😀`,`"` (3 code points) covers 4 UTF-16 units.
    assert.deepEqual(spanStartPosition(span(1, 4), '"😀" x'), {
      line: 0,
      character: 4,
    });
    // BMP non-ASCII (é, 2 UTF-8 bytes, 1 UTF-16 unit) needs no widening.
    assert.deepEqual(spanStartPosition(span(1, 4), '"é" x'), {
      line: 0,
      character: 3,
    });
  });

  it('clamps a column past the end of the line text', () => {
    assert.deepEqual(spanStartPosition(span(1, 99), 'short'), {
      line: 0,
      character: 5,
    });
  });
});

describe('air-model.utf16LengthForUtf8Bytes', () => {
  it('equals the byte length for pure ASCII', () => {
    assert.equal(utf16LengthForUtf8Bytes('fn add() {}', 0, 6), 6);
    assert.equal(utf16LengthForUtf8Bytes('fn add() {}', 3, 3), 3);
  });

  it('counts 2- and 3-byte BMP characters as one UTF-16 unit', () => {
    // 'é' = 2 UTF-8 bytes; '€' = 3 UTF-8 bytes; both 1 UTF-16 unit.
    assert.equal(utf16LengthForUtf8Bytes('café', 0, 5), 4);
    assert.equal(utf16LengthForUtf8Bytes('€9', 0, 4), 2);
  });

  it('counts astral characters as two UTF-16 units for four bytes', () => {
    // '😀' = 4 UTF-8 bytes, 2 UTF-16 units.
    assert.equal(utf16LengthForUtf8Bytes('😀x', 0, 5), 3);
    assert.equal(utf16LengthForUtf8Bytes('a😀b', 1, 4), 2);
  });

  it('returns 0 for a zero-byte (synthesized) span', () => {
    assert.equal(utf16LengthForUtf8Bytes('anything', 3, 0), 0);
  });

  it('clamps when the byte length runs past the end of the text', () => {
    assert.equal(utf16LengthForUtf8Bytes('abc', 1, 999), 2);
    assert.equal(utf16LengthForUtf8Bytes('abc', 99, 5), 0);
  });

  it('includes a code point the byte length ends inside of', () => {
    // 2 bytes requested, but '😀' is 4 bytes — the whole pair is included
    // rather than splitting a character.
    assert.equal(utf16LengthForUtf8Bytes('😀', 0, 2), 2);
  });
});
