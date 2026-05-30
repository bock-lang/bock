# Phase 0 — Codegen-Completeness Milestone (cross-module · user-enums · tail-`if`)

Plan agent (read-only) against main @ c9a241e, post the 3-agent codegen audit (audit.md 2026-05-30 18:00).
Phase 0 = the three foundational fixes (DV13/DV14/DV15) that gate the rest of the milestone.

## Decisive architectural finding (drives Item A)
The conformance harness + toolchain run plans compile+run a **single `main.<ext>` file**, never the emitted
tree (`compiler/tests/execution.rs:128` build_fixture; `bock-build/src/toolchain.rs:417-519` RunPlans —
`node main.js` / `tsc main.ts && node main.js` / `python3 main.py` / `rustc main.rs` / `go run main.go`).
`bock build` emits one file per module (`generator.rs:337-374` generate_project default; derive_output_path
generator.rs:47). So a cross-module program's extra files are ignored at run time. **Cross-module cannot run
under the harness regardless of import emission — the run model is single-file.** (NB `bock run` is
interpreter-based and already does cross-module via ModuleRegistry; this gap is codegen-only.)

## Sequencing: C → A → B (all touch bock-codegen + generator.rs → SEQUENTIAL, do not combine)
1. **C first** — smallest, lowest-risk, isolated; quick win + de-risks the worktree/build flow.
2. **A second** — the stdlib foundation; highest architectural uncertainty; own focused session.
3. **B third** — largest backend surface; the variant registry must be built across the A-bundle, so A first.

Per-session cross-cutting: build fresh in-worktree (`cargo build -p bock`); NEVER `cd /opt/claude-projects/bock`
(stale-binary trap → phantom results). Gate: fmt; clippy --workspace --all-targets -D warnings; test
--workspace; cargo doc -D warnings; `BOCK_CONFORMANCE_REQUIRE=all ./tools/scripts/run-conformance.sh`. Each
front-loads a T1 gate (minimal repro on all 5). Surface spec-divergence/new-DQ as OPEN, don't resolve.

## Item C — tail-position statement-`if` in loop bodies (DV15; 4/5; localized)
Root cause: `generator.rs:426-434 node_is_statement()` recognizes only Break/Continue/Return/Assign, not `If`.
A tail `if (c){return/break/…}` (no else, statement branch) routes through `emit_expr` → `/* unsupported */`
ternary (js/ts/python fail) / wrong return (Rust silent-wrong) / Go fail.
Fix: classify `If { else_block: None }` (and `If` whose both branches are statement bodies, via
`arm_body_is_statement` generator.rs:441) as a statement. Backends already have correct statement-`If` arms
(js.rs:1128; go.rs:1909/2131; rs/ts/py analogous). Essentially one function + a fixture.
Fixture: `exec/tail_if_in_loop.bock` — `for i in 0..10 { if (i==4) { return i } }` + break/continue variants, all 5.
Risk: don't misclassify value `if/else` (always has else + expression bodies → stays expression); if-let unaffected.

## Item A — cross-module `use` wiring (DV13; broken 5/5)
Current: ImportDecl emits non-functional imports (js/ts comment; py `from core.x import` of nonexistent;
rust `use core::x`; go `import "core/x"`). Stdlib baked in (bock-cli/build.rs include_str → EMBEDDED →
prepended build.rs:103), emitted to build/<t>/stdlib/core/<m>/<m>.<ext> but never wired into main.
DESIGN — **single-file bundling** (works with the single-file run model, no harness/toolchain change):
concatenate all compiled modules (stdlib + user) into the one entry file in dependency order (module_inputs
arrive topo-ordered, build.rs:147-188); ImportDecl → no-op.
- js/ts: concatenate top-level decls into main.js/main.ts (no module wrapper today → valid).
- python: concatenate into main.py.
- rust: flatten all module items to crate root (matches single-module emission today) — **A1: flatten vs
  inline `mod` (recommend flatten for P0; mod-wrapping needs `use`-path rewrites).**
