// Unit tests for the activation-resilience contract of VocabService (R2).
//
// `VocabService.load` runs inside `activate()`. If it threw on a missing or
// corrupt `assets/vocab.json`, the entire extension UI would fail to register
// (no commands, no spec panel, no decisions) — contradicting the README's
// "degrades gracefully" promise. These tests pin the degrade behaviour: a
// nonexistent extension path yields a service whose every getter returns
// `undefined`/`[]` without throwing, backed by the empty default vocab.
//
// The `vscode` module is stubbed via test/register-vscode.ts; the stub's
// `window.showErrorMessage` makes the load-failure toast a no-op here.

import * as assert from 'node:assert/strict';
import * as vscode from 'vscode';
import { VocabService, emptyVocab } from '../src/vocab';

// A ctx whose extensionPath points at a directory that does not exist, so the
// `fs.readFile` of `<dir>/assets/vocab.json` rejects with ENOENT and load()
// takes its degrade-to-empty branch. Only `extensionPath` is read.
const missingCtx = {
  extensionPath: '/no/such/dir',
} as unknown as vscode.ExtensionContext;

describe('VocabService.load — missing/corrupt vocab degrades to empty', () => {
  it('resolves (does not reject) when the vocab file is absent', async () => {
    // The whole point: this must not throw out of activate().
    const service = await VocabService.load(missingCtx);
    assert.ok(service, 'load() should resolve to a VocabService');
  });

  it('reports the empty fallback version, not a thrown error', async () => {
    const service = await VocabService.load(missingCtx);
    assert.equal(service.get().version, emptyVocab().version);
  });

  it('every getter returns undefined/[] without throwing', async () => {
    const service = await VocabService.load(missingCtx);

    assert.equal(service.getKeyword('fn'), undefined);
    assert.equal(service.getOperator('+'), undefined);
    assert.equal(service.getAnnotation('@pure'), undefined);
    assert.equal(service.getAnnotation('pure'), undefined);
    assert.equal(service.getDiagnostic('E0001'), undefined);
    assert.equal(service.getSpecRef('fn'), undefined);
    assert.equal(service.getStdlibSymbol('core.x', 'y'), undefined);
    assert.deepEqual(service.getBuiltinMethods('List'), []);
  });

  it('exposes a structurally-complete empty vocab (all nested arrays present)', async () => {
    // buildCache() in features/hover.ts iterates these arrays unguarded, so
    // they must exist (and be empty) on the fallback, not be undefined.
    const { language, stdlib, diagnostics, tooling } = (
      await VocabService.load(missingCtx)
    ).get();

    assert.deepEqual(language.keywords, []);
    assert.deepEqual(language.operators, []);
    assert.deepEqual(language.annotations, []);
    assert.deepEqual(language.primitive_types, []);
    assert.deepEqual(language.prelude_types, []);
    assert.deepEqual(language.prelude_functions, []);
    assert.deepEqual(language.prelude_traits, []);
    assert.deepEqual(language.prelude_constructors, []);
    assert.deepEqual(language.strictness_levels, []);
    assert.deepEqual(stdlib.modules, []);
    assert.deepEqual(stdlib.builtin_methods, []);
    assert.deepEqual(stdlib.builtin_globals, []);
    assert.deepEqual(diagnostics.codes, []);
    assert.deepEqual(tooling.targets, []);
    assert.deepEqual(tooling.ai_providers, []);
    assert.deepEqual(tooling.commands, []);
  });
});

describe('emptyVocab — getters are null-safe against a structurally partial vocab', () => {
  it('does not throw when nested arrays are missing entirely', () => {
    // Simulate a parseable-but-incomplete vocab.json (e.g. an old schema or a
    // truncated regen) where whole nested arrays are absent. The getters guard
    // with `?.`/`?? []`, so lookups return undefined/[] rather than throwing.
    const partial = { version: '9.9.9' } as unknown as ReturnType<
      typeof emptyVocab
    >;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any -- reach the
    // private ctor to inject a deliberately malformed vocab for this guard test.
    const service = new (VocabService as any)(partial, '/tmp/none.json');

    assert.doesNotThrow(() => {
      service.getKeyword('fn');
      service.getOperator('+');
      service.getAnnotation('pure');
      service.getDiagnostic('E0001');
      service.getSpecRef('fn');
      service.getStdlibSymbol('core.x', 'y');
      service.getBuiltinMethods('List');
    });
    assert.deepEqual(service.getBuiltinMethods('List'), []);
    assert.equal(service.getKeyword('fn'), undefined);
  });
});
