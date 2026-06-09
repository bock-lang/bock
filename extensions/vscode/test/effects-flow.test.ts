// Unit tests for the pure effect-flow rendering helpers in
// src/features/effects-flow.ts.
//
// These cover the Mermaid graph builder, node-id derivation, the navigation
// map, and the small presentation escapers. They run headlessly under
// Mocha + ts-node — no Extension Host — with `vscode` stubbed via
// test/register-vscode.ts. effects-flow.ts is free of
// `vscode-languageclient`, so (unlike effects.ts) it loads in the plain
// CommonJS resolver. The helpers only touch the stubbed `Uri`/`Range`/
// `Position` constructors at runtime.

import * as assert from 'node:assert/strict';
import {
  buildMermaid,
  nodeId,
  buildNavigationMap,
  escapeMermaid,
  targetStrategy,
  layerTag,
} from '../src/features/effects-flow';
import type {
  EffectFlow,
  EffectDef,
  EffectOpCall,
  HandlerBinding,
  Location,
} from '../src/features/effect-analyzer';
import { Uri, Position, Range } from './vscode-stub';

// ─── Fixtures ───────────────────────────────────────────────────────────────
// EffectFlow references `vscode.Uri`/`Range`/`Position`; the stub is
// structurally compatible for the members these helpers touch
// (`uri.toString()`, `uri.fsPath`, `range.start.{line,character}`). Cast the
// assembled object through `unknown` to the real type at the boundary.

function loc(fsPath: string, line: number, column: number): Location {
  return {
    uri: Uri.file(fsPath) as unknown as Location['uri'],
    line,
    column,
  };
}

interface FlowParts {
  functionName?: string;
  docPath?: string;
  fnStart?: { line: number; character: number };
  effects?: string[];
  effectDefs?: EffectDef[];
  callees?: EffectOpCall[];
  handlers?: HandlerBinding[];
}

function makeFlow(parts: FlowParts = {}): EffectFlow {
  const docPath = parts.docPath ?? '/ws/app.bock';
  const fnStart = parts.fnStart ?? { line: 10, character: 2 };
  const flow = {
    functionName: parts.functionName ?? 'doWork',
    documentUri: Uri.file(docPath),
    functionRange: new Range(
      new Position(fnStart.line, fnStart.character),
      new Position(fnStart.line + 5, 1),
    ),
    effects: parts.effects ?? [],
    effectDefs: parts.effectDefs ?? [],
    callees: parts.callees ?? [],
    handlers: parts.handlers ?? [],
  };
  return flow as unknown as EffectFlow;
}

// ─── nodeId ─────────────────────────────────────────────────────────────────

describe('effects-flow.nodeId', () => {
  it('prefixes by kind and leaves identifier-safe names intact', () => {
    assert.equal(nodeId('fn', 'doWork'), 'fn_doWork');
    assert.equal(nodeId('eff', 'Logger'), 'eff_Logger');
    assert.equal(nodeId('op', 'log'), 'op_log');
    assert.equal(nodeId('hnd', 'Log_StdoutLogger'), 'hnd_Log_StdoutLogger');
  });

  it('sanitizes every non-identifier character to underscore', () => {
    assert.equal(nodeId('op', 'std.io.print'), 'op_std_io_print');
    assert.equal(nodeId('eff', 'A+B'), 'eff_A_B');
    assert.equal(nodeId('op', 'a b!c'), 'op_a_b_c');
  });

  it('keeps distinct kinds from colliding for the same name', () => {
    // The kind prefix is what prevents an effect and an op of the same name
    // from sharing a Mermaid node id.
    assert.notEqual(nodeId('eff', 'log'), nodeId('op', 'log'));
  });
});

// ─── escapeMermaid ──────────────────────────────────────────────────────────

describe('effects-flow.escapeMermaid', () => {
  it('replaces double quotes with the #quot; entity', () => {
    assert.equal(escapeMermaid('say "hi"'), 'say #quot;hi#quot;');
  });

  it('backslash-escapes the pipe so it cannot end an edge label', () => {
    assert.equal(escapeMermaid('a|b'), 'a\\|b');
  });

  it('leaves ordinary text untouched', () => {
    assert.equal(escapeMermaid('plain text 42'), 'plain text 42');
  });
});

