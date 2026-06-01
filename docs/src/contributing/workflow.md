# Development workflow

This page covers building and testing the compiler, the verification
gate every pull request must clear, and the conformance suite. You need
a stable Rust toolchain; the optional cross-target execution tests also
want the Node, Python, Rust, and Go toolchains installed.

## Build and test

All commands are workspace-aware from the repository root:

```bash
cargo build                 # build every compiler crate
cargo test --workspace      # run all unit + integration tests
```

Work on a single crate while iterating:

```bash
cargo test -p bock-types    # one crate's tests
cargo doc -p bock-air --open
```

The editor extension lives outside the Cargo workspace:

```bash
cd extensions/vscode && npm install && npm run compile
```

## The pre-PR verification gate

`main` advances only by pull request, and the branch protection enforces
**no required status checks** — which means the verification gate *is*
the guard. Run exactly what CI runs, locally, before you push. All four
commands must exit zero:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
```

Why each one, and the traps:

- **`cargo fmt --all -- --check`** fails on any formatting drift. If it
  does, run `cargo fmt --all` (without `--check`) and amend the commit.
- **`cargo clippy --workspace --all-targets -- -D warnings`** — the
  `--all-targets` flag is load-bearing. Plain `cargo clippy` lints only
  library and binary code; it skips tests, examples, and benches. A
  clippy warning in test code is the single most common
  "passes locally, fails in CI" surprise. Always pass `--all-targets`.
- **`cargo test --workspace`** runs every crate's tests, not just the
  one in your current directory.
- **`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
  --all-features`** builds the rustdoc API docs and fails on any rustdoc
  warning — a broken or private intra-doc link, for instance. This is
  *distinct* from the mdBook build below: it checks the API docs
  generated from doc comments, not this prose site.

If your change touches the docs site, also build it (see [Documentation
sync](#documentation-sync)).

## Conformance

Language behavior is pinned by **conformance fixtures** under
`compiler/tests/conformance/<category>/`, where the categories are
`parse`, `types`, `effects`, `context`, `interp`, `stdlib`, `time`, and
`exec`. Each fixture is a `.bock` file carrying directive comments that
declare its expected outcome:

```bock
// TEST: exec_arithmetic_sum
// EXPECT: output "sum=5"
module main

fn main() -> Void {
  let x: Int = 2
  let y: Int = 3
  println("sum=${x + y}")
}
```

`// EXPECT:` understands `no errors`, `error E<code> at <line>:<col>`,
and `output "..."`. The harness has two halves, run together by the
wrapper script:

```bash
./tools/scripts/run-conformance.sh
```

1. **Directive tests** parse the `// TEST:` / `// EXPECT:` directives on
   every fixture and assert it loads and checks as declared.
2. **Execution tests** take every fixture with `// EXPECT: output "..."`,
   compile it with `bock build -t <target> --source-only`, run the
   emitted program on each installed target toolchain, and diff trimmed
   stdout against the expectation.

A target whose toolchain isn't installed is **skipped and reported**,
not failed. To require specific targets to be present (CI lanes do
this), set `BOCK_CONFORMANCE_REQUIRE`:

```bash
BOCK_CONFORMANCE_REQUIRE=all ./tools/scripts/run-conformance.sh
BOCK_CONFORMANCE_REQUIRE=js,python,rust ./tools/scripts/run-conformance.sh
```

When you change codegen or the standard library, add or update a
fixture that exercises the change and run the suite with the relevant
targets required, so the cross-target behavior is pinned.

## Documentation sync

Any change to user-facing behavior — CLI surface, language syntax or
semantics, stdlib signatures, project conventions — updates the
corresponding docs in the *same* pull request. Build the docs site
before pushing; the build is part of CI:

```bash
mdbook build docs           # produces docs/book/; fails on broken links
mdbook serve docs           # local preview at localhost:3000
```

`book.toml` sets `create-missing = false`, so every page referenced in
`SUMMARY.md` and every internal link must resolve, or the build fails.
When you add a page, write it under the right directory and add it to
`docs/src/SUMMARY.md` — mdBook ignores files not listed there.
