# Standard Library

> The standard library lives in `stdlib/`. The compiler-emitted
> vocabulary at `extensions/vscode/assets/vocab.json` is the machine-
> readable index of every public symbol.

## Top-Level Modules

- `std.collections` — `List`, `Map`, `Set`, iteration combinators.
- `std.string` — string operations matching the spec's `String`
  primitive.
- `std.math` — numeric helpers.
- `std.io` — file and process I/O (effectful).
- `std.time` — instants, durations, and clocks.

## Bridging into Targets

Each stdlib symbol is implemented in Bock and has a hand-written
mapping to each target's idiomatic equivalent (e.g. `std.string.upper`
emits `.toUpperCase()` in JS/TS, `.upper()` in Python). The mapping
table lives in `compiler/crates/bock-codegen/`.

> Exhaustive per-symbol documentation is generated from
> `bock-dump-vocab` and will appear here in a future release.
