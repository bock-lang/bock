# Spec Alignment Assessment (D1, refreshed)

**Scratch file. Drives the design-chat handoff for spec/impl
divergence. Deleted in D5 alongside `docs/INVENTORY.md` once
D2–D4 have absorbed every row.**

2026-05-13

## Why a refresh

The first D1 pass (PR #35, merged 2026-05-11) walked the 28 CLI-
and-config rows the D0 inventory enumerated (C01–C05, F01–F22,
K04). Design chat resolved every row through four spec-change
batches (`20260512-1400`, `1500`, `1600`, `1700`), closing the
matrix at the spec end and consolidating `spec/sections/` into
`spec/bock-spec.md`.

This refresh widens the pass: instead of the CLI surface, every
spec section is walked for normative claims and compared against
the implementation. The previous file's 28 rows are not
re-litigated — they are resolved or tracked via Item B Phase 1.

## Summary

- Spec sections walked: **23** (§1 – §23 plus Appendices A–D)
- Divergences surfaced: **19**
  - Direct contradictions: **1**
  - Spec stale: **15**
  - Spec gaps: **2**
  - Implementation-defined: **1**
  - Genuine questions: **0**
- Implementation bugs filed separately: **0**
- Already tracked elsewhere (Item B Phase 1; do not re-route):
  **3** (`--deliverable`, `--no-tests`, `--optimize`)

The volume of "Spec stale" entries reflects the spec's
forward-looking character: many sections describe v1 surfaces
that the implementation will grow into. The pattern is uniform —
present-tense spec prose for a feature the impl doesn't yet
ship. Design chat decides per row: defer in spec, prune from
spec, or schedule implementation.

The single direct contradiction is §20.1.1 (`bock check`
flags) — the spec was amended for the `--only=<aspect>` /
`--brief` shape in Batch 3 of the K04 series, and the CLI hasn't
caught up. This is implementation work, not a spec question.

## OPEN list for design chat

Sorted by spec section.

- **§1.3** — Supported targets list claims 9 targets; impl ships 5
- **§10.3** — Layer 3 (project-level `[effects]`) prose lingers after Appendix A.3 marked the field Reserved
- **§10.6** — Effect erasure optimization described as a v1 production-mode pass; no pass exists
- **§13.3** — Channel example shows `buffer: 10` parameter the impl doesn't accept; iteration over channels not implemented
- **§13.4** — Five sync primitives listed (`Mutex[T]`, `RwLock[T]`, `Atomic[T]`, `WaitGroup`, `OnceCell[T]`); none exposed to Bock code
- **§13.5** — Cancellation: `Cancel` ambient plumbed in adaptive handlers, but `check_cancel()`, `task.cancel()`, checkpoint insertion, and target mapping (tokio/AbortSignal/context.Context/asyncio) are unimplemented
- **§14.1** — `native` blocks with backtick-quoted target code: keyword tokenised; no parser or codegen path
- **§14.2** — FFI linter warning suggesting platform trait: no linter rule (gated on §14.1)
- **§15** — Annotation taxonomy lists ~19 annotations; 12 recognised, 6 (`@test`, `@benchmark`, `@property`, `@derive`, `@target`, `@platform`) parsed but silently dropped at C-AIR. No "unknown annotation" diagnostic
- **§16.3** — AIR-T and AIR-B serialization formats: structures exist; neither serializer implemented
- **§16.4** — Binary package compatibility (patch/minor/major fallback) depends on §16.3; not implemented
- **§17.6** — Capability gap synthesis table (6 rows: ADTs / pattern matching / ownership / channels / refinement types / effects) not systematically realised by codegen
- **§18.3** — Core modules: spec lists 15 modules; bock-core ships a partial subset with stubs for `concurrency`, `effect`, `error`, `math`, `memory`, `test`
- **§18.5** — Trait-language integration claims (`Comparable` → `<`/`>`, `Iterable` → `for..in`, `Displayable` → `${}`, operator overloading via `Add`/`Sub`/etc.) not verified end-to-end
- **§19.7** — Versioning stability tiers (`stable`/`beta`/`experimental`) not in manifest schema
- **§20.1.1** — Spec amended for `--only=<aspect>` and `--brief` (K04 Batch 3); CLI still ships `--types` / `--lint` / `--no-context` (**Direct contradiction**)
- **§20.3** — LSP advertises AI Context Panel, Target Preview, Capability Graph, Smart Completions; impl LSP has text sync, hover, definition, diagnostics only
- **§20.5** — Built-in interpreter debugger with breakpoints / stepping / ownership inspection / effect handler display: interpreter exists, debugger UI doesn't
- **§20.6** — Remote build cache, build hooks, distributed builds: described as v1 surface; none implemented (consistent with Appendix A.3's `[build.hooks]` / `[build.cache] remote` being Reserved)

## Already tracked (do not re-route to design chat)

These are pre-classified by the previous D1 pass and the Item B
implementation plan. They appear here only for completeness; no
new routing.

| Surface | Status | Closing path |
|---------|--------|--------------|
| `bock build --deliverable` (§17.5, §20.6.2) | Item B Phase 1 | Closes when JS codegen project mode lands |
| `bock build --no-tests` (§20.6.2) | Item B Phase 1 | Closes alongside test-inclusion default |
| `bock build --optimize` (§17.2 Tier 3) | Item B Phase 1 | Closes when Tier 3 surfaces |
| `--target`, `--all-targets`, `--smart`, `--coverage`, `--snapshot` on `bock test` (§20.4) | K04 Batch 1 deferred in spec | Spec marked v1.x; no impl gap |
| `bock cache list` / `prune` (§20.1) | K04 Batch 1 deferred | Spec marked v1.x |
| `bock pkg update` / `audit` / `publish` / `search` (§19) | K04 Batch 1 deferred | Spec marked v1.x |
| `bock model list` / `install` / `use` (§20.1) | K04 Batch 1 deferred | Spec marked v1.x |
| `bock fix` / `migrate` / `target` (§20.1) | K04 Batch 1 deferred | Spec marked v1.x |
| Appendix A.3 reserved fields | K04 Batch 2 deferred | Spec already in Reserved section |
| `spec/sections/` numbering | K04 Batch 4 resolved | Files deleted; `bock-spec.md` is sole source |
| §17.7 Codegen Rule Learning | Marked Post-v1 in spec | No divergence; aligned-by-design |
| §18.4 `std.*` modules | Ships via package manager | Stdlib empty; D4 scaffolds reference |

## Classification scheme

- **Direct contradiction** — Spec X, impl Y, X and Y are clearly
  incompatible.
- **Spec stale** — Impl evolved past spec; spec hasn't caught up.
- **Spec gap** — Impl made a decision spec doesn't address.
- **Implementation-defined** — Spec uses MAY or doesn't specify;
  impl chose something specific that should arguably be in spec.
- **Genuine question** — Both reasonable but different; no clear
  right answer.

The recommendations below are implementation chat's read. Design
chat decides.

---

## §1.3 — Supported targets list claims 9; impl ships 5

**Spec says:** "Bock transpiles to JavaScript, TypeScript,
Python, Rust, Go, Java, C++, C#, and Swift" with a 9-row table
of targets.

**Implementation does:** `TargetProfile::all_builtins()` returns
five profiles: javascript, typescript, python, rust, go.
`TargetProfile::from_id()` only resolves these five plus their
short aliases.

**Classification:** Spec stale

**Reference:**
- Spec: spec/bock-spec.md, lines 53, 57–69
- Impl: compiler/crates/bock-codegen/src/profile.rs, lines 441–464

**Recommendation for design chat:** Either ship the four missing
targets as project commitments or mark §1.3's java/cpp/csharp/
swift rows as "Planned for v1.x" alongside their current "core,
ships-with-compiler" framing. Today the present-tense table
overstates the implementation.

---

## §10.3 — Layer 3 (project-level `[effects]`) lingers in §10.3 after Appendix A.3 marked the field Reserved

**Spec says:** §10.3 describes three layers of handler
resolution (Local > Module > Project) with project defaults
configured in `bock.project [effects]`. Appendix A.3 then lists
`[effects]` and `[effects.overrides.<env>]` as Reserved for
future versions.

**Implementation does:** Local `handling` blocks and module-
level `handle ... with` are wired. `[effects]` is not read from
`bock.project`; Appendix A.3 already marks it Reserved.

**Classification:** Spec stale

**Reference:**
- Spec: spec/bock-spec.md, lines 826–852 (§10.3 three-layer
  prose), 2302–2303 (Appendix A.3 `[effects]` Reserved entry)
- Impl: compiler/crates/bock-ast/src/lib.rs:1020–1022
  (`ModuleHandleDecl`), compiler/crates/bock-parser/src/lib.rs
  (handling-block parsing); no reader for `[effects]` in
  `bock-cli` or `bock-pkg`

**Recommendation for design chat:** Update §10.3 to mark "Layer
3 — Project defaults" as Reserved for v1.x in line with Appendix
A.3, or strike it from the resolution diagram for v1. Today's
text and A.3 disagree on whether Layer 3 exists.

---

## §10.6 — Effect erasure optimization described as a v1 production-mode pass; no pass exists

**Spec says:** "When a handler is statically known, the compiler
can inline it and erase the indirection entirely (effect erasure
optimization, applied in `production` mode)."

**Implementation does:** No dedicated effect-erasure pass.
Interpreter dispatches effect operations through inline-handler
lookup at runtime; codegen does not specialize on statically
known handlers.

**Classification:** Spec stale

**Reference:**
- Spec: spec/bock-spec.md, line 886 (§10.6)
- Impl: compiler/crates/bock-interp/src/interp.rs (inline
  handler dispatch path); no equivalent in bock-codegen

**Recommendation for design chat:** The wording is permissive
("the compiler can inline"), but reading it as a v1 commitment
overpromises. Either soften to "MAY apply" with a forward
reference, or commit a v1.x roadmap entry.

---

## §13.3 — Channel example shows a `buffer:` argument; impl is unbounded with no buffer param

**Spec says:** `let ch = Channel[Message].new(buffer: 10)` and
`for msg in ch { process(msg) }`.

**Implementation does:** `Channel.new()` takes no arguments and
returns an unbounded MPSC pair (sender, receiver). No iteration
syntax over channels is registered.

**Classification:** Spec stale

**Reference:**
- Spec: spec/bock-spec.md, lines 1156–1167
- Impl: compiler/crates/bock-core/src/concurrency.rs, lines
  3–46 (`Channel.new` registration, "unbounded async MPSC")

**Recommendation for design chat:** Decide whether v1 ships
bounded channels with a buffer parameter or rewrites the §13.3
example to use the unbounded API. The iteration form
`for msg in ch` may or may not be intended for v1 — both the
buffer argument and the iteration desugar need a decision.

---

## §13.4 — Five sync primitives listed; none exposed to Bock code

**Spec says:** "`Mutex[T]`, `RwLock[T]`, `Atomic[T]`,
`WaitGroup`, `OnceCell[T]` — available from `core.concurrency`."

**Implementation does:** `core.concurrency` registers `Channel`
methods only. Internal Rust uses of `std::sync::{Mutex, RwLock}`
in `bock-core` are implementation details, not Bock-visible
types.

**Classification:** Spec stale

**Reference:**
- Spec: spec/bock-spec.md, line 1171 (§13.4)
- Impl: compiler/crates/bock-core/src/concurrency.rs (Channel
  registration only); no Mutex/RwLock/Atomic/WaitGroup/OnceCell
  registrations in `bock-core/src/lib.rs`

**Recommendation for design chat:** Defer in spec — mark §13.4
"Reserved for v1.x". The full sync surface is a stdlib build-out
project, not a syntactic feature; v1 ships with channels alone.

---

## §13.5 — Cancellation: only the adaptive-handler integration is wired

**Spec says:** Ambient `Cancel` effect; checkpoint insertion at
every `await`, every effect operation, explicit `check_cancel()`
calls, and loop iteration boundaries in `@concurrent` blocks.
`task.cancel()` API. Target mapping table
(`tokio::sync::CancellationToken` / `AbortSignal` /
`context.Context` / `asyncio.Task.cancel()`). Strictness table
gating per-mode checkpoint requirements.

**Implementation does:** `Cancel` is plumbed through adaptive
handler combinators (cancellation flag, on_cancel hook). No
`check_cancel()` builtin, no `task.cancel()`, no checkpoint
insertion pass, no target-specific cancellation codegen.

**Classification:** Spec stale

**Reference:**
- Spec: spec/bock-spec.md, lines 1175–1230 (§13.5)
- Impl: compiler/crates/bock-core/src/adaptive.rs, lines 286,
  584–608 (cancellation flag in combinators only)

**Recommendation for design chat:** Cancellation is large. Either
scope it across v1.x milestones (basic `check_cancel` first;
target mapping with codegen; strictness gating last) or trim
§13.5 to the adaptive-handler surface that exists and defer the
remainder.

---

## §14.1 — `native` keyword tokenized; no parser rule for native function declarations

**Spec says:** `@target(js) native fn query_selector(sel: String) -> Optional[Element] { \`document.querySelector(${sel})\` }`

**Implementation does:** Lexer recognizes the `native` keyword.
No parser production for `native_fn_decl` (§21.14 of the spec),
no backtick token handling for the inline target code, no
codegen pass that consumes a native block.

**Classification:** Spec stale

**Reference:**
- Spec: spec/bock-spec.md, lines 1235–1244 (§14.1), 2169–2175
  (§21.14 grammar)
- Impl: compiler/crates/bock-lexer/src/token.rs:266,397,524
  (`Native` token); no consumer in `bock-parser/src/lib.rs`

**Recommendation for design chat:** Defer §14.1 in spec — mark
"Reserved for v1.x". FFI surface is a discrete capability that
ships with its own design pass (backtick tokenization,
per-target inline code validation, capability gap interaction).

---

## §14.2 — FFI linter warning gated on §14.1

**Spec says:** "FFI usage in multi-target projects triggers a
linter warning suggesting migration to a platform trait."

**Implementation does:** No native blocks exist (§14.1), so no
linter rule. Multi-target detection logic is also absent at the
lint layer.

**Classification:** Spec stale

**Reference:**
- Spec: spec/bock-spec.md, lines 1246–1248
- Impl: no linter pass handling FFI patterns

**Recommendation for design chat:** Defer alongside §14.1. The
warning is meaningless until native blocks parse.

---

## §15 — Annotation taxonomy: 6 annotations parsed but silently dropped at C-AIR

**Spec says:** §15 enumerates: `@concurrent`, `@managed`,
`@deterministic`, `@inline`, `@deprecated`, `@cold`, `@hot`,
`@requires`, `@target`, `@platform`, `@context`, `@performance`,
`@invariant`, `@security`, `@domain`, `@test`, `@test(skip)`,
`@benchmark`, `@property`, `@derive`.

**Implementation does:** Annotation syntax parses everywhere.
The C-AIR context interpreter (`bock-air/src/context.rs:195–211`)
handles `@context`, `@requires`, `@performance`, `@invariant`,
`@security`, `@domain`, `@concurrent`, `@managed`,
`@deterministic`, `@inline`, `@cold`, `@hot`, `@deprecated`
(13 total). The remaining six — `@target`, `@platform`, `@test`,
`@benchmark`, `@property`, `@derive` — fall through the
catch-all `_ => {}` arm and are silently dropped. No "unknown
annotation" diagnostic is emitted for typos.

**Classification:** Spec gap

**Reference:**
- Spec: spec/bock-spec.md, lines 1252–1266
- Impl: compiler/crates/bock-air/src/context.rs:195–211

**Recommendation for design chat:** Two facets:
1. **Recognition policy:** decide whether unknown annotations
   warn (catches typos), error (production-strict only?), or
   pass silently. Spec doesn't say.
2. **Per-annotation status:** `@test`, `@benchmark`, `@property`
   are test-framework hooks the test runner may consume directly
   without going through C-AIR. `@derive` is a codegen hook.
   `@target` and `@platform` are conditional-compilation hooks
   that probably belong with FFI / native-block work (§14).
   Each needs a routing decision; some may be "wired via
   different path" rather than "missing."

---

## §16.3 — AIR-T and AIR-B serialization formats not implemented

**Spec says:** "**AIR-T (text format):** Human-readable, designed
for AI consumption. **AIR-B (binary format):** Compact,
content-addressed, module-level granularity. Used for build
caches and binary package distribution."

**Implementation does:** AIRNode structures live in
`bock-air/src/node.rs`; no serializer or deserializer exists for
either format.

**Classification:** Spec stale

**Reference:**
- Spec: spec/bock-spec.md, lines 1307–1311
- Impl: compiler/crates/bock-air/src/node.rs (struct definitions
  only; no `serde` derives, no `to_text`/`from_binary` paths)

**Recommendation for design chat:** Defer in spec — mark §16.3
"Post-v1". Both serializers are infrastructure for downstream
features (build cache reuse, binary package distribution) that
aren't on the v1 critical path.

---

## §16.4 — Binary package compatibility depends on absent AIR-B

**Spec says:** "Packages distribute pre-compiled T-AIR alongside
source. ... The compiler checks AIR format version and falls
back to source compilation transparently when incompatible."

**Implementation does:** No pre-compiled T-AIR distribution
infrastructure. Packages distribute source.

**Classification:** Spec stale

**Reference:**
- Spec: spec/bock-spec.md, lines 1313–1321
- Impl: compiler/crates/bock-pkg/ (manifest, resolver, install
  cover source distribution only)

**Recommendation for design chat:** Defer with §16.3. Pre-
compiled T-AIR is a v1.x cache/distribution feature, not a v1
package-manager capability.

---

## §17.6 — Capability gap synthesis table not systematically realised

**Spec says:** Six-row table mapping AIR constructs to synthesis
strategies for capability-deficient targets — algebraic types →
tagged objects + switch (JS); pattern matching → if/else chains
(Go); ownership → erase (JS, Python); channels → AsyncIterator +
Queue (JS); refinement types → boundary validation; effects →
parameter passing.

**Implementation does:** `bock-ai/src/request.rs` carries a
`CapabilityGap` concept used by the AI provider's Generate
mode. Specific synthesis strategies per (construct, target)
pair are not encoded as a single deterministic table; they live
implicitly in each target's codegen path.

**Classification:** Spec stale

**Reference:**
- Spec: spec/bock-spec.md, lines 1403–1412
- Impl: compiler/crates/bock-ai/src/request.rs (gap concept);
  compiler/crates/bock-codegen/src/{js,ts,python,rust,go}.rs
  (per-target codegen)

**Recommendation for design chat:** Decide whether the §17.6
table is normative (codegen must implement these specific
synthesis strategies) or illustrative (these are examples; the
synthesis is whatever each target's codegen package chooses).
Today's impl behaves as if illustrative; the spec reads as
normative.

---

## §18.3 — Core module list is partial

**Spec says:** §18.3 lists 15 core modules: `core.types`,
`core.collections`, `core.string`, `core.math`, `core.option`,
`core.result`, `core.iter`, `core.compare`, `core.convert`,
`core.error`, `core.effect`, `core.concurrency`, `core.memory`,
`core.time`, `core.test`.

**Implementation does:** `bock-core` registers `core.time`,
collections, string primitives, options/results, trait dispatch
infrastructure, and `core.test`'s assertion shims. Modules
`core.concurrency`, `core.effect`, `core.error`, `core.math`,
and `core.memory` are stubs per `bock-core/src/lib.rs` (lines
21–29, marked "unimplemented").

**Classification:** Spec stale

**Reference:**
- Spec: spec/bock-spec.md, lines 1488–1502
- Impl: compiler/crates/bock-core/src/lib.rs, lines 21–73

**Recommendation for design chat:** Either mark the unstubbed
modules "Reserved for v1.x" in §18.3 or schedule the build-out
as a stdlib milestone. The spec implies v1 ships these; today
the impl ships a fraction.

---

## §18.5 — Trait-language integration claims not end-to-end verified

**Spec says:** "Core traits opt types into language features:
`Comparable` enables `<`/`>`, `Iterable` enables `for..in`,
`Displayable` enables `${}` interpolation, `Add`/`Sub`/etc.
enable operator overloading."

**Implementation does:** `bock-core` exposes `TraitDispatch`
(`bock-core/src/lib.rs:34`). The wiring between trait
implementations and the corresponding language constructs is
not centrally documented and was not verified end-to-end during
this pass — e.g., does declaring `impl Comparable for MyType`
make `<` work on `MyType` values in `match` guards and `if`
conditions, today?

**Classification:** Spec gap

**Reference:**
- Spec: spec/bock-spec.md, lines 1605–1607
- Impl: compiler/crates/bock-core/src/lib.rs:34 (TraitDispatch);
  no single integration test verifies the four claims

**Recommendation for design chat:** Decide on a normative
expectation. If `Comparable → </>`, `Iterable → for..in`,
`Displayable → ${}`, and operator overloading are v1
commitments, add a §18.5 conformance test surface. If they're
aspirational, soften the wording. The implementation chat can
follow up with an audit once the design intent is clear.

---

## §19.7 — Versioning stability tiers not in manifest schema

**Spec says:** "Stability tiers: `stable`, `beta`,
`experimental`. Production strictness can reject dependencies
below a stability threshold."

**Implementation does:** `bock.package` manifest carries
`name`/`version`/`license`/dependencies/features; no stability
field. The resolver makes no decisions based on stability.

**Classification:** Spec stale

**Reference:**
- Spec: spec/bock-spec.md, lines 1675–1677
- Impl: compiler/crates/bock-pkg/src/manifest.rs (no
  `stability` field), compiler/crates/bock-pkg/src/resolver.rs
  (no stability-based filtering)

**Recommendation for design chat:** Defer. Stability tiers
become useful once an ecosystem of packages exists; v1 has none.
Mark §19.7 "v1.x" or remove the tier sentence until production-
strictness rejection logic is designed.

---

## §20.1.1 — `--only=<aspect>` / `--brief` spec amendment not implemented in CLI

**Spec says:** As amended by `20260512-1600-specs-changes.md`
(K04 Batch 3): `bock check --only=<aspect>` accepts comma-
separated or repeated values; v1 aspects `types` and `context`;
`--brief` produces compact error output (omits source-context
snippets).

**Implementation does:** `bock check` still ships the
pre-amendment surface: `--types`, `--lint`, `--no-context`
(`bock-cli/src/main.rs:100–110`). The aspect-selection
mechanism is mutually-exclusive booleans rather than an
`--only=<aspect>` list.

**Classification:** Direct contradiction

**Reference:**
- Spec: spec/bock-spec.md, lines 1692, 1718–1735 (§20.1.1 as
  amended)
- Impl: compiler/crates/bock-cli/src/main.rs:96–116, 456–468

**Recommendation for design chat:** This is implementation work,
not a spec question. The CLI needs the F04 resolution applied:
add `--only=<aspect>`, rename `--no-context` to `--brief`,
remove the three pre-amendment flags. Open a follow-up session
to land the change.

---

## §20.3 — LSP advertises features the impl doesn't have

**Spec says:** §20.3 lists AI Context Panel, Target Preview,
Capability Graph, Smart Completions, Inline Diagnostics as Bock-
specific LSP extensions.

**Implementation does:** `bock-lsp` advertises text-document
sync, hover, definition, and diagnostics. The four
Bock-specific extensions are not present.

**Classification:** Spec stale

**Reference:**
- Spec: spec/bock-spec.md, lines 1749–1761
- Impl: compiler/crates/bock-lsp/src/server.rs:48–60

**Recommendation for design chat:** Defer the four extensions in
spec — mark "Planned for v1.x" or remove from §20.3 entirely.
LSP feature parity is a long-tail effort orthogonal to language
v1.

---

## §20.5 — Built-in interpreter debugger described as v1; UI absent

**Spec says:** "Built-in interpreter debugger with breakpoints,
stepping, expression evaluation, ownership state inspection,
effect handler display, and context viewing."

**Implementation does:** `bock-interp` exists. No debug protocol
(DAP) bridge, no breakpoint/stepping primitives, no
ownership-state inspection surface.

**Classification:** Spec stale

**Reference:**
- Spec: spec/bock-spec.md, lines 1779–1781
- Impl: compiler/crates/bock-interp/ (interpreter only; no
  debugger module)

**Recommendation for design chat:** Defer §20.5 to v1.x. Source
maps in §20.5's second sentence are already covered by
`--source-map`/`--no-source-map` flags (Batch 1); the debugger
UI is the deferrable piece.

---

## §20.6 — Build system features (remote cache, build hooks, distributed builds) described as v1; none implemented

**Spec says:** "Remote build cache. Build hooks (Bock scripts).
Distributed builds for CI."

**Implementation does:** Build pipeline (Parse → ... →
Assemble) exists in `bock-build`. Remote cache, build hooks, and
distributed builds are not present. Appendix A.3 already marks
`[build.hooks]` and `[build.cache] remote` as Reserved.

**Classification:** Spec stale

**Reference:**
- Spec: spec/bock-spec.md, line 1785 (§20.6 opening paragraph)
- Impl: compiler/crates/bock-build/ (incremental cache exists;
  remote/hooks/distributed do not)

**Recommendation for design chat:** Strike or qualify the §20.6
opening sentence. Appendix A.3 has already deferred the
configuration surfaces; §20.6's introductory feature list
should match.

---

## Notes for downstream phases

- **D2/D3 cross-references.** Cite `bock-spec.md` by section
  number directly. The K04 consolidation removed
  `spec/sections/`, so no mapping table is needed.
- **Item B Phase 1.** Three rows in the "Already tracked" table
  (`--deliverable`, `--no-tests`, `--optimize`) close when Item
  B Phase 1 lands and add no work to this D1 refresh's design
  chat. The S22 / S20.6.2 surface area (project mode, deep/
  shallow tooling config, test inclusion default) is the same
  Item B work; not separately routed.
- **§20.1.1 implementation lag** is the one row that does need
  implementation work, not design discussion. Open a CLI
  follow-up session to land the `--only=<aspect>` / `--brief`
  rename.
- **D6 (changelog backfill)** does not depend on this refresh.
  Every item above is a spec deferral or a future-work routing;
  none changes the changelog history.
