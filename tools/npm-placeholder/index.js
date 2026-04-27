'use strict';

/**
 * @bock-lang/cli — placeholder for Bock language Node.js tooling.
 *
 * This package reserves the `@bock-lang` scope on npm for future
 * Node.js tooling around the Bock language. Bock itself is
 * implemented in Rust and transpiles to multiple targets;
 * Node.js tooling (REPL bindings, test runners, build plugins)
 * is post-v1.0 work.
 *
 * For the actual language and compiler, visit:
 *   https://bocklang.org
 *
 * To install the Bock compiler:
 *   cargo install bock
 *
 * This package contains no functional code. Requiring or executing
 * it prints a notice and exits.
 */

const NOTICE = `
The '@bock-lang/cli' npm package is currently a namespace placeholder.

Bock is a feature-declarative, target-agnostic programming language
implemented in Rust. The compiler is available via:

    cargo install bock

Or download a pre-built binary from:

    https://bocklang.org/install

Node.js tooling for Bock is planned for post-v1.0 releases. Follow
progress at https://bocklang.org and https://github.com/bock-lang/bock.
`;

function showNotice() {
  process.stderr.write(NOTICE + '\n');
}

showNotice();

if (require.main === module) {
  process.exit(0);
}

module.exports = {
  version: '0.0.1',
  notice: NOTICE.trim(),
};
