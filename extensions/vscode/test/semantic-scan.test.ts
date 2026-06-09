// Unit tests for the pure semantic-token scanner in
// src/features/semantic-scan.ts.
//
// Runs headlessly under Mocha + ts-node with `vscode` stubbed via
// test/register-vscode.ts (the scanner touches only the stubbed
// `Uri.file`, which it hands to the reused `extractEffects` parser).
// Covers: declarations of every kind, enum variants, suppression inside
// strings / line comments / block comments / triple-quoted blocks, in-file
// effect names + operation call sites, annotations, module/use paths,
// prelude-vocab recognition (defaultLibrary), the empty-vocab degraded
// path, and a realistic stdlib-like snippet.

import * as assert from 'node:assert/strict';
import {
  scanSemanticTokens,
  semanticVocabInput,
  maskLine,
  initialMaskState,
  SEMANTIC_TOKEN_TYPES,
  SEMANTIC_TOKEN_MODIFIERS,
  type MaskState,
  type SemanticToken,
  type SemanticVocabInput,
} from '../src/features/semantic-scan';
import type { Vocab } from '../src/shared/types';

// ─── Fixtures and helpers ────────────────────────────────────────────────────

const VOCAB: SemanticVocabInput = {
  primitiveTypes: ['Int', 'Float', 'Bool', 'String', 'Char', 'Void', 'Never'],
  preludeTypes: ['Optional', 'Result', 'List', 'Map', 'Set', 'Fn', 'Ordering'],
  preludeFunctions: ['print', 'println', 'assert', 'todo'],
  preludeTraits: ['Comparable', 'Equatable', 'Iterator', 'Error'],
  preludeConstructors: ['Some', 'None', 'Ok', 'Err', 'Less', 'Equal', 'Greater'],
};

const EMPTY: SemanticVocabInput = {
  primitiveTypes: [],
  preludeTypes: [],
  preludeFunctions: [],
  preludeTraits: [],
  preludeConstructors: [],
};

function scan(text: string, vocab: SemanticVocabInput = VOCAB): SemanticToken[] {
  return scanSemanticTokens(text, vocab);
}

function ofType(tokens: SemanticToken[], tokenType: string): SemanticToken[] {
  return tokens.filter((t) => t.tokenType === tokenType);
}

function at(
  tokens: SemanticToken[],
  line: number,
  char: number,
): SemanticToken | undefined {
  return tokens.find((t) => t.line === line && t.char === char);
}

/** Asserts a token exists at (line, char) with the given shape. */
function expectToken(
  tokens: SemanticToken[],
  line: number,
  char: number,
  length: number,
  tokenType: string,
  tokenModifiers: string[],
): void {
  const t = at(tokens, line, char);
  assert.ok(t, `expected a token at ${line}:${char}`);
  assert.equal(t.length, length, `length at ${line}:${char}`);
  assert.equal(t.tokenType, tokenType, `tokenType at ${line}:${char}`);
  assert.deepEqual(t.tokenModifiers, tokenModifiers, `modifiers at ${line}:${char}`);
}

// ─── maskLine ────────────────────────────────────────────────────────────────