// ─── targetStrategy ─────────────────────────────────────────────────────────

describe('effects-flow.targetStrategy', () => {
  it('maps the script targets to parameter passing', () => {
    for (const id of ['js', 'ts', 'python']) {
      assert.deepEqual(targetStrategy(id), {
        support: 'Emulated',
        strategy: 'Parameter passing',
      });
    }
  });

  it('uses trait/interface strategies for rust and go', () => {
    assert.deepEqual(targetStrategy('rust'), {
      support: 'Emulated',
      strategy: 'Trait parameter',
    });
    assert.deepEqual(targetStrategy('go'), {
      support: 'Emulated',
      strategy: 'Interface parameter',
    });
  });

  it('falls back to parameter passing for unknown targets', () => {
    assert.deepEqual(targetStrategy('zig'), {
      support: 'Emulated',
      strategy: 'Parameter passing',
    });
  });
});

// ─── layerTag ───────────────────────────────────────────────────────────────

describe('effects-flow.layerTag', () => {
  it('returns the layer name verbatim', () => {
    assert.equal(layerTag('local'), 'local');
    assert.equal(layerTag('module'), 'module');
    assert.equal(layerTag('project'), 'project');
  });
});

// ─── buildMermaid ───────────────────────────────────────────────────────────

describe('effects-flow.buildMermaid', () => {
  it('starts a left-to-right graph with the function node', () => {
    const src = buildMermaid(makeFlow({ functionName: 'greet' }));
    const lines = src.split('\n');
    assert.equal(lines[0], 'graph LR');
    assert.match(src, /fn_greet\["greet\(…\)"\]:::fnNode/);
  });

  it('emits one effect node per declared effect', () => {
    const src = buildMermaid(makeFlow({ effects: ['Log', 'Clock'] }));
    assert.match(src, /eff_Log\(\["Log"\]\):::effNode/);
    assert.match(src, /eff_Clock\(\["Clock"\]\):::effNode/);
  });

  it('dedups operation nodes when the same op is called twice', () => {
    const callees: EffectOpCall[] = [
      { operation: 'log', effect: 'Log', location: loc('/ws/app.bock', 12, 4) },
      { operation: 'log', effect: 'Log', location: loc('/ws/app.bock', 14, 4) },
    ];
    const src = buildMermaid(makeFlow({ effects: ['Log'], callees }));
    const opNodeDefs = src
      .split('\n')
      .filter((l) => /op_log\[\["log\(\)"\]\]:::opNode/.test(l));
    assert.equal(opNodeDefs.length, 1, 'op node should be declared once');
  });

  it('draws the op→effect membership edge only for declared effects', () => {
    // `save` belongs to `Db`, which is NOT in the with-clause, so the
    // membership edge must be filtered out; `log`/`Log` survives.
    const callees: EffectOpCall[] = [
      { operation: 'log', effect: 'Log', location: loc('/ws/app.bock', 12, 4) },
      { operation: 'save', effect: 'Db', location: loc('/ws/app.bock', 13, 4) },
    ];
    const src = buildMermaid(makeFlow({ effects: ['Log'], callees }));
    assert.match(src, /op_log -\.->\|of\| eff_Log/);
    assert.doesNotMatch(src, /op_save -\.->\|of\| eff_Db/);
  });

  it('omits the membership edge when the op has no resolved effect', () => {
    const callees: EffectOpCall[] = [
      { operation: 'mystery', location: loc('/ws/app.bock', 12, 4) },
    ];
    const src = buildMermaid(makeFlow({ effects: ['Log'], callees }));
    assert.doesNotMatch(src, /op_mystery -\.->\|of\|/);
  });

  it('labels fn→op edges with the with-clause effects', () => {
    const callees: EffectOpCall[] = [
      { operation: 'log', effect: 'Log', location: loc('/ws/app.bock', 12, 4) },
    ];
    const src = buildMermaid(
      makeFlow({ functionName: 'run', effects: ['Log', 'Clock'], callees }),
    );
    assert.match(src, /fn_run -->\|with Log, Clock\| op_log/);
  });

  it('falls back to a "calls" edge label when there are no effects', () => {
    const callees: EffectOpCall[] = [
      { operation: 'noop', location: loc('/ws/app.bock', 12, 4) },
    ];
    const src = buildMermaid(makeFlow({ effects: [], callees }));
    assert.match(src, /fn_doWork -->\|calls\| op_noop/);
  });

  it('dedups handler nodes and links each effect to its handler', () => {
    const handlers: HandlerBinding[] = [
      { effect: 'Log', handler: 'Stdout', layer: 'module' },
      { effect: 'Log', handler: 'Stdout', layer: 'module' },
    ];
    const src = buildMermaid(makeFlow({ effects: ['Log'], handlers }));
    const handlerDefs = src
      .split('\n')
      .filter((l) => /hnd_Log_Stdout\["Stdout \[module\]"\]/.test(l));
    assert.equal(handlerDefs.length, 1, 'handler node declared once');
    // But the effect→handler edge is emitted per binding occurrence.
    const edges = src
      .split('\n')
      .filter((l) => /eff_Log -\.->\|handled by\| hnd_Log_Stdout/.test(l));
    assert.equal(edges.length, 2);
  });

  it('emits a click binding for every interactive node', () => {
    const callees: EffectOpCall[] = [
      { operation: 'log', effect: 'Log', location: loc('/ws/app.bock', 12, 4) },
    ];
    const handlers: HandlerBinding[] = [
      { effect: 'Log', handler: 'Stdout', layer: 'local' },
    ];
    const src = buildMermaid(
      makeFlow({ functionName: 'run', effects: ['Log'], callees, handlers }),
    );
    assert.match(src, /click fn_run bockNavigate/);
    assert.match(src, /click eff_Log bockNavigate/);
    assert.match(src, /click op_log bockNavigate/);
    assert.match(src, /click hnd_Log_Stdout bockNavigate/);
  });
});

