<!-- DO NOT EDIT — generated from tracking/ by tools/scripts/gen-tracking-views.sh -->

# Status

## Active work

Live summary derived from `tracking/queue.md` (items per section):

- Ready: 14
- v1-blocking: 2
- Blocked: 5
- Deferred: 1

## Build status (as of main e9204ab, 2026-06-01)

| What | State |
|------|-------|
| `cargo test --workspace` | passing (~2471 tests, 0 failed — per #152) |
| `cargo clippy --workspace --all-targets -D warnings` | clean |
| `cargo fmt --all -- --check` | clean |
| `cargo doc --workspace --no-deps -D warnings` | clean (now in the pre-PR gate + CI) |
| `mdbook build docs` | clean |
| CI on `main` | green; cache via Swatinem/rust-cache@v2.9.1 (#116, faster) |
| Conformance | parse/discover **+ execution** — compile+run+diff stdout per target (#114/#115); `run-conformance.sh`; **340 exec pairs (68 fixtures × 5 targets), 0 failed under `REQUIRE=all`** (incl. the full `core.iter` generic-combinator surface [#151/#152], the `core` **effect** invocation forms [#155 — §10.2/§10.4 bare-op, Layer-1/2 handlers, cross-module], and the **`core.effect`** module [#157]) |
| `bock check` on examples | 20/20 exit 0 |

## What works today

- **Compiler pipeline end-to-end** — 17 `bock-*` crates; CLI `bock`
  exposes `new`, `build`, `run`, `check` (incl. `--only`/`--brief`/
  `--strict`), `test`, `fmt`, `repl`, `inspect`, `pin`, `unpin`,
  `override`, `cache`, `promote`, `pkg`, `doc`, `model`, `lsp`.
- **Targets** — JS, TS, Python, Rust, Go codegen, now **execution-tested** for
  cross-target parity. DV9 (the parity gap the `core.iter` spike exposed) is
  CLOSED: Q-fconf execution conformance (#114/#115 — compile + run + diff stdout
  per target) + Q-codegen-fixes (#121 — statement-bodied match arms, self-methods
  on Rust/Go/Python, Go `Optional` runtime, interp method-env all fixed); 32/32
  exec fixture×target pairs green under `REQUIRE=all`. The Optional-payload residue has since
  been CLOSED across all 5 (#124 TS self/Optional · #126 Python runtime + Go typed-payload ·
  #127 Go match-in-loop; 55+ exec pairs). Remaining: Q-match-exprpos (expr-position), and the
  newly-found **List built-in method codegen gap (DV10 / Q-list-codegen)** — no backend lowers
  `List.len()`/`.get()`/`.push()` — which blocks core.iter (+ Q-go-list-literal, Q-ts-generic-impl).
- **Type system** — bidirectional inference, generics, trait-style
  constraints, effect inference.
- **Conformance** — fixtures across `effects/interp/parse/time/types`
  (+ effect-handler #74; stdlib/* + exec/* fixtures); the harness now
  **executes** `// EXPECT: output` fixtures — compiles to each target, runs the
  toolchain, diffs stdout (#114/#115); `tools/scripts/run-conformance.sh`.
- **VS Code extension** — builds to a working `.vsix`; vocab synced
  from the compiler; deps current (ESLint 10, etc., #80).
- **Docs** — mdBook with tooling reference in sync with the CLI (#90).
- **Website** — Astro static site; Cloudflare Workers deploy green
  (#85); deps current (#78).

## Standard library

The embedded source-compiled loading mechanism is **live** (#103): `core.*`
modules ship as Bock source bundled in the `bock` binary and resolve through
the module registry (hermetic; works from any cwd). **5 of 11 v1 modules
landed** — `core.error` (#103), `core.compare` (#104), `core.convert` (#110), **`core.iter` (#151/#152)**,
**`core.effect` (#157)** — and all five **EXECUTE cross-module on all 5 targets** (the codegen-completeness milestone
#131-#152 closed the DV13 cross-module-`use` gap + the generic-combinator codegen; #155 made the §10 effect system
execute ×5; #157 shipped `core.effect` + the `effect`-keyword module-path parser fix + an interpreter
module-registration determinism fix). **R1 is COMPLETE.**
The 2026-05-30 Design stdlib batch (DQ6–DQ9) is reconciled into the spec (#106);
**Q-bridge (#108)** wired the trait-impl table + canonical primitive conformances
(primitives satisfy bounds; `where`-bounds enforced; DV6 fixed); **#110** added
parameterized-trait resolution (From/Into/TryFrom + blanket + primitive
conversions). #129 landed read-only List method codegen (all 5). But `core.iter`'s pursuit (4
attempts, each stopping at a deeper codegen layer) prompted a **3-agent codegen audit** that
established the v1 codegen substrate is **materially incomplete** for the stdlib: **cross-module
`use` and user-defined enums are broken on ALL 5** (DV13/DV14 — so the 3 "landed" modules are
check-only, never executed cross-module), and Result/generics/closures/Optional-methods are broken
on 3-4/5. Operator decided (2026-05-30) a **codegen-completeness MILESTONE** (`Q-codegen-completeness`,
v1-blocking, phased P0-P4, ~10-15 PRs): fix comprehensively, THEN resume the stdlib. **Q-stdlib R1 is
PAUSED** behind it. The for→Iterable desugar is proven (T1 ×5) and resumes after the milestone's P0/P1.
**Phase 0 + Phase 1 DONE** (#131-#138): cross-module single-file bundling, user-enum registry, generics on all 5
(DV12), the `recv_kind` receiver-type annotation (#137), primitive-bridge dispatch, Result runtime +
Optional/Result methods. **Phase 2 DONE** (#140 trait self/defaults/bounded-dispatch · #141 Self-subst · #142
match guards/or/nested/tuple) — **the stdlib's trait-using modules now EXECUTE cross-module on all 5** (proven by
`use_core_compare.bock`). Generics/Result/Optional/traits/match/primitive-bridge all work ×5 (~195 exec pairs).
**Phase 3 DONE** (#144 Go collection element typing + record-spread + Self-in-plain-impl · #145 Map/Set method
dispatch + literals + range()). **Collections work ×5** — the codegen substrate is essentially built (cross-module,
enums, generics, Optional/Result, primitive-bridge, traits, match, collections). **P4-codegen DONE** (#147 tuple-`.N`
diagnostic · #148 TS Self-in-plain-impl + expr-position match · #149 generic-container/trait residue — the 4 gaps
core.iter's v5 STOP exposed; the audit had under-covered them). The codegen substrate is COMPLETE. **`core.iter`
R1 is now DONE on all 5 (#151 module + for→Iterable checker desugar; #152 Rust/Go generic-combinator codegen)** —
the 6th and final core.iter probe (the real combinator surface) exposed Rust/Go residue (transitive `T: Clone`, Go
generic-record-construct / typed concat-arg / generic-trait interface / lambda specialization), now fixed; ~300 exec
pairs ×5. **4/11 stdlib modules landed; main 9f1a2bd (2026-05-31).** Remaining R1: (1) **P4-hygiene**
(mutating-collection + `m.contains` guarding diagnostics, bock-types/checker.rs — both design-gated DQ18/DQ22); (2)
**core.effect** is DONE (#157) — DQ25 decided by the owner (primitives + a `Log` effect). The effect FOUNDATION
was hardened first (#155): the language effect system (§10) now EXECUTES on all 5 (the conformance/effects suite
was previously INERT — never checked/run; #155 fixed the §10.2/§10.4 bare-op resolution + the Rust op-in-interpolation
codegen + wired the suite into the exec lane). #157 then shipped the module (+ the `effect`-keyword module-path parser
fix + an interpreter topological-registration determinism fix). **R1 is COMPLETE; next is R2** (option/result/string/
time), then R3 (collections/test). Design-gated (non-blocking, → Design): DQ24 (core.iter surface —
combinator set / dropped Iterator-impl / omitted enumerate, NEW), DQ23 (Int/Int §3.6), DQ18/20/21/22,
DQ10-DQ15/DQ19, Bool-interp spelling; + Go nested-runtime-payload arith (#142) & Rust by-value-reuse (#149)
codegen follow-ups. Known interpreter gap: `mut self` iterator drive hangs under `bock run` (Q-iter-interp-mutself;
compiled targets fine).
**§18.2 prelude auto-import is live** (#120): the core-defined prelude symbols
(`Ordering`/`Less`/`Equal`/`Greater`, `Comparable`/`Equatable`, `Into`/`From`/
`TryFrom`/`Displayable`, `Error`) resolve without an explicit `use` (the membership
of `TryFrom`/`Error` vs §18.2's literal list → DQ13). See DV1, MS-stdlib.

## Phase history

A (Foundation Lock) · B (Module System) · C (Effect Codegen) ·
D (Generics) · E (Stdlib *Bridging* — the checker↔`bock-core` method
registry, **not** the stdlib modules) · F (AI Pipeline). All complete.

## Migration notes

Migrated from the internal `aura-dev` tree (commit `38ef9fe`). The
Aura→Bock rename is recorded in the spec changelogs; historical
changelog content preserves the Aura name verbatim. Active spec,
source, examples, extension, and docs are all under the Bock identity.