describe('semantic-scan.maskLine', () => {
  it('masks a // line comment to the end of the line', () => {
    const r = maskLine('let x = 1 // fn fake()', initialMaskState);
    assert.equal(r.masked, 'let x = 1             ');
    assert.equal(r.state.kind, 'code');
  });

  it('masks string contents but keeps surrounding code, indices intact', () => {
    const line = 'let s = "record X" + tail';
    const r = maskLine(line, initialMaskState);
    assert.equal(r.masked.length, line.length);
    assert.ok(!r.masked.includes('record'));
    assert.ok(r.masked.includes('tail'));
    assert.equal(r.masked.indexOf('tail'), line.indexOf('tail'));
  });

  it('does not end a string at an escaped quote', () => {
    const r = maskLine('let s = "a\\" fn nope()"', initialMaskState);
    assert.ok(!r.masked.includes('fn'));
  });

  it('opens and closes a triple-quoted block across lines', () => {
    let state: MaskState = initialMaskState;
    state = maskLine('let doc = """', state).state;
    assert.equal(state.kind, 'triple');
    const mid = maskLine('fn fake() -> Void', state);
    assert.equal(mid.masked.trim(), '');
    assert.equal(mid.state.kind, 'triple');
    const close = maskLine('""" + rest', mid.state);
    assert.equal(close.state.kind, 'code');
    assert.ok(close.masked.includes('rest'));
  });

  it('tracks nested block comments across lines', () => {
    let state: MaskState = maskLine('/* outer /* inner', initialMaskState).state;
    assert.deepEqual(state, { kind: 'block', depth: 2 });
    state = maskLine('still hidden */', state).state;
    assert.deepEqual(state, { kind: 'block', depth: 1 });
    const done = maskLine('*/ fn shown()', state);
    assert.equal(done.state.kind, 'code');
    assert.ok(done.masked.includes('fn shown'));
  });

  it('masks char literals', () => {
    const r = maskLine("let c = 'x'", initialMaskState);
    assert.ok(!r.masked.includes('x'));
    assert.equal(r.state.kind, 'code');
  });
});

// ─── Declarations ────────────────────────────────────────────────────────────

describe('semantic-scan declarations', () => {
  it('tags a fn declaration name as function+declaration', () => {
    const tokens = scan('fn add(a: Int) -> Int {}');
    expectToken(tokens, 0, 3, 3, 'function', ['declaration']);
    // The two Int mentions are prelude types.
    expectToken(tokens, 0, 10, 3, 'type', ['defaultLibrary']);
    expectToken(tokens, 0, 18, 3, 'type', ['defaultLibrary']);
  });

  it('tags a public fn declaration (modifier prefix does not matter)', () => {
    const tokens = scan('public fn run() -> Void {}');
    expectToken(tokens, 0, 10, 3, 'function', ['declaration']);
  });

  it('tags record declarations as struct+declaration', () => {
    const tokens = scan('public record Point {\n  x: Int\n}');
    expectToken(tokens, 0, 14, 5, 'struct', ['declaration']);
    // Field names are not tagged; the field type is vocab-known.
    assert.equal(at(tokens, 1, 2), undefined);
    expectToken(tokens, 1, 5, 3, 'type', ['defaultLibrary']);
  });

  it('tags class declarations as class+declaration', () => {
    const tokens = scan('class Button {\n}');
    expectToken(tokens, 0, 6, 6, 'class', ['declaration']);
  });

  it('tags trait declarations as interface+declaration', () => {
    const tokens = scan('public trait Describable {\n  fn describe() -> String\n}');
    expectToken(tokens, 0, 13, 11, 'interface', ['declaration']);
    expectToken(tokens, 1, 5, 8, 'function', ['declaration']);
  });

  it('a generic trait name stops before the bracket', () => {
    const tokens = scan('public trait From[T] {\n}');
    expectToken(tokens, 0, 13, 4, 'interface', ['declaration']);
    // The type parameter is not tagged (unknown kind).
    assert.equal(at(tokens, 0, 18), undefined);
  });

  it('tags a single-line enum and its variants', () => {
    const tokens = scan('enum Color { Red, Green, Blue }');
    expectToken(tokens, 0, 5, 5, 'enum', ['declaration']);
    expectToken(tokens, 0, 13, 3, 'enumMember', ['declaration']);
    expectToken(tokens, 0, 18, 5, 'enumMember', ['declaration']);
    expectToken(tokens, 0, 25, 4, 'enumMember', ['declaration']);
  });

  it('tags multi-line enum variants but not payload fields/types as members', () => {
    const text = [
      'public enum OrderStatus {',
      '  Pending',
      '  Priced { total: Float }',
      '  Failed { reason: String }',
      '}',
      'fn after() -> Void {}',
    ].join('\n');
    const tokens = scan(text);
    expectToken(tokens, 0, 12, 11, 'enum', ['declaration']);
    expectToken(tokens, 1, 2, 7, 'enumMember', ['declaration']);
    expectToken(tokens, 2, 2, 6, 'enumMember', ['declaration']);
    expectToken(tokens, 3, 2, 6, 'enumMember', ['declaration']);
    // Payload internals: field untagged, type tagged as prelude type only.
    assert.equal(at(tokens, 2, 11), undefined);
    expectToken(tokens, 2, 18, 5, 'type', ['defaultLibrary']);
    // Body tracking ends at `}` — the following fn is a normal declaration.
    expectToken(tokens, 5, 3, 5, 'function', ['declaration']);
  });

  it('does not tag tuple-variant payload types as members', () => {
    const tokens = scan('enum MyOpt {\n  MySome(Int)\n  MyNone\n}');
    expectToken(tokens, 1, 2, 6, 'enumMember', ['declaration']);
    expectToken(tokens, 2, 2, 6, 'enumMember', ['declaration']);
    // Int inside the payload parens is a prelude type, not an enumMember.
    expectToken(tokens, 1, 9, 3, 'type', ['defaultLibrary']);
  });

  it('tags effect block declarations as type+declaration', () => {
    const tokens = scan('public effect Log {\n  fn log(message: String) -> Void\n}');
    expectToken(tokens, 0, 14, 3, 'type', ['declaration']);
    expectToken(tokens, 1, 5, 3, 'function', ['declaration']);
  });

  it('tags composite effect aliases and their in-file components', () => {
    const text = [
      'effect Database {',
      '  fn query(sql: String) -> Void',
      '}',
      'effect Logger {',
      '  fn log(message: String) -> Void',
      '}',
      'effect ServiceStack = Database + Logger',
    ].join('\n');
    const tokens = scan(text);
    expectToken(tokens, 6, 7, 12, 'type', ['declaration']);
    // Component references resolve to in-file effects.
    expectToken(tokens, 6, 22, 8, 'type', []);
    expectToken(tokens, 6, 33, 6, 'type', []);
  });
});

