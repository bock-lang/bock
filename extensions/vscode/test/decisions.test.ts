// Unit tests for the pure decision-database logic in
// src/features/decisions.ts.
//
// `isValidDecisionRecord` is the load-time gate that keeps a single
// malformed `.bock/decisions/**.json` file from crashing tree / tooltip /
// detail rendering (e.g. `record.id.slice`, `confidence.toFixed`,
// `alternatives.length` on `undefined`). These exercise the guard directly
// against a known-good record and a battery of malformed shapes.
//
// The query helpers — `applyDecisionFilter` (facet filtering),
// `sortLoadedDecisions` (sort modes), `describeDecisionView` (view
// description), `parseConfidenceInput`, and `findRecordLine` (jump-to-source
// line lookup) — are exported pure functions, exercised below without any
// `vscode` runtime.

import * as assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import {
  applyDecisionFilter,
  describeDecisionView,
  DECISION_TYPE_TAGS,
  findRecordLine,
  isFilterActive,
  isValidDecisionRecord,
  loadAllDecisions,
  parseConfidenceInput,
  sortLoadedDecisions,
} from '../src/features/decisions';
import type {
  DecisionFilter,
  DecisionTypeTag,
  LoadedDecision,
} from '../src/features/decisions';

/** A structurally complete, valid decision record. */
function validRecord(): Record<string, unknown> {
  return {
    id: 'abc123def456',
    module: 'app/main.bock',
    target: null,
    decision_type: 'codegen',
    choice: 'emit a for-loop',
    alternatives: ['use map', 'use reduce'],
    reasoning: 'fastest for the target',
    model_id: 'claude-opus',
    confidence: 0.92,
    pinned: false,
    pin_reason: null,
    pinned_at: null,
    pinned_by: null,
    superseded_by: null,
    timestamp: '2026-06-09T00:00:00Z',
  };
}

describe('decisions.isValidDecisionRecord', () => {
  it('accepts a structurally complete record', () => {
    assert.equal(isValidDecisionRecord(validRecord()), true);
  });

  it('accepts the minimal required fields (optionals absent)', () => {
    const minimal = {
      id: 'x',
      module: 'm',
      choice: 'c',
      decision_type: 'repair',
      model_id: 'mdl',
      confidence: 0,
      alternatives: [],
      pinned: true,
      timestamp: 't',
    };
    assert.equal(isValidDecisionRecord(minimal), true);
  });

  it('rejects non-objects', () => {
    assert.equal(isValidDecisionRecord(null), false);
    assert.equal(isValidDecisionRecord(undefined), false);
    assert.equal(isValidDecisionRecord('a string'), false);
    assert.equal(isValidDecisionRecord(42), false);
    assert.equal(isValidDecisionRecord([]), false);
  });

  it('rejects a record missing `id`', () => {
    const r = validRecord();
    delete r.id;
    assert.equal(isValidDecisionRecord(r), false);
  });

  it('rejects a record whose `id` is not a string', () => {
    const r = validRecord();
    r.id = 123;
    assert.equal(isValidDecisionRecord(r), false);
  });

  it('rejects a record whose `confidence` is not a number', () => {
    const r = validRecord();
    r.confidence = '0.9';
    assert.equal(isValidDecisionRecord(r), false);
  });

  it('rejects a record whose `confidence` is NaN/Infinity', () => {
    const nan = validRecord();
    nan.confidence = Number.NaN;
    assert.equal(isValidDecisionRecord(nan), false);

    const inf = validRecord();
    inf.confidence = Number.POSITIVE_INFINITY;
    assert.equal(isValidDecisionRecord(inf), false);
  });

  it('rejects a record missing `alternatives`', () => {
    const r = validRecord();
    delete r.alternatives;
    assert.equal(isValidDecisionRecord(r), false);
  });

  it('rejects a record whose `alternatives` is not an array', () => {
    const r = validRecord();
    r.alternatives = 'use map';
    assert.equal(isValidDecisionRecord(r), false);
  });

  it('rejects a record missing `module`', () => {
    const r = validRecord();
    delete r.module;
    assert.equal(isValidDecisionRecord(r), false);
  });

  it('rejects a record missing `choice`', () => {
    const r = validRecord();
    delete r.choice;
    assert.equal(isValidDecisionRecord(r), false);
  });

  it('rejects a record whose `pinned` is not a boolean', () => {
    const r = validRecord();
    r.pinned = 'yes';
    assert.equal(isValidDecisionRecord(r), false);
  });
});

