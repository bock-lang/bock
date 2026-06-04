<!-- DO NOT EDIT — generated from tracking/ by tools/scripts/gen-tracking-views.sh -->

# Status

## Active work

Live summary derived from `tracking/queue.md` (items per section):

- Ready: 39
- v1-blocking: 2
- Blocked: 22
- Deferred: 1

## Build status (as of main e2117ee, 2026-06-03)

| What | State |
|------|-------|
| `cargo test --workspace` | passing (~2471 tests, 0 failed — per #152) |
| `cargo clippy --workspace --all-targets -D warnings` | clean |
| `cargo fmt --all -- --check` | clean |
| `cargo doc --workspace --no-deps -D warnings` | clean (now in the pre-PR gate + CI) |
| `mdbook build docs` | clean |
| CI on `main` | green; cache via Swatinem/rust-cache@v2.9.1 (#116, faster) |
| Conformance | parse/discover **+ execution** — compile+run+diff stdout per target (#114/#115); `run-conformance.sh`; **124 conformance fixtures, 0 failed under `BOCK_CONFORMANCE_REQUIRE=all`** (was 95 fixtures/430 pairs; +29 real-world-shaped fixtures across the MS-examples-hardening workstream #205–#220, all backends). **★ MS-projectmode COMPLETE (ItemB, S0–S8, #181–#194) — DV13 + DV18 CLOSED:** ALL 5 targets emit as **per-module native-import trees** (sole path; bundling retired) — py package imports; js/ts ES modules; rust `src/`-rooted cargo crate (`cargo run`); go flat `package main` (`go run .`). **Project mode is real:** the `Scaffolder` (project mode only; `--source-only` is bare) emits per-target manifests + formatter/opt-in-linter configs + README first-contact + **`@test` functions transpiled to each target's test framework** (Vitest|Jest / pytest|unittest / cargo test / go test), honoring `bock.project` `[targets.<T>]`/`[targets.<T>.scaffolding]` config (defaults per §20.6.2). The conformance harness builds in **project mode** and runs the scaffolded project per target. Transpiled tests are **RUN-verified on all 5** in CI (#196 — the ubuntu lane installs vitest/pytest/prettier/black/ruff and sets `BOCK_PROJECTMODE_REQUIRE=all`; js/ts/python tests pass as-emitted); the formatter-clean `--check` gate covers the emitted tree on rust/go (full, #198) + test files on js/ts/python. **⚠ COVERAGE CAVEAT (CONFIRMED by full audit 2026-06-03 13:44):** conformance (430/0) is real but the fixtures are NARROW. The **complete examples-exec audit (all 20 examples × 5, project mode, built out-of-tree)** gives the TRUE matrix: **js 10/20 compile·2/10 run · ts 2/20·2/2 · python 15/20·7/15 · rust 3/20·2/3 (in-repo 0/20 — workspace bug masks) · go 1/20·1/1** — hello-world is the ONLY example green on all 5 (js/py "compile" is syntax-only, so RUN is their real signal). rust/go fail on **real codegen**, not just the env bug (proven: fizzbuzz-rust passes out-of-tree, fails in-repo). **~9 evidence-confirmed root-cause clusters** (queue.md): Q-list-method-codegen (HIGH, receiver dup'd as 1st arg), Q-list-concat-codegen, Q-const-enum-naming, Q-match-exprpos (un-deferred; subsumes chat-protocol), Q-go-enum-return-boxing, Q-rust-move-codegen, Q-rust-string-num-methods, Q-js-effect-export, Q-py-circular-import, + Q-examples-codegen-misc; plus Q-rust-cargo-workspace (masking-only) + Q-examples-exec-coverage (the gate). So "project mode works on all 5" holds for the conformance fixtures but NOT for real-world programs yet. Operator decided (2026-06-03): **v1.0 holds all 5 at the 'examples green' bar**, fixed in leverage order; the examples-exec gate lands informational-first. → **MS-examples-hardening** is the v1.0 prerequisite workstream.
**UPDATE 23:05: 5-WAY PARALLEL FAN-OUT (#216 rust · #217 js · #218 py · #219 ts · #220 go — file-disjoint, generator.rs untouched in all). Combined conformance 0 failed across 124 fixtures (REQUIRE=all, verified on merged main). Examples LEAPT: runtime-working js 14·ts 7·py 12·rust 9·go 7 / 20 (go 1→7!). Per-backend clusters done (effect-export, circular-import, utf8-stdout, rust ownership, go Result-payload/Char/int-width/unused-var, per-backend match-exprpos). The fan-out CONVERGED on the remaining SHARED work: Q-exprpos-shared-desugar (the match-exprpos core), Q-propagate-operator-noop (`?` no-op js/ts/py), Q-list-range-pattern-shared, Q-guard-let-shared, Q-let-shadow-const — next focused (non-parallel) phase. 0 regressions.** The narrower conformance still covers the **entire v1 stdlib ×5** (iter/effect/option/result/string/test/collections + time) and the codegen substrate it exercises (generic containers over user Comparable types, sealed-trait bounds on primitives, generic free-fns over Optional/Result on Go). NOTE: the local stale-`bock` hazard is now handled in-harness — `run-conformance.sh` force-rebuilds `bock` (touch `bock-cli/build.rs` + `cargo build -p bock`) before tests (#175, Q-conformance-clean-rebuild DONE) |
| `bock check` on examples | 20/20 exit 0 |
| `bock build`/run examples ×5 (project mode) | **NOT clean but CLIMBING fast — MS-examples-hardening underway.** After the 5-backend fan-out (#216–#220): **runtime-working js 14·ts 7·py 12·RUST 9·go 7 / 20** (was js7/ts5/py9/rust8/go1; pre-workstream 2/2/7/2/1). 30→49 example-target passes; **go 1→7** (the all-5 bet). Exec-gated in CI **informationally** (#204; baseline ratcheted #221). **Operator: go holds the all-5 v1.0 bar.** REMAINING is now mostly SHARED-lowering: Q-exprpos-shared-desugar (the match-exprpos core, go-blocking), Q-propagate-operator-noop, Q-list-range-pattern-shared, Q-guard-let-shared, Q-let-shadow-const → MS-examples-hardening (v1.0 prerequisite). |

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
  constraints, effect inference. **impl/class method BODIES are now
  type-checked (#207)** — previously only top-level fns/consts were
  (caught a latent `core.error` field/method value-position bug).
- **Conformance** — fixtures across `effects/interp/parse/time/types`
  (+ effect-handler #74; stdlib/* + exec/* fixtures); the harness now
  **executes** `// EXPECT: output` fixtures — compiles to each target, runs the
  toolchain, diffs stdout (#114/#115); `tools/scripts/run-conformance.sh`.
- **VS Code extension** — builds to a working `.vsix`; vocab synced
  from the compiler; deps current (ESLint 10, etc., #80).
- **Docs** — mdBook with tooling reference in sync with the CLI (#90);
  the v1 **stdlib reference** (D4, #172) and a proper **Contributing** section
  (D5, #174 — overview/architecture/workflow/spec-changes) are live.
- **Website** — Astro static site; Cloudflare Workers deploy green
  (#85); deps current (#78).

## Standard library

The embedded source-compiled loading mechanism is **live** (#103): `core.*`
modules ship as Bock source bundled in the `bock` binary and resolve through
the module registry (hermetic; works from any cwd). **★ ALL 11 v1 modules landed — the v1 standard library is
COMPLETE, running on all 5 targets. ★** `core.error` (#103), `core.compare` (#104), `core.convert` (#110),
`core.iter` (#151/#152), `core.effect` (#157), `core.option` (#159/#162/#165), `core.result` (#161/#165),
`core.string` (#162/#163), `core.test` (#169), `core.collections` (#170) as Bock modules; **`core.time`** (#160 — its
§18.3.1 surface is a compiler builtin, pinned with a conformance floor). All EXECUTE cross-module ×5. The enabling
codegen across the batch: #162 (String-method dispatch + reserved-keyword escaping + Rust Optional-payload T:Clone +
deterministic bundling), #164 (dep_graph determinism), #165 (Go generic free-fns over Optional/Result), #167 (bock
test loads embedded core), #168 (Go generic record-over-List[T] + sealed-core-trait bounds firing the primitive
bridge ×5), #170 (collections Go/Rust codegen residue). **The codegen substrate is now exercised by the full
stdlib.** Q-stdlib (v1-blocking) is DONE → D4 (stdlib reference docs) is the next critical-path item.
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
fix + an interpreter topological-registration determinism fix). **R1+R2+R3 are ALL COMPLETE — the v1 stdlib is DONE
(11/11 ×5, main 53df918).** [R3 detail in audit.md 2026-06-01 17:36.] Next critical-path item is D4 (stdlib reference
docs), now unblocked. Design-gated (non-blocking, →
Design): DQ24 (core.iter surface —
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