// ─── Suppression inside strings and comments ─────────────────────────────────

describe('semantic-scan suppression', () => {
  it('emits nothing for declarations inside a line comment', () => {
    assert.deepEqual(scan('// fn fake() and record Bogus and @nope'), []);
  });

  it('emits nothing for declarations inside a string literal', () => {
    assert.deepEqual(scan('let s = "record Hidden { }"'), []);
  });

  it('emits nothing inside a triple-quoted block, resumes after it', () => {
    const text = [
      'let doc = """',
      'fn fake() -> Void',
      '@nope',
      'record Bogus {',
      '"""',
      'fn real() -> Void {}',
    ].join('\n');
    const tokens = scan(text);
    assert.deepEqual(ofType(tokens, 'decorator'), []);
    assert.deepEqual(ofType(tokens, 'struct'), []);
    const fns = ofType(tokens, 'function');
    assert.equal(fns.length, 1);
    expectToken(tokens, 5, 3, 4, 'function', ['declaration']);
  });

  it('emits nothing inside a block comment, resumes after the closer', () => {
    const text = ['/*', 'fn hidden() {}', '*/ fn shown() {}'].join('\n');
    const tokens = scan(text);
    const fns = ofType(tokens, 'function');
    assert.equal(fns.length, 1);
    expectToken(tokens, 2, 6, 5, 'function', ['declaration']);
  });

  it('an escaped quote does not end the string early', () => {
    assert.deepEqual(scan('let s = "a\\" record Sneaky {"'), []);
  });

  it('an effect declared only inside a comment contributes no references', () => {
    const text = [
      '// effect Ghost {',
      '//   fn haunt() -> Void',
      '// }',
      'fn calls() -> Void with Ghost {',
      '  haunt("boo")',
      '}',
    ].join('\n');
    const tokens = scan(text);
    // No type token for Ghost, no function token for the haunt call.
    assert.deepEqual(ofType(tokens, 'type').filter((t) => t.tokenModifiers.length === 0), []);
    assert.equal(at(tokens, 4, 2), undefined);
  });

  it('prelude names inside strings are not tagged', () => {
    const tokens = scan('let s = "Int println(Ok)"');
    assert.deepEqual(tokens, []);
  });
});

