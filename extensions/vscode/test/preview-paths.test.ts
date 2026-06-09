// Unit tests for the pure target-preview path helpers in
// src/features/preview-paths.ts: project-root discovery, module-declaration
// parsing, and the per-target source → emitted-file mapping.
//
// The expected layouts are not guessed: they mirror
// compiler/crates/bock-codegen (generator::derive_output_path,
// module_tree_relpath, go::go_module_filename, rs.rs's `src/`-prefixed crate)
// and were verified by running `bock build -t <target> --source-only` for all
// five targets over a nested-module project. preview-paths.ts is vscode-free,
// so these run headlessly under Mocha + ts-node.

import * as assert from 'node:assert/strict';
import {
  TARGETS,
  TARGET_EXTENSIONS,
  buildDirFor,
  emittedCandidates,
  findProjectRoot,
  parseModuleDecl,
  resolveEmittedFile,
} from '../src/features/preview-paths';

/** An `exists` predicate over a fixed set of absolute paths. */
function fsOf(...paths: string[]): (p: string) => boolean {
  const set = new Set(paths);
  return (p) => set.has(p);
}

describe('preview-paths.findProjectRoot', () => {
  it('finds the marker in the starting directory itself', () => {
    const exists = fsOf('/work/app/bock.project');
    assert.equal(findProjectRoot('/work/app', exists), '/work/app');
  });

  it('walks up through nested source directories', () => {
    const exists = fsOf('/work/app/bock.project');
    assert.equal(findProjectRoot('/work/app/src/utils', exists), '/work/app');
  });

  it('picks the nearest marker when projects nest', () => {
    const exists = fsOf(
      '/work/bock.project',
      '/work/examples/demo/bock.project',
    );
    assert.equal(
      findProjectRoot('/work/examples/demo/src', exists),
      '/work/examples/demo',
    );
    assert.equal(findProjectRoot('/work/examples', exists), '/work');
  });

  it('returns undefined when no marker exists up to the root', () => {
    assert.equal(findProjectRoot('/work/app/src', fsOf()), undefined);
  });

  it('checks the filesystem root itself', () => {
    const exists = fsOf('/bock.project');
    assert.equal(findProjectRoot('/deep/down/here', exists), '/');
  });

  it('supports Windows-style separators via the sep parameter', () => {
    const exists = fsOf('C:\\work\\app\\bock.project');
    assert.equal(
      findProjectRoot('C:\\work\\app\\src', exists, '\\'),
      'C:\\work\\app',
    );
  });
});

describe('preview-paths.parseModuleDecl', () => {
  it('extracts a simple module path', () => {
    assert.equal(parseModuleDecl('module main\n\nfn main() {}\n'), 'main');
  });

  it('extracts a nested dotted module path', () => {
    assert.equal(
      parseModuleDecl('//! Doc header.\n\nmodule utils.parse\n'),
      'utils.parse',
    );
  });

  it('ignores indentation and CRLF line endings', () => {
    assert.equal(parseModuleDecl('  module a.b.c\r\nfn f() {}\r\n'), 'a.b.c');
  });

  it('returns undefined when no module is declared', () => {
    assert.equal(parseModuleDecl('fn main() {}\n'), undefined);
  });

  it('does not match module references inside other statements', () => {
    assert.equal(parseModuleDecl('use utils.parse.*\n'), undefined);
    // `module` needs a bare dotted identifier to the end of the line.
    assert.equal(parseModuleDecl('let module = 3\n'), undefined);
  });
});

describe('preview-paths.buildDirFor', () => {
  it('maps every target to build/<target>', () => {
    for (const t of TARGETS) {
      assert.equal(buildDirFor(t), `build/${t}`);
    }
  });
});