// ─── buildNavigationMap ─────────────────────────────────────────────────────

describe('effects-flow.buildNavigationMap', () => {
  it('always maps the function node to the function range start', () => {
    const flow = makeFlow({
      functionName: 'greet',
      docPath: '/ws/app.bock',
      fnStart: { line: 7, character: 4 },
    });
    const map = buildNavigationMap(flow);
    assert.deepEqual(map['fn_greet'], {
      uri: 'file:///ws/app.bock',
      line: 7,
      column: 4,
    });
  });

  it('maps an effect node only when its definition has a location', () => {
    const effectDefs: EffectDef[] = [
      {
        name: 'Log',
        operations: ['log'],
        components: [],
        defined: loc('/ws/log.bock', 3, 0),
      },
      // `Clock` has no `defined` → should not appear in the map.
      { name: 'Clock', operations: ['now'], components: [] },
    ];
    const map = buildNavigationMap(
      makeFlow({ effects: ['Log', 'Clock'], effectDefs }),
    );
    assert.deepEqual(map['eff_Log'], {
      uri: 'file:///ws/log.bock',
      line: 3,
      column: 0,
    });
    assert.equal(map['eff_Clock'], undefined);
  });

  it('maps operation nodes first-wins (the earliest call site sticks)', () => {
    const callees: EffectOpCall[] = [
      { operation: 'log', effect: 'Log', location: loc('/ws/app.bock', 12, 4) },
      { operation: 'log', effect: 'Log', location: loc('/ws/app.bock', 20, 8) },
    ];
    const map = buildNavigationMap(makeFlow({ effects: ['Log'], callees }));
    assert.deepEqual(map['op_log'], {
      uri: 'file:///ws/app.bock',
      line: 12,
      column: 4,
    });
  });

  it('maps handler nodes that carry a location, first-wins', () => {
    const handlers: HandlerBinding[] = [
      {
        effect: 'Log',
        handler: 'Stdout',
        layer: 'module',
        location: loc('/ws/handlers.bock', 5, 2),
      },
      {
        effect: 'Log',
        handler: 'Stdout',
        layer: 'module',
        location: loc('/ws/handlers.bock', 99, 0),
      },
      // Location-less project binding → no map entry of its own.
      { effect: 'Net', handler: 'native', layer: 'project' },
    ];
    const map = buildNavigationMap(
      makeFlow({ effects: ['Log', 'Net'], handlers }),
    );
    assert.deepEqual(map['hnd_Log_Stdout'], {
      uri: 'file:///ws/handlers.bock',
      line: 5,
      column: 2,
    });
    assert.equal(map['hnd_Net_native'], undefined);
  });
});