// ─── Effects: names and operation call sites ─────────────────────────────────

describe('semantic-scan effects', () => {
  const text = [
    'effect Logger {',
    '  fn log(level: String, message: String) -> Void',
    '}',
    '',
    'fn run() -> Void with Logger {',
    '  log("info", "hi")',
    '  obj.log("not the effect op")',
    '  let f = log',
    '}',
    'handle Logger with console()',
  ].join('\n');

  it('tags the effect name reference in a with clause', () => {
    const tokens = scan(text);
    expectToken(tokens, 4, 22, 6, 'type', []);
  });

  it('tags the effect name reference in a module-level handle', () => {
    const tokens = scan(text);
    expectToken(tokens, 9, 7, 6, 'type', []);
  });

  it('tags a bare operation call site as function', () => {
    const tokens = scan(text);
    expectToken(tokens, 5, 2, 3, 'function', []);
  });

  it('does not tag a dot-qualified call with an op name', () => {
    const tokens = scan(text);
    assert.equal(at(tokens, 6, 6), undefined);
  });

  it('does not tag a bare op name that is not called', () => {
    const tokens = scan(text);
    assert.equal(at(tokens, 7, 10), undefined);
  });

  it('tags effect references inside a handling block header', () => {
    const tokens = scan(
      [
        'effect Clock {',
        '  fn now() -> Int',
        '}',
        'fn timed() -> Int {',
        '  handling (Clock with system_clock()) {',
        '    now()',
        '  }',
        '}',
      ].join('\n'),
    );
    expectToken(tokens, 4, 12, 5, 'type', []);
    expectToken(tokens, 5, 4, 3, 'function', []);
    // The handler constructor is not an effect op — untagged.
    assert.equal(at(tokens, 4, 23), undefined);
  });

  it('tags the effect name in an impl-for line', () => {
    const tokens = scan(
      [
        'effect Log {',
        '  fn log(message: String) -> Void',
        '}',
        'record ConsoleLog {',
        '}',
        'impl Log for ConsoleLog {',
        '  public fn log(message: String) -> Void {',
        '    println(message)',
        '  }',
        '}',
      ].join('\n'),
    );
    expectToken(tokens, 5, 5, 3, 'type', []);
    // The impl body fn is a declaration; println is a prelude function.
    expectToken(tokens, 6, 12, 3, 'function', ['declaration']);
    expectToken(tokens, 7, 4, 7, 'function', ['defaultLibrary']);
  });
});

// ─── Annotations ─────────────────────────────────────────────────────────────

describe('semantic-scan annotations', () => {
  it('tags a top-level annotation including the @', () => {
    const tokens = scan('@derive(Equatable)');
    expectToken(tokens, 0, 0, 7, 'decorator', []);
    expectToken(tokens, 0, 8, 9, 'interface', ['defaultLibrary']);
  });

  it('tags an indented annotation', () => {
    const tokens = scan('  @managed\n  fn helper() -> Void {}');
    expectToken(tokens, 0, 2, 8, 'decorator', []);
  });

  it('ignores annotation-like text after code on the same line', () => {
    assert.deepEqual(ofType(scan('let email = name @ host'), 'decorator'), []);
  });

  it('ignores @markers inside a @context triple-quoted body', () => {
    const text = [
      '@context("""',
      '  @intent: explain things',
      '""")',
      'record Customer {',
      '}',
    ].join('\n');
    const tokens = scan(text);
    const decorators = ofType(tokens, 'decorator');
    assert.equal(decorators.length, 1);
    expectToken(tokens, 0, 0, 8, 'decorator', []);
    expectToken(tokens, 3, 7, 8, 'struct', ['declaration']);
  });
});

// ─── module / use paths ──────────────────────────────────────────────────────