describe('decisions.loadAllDecisions (drop-count threading)', () => {
  let root: string;

  beforeEach(() => {
    const ns = process.env.BOCK_TEST_NAMESPACE ?? 'bock-decisions';
    root = fs.mkdtempSync(path.join(os.tmpdir(), `${ns}-decisions-`));
  });

  afterEach(() => {
    fs.rmSync(root, { recursive: true, force: true });
  });

  /** Write JSON content to `<root>/.bock/decisions/<scope>/<name>`. */
  function writeDecision(
    scope: 'build' | 'runtime',
    name: string,
    content: string,
  ): void {
    const dir = path.join(root, '.bock', 'decisions', scope);
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(path.join(dir, name), content);
  }

  it('returns valid records and zero skipped when all files are well-formed', async () => {
    writeDecision('build', 'a.json', JSON.stringify(validRecord()));
    writeDecision(
      'runtime',
      'b.json',
      JSON.stringify([validRecord(), validRecord()]),
    );

    const { decisions, skipped } = await loadAllDecisions(root);
    assert.equal(decisions.length, 3);
    assert.equal(skipped, 0);
    assert.deepEqual(
      decisions.map((d) => d.scope).sort(),
      ['build', 'runtime', 'runtime'],
    );
  });

  it('counts a malformed-JSON file as one skip and keeps the rest', async () => {
    writeDecision('build', 'good.json', JSON.stringify(validRecord()));
    writeDecision('build', 'broken.json', '{ not valid json');

    const { decisions, skipped } = await loadAllDecisions(root);
    assert.equal(decisions.length, 1);
    assert.equal(skipped, 1);
  });

  it('counts each invalid-shape record as a skip (array of mixed records)', async () => {
    const invalidNoId = validRecord();
    delete invalidNoId.id;
    const invalidConfidence = validRecord();
    invalidConfidence.confidence = 'high';

    writeDecision(
      'build',
      'mixed.json',
      JSON.stringify([validRecord(), invalidNoId, invalidConfidence]),
    );

    const { decisions, skipped } = await loadAllDecisions(root);
    assert.equal(decisions.length, 1, 'only the one valid record survives');
    assert.equal(skipped, 2, 'both malformed records counted');
  });

  it('returns an empty result with zero skips when no decisions dir exists', async () => {
    const { decisions, skipped } = await loadAllDecisions(root);
    assert.equal(decisions.length, 0);
    assert.equal(skipped, 0);
  });
});

// ─── Query helpers ──────────────────────────────────────────────────────────

/** Build a LoadedDecision with sensible defaults, overridable per test. */
function loaded(over: {
  id?: string;
  module?: string;
  decision_type?: DecisionTypeTag;
  pinned?: boolean;
  confidence?: number;
  timestamp?: string;
  scope?: 'build' | 'runtime';
  sourceFile?: string;
}): LoadedDecision {
  return {
    record: {
      id: over.id ?? 'id-default',
      module: over.module ?? 'app/main.bock',
      target: null,
      decision_type: over.decision_type ?? 'codegen',
      choice: 'emit a for-loop',
      alternatives: [],
      reasoning: null,
      model_id: 'claude-opus',
      confidence: over.confidence ?? 0.9,
      pinned: over.pinned ?? false,
      pin_reason: null,
      pinned_at: null,
      pinned_by: null,
      superseded_by: null,
      timestamp: over.timestamp ?? '2026-06-09T00:00:00Z',
    },
    scope: over.scope ?? 'build',
    sourceFile: over.sourceFile ?? '/proj/.bock/decisions/build/a.json',
  };
}

function idsOf(list: LoadedDecision[]): string[] {
  return list.map((d) => d.record.id);
}