- go: must emit ONE `package main` + merged/deduped `import (...)` + runtime blocks at most once + bodies
  (generate_project go.rs:291-384 currently emits these per-module → dedup is the highest-risk piece).
Where: per-backend `generate_project` overrides (Go already overrides) + a thin shared concat helper in
generator.rs. build.rs unchanged (bundling collapses N OutputFiles → 1).
Harness: `TestCase.source` is a single String (harness/mod.rs:37); the "main + local module + use core.*"
fixture needs multi-file support. **A2: extend harness for multi-file fixtures now (reusable) vs single-file
proxy (a `main` that `use core.compare.{Ordering,…}` + calls a core symbol — exercises the user→stdlib path
with today's harness). Recommend: land the proxy in P0-A; file harness multi-file as a fast-follow.**
**A3 (spec): §20.6.1 mandates one-file-per-module output; bundling diverges → surface OPEN** (per-module tree
can remain a future "library build" mode).
Owned: bock-codegen/{js,ts,py,rs,go}.rs (generate_project + ImportDecl) + generator.rs (bundling helper);
possibly tests/{execution.rs,harness/mod.rs}; new exec fixture; docs §20.6.1 + build-output ref.
Risks: symbol collisions on bundle (v1 stdlib names distinct; namespacing fallback); Go preamble/runtime dedup;
confirm topo order; source-map invalidation (acceptable P0).

## Item B — user-defined enum codegen (DV14; broken 5/5)
Enum DECLARATIONS already emit correctly (js tagged factories js.rs:672/1056; rust real enum rs.rs:689; go
sealed iface + variant structs go.rs:1216). Gaps at CONSTRUCTION + MATCH (no variant registry):
- Construction: unit `Red`→Identifier→`to_camel_case` `red` (js.rs:1372; should be `Color_Red`); struct/tuple
  `Circle{..}`→RecordConstruct→bare object (js.rs:1569; should be `Shape_Circle(...)`); Rust unqualified
  `Circle` not `Shape::Circle` (rs.rs:1729/2476).
- Match: js `is_adt` true only for ConstructorPat, not RecordPat (js.rs:1853) → struct-payload arms all
  `default:`; Rust paths unqualified (rs.rs:2117/2137); Go value-switch on undefined types (go.rs:2614/2866 —
  needs type-switch `switch v := s.(type)` + field extraction); Python no union alias + no payload binding.
MODEL: Optional (Some/None) is lowered bespoke and works on all 5 — generalize it. Build a shared
**enum-variant registry** in generator.rs (`collect_enum_variants(modules)` pre-scan → variant→{enum, payload})
built across the A-bundle; each backend consults it. Rust = smallest change (path qualification only); Go =
largest (value-switch → type-switch, reuse the #127 loop-label machinery for break-in-switch).
**B1: pre-seed Optional/Result (Some/None/Ok/Err) as built-in registry entries (one mechanism) — keep the
bespoke handlers until fixtures prove equivalence. B2: verify Go per-variant struct emits the sealed-interface
method (go.rs:1837).** Scope B to MONOMORPHIC user enums (generic enums = P1/DV12). Result runtime (TS/Py/Go)
= P1 but design the registry so built-ins are pre-seeded entries (avoid two parallel mechanisms).
Fixtures (all 5): enum_no_payload_match, enum_tuple_payload_match, enum_struct_payload_match (the last exposes
the RecordPat/is_adt + Go type-switch gaps). Model on exec/optional_match_*.bock.

## Anchors
generator.rs:426 (node_is_statement, C); generate_project default 337/derive_output_path 47 (A); registry home (B).
go.rs:291 (generate_project override, A) / 2614 (match→type-switch, B) / 1216,1837 (enum decl/variant) / 1157 (import).
js.rs:1372,1569,460 (construction, B) / 1853,1976 (match is_adt/RecordPat) / 607 (import, A) / 2060 (block tail, C).
bock-build/src/toolchain.rs:417-519 (single-file RunPlans). tests/execution.rs:128 + harness/mod.rs:37 (single-main).