describe('semantic-scan module and use paths', () => {
  it('tags the module declaration path as namespace+declaration', () => {
    const tokens = scan('module core.effect');
    expectToken(tokens, 0, 7, 11, 'namespace', ['declaration']);
    // The path is claimed as a whole — no stray tokens on its segments.
    assert.equal(tokens.length, 1);
  });

  it('tags a use path and recognizes prelude names in the import group', () => {
    const tokens = scan('use core.compare.{ Comparable }');
    expectToken(tokens, 0, 4, 12, 'namespace', []);
    expectToken(tokens, 0, 19, 10, 'interface', ['defaultLibrary']);
  });

  it('tags a wildcard use path without the star', () => {
    const tokens = scan('use models.*');
    expectToken(tokens, 0, 4, 6, 'namespace', []);
    assert.equal(tokens.length, 1);
  });
});

// ─── Vocabulary recognition ──────────────────────────────────────────────────

describe('semantic-scan vocabulary recognition', () => {
  it('tags prelude and primitive types with defaultLibrary', () => {
    const tokens = scan('fn f(xs: List[Int]) -> Result[String, Never] {}');
    expectToken(tokens, 0, 9, 4, 'type', ['defaultLibrary']);
    expectToken(tokens, 0, 14, 3, 'type', ['defaultLibrary']);
    expectToken(tokens, 0, 23, 6, 'type', ['defaultLibrary']);
    expectToken(tokens, 0, 30, 6, 'type', ['defaultLibrary']);
    expectToken(tokens, 0, 38, 5, 'type', ['defaultLibrary']);
  });

  it('tags prelude traits with defaultLibrary', () => {
    const tokens = scan('impl Comparable for Money {\n}');
    expectToken(tokens, 0, 5, 10, 'interface', ['defaultLibrary']);
    // Money is unknown — untagged.
    assert.equal(at(tokens, 0, 20), undefined);
  });

  it('tags prelude constructors with defaultLibrary', () => {
    const tokens = scan('let r = if (ok) { Ok(1) } else { Err("no") }');
    expectToken(tokens, 0, 18, 2, 'enumMember', ['defaultLibrary']);
    expectToken(tokens, 0, 33, 3, 'enumMember', ['defaultLibrary']);
  });

  it('tags a prelude function only in call position', () => {
    const tokens = scan('println("hi")\nlet p = println\nx.println("no")');
    expectToken(tokens, 0, 0, 7, 'function', ['defaultLibrary']);
    assert.equal(at(tokens, 1, 8), undefined);
    assert.equal(at(tokens, 2, 2), undefined);
  });

  it('allows whitespace between a prelude function and its paren', () => {
    const tokens = scan('print ("spaced")');
    expectToken(tokens, 0, 0, 5, 'function', ['defaultLibrary']);
  });

  it('a declaration claims its name before vocab tagging (no double token)', () => {
    // `Ordering` is a prelude type, but here it is the user's own record.
    const tokens = scan('record Ordering {\n}');
    expectToken(tokens, 0, 7, 8, 'struct', ['declaration']);
    assert.equal(ofType(tokens, 'type').length, 0);
  });
});

// ─── Degraded vocab ──────────────────────────────────────────────────────────

