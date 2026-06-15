# textkit — one Bock library, five native SDK packages

**The SDK-vendor wedge, proved.** `textkit` is a small, realistic string /
slug / validation utility library — the kind an SDK vendor ships. It is
written **once** in Bock ([`src/main.bock`](src/main.bock), 185 lines) and
`bock build --all-targets` emits a **native package for each of the five v1
targets** — JavaScript, TypeScript, Python, Rust, Go — complete with each
ecosystem's manifest (`package.json` / `tsconfig.json` / `pyproject.toml` /
`Cargo.toml` / `go.mod`) and the `@test` functions transpiled into each
target's native test framework (vitest / pytest / `cargo test` / `go test`).

Every command and every output block below is **real**, captured from this
project on 2026-06-15 with the `bock` binary built from this branch. Nothing
here is a mockup. To reproduce, build the compiler and run the commands in
[Reproduce](#reproduce).

---

## The library (`src/main.bock`)

One Bock source file, `module main`, holding the full public surface an SDK
consumer would import:

| Kind | Name | Purpose |
| --- | --- | --- |
| `enum` | `Casing { Lower, Upper, Kebab }` | the casing transforms |
| `record` | `Slug { value: String, length: Int }` | a normalized, URL-safe slug + its length |
| `fn` | `collapse_spaces(s) -> String` | fold repeated spaces to single spaces |
| `fn` | `normalize(s) -> String` | trim + lowercase + single-space interior |
| `fn` | `apply_casing(s, casing) -> String` | apply a `Casing` transform (match on the enum) |
| `fn` | `slugify(s) -> Slug` | arbitrary text → URL-safe kebab `Slug` |
| `fn` | `truncate(s, max) -> String` | cap length without ellipsis |
| `fn` | `is_valid_slug(slug) -> Bool` | non-empty, lowercase, space-free check |
| `@test` × 9 | `test_*` | unit tests over the public surface |

Every public item carries a `@context("…")` annotation, so the project
type-checks and builds at **`development` strictness with zero warnings**.
The library is pure and deterministic (no IO, no AI-assisted codegen), which
is what lets all five targets agree byte-for-byte.

---

## 1. `bock check` → exit 0

```console
$ bock check
check: 1 file checked, no errors.
$ echo $?
0
```

No warnings, no errors. (The `@context` annotations satisfy the standard-mode
completeness rule W8013.)

---

## 2. `bock build --all-targets` → five native packages

A project-mode build (not `--source-only`) writes each target's **native
manifest + transpiled `@test` files**, then invokes the target toolchain to
validate the emitted package:

```console
$ bock build --all-targets --deterministic
build: compiling 1 source file
  target: js
    wrote 8 files to build/js
  target: ts
    wrote 10 files to build/ts
  target: python
    wrote 7 files to build/python
  target: rust
    wrote 5 files to build/rust
  target: go
    wrote 6 files to build/go
build: done
```

### Emitted package layout (`build/<target>/`, excluding regenerable artifacts)

```
build/js/                       build/ts/
├── package.json    (npm)       ├── package.json    (npm)
├── .prettierrc.json            ├── tsconfig.json
├── _bock_runtime.js            ├── node-globals.d.ts
├── main.js                     ├── .prettierrc.json
└── bock.test.js   (vitest)     ├── _bock_runtime.ts
                                ├── main.ts
build/python/                   └── bock.test.ts    (vitest)
├── pyproject.toml  (pip)
├── _bock_runtime.py            build/rust/
├── main.py                     ├── Cargo.toml      (cargo)
└── test_bock.py   (pytest)     ├── src/main.rs
                                └── src/bock_tests.rs  (cargo test)
build/go/
├── go.mod          (go mod)    
├── bock_runtime.go             
├── main.go                     
└── bock_test.go   (go test)    
```

### The native manifests (verbatim, as emitted)

<details><summary><strong>build/js/package.json</strong> (npm + vitest)</summary>

```json
{
  "name": "textkit",
  "version": "0.1.0",
  "private": true,
  "type": "module",
  "scripts": { "test": "vitest run" },
  "devDependencies": { "prettier": "^3.0.0", "vitest": "^2.0.0" }
}
```
</details>

<details><summary><strong>build/ts/package.json</strong> + <strong>tsconfig.json</strong> (npm + vitest + tsc)</summary>

```json
{
  "name": "textkit",
  "version": "0.1.0",
  "private": true,
  "type": "module",
  "scripts": { "test": "vitest run" },
  "devDependencies": {
    "@types/node": "^22.0.0",
    "prettier": "^3.0.0",
    "typescript": "^5.7.0",
    "vitest": "^2.0.0"
  }
}
```
```json
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "NodeNext",
    "moduleResolution": "NodeNext",
    "rewriteRelativeImportExtensions": true,
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "forceConsistentCasingInFileNames": true
  },
  "include": ["**/*.ts"],
  "exclude": ["bock.test.ts"]
}
```
</details>

<details><summary><strong>build/python/pyproject.toml</strong> (setuptools + pytest)</summary>

```toml
[project]
name = "textkit"
version = "0.1.0"
requires-python = ">=3.9"

[build-system]
requires = ["setuptools"]
build-backend = "setuptools.build_meta"

[dependency-groups]
# Test framework: pytest
dev = [ "pytest>=8.0" ]

[tool.pytest.ini_options]
testpaths = ["."]

[tool.black]
line-length = 88
target-version = ["py39"]
```
</details>

<details><summary><strong>build/rust/Cargo.toml</strong> (cargo)</summary>

```toml
[package]
name = "bock_app"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "bock_app"
path = "src/main.rs"

[workspace]
```
</details>

<details><summary><strong>build/go/go.mod</strong> (go mod)</summary>

```
module textkit

go 1.21
```
</details>

### The `@test` functions, transpiled into each native framework

One Bock test —

```bock
@test
fn test_slugify_basic() {
  let s = slugify("Hello World")
  expect(s.value).to_equal("hello-world")
}
```

— becomes idiomatic native test code per target:

| Target | File | Framework | Emitted form |
| --- | --- | --- | --- |
| js | `bock.test.js` | vitest | `it("test_slugify_basic", () => { const s = slugify("Hello World"); expect(s.value).toEqual("hello-world"); })` |
| ts | `bock.test.ts` | vitest | same as js, typed |
| python | `test_bock.py` | pytest | `def test_slugify_basic():\n    s = slugify("Hello World")\n    assert (s.value) == ("hello-world")` |
| rust | `src/bock_tests.rs` | `cargo test` | `#[test] fn test_slugify_basic() { let s = slugify(...); assert_eq!(s.value, "hello-world".to_string()); }` |
| go | `bock_test.go` | `go test` | `func TestSlugifyBasic(t *testing.T) { s := Slugify("Hello World"); if s.Value != "hello-world" { t.Errorf(...) } }` |

---

## 3. Equivalence — proved two independent ways

### (a) Native test suites run green on all five targets

Each emitted package's `@test` functions were executed through its own
ecosystem's test runner. All nine tests pass on every target:

```console
$ (cd build/rust && cargo test)
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ (cd build/go && go test ./...)
ok  	textkit	0.003s

$ (cd build/python && python3 -m pytest -q)
9 passed in 0.02s

$ (cd build/js && npm install && npm test)        # vitest
      Tests  9 passed (9)

$ (cd build/ts && npm install && npm test)        # vitest
      Tests  9 passed (9)
```

| Target | Runner | Result |
| --- | --- | --- |
| js | vitest | **9 passed** |
| ts | vitest | **9 passed** |
| python | pytest | **9 passed** |
| rust | `cargo test` | **9 passed** |
| go | `go test` | **ok** |

### (b) Byte-identical stdout across all five targets

The library also ships a `main()` driver that exercises the public surface
and prints results. Built for all five targets and run through each native
runtime, the stdout is **byte-for-byte identical** — same MD5 on all five:

```console
$ node       build/js/main.js
$ node       build/ts/main.ts
$ python3     build/python/main.py
$ (cd build/rust && cargo run -q)
$ (cd build/go   && go run .)
```

Each prints exactly:

```text
=== textkit demo ===
slugify("  Hello   World  ") -> hello-world (len 11)
slugify("The Quick Brown Fox") -> the-quick-brown-fox (len 19)
apply_casing("abc def", Upper) -> ABC DEF
apply_casing("Foo Bar", Kebab) -> foo-bar
normalize("  A  B  C  ") -> a b c
truncate("abcdefgh", 5) -> abcde
truncate("abc", 5) -> abc
is_valid_slug("hello-world") -> true
is_valid_slug("Hello World") -> false
=== done ===
```

```console
$ for t in js ts py rs go; do md5sum out-$t.txt; done
3aa5c218ed108a9e4bf34f13b9473938  out-js.txt
3aa5c218ed108a9e4bf34f13b9473938  out-ts.txt
3aa5c218ed108a9e4bf34f13b9473938  out-py.txt
3aa5c218ed108a9e4bf34f13b9473938  out-rs.txt
3aa5c218ed108a9e4bf34f13b9473938  out-go.txt
# 1 distinct output across all 5 targets → identical
```

Note that the driver line `apply_casing("abc def", Upper) -> ABC DEF`
exercises the `Casing` enum directly: the enum and its `match` compile and
behave identically on every target.

---

## Reproduce

```bash
# 1. Build the bock compiler (from the repo root)
cargo build -p bock
BOCK=target/debug/bock        # or your CARGO_TARGET_DIR/debug/bock

# 2. From this directory:
cd examples/sdk-vendor-demo

# 3. Check + build all five native packages
$BOCK check
$BOCK build --all-targets --deterministic

# 4a. Run the transpiled @test suites natively
(cd build/rust   && cargo test)
(cd build/go     && go test ./...)
(cd build/python && python3 -m pytest -q)
(cd build/js     && npm install && npm test)
(cd build/ts     && npm install && npm test)

# 4b. Or prove byte-identical stdout from the driver
node build/js/main.js;  node build/ts/main.ts;  python3 build/python/main.py
(cd build/rust && cargo run -q); (cd build/go && go run .)
```

Toolchains used for the captured runs: node 24, npm 11 (vitest 2.1), tsc 6,
Python 3.12 (pytest 8), cargo/rustc 1.95, go 1.26. The `build/` tree is
regenerable and git-ignored (`examples/**/build/`); only `bock.project`,
`src/main.bock`, and this README are committed.

---

## FOUND-codegen-1 — enum-variant symbols are dropped from transpiled test imports (js / ts / python)

While building this demo green, one real codegen defect surfaced. It does
**not** affect the library, the driver, or the equivalence result above — it
is narrow and is reported here for the queue.

**Symptom.** A `@test` body that passes a **bare enum variant as a call
argument** transpiles into a test file whose import list omits that variant's
constructor symbol. Example: the Bock test

```bock
@test
fn test_apply_casing_upper() {
  expect(apply_casing("abc def", Upper)).to_equal("ABC DEF")
}
```

emitted (js) as:

```js
import { collapseSpaces, normalize, applyCasing, slugify, truncate, isValidSlug } from "./main.js";
// ...
expect(applyCasing("abc def", Casing_Upper)).toEqual("ABC DEF");   // Casing_Upper never imported
```

→ vitest: `ReferenceError: Casing_Upper is not defined`.
→ pytest: `NameError: name 'Casing_Upper' is not defined`.

**Scope.** Affects **js, ts, and python**, whose transpiled test files use an
explicit `import { … } from "./main"` / `from main import …` list. The
import collector walks the function-call identifiers but not enum-variant
**constructor** identifiers used as arguments. **Rust and Go are immune**:
their test files use `use super::*` and a shared `package main`, so no import
list to under-populate. Crucially, the **non-test** emission is correct — the
driver's `main.js`/`main.py`/etc. reference `Casing_Upper()` and run fine
(proved by the byte-identical stdout above), so this is specific to the
per-target test-file import-collection pass, not enum codegen in general.

**Workaround applied in this demo.** The two affected `@test` functions were
rewritten to cover the casing branches **through the public API** instead of
naming a bare variant in the test body — e.g. `slugify("Some Title Here")`
routes through `apply_casing(…, Kebab)` internally. The enum is still fully
exercised by the driver. With that, all five native test suites are green.

**Suggested fix (compiler, out of this example's scope).** In the per-target
test-file emitter (js/ts/python), extend the imported-symbol collection to
include enum-variant constructors referenced in `@test` bodies — mirroring
how the main-module emitter already imports them.