describe('decisions.applyDecisionFilter', () => {
  const base = [
    loaded({ id: 'a', decision_type: 'codegen', pinned: false, confidence: 0.4 }),
    loaded({ id: 'b', decision_type: 'repair', pinned: true, confidence: 0.7 }),
    loaded({ id: 'c', decision_type: 'optimize', pinned: false, confidence: 0.95 }),
    loaded({ id: 'd', decision_type: 'codegen', pinned: true, confidence: 0.99 }),
  ];

  it('returns everything for the empty filter', () => {
    assert.deepEqual(idsOf(applyDecisionFilter(base, {})), ['a', 'b', 'c', 'd']);
  });

  it('does not mutate the input array', () => {
    const input = [...base];
    applyDecisionFilter(input, { pinned: 'pinned', minConfidence: 0.9 });
    assert.deepEqual(idsOf(input), ['a', 'b', 'c', 'd']);
  });

  it('filters by a single decision type', () => {
    assert.deepEqual(
      idsOf(applyDecisionFilter(base, { types: ['codegen'] })),
      ['a', 'd'],
    );
  });

  it('filters by multiple decision types (OR within the facet)', () => {
    assert.deepEqual(
      idsOf(applyDecisionFilter(base, { types: ['repair', 'optimize'] })),
      ['b', 'c'],
    );
  });

  it('treats an empty types array as no type constraint', () => {
    assert.deepEqual(idsOf(applyDecisionFilter(base, { types: [] })), [
      'a',
      'b',
      'c',
      'd',
    ]);
  });

  it('filters by pin state: pinned / unpinned / all', () => {
    assert.deepEqual(idsOf(applyDecisionFilter(base, { pinned: 'pinned' })), [
      'b',
      'd',
    ]);
    assert.deepEqual(idsOf(applyDecisionFilter(base, { pinned: 'unpinned' })), [
      'a',
      'c',
    ]);
    assert.deepEqual(idsOf(applyDecisionFilter(base, { pinned: 'all' })), [
      'a',
      'b',
      'c',
      'd',
    ]);
  });

  it('applies minimum confidence inclusively (>=)', () => {
    assert.deepEqual(
      idsOf(applyDecisionFilter(base, { minConfidence: 0.7 })),
      ['b', 'c', 'd'],
      'record at exactly 0.7 is kept',
    );
  });

  it('keeps everything at minConfidence 0 (still an active facet)', () => {
    assert.deepEqual(idsOf(applyDecisionFilter(base, { minConfidence: 0 })), [
      'a',
      'b',
      'c',
      'd',
    ]);
  });

  it('combines facets with AND', () => {
    const filter: DecisionFilter = {
      types: ['codegen'],
      pinned: 'pinned',
      minConfidence: 0.9,
    };
    assert.deepEqual(idsOf(applyDecisionFilter(base, filter)), ['d']);
  });

  it('returns empty when no record satisfies every facet', () => {
    assert.deepEqual(
      idsOf(applyDecisionFilter(base, { types: ['repair'], pinned: 'unpinned' })),
      [],
    );
  });
});

describe('decisions.isFilterActive', () => {
  it('is inactive for the empty filter', () => {
    assert.equal(isFilterActive({}), false);
  });

  it('is inactive for the no-op facet values', () => {
    assert.equal(isFilterActive({ pinned: 'all' }), false);
    assert.equal(isFilterActive({ types: [] }), false);
  });

  it('is active when any facet constrains the set', () => {
    assert.equal(isFilterActive({ types: ['codegen'] }), true);
    assert.equal(isFilterActive({ pinned: 'unpinned' }), true);
    assert.equal(isFilterActive({ minConfidence: 0 }), true);
  });
});