describe('semantic-scan with an empty vocabulary', () => {
  it('still emits structural tokens and does not crash', () => {
    const text = [
      'module app',
      '@managed',
      'fn go(x: Int) -> Int {',
      '  println("hi")',
      '  Ok(x)',
      '}',
    ].join('\n');
    const tokens = scan(text, EMPTY);
    expectToken(tokens, 0, 7, 3, 'namespace', ['declaration']);
    expectToken(tokens, 1, 0, 8, 'decorator', []);
    expectToken(tokens, 2, 3, 2, 'function', ['declaration']);
    // No vocab — no defaultLibrary tokens at all.
    assert.deepEqual(
      tokens.filter((t) => t.tokenModifiers.includes('defaultLibrary')),
      [],
    );
  });

  it('semanticVocabInput degrades a structurally-incomplete vocab to empty lists', () => {
    const partial = { version: '0.0.0', language: {} } as unknown as Vocab;
    const input = semanticVocabInput(partial);
    assert.deepEqual(input, {
      primitiveTypes: [],
      preludeTypes: [],
      preludeFunctions: [],
      preludeTraits: [],
      preludeConstructors: [],
    });
    assert.deepEqual(scan('fn f() -> Int {}', input).length, 1);
  });

  it('semanticVocabInput projects a real-shaped vocab', () => {
    const vocab = {
      version: '0.1.0',
      language: {
        keywords: [],
        operators: [],
        annotations: [],
        strictness_levels: [],
        primitive_types: [{ name: 'Int' }],
        prelude_types: [{ name: 'List' }],
        prelude_functions: [{ name: 'println', kind: 'function', signature: '' }],
        prelude_traits: [{ name: 'Equatable', kind: 'trait', signature: '' }],
        prelude_constructors: [{ name: 'Some', kind: 'constructor', signature: '' }],
      },
      stdlib: { modules: [], builtin_methods: [], builtin_globals: [] },
      diagnostics: { codes: [] },
      tooling: { targets: [], ai_providers: [], commands: [] },
    } as unknown as Vocab;
    const input = semanticVocabInput(vocab);
    assert.deepEqual(input.primitiveTypes, ['Int']);
    assert.deepEqual(input.preludeFunctions, ['println']);
  });
});

// ─── Legend constants ────────────────────────────────────────────────────────

describe('semantic-scan legend constants', () => {
  // The standard legend from the VS Code semantic-highlighting docs. Emitting
  // only these guarantees built-in themes color the tokens with no
  // package.json contributes.
  const STANDARD_TYPES = new Set([
    'namespace', 'class', 'enum', 'interface', 'struct', 'typeParameter',
    'type', 'parameter', 'variable', 'property', 'enumMember', 'decorator',
    'event', 'function', 'method', 'macro', 'label', 'comment', 'string',
    'keyword', 'number', 'regexp', 'operator',
  ]);
  const STANDARD_MODIFIERS = new Set([
    'declaration', 'definition', 'readonly', 'static', 'deprecated',
    'abstract', 'async', 'modification', 'documentation', 'defaultLibrary',
  ]);

  it('uses only standard token types', () => {
    for (const t of SEMANTIC_TOKEN_TYPES) {
      assert.ok(STANDARD_TYPES.has(t), `${t} is not a standard token type`);
    }
  });

  it('uses only standard token modifiers', () => {
    for (const m of SEMANTIC_TOKEN_MODIFIERS) {
      assert.ok(STANDARD_MODIFIERS.has(m), `${m} is not a standard modifier`);
    }
  });

  it('every emitted token uses legend entries', () => {
    const tokens = scan(
      'module a.b\n@x\nfn f() -> Int { println("s") }\neffect E { fn op() -> Void }\nenum C { A }',
    );
    for (const t of tokens) {
      assert.ok((SEMANTIC_TOKEN_TYPES as readonly string[]).includes(t.tokenType));
      for (const m of t.tokenModifiers) {
        assert.ok((SEMANTIC_TOKEN_MODIFIERS as readonly string[]).includes(m));
      }
    }
  });
});

// ─── Output invariants ───────────────────────────────────────────────────────

describe('semantic-scan output invariants', () => {
  const busy = [
    'module app.core',
    'use core.effect.{ Log }',
    'effect Clock {',
    '  fn now() -> Int',
    '}',
    'fn stamp() -> Int with Clock {',
    '  let t = now()',
    '  println("t=${t}")',
    '  t',
    '}',
  ].join('\n');

  it('is sorted by line then start character', () => {
    const tokens = scan(busy);
    for (let i = 1; i < tokens.length; i++) {
      const a = tokens[i - 1];
      const b = tokens[i];
      assert.ok(
        a.line < b.line || (a.line === b.line && a.char < b.char),
        `tokens out of order at index ${i}`,
      );
    }
  });

  it('never emits overlapping tokens', () => {
    const tokens = scan(busy);
    for (let i = 1; i < tokens.length; i++) {
      const a = tokens[i - 1];
      const b = tokens[i];
      if (a.line === b.line) {
        assert.ok(a.char + a.length <= b.char, `overlap at index ${i}`);
      }
    }
  });

  it('emits nothing for an empty document', () => {
    assert.deepEqual(scan(''), []);
  });
});

