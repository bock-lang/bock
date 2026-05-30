// Unit tests for the pure parsers in src/features/effect-analyzer.ts.
//
// These exercise real extension logic — the regex/section parsers that turn
// raw `.bock` and `bock.project` text into effect definitions and handler
// bindings — without any Extension Host. The `vscode` module is stubbed via
// test/register-vscode.ts.

import * as assert from 'node:assert/strict';
import {
  extractEffects,
  parseProjectEffects,
  type EffectDef,
} from '../src/features/effect-analyzer';
import { Uri } from './vscode-stub';

// The functions take `vscode.Uri`; the stub Uri is structurally compatible
// for the members they touch. Cast through `unknown` at the call site.
const uri = Uri.file('/ws/example.bock') as unknown as Parameters<
  typeof extractEffects
>[0];

describe('effect-analyzer.extractEffects', () => {
  it('extracts an effect block with its operation names', () => {
    const text = [
      'module demo',
      '',
      'public effect Logger {',
      '  fn log(message: String) -> Void',
      '  fn warn(message: String) -> Void',
      '}',
      '',
    ].join('\n');

    const out = new Map<string, EffectDef>();
    extractEffects(uri, text, out);

    const logger = out.get('Logger');
    assert.ok(logger, 'Logger effect should be registered');
    assert.deepEqual(logger.operations, ['log', 'warn']);
    assert.deepEqual(logger.components, []);
  });

  it('extracts a composite effect alias into its component names', () => {
    const text = [
      'effect Logger { fn log(m: String) -> Void }',
      'effect Clock { fn now() -> Int }',
      'public effect App = Logger + Clock',
    ].join('\n');

    const out = new Map<string, EffectDef>();
    extractEffects(uri, text, out);

    const app = out.get('App');
    assert.ok(app, 'App composite should be registered');
    assert.deepEqual(app.components, ['Logger', 'Clock']);
    // A composite alias has no operations of its own.
    assert.deepEqual(app.operations, []);
  });
});

describe('effect-analyzer.parseProjectEffects', () => {
  const projectUri = Uri.file('/ws/bock.project') as unknown as Parameters<
    typeof parseProjectEffects
  >[0];

  it('reads handler bindings only from the [effects] section', () => {
    const toml = [
      '[package]',
      'name = "demo"',
      'logger = "ignored.outside.effects"',
      '',
      '# handlers for the demo app',
      '[effects]',
      'Logger = "std.io.ConsoleLogger"',
      "Clock = 'std.time.SystemClock'",
      '',
      '[build]',
      'target = "js"',
    ].join('\n');

    const bindings = parseProjectEffects(projectUri, toml);

    assert.equal(bindings.length, 2, 'only the two [effects] entries');
    assert.deepEqual(
      bindings.map((b) => ({ effect: b.effect, handler: b.handler, layer: b.layer })),
      [
        { effect: 'Logger', handler: 'std.io.ConsoleLogger', layer: 'project' },
        { effect: 'Clock', handler: 'std.time.SystemClock', layer: 'project' },
      ],
    );
    // The binding's line should point at the source row inside [effects].
    assert.equal(bindings[0].location?.line, 6);
  });

  it('returns no bindings when there is no [effects] section', () => {
    const toml = '[package]\nname = "demo"\n[build]\ntarget = "rust"\n';
    assert.deepEqual(parseProjectEffects(projectUri, toml), []);
  });
});