describe('decisions.sortLoadedDecisions', () => {
  const list = [
    loaded({ id: 'b', module: 'zz.bock', pinned: true, confidence: 0.9, timestamp: '2026-06-01T00:00:00Z' }),
    loaded({ id: 'a', module: 'aa.bock', pinned: false, confidence: 0.5, timestamp: '2026-06-03T00:00:00Z' }),
    loaded({ id: 'd', module: 'aa.bock', pinned: true, confidence: 0.5, timestamp: '2026-06-02T00:00:00Z' }),
    loaded({ id: 'c', module: 'zz.bock', pinned: false, confidence: 0.7, timestamp: '2026-06-04T00:00:00Z' }),
  ];

  it('default: unpinned first, then by id', () => {
    assert.deepEqual(idsOf(sortLoadedDecisions(list, 'default')), [
      'a',
      'c',
      'b',
      'd',
    ]);
  });

  it('confidence-asc: ascending confidence, ties broken by id', () => {
    assert.deepEqual(idsOf(sortLoadedDecisions(list, 'confidence-asc')), [
      'a',
      'd',
      'c',
      'b',
    ]);
  });

  it('confidence-desc: descending confidence, ties broken by id', () => {
    assert.deepEqual(idsOf(sortLoadedDecisions(list, 'confidence-desc')), [
      'b',
      'c',
      'a',
      'd',
    ]);
  });

  it('newest: timestamp descending (ISO-8601 lexicographic)', () => {
    assert.deepEqual(idsOf(sortLoadedDecisions(list, 'newest')), [
      'c',
      'a',
      'd',
      'b',
    ]);
  });

  it('newest: equal timestamps fall back to id order', () => {
    const same = [
      loaded({ id: 'y', timestamp: '2026-06-09T12:00:00Z' }),
      loaded({ id: 'x', timestamp: '2026-06-09T12:00:00Z' }),
    ];
    assert.deepEqual(idsOf(sortLoadedDecisions(same, 'newest')), ['x', 'y']);
  });

  it('module: module ascending, default order within a module', () => {
    assert.deepEqual(idsOf(sortLoadedDecisions(list, 'module')), [
      'a',
      'd',
      'c',
      'b',
    ]);
  });

  it('is pure: never mutates the input array', () => {
    const input = [...list];
    for (const mode of ['default', 'confidence-asc', 'confidence-desc', 'newest', 'module'] as const) {
      sortLoadedDecisions(input, mode);
      assert.deepEqual(idsOf(input), ['b', 'a', 'd', 'c'], `mutated by ${mode}`);
    }
  });

  it('handles the empty list', () => {
    assert.deepEqual(sortLoadedDecisions([], 'newest'), []);
  });
});

describe('decisions.describeDecisionView', () => {
  it('returns undefined when nothing diverges from the defaults', () => {
    assert.equal(describeDecisionView({}, 'default'), undefined);
    assert.equal(describeDecisionView({ pinned: 'all', types: [] }, 'default'), undefined);
  });

  it('describes a single type facet', () => {
    assert.equal(
      describeDecisionView({ types: ['codegen'] }, 'default'),
      'type:codegen',
    );
  });

  it('describes multiple types comma-joined', () => {
    assert.equal(
      describeDecisionView({ types: ['codegen', 'repair'] }, 'default'),
      'type:codegen,repair',
    );
  });

  it('describes pin state and minimum confidence', () => {
    assert.equal(describeDecisionView({ pinned: 'unpinned' }, 'default'), 'unpinned');
    assert.equal(describeDecisionView({ minConfidence: 0.8 }, 'default'), 'conf≥0.8');
  });

  it('describes a non-default sort mode', () => {
    assert.equal(describeDecisionView({}, 'newest'), 'sort:newest');
    assert.equal(describeDecisionView({}, 'confidence-asc'), 'sort:conf↑');
    assert.equal(describeDecisionView({}, 'confidence-desc'), 'sort:conf↓');
    assert.equal(describeDecisionView({}, 'module'), 'sort:module');
  });

  it('joins active parts with the compact separator, filter facets first', () => {
    assert.equal(
      describeDecisionView(
        { types: ['codegen'], pinned: 'pinned', minConfidence: 0.9 },
        'newest',
      ),
      'type:codegen · pinned · conf≥0.9 · sort:newest',
    );
  });
});