// ─── Realistic snippet ───────────────────────────────────────────────────────

describe('semantic-scan realistic stdlib-like snippet', () => {
  const text = [
    'module chat', //                                                    0
    '', //                                                               1
    'use core.effect.{ Log }', //                                        2
    '', //                                                               3
    '/// The type of a chat message.', //                                4
    'public enum MessageType { Text, Image, System, Ack }', //           5
    '', //                                                               6
    'public record Message {', //                                        7
    '  id: Int', //                                                      8
    '  content: String', //                                              9
    '}', //                                                             10
    '', //                                                              11
    'public trait Renderer {', //                                       12
    '  fn render(msg: Message) -> String', //                           13
    '}', //                                                             14
    '', //                                                              15
    'effect Clock {', //                                                16
    '  fn now() -> Int', //                                             17
    '}', //                                                             18
    '', //                                                              19
    '@managed', //                                                      20
    'public fn stamp(msg: Message) -> Result[Message, String] with Clock {', // 21
    '  let t = now()', //                                               22
    '  guard (t > 0) else {', //                                        23
    '    return Err("bad clock at ${t}")', //                           24
    '  }', //                                                           25
    '  println("stamped ${msg.id}")', //                                26
    '  Ok(msg)', //                                                     27
    '}', //                                                             28
  ].join('\n');

  it('produces the expected token kinds across the file', () => {
    const tokens = scan(text);

    expectToken(tokens, 0, 7, 4, 'namespace', ['declaration']);
    expectToken(tokens, 2, 4, 11, 'namespace', []);

    // enum + its four variants
    expectToken(tokens, 5, 12, 11, 'enum', ['declaration']);
    expectToken(tokens, 5, 26, 4, 'enumMember', ['declaration']);
    expectToken(tokens, 5, 32, 5, 'enumMember', ['declaration']);
    expectToken(tokens, 5, 39, 6, 'enumMember', ['declaration']);
    expectToken(tokens, 5, 47, 3, 'enumMember', ['declaration']);

    expectToken(tokens, 7, 14, 7, 'struct', ['declaration']);
    expectToken(tokens, 12, 13, 8, 'interface', ['declaration']);
    expectToken(tokens, 13, 5, 6, 'function', ['declaration']);
    expectToken(tokens, 16, 7, 5, 'type', ['declaration']);
    expectToken(tokens, 17, 5, 3, 'function', ['declaration']);

    expectToken(tokens, 20, 0, 8, 'decorator', []);
    expectToken(tokens, 21, 10, 5, 'function', ['declaration']);
    // with Clock — in-file effect reference.
    expectToken(tokens, 21, 62, 5, 'type', []);
    // now() — in-file effect op call.
    expectToken(tokens, 22, 10, 3, 'function', []);

    // Prelude recognition.
    expectToken(tokens, 8, 6, 3, 'type', ['defaultLibrary']);
    expectToken(tokens, 21, 33, 6, 'type', ['defaultLibrary']);
    expectToken(tokens, 24, 11, 3, 'enumMember', ['defaultLibrary']);
    expectToken(tokens, 26, 2, 7, 'function', ['defaultLibrary']);
    expectToken(tokens, 27, 2, 2, 'enumMember', ['defaultLibrary']);
  });

  it('emits nothing inside doc comments or interpolated strings', () => {
    const tokens = scan(text);
    // Line 4 is a doc comment mentioning "type" and "message".
    assert.deepEqual(tokens.filter((t) => t.line === 4), []);
    // The `${msg.id}` interpolation on line 26 is masked — only println shows.
    assert.deepEqual(
      tokens.filter((t) => t.line === 26).map((t) => t.char),
      [2],
    );
  });

  it('does not tag Log (imported, not declared in-file) as an effect', () => {
    const tokens = scan(text);
    const logRef = tokens.find(
      (t) => t.line === 2 && t.char === 18 && t.tokenType === 'type',
    );
    assert.equal(logRef, undefined);
  });
});
