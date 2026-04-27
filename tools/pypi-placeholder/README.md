# bock-lang

**Namespace placeholder for the [Bock programming language](https://bocklang.org).**

This is a **placeholder package**. It reserves the `bock-lang`
name on PyPI for future Python tooling around Bock. It contains
no functional code.

## What is Bock?

Bock is a feature-declarative, target-agnostic programming language
that transpiles to JavaScript, TypeScript, Python, Rust, and Go.
The compiler is implemented in Rust and uses an AI-native code
generation pipeline with deterministic fallback.

Bock is **not** a Python library. The Python ecosystem is one of
its compilation targets — you write Bock, the compiler emits
Python — but Bock itself runs as a native Rust binary.

## How to actually use Bock

Install the Bock compiler:

```bash
cargo install bock
```

Or download a pre-built binary from
[bocklang.org/install](https://bocklang.org/install).

Documentation: [bocklang.org/docs](https://bocklang.org/docs)
Spec: [github.com/bock-lang/bock/blob/main/spec/bock-spec.md](https://github.com/bock-lang/bock/blob/main/spec/bock-spec.md)

## What this package will become

Future versions of `bock-lang` (after the language reaches v1.0)
may include:

- Python bindings for the Bock interpreter
- Test runners and CI integrations
- Build-system plugins for projects that mix Bock with Python
- Bock-to-Python migration tools

None of these exist yet. If you've installed `bock-lang` looking
for any of them, you're early. Follow
[github.com/bock-lang/bock](https://github.com/bock-lang/bock)
for updates.

## License

MIT. See [LICENSE](LICENSE).