describe('decisions.parseConfidenceInput', () => {
  it('accepts in-range numbers, including the bounds', () => {
    assert.equal(parseConfidenceInput('0.85'), 0.85);
    assert.equal(parseConfidenceInput('0'), 0);
    assert.equal(parseConfidenceInput('1'), 1);
  });

  it('tolerates surrounding whitespace', () => {
    assert.equal(parseConfidenceInput('  0.5 '), 0.5);
  });

  it('rejects empty and non-numeric input', () => {
    assert.equal(parseConfidenceInput(''), undefined);
    assert.equal(parseConfidenceInput('   '), undefined);
    assert.equal(parseConfidenceInput('high'), undefined);
  });

  it('rejects out-of-range and non-finite values', () => {
    assert.equal(parseConfidenceInput('1.5'), undefined);
    assert.equal(parseConfidenceInput('-0.1'), undefined);
    assert.equal(parseConfidenceInput('NaN'), undefined);
    assert.equal(parseConfidenceInput('Infinity'), undefined);
  });
});

describe('decisions.findRecordLine', () => {
  it('finds the id line in pretty-printed JSON (array of records)', () => {
    const a = validRecord();
    a.id = 'first-id';
    const b = validRecord();
    b.id = 'second-id';
    const text = JSON.stringify([a, b], null, 2);
    const lines = text.split('\n');

    const lineA = findRecordLine(text, 'first-id');
    const lineB = findRecordLine(text, 'second-id');
    assert.notEqual(lineA, undefined);
    assert.notEqual(lineB, undefined);
    assert.match(lines[lineA as number], /"id": "first-id"/);
    assert.match(lines[lineB as number], /"id": "second-id"/);
    assert.ok((lineA as number) < (lineB as number));
  });

  it('returns line 0 for compact single-line JSON', () => {
    const text = JSON.stringify(validRecord());
    assert.equal(findRecordLine(text, 'abc123def456'), 0);
  });

  it('handles whitespace variants around the colon', () => {
    const text = '{\n  "id" :\t"spaced-id",\n  "module": "m"\n}';
    assert.equal(findRecordLine(text, 'spaced-id'), 1);
  });

  it('counts lines correctly with CRLF line endings', () => {
    const text = '{\r\n  "module": "m",\r\n  "id": "crlf-id"\r\n}';
    assert.equal(findRecordLine(text, 'crlf-id'), 2);
  });

  it('returns undefined when the id is not present', () => {
    const text = JSON.stringify([validRecord()], null, 2);
    assert.equal(findRecordLine(text, 'not-there'), undefined);
  });

  it('returns undefined for the empty id', () => {
    assert.equal(findRecordLine('{"id": ""}', ''), undefined);
  });

  it('does not false-positive on the same hash under another key', () => {
    // `superseded_by` holds the searched id, but no `"id"` key does.
    const text = JSON.stringify(
      { id: 'other', superseded_by: 'searched-id', model_id: 'searched-id' },
      null,
      2,
    );
    assert.equal(findRecordLine(text, 'searched-id'), undefined);
  });

  it('matches the real `"id"` key even when keys merely end in `id`', () => {
    const r = validRecord();
    r.id = 'shared-hash';
    r.model_id = 'shared-hash';
    const text = JSON.stringify(r, null, 2);
    const line = findRecordLine(text, 'shared-hash');
    assert.notEqual(line, undefined);
    assert.match(text.split('\n')[line as number], /"id": "shared-hash"/);
  });

  it('escapes regex metacharacters in the id', () => {
    // Positive: an id full of metacharacters is still found literally
    // (unescaped, `(1)` would be a group and `$` an anchor — no match).
    const text = JSON.stringify({ id: 'a.b+c(1)$' }, null, 2);
    assert.equal(findRecordLine(text, 'a.b+c(1)$'), 1);
    // Negative: `.` must not act as a wildcard (unescaped, `a.b` would
    // match the different id `aXb`).
    assert.equal(
      findRecordLine(JSON.stringify({ id: 'aXb' }, null, 2), 'a.b'),
      undefined,
    );
  });
});

describe('decisions.DECISION_TYPE_TAGS', () => {
  it('covers every decision type exactly once', () => {
    assert.deepEqual(
      [...DECISION_TYPE_TAGS].sort(),
      [
        'adaptive_recovery',
        'codegen',
        'handler_choice',
        'optimize',
        'repair',
        'rule_applied',
      ],
    );
  });
});
