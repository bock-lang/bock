# @bock-lang/cli

**Namespace placeholder for the [Bock programming language](https://bocklang.org).**

This is a **placeholder package**. It reserves the `@bock-lang`
scope on npm for future Node.js tooling around Bock. It contains
no functional code.

## What is Bock?

Bock is a feature-declarative, target-agnostic programming language
that transpiles to JavaScript, TypeScript, Python, Rust, and Go.
The compiler is implemented in Rust and uses an AI-native code
generation pipeline with deterministic fallback.

JavaScript and TypeScript are among Bock's compilation targets —
you write Bock, the compiler emits JS or TS — but Bock itself runs
as a native Rust binary, not as a Node.js library.

## How to actually use Bock

Install the Bock compiler:

```bash
cargo install bock
```

Or download a pre-built binary from
[bocklang.org/install](https://bocklang.org/install).

Documentation: [bocklang.org/docs](https://bocklang.org/docs)

VS Code extension: [marketplace.visualstudio.com/items?itemName=bock-lang.bock-lang](https://marketplace.visualstudio.com/items?itemName=bock-lang.bock-lang)

## What this package will become

Future versions of `@bock-lang/cli` (after the language reaches
v1.0) may include:

- A Node-callable wrapper around the `bock` binary, for build-tool
  integration (webpack/vite/rollup plugins, etc.)
- A REPL bindings package for embedding Bock in Node.js scripts
- Test runner integrations for projects that mix Bock with JavaScript
- Tooling for migrating JavaScript projects to Bock

None of these exist yet. If you've installed `@bock-lang/cli`
expecting any of them, you're early. Follow
[github.com/bock-lang/bock](https://github.com/bock-lang/bock)
for updates.

## Related Packages (Future)

The `@bock-lang` scope will host additional packages over time:

- `@bock-lang/cli` — this package, will become the Node CLI wrapper
- `@bock-lang/repl` — embed-in-Node REPL bindings
- `@bock-lang/loader` — build-tool integration

These names are reserved by the existence of this scope; squatters
cannot register packages under `@bock-lang`.

## License

MIT. See [LICENSE](LICENSE).
