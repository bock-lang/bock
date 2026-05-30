// Mocha bootstrap: make `require('vscode')` resolve to the test stub.
//
// Source modules import the `vscode` module at the top level, which is only
// resolvable inside a running Extension Host. For headless unit tests we
// intercept that single specifier and hand back `./vscode-stub`, leaving
// every other module resolution untouched. This keeps the test runner in
// plain Node (no `@vscode/test-electron`, no Electron download), which is
// what makes it CI-friendly.

import Module from 'module';
import * as vscodeStub from './vscode-stub';

interface LoadableModule {
  _load(request: string, parent: unknown, isMain: boolean): unknown;
}

const loadable = Module as unknown as LoadableModule;
const originalLoad = loadable._load.bind(loadable);

loadable._load = function patchedLoad(
  request: string,
  parent: unknown,
  isMain: boolean,
): unknown {
  if (request === 'vscode') {
    return vscodeStub;
  }
  return originalLoad(request, parent, isMain);
};