describe('preview-paths.emittedCandidates', () => {
  it('prefers the declared module path for js/ts/python', () => {
    for (const t of ['js', 'ts', 'python'] as const) {
      const ext = TARGET_EXTENSIONS[t];
      assert.deepEqual(
        emittedCandidates('src/utils/parse.bock', t, 'utils.parse'),
        [`utils/parse.${ext}`],
      );
    }
  });

  it('falls back to the source-mirrored path (src/ stripped) without a module decl', () => {
    assert.deepEqual(emittedCandidates('src/utils/parse.bock', 'js'), [
      'utils/parse.js',
    ]);
  });

  it('keeps the full path for sources outside src/', () => {
    assert.deepEqual(emittedCandidates('tools/gen.bock', 'ts'), [
      'tools/gen.ts',
    ]);
  });

  it('prefixes src/ for rust and includes the fixed entry name for main', () => {
    assert.deepEqual(emittedCandidates('src/main.bock', 'rust', 'main'), [
      'src/main.rs',
    ]);
    assert.deepEqual(
      emittedCandidates('src/utils/parse.bock', 'rust', 'utils.parse'),
      ['src/utils/parse.rs'],
    );
  });

  it('flattens to a dotted filename for go', () => {
    assert.deepEqual(
      emittedCandidates('src/utils/parse.bock', 'go', 'utils.parse'),
      ['utils.parse.go', 'parse.go'],
    );
    assert.deepEqual(emittedCandidates('src/main.bock', 'go', 'main'), [
      'main.go',
    ]);
  });

  it('offers the fixed entry name only for main-stemmed sources', () => {
    const rust = emittedCandidates('src/app.bock', 'rust', 'app');
    assert.ok(!rust.includes('src/main.rs'), `unexpected entry: ${rust}`);
    const go = emittedCandidates('src/app.bock', 'go');
    assert.ok(!go.includes('main.go'), `unexpected entry: ${go}`);
  });

  it('normalizes Windows separators and ./ prefixes in the source path', () => {
    assert.deepEqual(emittedCandidates('src\\utils\\parse.bock', 'js'), [
      'utils/parse.js',
    ]);
    assert.deepEqual(emittedCandidates('./src/main.bock', 'python'), [
      'main.py',
    ]);
  });
});

describe('preview-paths.resolveEmittedFile', () => {
  // The exact trees `bock build --source-only` produced for a project with
  // src/main.bock (`module main`) and src/utils/parse.bock
  // (`module utils.parse`), minus the .map files.
  const trees = {
    js: ['main.js', 'utils/parse.js'],
    ts: ['main.ts', 'utils/parse.ts'],
    python: ['_bock_runtime.py', 'main.py', 'utils/parse.py'],
    rust: ['src/main.rs', 'src/utils.rs', 'src/utils/parse.rs'],
    go: ['main.go', 'utils.parse.go'],
  } as const;

  it('maps the entry module on every target', () => {
    const expected = {
      js: 'main.js',
      ts: 'main.ts',
      python: 'main.py',
      rust: 'src/main.rs',
      go: 'main.go',
    } as const;
    for (const t of TARGETS) {
      assert.equal(
        resolveEmittedFile('src/main.bock', t, trees[t], 'main'),
        expected[t],
        t,
      );
    }
  });

  it('maps a nested module on every target', () => {
    const expected = {
      js: 'utils/parse.js',
      ts: 'utils/parse.ts',
      python: 'utils/parse.py',
      rust: 'src/utils/parse.rs',
      go: 'utils.parse.go',
    } as const;
    for (const t of TARGETS) {
      assert.equal(
        resolveEmittedFile(
          'src/utils/parse.bock',
          t,
          trees[t],
          'utils.parse',
        ),
        expected[t],
        t,
      );
    }
  });

  it('never maps a nested module onto a rust wiring file', () => {
    // `src/utils.rs` is mod-tree wiring, not the module body.
    assert.equal(
      resolveEmittedFile('src/utils/parse.bock', 'rust', trees.rust, 'utils.parse'),
      'src/utils/parse.rs',
    );
  });

  it('matches listings that use backslash separators', () => {
    assert.equal(
      resolveEmittedFile(
        'src/utils/parse.bock',
        'rust',
        ['src\\main.rs', 'src\\utils\\parse.rs'],
        'utils.parse',
      ),
      'src\\utils\\parse.rs',
    );
  });

  it('falls back to the source-mirrored path when the module declares no path', () => {
    assert.equal(
      resolveEmittedFile('src/models.bock', 'js', ['main.js', 'models.js']),
      'models.js',
    );
  });

  it('returns undefined when nothing matches', () => {
    assert.equal(
      resolveEmittedFile('src/orphan.bock', 'go', trees.go, 'orphan'),
      undefined,
    );
  });
});
