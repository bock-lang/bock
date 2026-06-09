// Unit tests for the decision-record validation guard in
// src/features/decisions.ts.
//
// `isValidDecisionRecord` is the load-time gate that keeps a single
// malformed `.bock/decisions/**.json` file from crashing tree / tooltip /
// detail rendering (e.g. `record.id.slice`, `confidence.toFixed`,
// `alternatives.length` on `undefined`). These exercise the guard directly
// against a known-good record and a battery of malformed shapes.

import * as assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import {
  isValidDecisionRecord,
  loadAllDecisions,
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
