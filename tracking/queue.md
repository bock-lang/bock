# Queue — active work

**The one question:** what work is to-be / being done?

Orchestrator-owned. Actionable items only (impl / spec / docs / chore /
bug). Factual spec↔impl mismatches live in `divergences.md`; undecided
behavior in `design-questions.md`; version mapping in `milestones.md`;
present-state in `snapshot.md`. Each item has a stable ID, named once
here and referenced elsewhere. Raw OPEN/FOUND tags arrive via PR
descriptions; the orchestrator triages them into the right file.

Schema: `[ID] title — type · status · owned-files · blocked-by ·
links · note`. Status ∈ {ready, in-flight, blocked, deferred}.

_**Last reconciled 2026-06-09 — main 5994e9a, 0 open PRs, clean, CI green. ★ BACKLOG-DRAIN + DESIGN-GATE.**
Solo engineer lane (the smaller of the two open `ready` items; the other is `solo` and they collide on `py.rs`, so they
sequence). **#306** Q-py-enum-variant-import (python import lowering drops **unaliased** braced enum-variant leaf names from
`from {module} import …` — the variant is emitted as the dataclass `Ordering_Less`, not `Less`, so the bare-name import raised
`ImportError`; reached now via use-site + implicit-import, mirroring the js/ts `Named` filter and the #303 rust fix; `python`
re-added to the variant-bracing fixture). PR-own-CI was the combined-tree check (solo on unchanged base): full CI green — all 6
test cells, clippy, blocking examples matrix, stdlib-fmt; conformance REQUIRE=all **824/0/0** ×5. Orchestrator re-verified the
diff scope (2 owned files) + CI before squash-merge. **1 CLOSED.** **The enum-variant-import mirror is now COMPLETE across
js/ts/python/rust** (go never affected). **BOARD NOW DESIGN-GATED:** the two remaining backlog items both await an owner ruling
and there is **no autonomous `ready` engineering left**: (1) **Q-list-mut-pop-insert-remove → DQ30 (NEW, escalated)** — §18.3 is
silent on `pop`/`insert`/`remove`/`reverse` return contracts (contested: `remove` by-index return type, OOB behavior,
`pop`-on-empty); surfaced to owner, deferred ("will circle back"); (2) **Q-equatable-gating-user-types → DQ29** (still pending).
**Awaiting owner: DQ30 ruling** (List mutator signatures — recommended **Optional-safe**: `pop`→`Optional[T]`, `remove(i)`→`Optional[T]`,
`insert(i,v)`→`Void`, `reverse`→`Void`, all `mut self`) **+ DQ29 ruling** (Equatable `==`/`!=` gating — recommended **R1
auto-conform**). ↓ —
PRIOR: **Last reconciled 2026-06-09 — main 5137a62, 0 open PRs, clean, CI green. ★ NIGHT WIND-DOWN.**
Disjoint pair (bock-codegen ⨯ bock-fmt): **#303** Q-rust-enum-variant-import (rust drops braced enum-VARIANT items from a
`use` and imports the enum TYPE instead → no more `E0432`; variant-bracing fixture builds+runs ×5) + **#304**
Q-fmt-doccomment-indent (preserve `//!` continuation-line indentation via a ZERO-RIPPLE bock-fmt seam — re-derive from the raw
comment stream; **lexer untouched**, so parser/`bock doc`/LSP unaffected; stdlib-fmt stays clean). Combined tree re-verified
(fmt/clippy/**test 0 failed**/doc; conformance REQUIRE=all **0 failed** ×5; stdlib-fmt clean). **2 CLOSED.** **1 NEW filed:**
Q-py-enum-variant-import (#303 FOUND, LOW — Python has the same enum-variant import bug: `from … import Less` but the variant is
class `Ordering_Less` → `ImportError`). **NIGHTLY STATE: main green, 0 open PRs, all worktrees pruned.** Open backlog (all
non-blocking): Q-py-enum-variant-import (LOW · py.rs), Q-list-mut-pop-insert-remove (types+codegen · solo),
Q-equatable-gating-user-types (**BLOCKED on DQ29 → Design**). **Awaiting owner: DQ29 ruling** (`==`/`!=` Equatable gating —
R1 auto-conform / R2 defer-to-derive / R3 strict[rejected]). ↓ —
PRIOR: **Last reconciled 2026-06-08 — main 2b0f8c2, 1 open PR (#300 doc-only design-OPEN — PROPOSED close), clean, CI green.**
★ FOLLOW-UP WAVE. The proposed "trio" (Q-user-comparison-codegen + Q-equatable-gating + Q-rust-host-sleep) could NOT run 3-way:
all three collide on bock-codegen/bock-types (the rust scaffold is in bock-codegen, NOT bock-build; and the comparison lowering
needs a checker stamp in `infer_binop`) — so it ran as **Cmp SOLO → Eq+Sleep PAIR**. **3 items CLOSED:** Q-user-comparison-codegen
(**#299** — user-type `<`/`>`/`<=`/`>=` now lower through `compare()` ×5 via a new `USER_COMPARE_META_KEY` stamp; parked fixture
flipped; conformance 814/0), Q-rust-host-sleep-tokio-dep (**#301** — rust scaffold adds `tokio` via a `tokio::`-content-scan
trigger; bare-host `sleep` builds+runs ×5; 819/0), and with #299 the Q-list-operator-gating codegen half is complete. **1
ESCALATED → Design:** Q-equatable-gating-user-types → **DQ29** (#300, doc-only investigation: records/enums have free structural
`==` but NO checker-visible `Equatable`, and `@derive` is v1.x-reserved → a strict gate breaks idiomatic `record == record`;
R1 auto-conform / R2 defer-to-derive / R3 strict[rejected]). **1 NEW filed:** Q-rust-enum-variant-import (#299 FOUND, LOW — rust
`use core.compare.{Less,Equal,Greater}` lowers to a non-existent free import → E0432). Both compiler PRs were solo on an unchanged
base → their own CI was the combined-tree check (green). **PROPOSED:** `gh pr close #300` (doc-only; content captured in DQ29 +
escalations). ↓ —
PRIOR: **Last reconciled 2026-06-08 — main 8faf8d7, 0 open PRs, clean, CI green.** ★ PAIR FAN-OUT (bock-codegen ⨯ bock-types) — both
merged + combined tree re-verified on the octopus merge (fmt/clippy/**test 0 failed**/doc; conformance REQUIRE=all **0 failed**
×5; examples-exec 20/20 build, **no regressions**). PRs: **#297** codegen · **#296** types. **2 items CLOSED:**
Q-clock-handler-routing (**#297** — `Instant.now`/`sleep`/`elapsed` now dispatch through the installed `Clock` handler when in
scope [host primitive stays the no-handler default]; interception verified ×5 with a self-contained user handler → §18.4 virtual
time is now achievable), Q-list-operator-gating-user-types (**#296** — `<`/`>`/`<=`/`>=` now require `impl Comparable` on user
operands [**E4005** + suggestion], also enforces "Bool is not Comparable"; conservative, no false-positives, examples 20/20 ×5
no-regression). **3 NEW filed (all non-blocking):** Q-user-comparison-codegen (#296 FOUND — a user-`Comparable` comparison still
lowers to a NATIVE `<`, broken ×5 [py TypeError, go/rust compile errors, js silent-wrong]; must route through `compare()`; a
`.skip` exec fixture is parked → the natural next codegen lane), Q-rust-host-sleep-tokio-dep (#297 FOUND, LOW — rust bare-host
`sleep` needs a tokio scaffold dep; bock-build), Q-equatable-gating-user-types (#296, LOW — `==`/`!=` Equatable gating deferred;
records carry structural equality). **Remaining backlog:** Q-user-comparison-codegen, Q-rust-host-sleep-tokio-dep,
Q-equatable-gating-user-types, Q-list-mut-pop-insert-remove, Q-fmt-doccomment-indent (LOW). ↓ —
PRIOR: **Last reconciled 2026-06-08 — main 52061ff, 0 open PRs, clean, CI green.** ★ Q-prim-assoc COMPLETE (solo session). **#294**
lands the PRIMITIVE half of Q-prim-assoc (the user-type half was #288): `Float.from`/`Int.from`/`String.from` +
`Int.try_from`/`Float.try_from` (→ `Result[_, ConvertError]`) now check AND execute ×5 — the already-registered canonical
conversion matrix, NO new semantics (lossy/narrowing still `E4012`); coupled checker resolution + per-target lowering (py
`float(..)`/`int(..)`, rust `i64::try_from`, go native casts). **FOUND+fixed a pre-existing Rust bug:** `core.convert`'s
`From`/`TryFrom` trait decls emitted associated methods with a spurious `&self` (`E0186`), so ANY Rust program importing
`core.convert` failed to build (now omits the receiver + adds `where Self: Sized`). Verified: 4-gate clean + conformance
REQUIRE=all **789/0** ×5; PR CI green on the unchanged base (solo PR → its own CI is the combined-tree check). **OPEN §18.3**
primitive-conversion-matrix RATIFICATION is the pre-existing Design item (design-questions.md, parallels DQ10) — #294 shipped the
floor, did not ratify/extend. **Remaining backlog (all non-blocking):** Q-clock-handler-routing, Q-list-mut-pop-insert-remove,
Q-list-operator-gating-user-types, Q-fmt-doccomment-indent (LOW). ↓ —
PRIOR: **Last reconciled 2026-06-08 — main d79ae4c, 0 open PRs, clean, CI green.** ★ WAVE-3 BACKLOG FAN-OUT — 3 file-disjoint
lanes (extensions/vscode ⨯ bock-types ⨯ bock-codegen), all merged + the combined COMPILER tree re-verified on the octopus
merge (fmt/clippy/**test 0 failed**/doc; conformance REQUIRE=all **0 failed** ×5). PRs: **#290** vscode · **#292** types ·
**#291** codegen. **3 items CLOSED:** Q-vscode-langclient-v10 (**#290** — migrated to `vscode-languageclient` v10; root cause
was tsconfig `moduleResolution` [v10 added an `exports` map that node10 resolution ignores], NOT the imports — no `.ts` source
changed; the `vscode extension` CI job now passes; **dependabot #285 auto-closed**. ⚠ **USER-FACING:** required `engines.vscode`
^1.75→^1.91 [VS Code Jun-2024, v10's floor]), Q-checker-method-generic-call-infer (**#292** — a method's own type param `U` in
`Box[T].map[U]` is now inferred from the call args [freshened per-call] at both method-resolution paths; `b.map(dbl)` checks AND
**executes ×5** — no codegen gap; the receiver still pins `T`), Q-xmod-bounds-codegen (**#291** — ts/go now fold a `where`-clause
bound onto the generic param [`<T extends Show>` / `[T Show]`]; `xmod_where_bound_dispatch` runs on all 5. **FOUND
broader-than-#286's-note:** the bound was dropped for LOCALLY-defined `where (T: Ranked)` fns too [inline `[T: Ranked]` already
worked — it lands on `GenericParam.bounds`; the `where`-clause lands in a separate field the ts/go renderers never read]; one
`merge_where_bounds_into_generics` fold helper fixes both local + imported). **Q-fmt-doccomment-indent** (LOW, lexer) is now the
only open wave-2/3 follow-up. ↓ —
PRIOR: **Last reconciled 2026-06-08 — main 3bcaebb, 1 open PR (#285, blocked), clean.** ★ DEPENDABOT WAVE + WAVE-2 BACKLOG
FAN-OUT. **Dependabot:** 9/10 routine bumps merged (#276–#284 — setup-go, checkout, chrono, @types/node, cloudflare, marked,
astro, vsce, wrangler), merged round-robin across shared lockfiles (one per group, dependabot-recreate for the conflicting
astro). **#285** (vscode-languageclient 9→10, major) is BLOCKED on an extension code migration — v10 dropped the
`vscode-languageclient/node` subpath export → 5× `TS2307`, reddening the `vscode extension` CI job → filed Q-vscode-langclient-v10.
**Wave-2:** 3 file-disjoint engineer lanes, all merged + integrated-state re-verified by the orchestrator on the COMBINED tree
(octopus merge → fmt/clippy/**test 0 failed**/doc clean; conformance REQUIRE=all **772 passed / 0 failed / 0 skipped** ×5
[go/js/python/rust/ts]). PRs: **#287** fmt · **#286** types · **#288** codegen. **5 items CLOSED:** Q-bockfmt-cfarm-comma +
Q-bockfmt-utf8-panic (**#287** — both `bock fmt` bugs fixed: value-less cf-arm bodies drop the illegal trailing comma, line-wrap
snaps to char boundaries; `iter.bock`+`collections.bock` folded into the stdlib-fmt gate → now **10/10, 0 excluded**),
Q-xmod-bounds + Q-xmod-impl (**#286** — cross-module where-bounds now enforced + cross-module From/Into impl-table seeded,
threaded via the existing exported-`TypeRef` channel + synthetic `__bock_impl__` markers since `ExportedSymbol` lives in
bock-air), Q-blanket-into-codegen (**#288** — derived blanket `.into()` → `Target.from(self)` via a post-typecheck codegen
pre-pass, exec-verified ×5). **NEW (filed Ready):** Q-vscode-langclient-v10 (#285's blocker), Q-xmod-bounds-codegen (OPEN from
#286 — ts/go don't re-emit the generic-param constraint for an imported generic fn; the where-bound exec fixture is
js/py/rust-only), Q-fmt-doccomment-indent (LOW, FOUND from #287 — the lexer `.trim()`s doc-comment lines so `bock fmt` can't
reconstruct prose indentation; a lexer fix, not fmt). **Q-prim-assoc re-scoped:** #288 FOUND+fixed the USER-type associated-fn
codegen half (`Type.from`/`Type.origin` ×5); the PRIMITIVE half (`Float.from(3)`) remains checker+codegen-coupled. ↓ —
PRIOR: **Last reconciled 2026-06-07 — main 09427b8, 0 open PRs, clean, CI green.** ★ BROAD BACKLOG FAN-OUT (wave 1) — 4
file-disjoint lanes, all merged + integrated-state gate re-verified by the orchestrator on the combined tree (octopus
merge → fmt/clippy/**test 2730+/0**/doc clean; conformance REQUIRE=all **0 failed**; examples-exec **STRICT** 20/20 build ·
19/20 ran (+1 STUB) ×5, **no regressions**; new stdlib-fmt-check). PRs: **#274** types · **#273** interp · **#272** CI ·
**#271** codegen. **~9 items CLOSED:** Q-checker-unknown-method-concrete (★ soundness — an unknown method on a CONCRETE
receiver now errors **E4013** + nearest-name suggestion instead of resolving to a fresh var; gated to closed-method-set
receivers [primitives, built-in List/Map/Set, Optional/Result, in-scope user records/classes], §4.9 Flexible/sketch EXEMPT;
fix also surfaced+closed a trait-default-method resolution false-positive `Eq::not_equals`), Q-import-reject (bare `use
core.error` → **E4014** pointing at the braced form), Q-self-subst (verified already-resolved #141), Q-iter-interp-mutself
(`mut self` field writes now persist across interp method-call frames — the `loop { match it.next() }` hang is gone),
Q-interp-enum (2/3: user associated-fn dispatch + user-impl `to_string` shadowing the builtin; blanket `.into()` split out →
Q-blanket-into-codegen), Q-py-valpos-stmt-arms (py value-position match no longer drops a stmt-arm's leading statements),
Q-rust-str-mixed-binding (rust `String` match mixing `&str` literal + whole-scrutinee bind), Q-stdlib-fmtcheck (8/10 stdlib
files `bock fmt`'d — behavior-equivalence proven — + new **BLOCKING** `stdlib-fmt` CI job), Q-error-message-jstspy (verified
already-fixed at base #193; fixture strengthened to read both field + method ×5). **examples-exec CI gate RATCHETED
informational → BLOCKING** (+ a real `tsc --noEmit` on the ts row via a pinned `typescript@5`). Q-interp-effect-op-collision
evaluated, left as-is (deterministic dependency-order shadowing #157 is correct for v1). **NEW FOUND (none blocking, all
filed in Ready below):** **Q-bockfmt-cfarm-comma** + **Q-bockfmt-utf8-panic** (two `bock fmt` bugs that block
`collections.bock`/`iter.bock` from the new fmt gate), **Q-prim-assoc re-scoped** (checker+codegen-COUPLED — enabling the
checker alone yields broken `Type.from` codegen ×5; not checker-only as first noted), **Q-blanket-into-codegen** (derived
`Into` `.into()` is unexecutable on the JS compiled target too — a codegen/AIR gap, not interpreter-only; pairs with
Q-xmod-impl). **WAVE-2 backlog (deferred this wave only because they crate-conflict with the wave-1 lanes — bock-types ⨯
Lane A, bock-codegen ⨯ Lane D):** Q-xmod-bounds + Q-xmod-impl, Q-checker-method-generic-call-infer,
Q-list-operator-gating-user-types, Q-list-mut-pop-insert-remove, Q-propagate-exprpos-shared (LOW), low-pri effect
diagnostics (Q-effect-op-node-lowering, Q-effect-import-unused), Q-clock-handler-routing. ↓ —
PRIOR: Last reconciled 2026-06-06 (b) — main 56eece6, 0 open PRs, clean, CI green.** ★ DQ18 + DQ22 DONE (#269) + STDLIB-SURFACE
RATIFICATION BATCH → **Design board empty except DQ1 (non-core CLI).** **#269:** List `push`/`append` are `mut self` Void
mutators (mut-receiver `E5004`; codegen ×5 incl. go `recv = append(recv, x)`); Map `contains` rejected (`E4013` →
`contains_key`); spec §18.3 + changelog; conformance 749/0. **Ratification (this PR, spec-only):** DQ10 primitive-conformance
matrix (§18.5 note — Float IEEE-partial, Float not Hashable, Bool not Comparable), DQ11 convert surface ratified, DQ12 iter
protocol (generic/eager/List-returning/dual model), DQ13 §18.2 +`TryFrom`/`Error`, DQ14 `iter()->ListIterator[T]` floor, DQ15
concrete dispatch, DQ24 6-combinator floor (§18.3 + forward-refinement vs `20260529-2251`/DQ16) + §6.5 associated-type
Reserved-v1.x note, DV17 core.test benchmarking dropped — one changelog `20260606-stdlib-surface-ratification.md`. **DQ17
CLOSED** (non-normative); **DQ21 → impl backlog** (no language decision). NEW follow-up impl items (none blocking):
Q-checker-unknown-method-concrete, Q-list-operator-gating-user-types, Q-list-mut-pop-insert-remove. ↓ —
PRIOR: Last reconciled 2026-06-06 — main 9c53c0f, 0 open PRs, clean, CI green.** ★ DQ23 DECIDED + DONE + README refreshed.
**DQ23** ruled Option A (truncating-toward-zero integer division) and shipped in **#264** (checker `int_arith`/`bool_stringify`
stamps; js/ts/py division+modulo arms — toward-zero truncation, dividend-sign `%`, zero-divisor abort; rust/go already
conformant; spec §3.6/§3.5 + changelog; acceptance fixtures green ×5 incl. negative operands + zero-divisor abort). DQ20 also
CLOSED (done-by-impl). **With DQ23 + DQ20 closed, NO cross-target-correctness decision is open** — remaining design items are
non-blocking (DQ18 list mutability; DQ22 `m.contains`; the DQ10–15/24+DV17 ratification batch). Side-quest: **root README
refreshed** (#265 — verified commands/links, marketing-locked voice incl. the canonical three-paragraph ¶1; AI kept out of the
lead). FOLLOW-UPS (operator/website, non-blocking): set the GitHub "About" to the 12-word locked descriptor; create the Bluesky
handle + enable GitHub Discussions (until then the README omits both, and the website footer's Discussions link should be
dropped to avoid a 404). ↓ —
PRIOR: Last reconciled 2026-06-05 22:10 — main c095258, 0 open PRs, clean, CI green.** ★ todo()/guessing-game RULING APPLIED + 3
codegen reds fixed → examples **95/100 run-to-completion + 5 stub-showcase = 100/100 non-red, 100/100 build-clean ×5.** #262
fixed the 3 real codegen reds (**Q-calculator-ts-eval** — ts strict-mode `eval`→`eval_`; **Q-py-collections-builtin-shadow** —
py builtin-shadow rename `list`→`list__bN`; **Q-systems-allocator-go-build** — go `obj.field` element typing). Design ruled
on **todo()** (Never-typed; aborts via the Panic ambient effect §10.5; optional message — §18.2 normative + changelog) and
**guessing-game** (a `todo()`-stub showcase: compile-verified, NOT run-to-completion — its stubs need v1.x RNG/stdin;
recategorized in the audit as **STUB** = non-red — the honest +5; baseline re-recorded). **DQ20 CLOSED** (done-by-impl:
Q-propagate-operator-noop #226–#229; only the LOW Q-propagate-exprpos-shared residual remains, no v1 example hits it). DQ23
feasibility PROBED (orchestrator, read-only): operand type isn't available at the codegen `/` site, but the established
`list_concat`/`string_concat` checker-stamp pattern makes the prerequisite cheap; **Option A (truncating-Int) recommended**
(3 codegen arms + 1 stamp; result type stays Int) over B (always-Float, ripples through inference). **NEXT design decision =
DQ23** (escalated, awaiting ruling). Remaining open codegen: **Q-checker-method-generic-call-infer** (type-zoo/go `b.map(dbl)`
inference — the DQ28 residual). ↓ —
PRIOR: Last reconciled 2026-06-05 18:45 — main e096253, 0 open PRs, clean, CI green.** ★ DQ27/DQ28 SHIPPED + EXAMPLES 84→92/100.
The operator relayed Design's DQ27/DQ28 rulings (handoff folded into the hub); a file-disjoint fan-out landed both rulings +
the non-blocking lane in two waves — **6 PRs**: #255 ts-tsc-gate · #256 **DQ28** go free-fn method-generics + chained-combinator
+ compose(go) · #257 nested-compose js/ts · #258 **DQ27** single-method-namespace (checker **E4012** + react-components fix +
spec §6.4/6.5/6.7 + changelog) · #259 chat-protocol py · #260 chat-protocol ts + bock-build per-project `tsc` flip.
**react-components now runs on all 5** (the last all-red example); type-zoo/go method-generics + data-pipeline/ml-data-prep
compose green ×js/ts/go; chat-protocol green ×5 (**rust was already fixed at base — that residual was stale**). Honest audit
re-recorded (`tools/examples-exec-baseline.txt`): **js 19 · ts 18 · py 18 · rust 19 · go 18 = 92/100** (was 84). **HONESTY
NOTE:** #259 fixed a python statement-`match`→early-`return` bug that had silently TRUNCATED examples (exit 0 = false 'pass');
chat-protocol py is now a true pass, but **type-zoo py honestly flipped pass→run-FAIL** on a separate builtin-shadow bug
(de-masking, not a regression). Remaining **8 reds: 5 are guessing-game** (its own `todo()` stubs — not codegen) + 3 real
codegen reds → newly filed Q-calculator-ts-eval, Q-py-collections-builtin-shadow, Q-systems-allocator-go-build (+ the type-zoo/go
residual Q-checker-method-generic-call-infer). INCIDENT: a cross-session `git stash` race (#257 popped #258's stash — sibling
worktrees share one `.git`); recovered, #258 finished by the orchestrator, gate re-verified (conformance REQUIRE=all 0 failed);
dispatch prompts now forbid `git stash` ([[parallel-worktree-git-stash-hazard]]). DECIDED: DQ27 + DQ28 (design-questions.md;
escalation resolved) + Design's Tier A–D prioritization folded in — **DQ23 (Int/Int division cross-target divergence) + DQ20
(`?` propagation) are next-highest leverage.** ↓ —
PRIOR: Last reconciled 2026-06-05 07:34 — main e2200f5 (+#250 +this PR), 0 open PRs after merge, clean, CI green.** ★ EXAMPLES-
GREENING + CLASS-CODEGEN PUSH (#238–#252 + perf-gate #248) — a sustained parallel fan-out drove examples **63→84/100
runtime-working** (js 18 · ts 13 · py 18 · rust 19 · go 16; **49→84 across the whole session**). Waves: (a) per-target
build-error fan-out #238–#242 (go/rust/ts/py emitters + the **Q-conformance-target-race** harness fix) → 74; (b) loop-tail-
return (#243 js/#244 py; ts was #240) + **Q-glob-import-enum-variant** (#245) + go tuple-in-Result (#246) + rust residual
builds (#247) → 80; (c) **Q-class-codegen** (#249 js/ts construction · #250 py methods · #251 go casing · #252 rust Fn/move)
→ 84 — **react-components, the last all-red example, now passes py/rust/go.** Plus **Q-perf-gate-ci** (#248 — informational
perf-regression CI gate, operator-requested) and a CRLF-normalize Windows hotfix (#250). **0 net regressions across ~20 PRs.**
INCIDENTS: 4 sub-agent background-and-wait stalls (recovered by orchestrator re-verify+commit; [[engineer-subagent-dispatch-discipline]]
sharpened); #250 Windows CRLF; a suspected perf regression INVESTIGATED + cleared (CI-vs-CI conformance 119s→107s, flat —
local swing was cold-cache; [[perf-regression-watch]] recorded). **★★ AWAITING OPERATOR/DESIGN — 2 questions (see
escalations.md + design-questions.md DQ27/DQ28):** (1) **Q-method-collision-inherent-trait** — an inherent method + a
same-named trait method (`impl Component for Button { fn render = self.render() }`) → infinite recursion on overload-less
targets (js/ts) AND in the reference interpreter; blocks react-components js/ts. (2) **Q-go-method-generics** — Go forbids
type params on methods (`Box[T].map[U]`); needs a monomorphization/free-fn decision; blocks type-zoo go. NEW FOUND→queue:
Q-go-chained-combinator-typing, Q-nested-compose-jstsgo (compose `f>>g>>h` mis-lowers on js/ts/go), Q-interp-method-collision.
Baseline ratcheted to 84. ↓ —
PRIOR: Last reconciled 2026-06-04 21:51 — main 5e4d6c3. ★ RESIDUAL PER-BACKEND
FAN-OUT LANDED (#233 go · #234 ts · #235 py · #236 rust) — **8 FOUND codegen bugs cleared** across the long-pole targets
(go `**`/pow, `.map` element typing, value-position bind/plain-record/nested-Optional match; ts match-narrowing; py
matcharm-lambda + plain-record; rust str-literal match). 4 file-disjoint sessions, combined-state verified (conformance
REQUIRE=all, 0 failed) + per-PR CI gated. **Examples matrix js 16 · ts 11→12 · py 13→14 · rust 10→11 · go 9→10 / 20 — 59→63
runtime-working (49→63 across this session).** Baseline ratcheted to 63 (this PR). INCIDENT: #235 flaked every CI lane but
ubuntu-stable — a shared fixed temp path in `check_py_syntax` raced under parallel `cargo test`; hotfixed (unique per-call
path) → all lanes green. NEW FOUND→queue: Q-examples-ts-tsc-gate (audit strip-types ≠ `tsc`), Q-py-valpos-stmt-arms,
Q-rust-str-mixed-binding (LOW). **No remaining examples blocker is a shared-architecture gap** — what's left is per-backend
residue + LOW Q-propagate-exprpos-shared + Q-conformance-target-race (test harness). ↓ —
PRIOR: Last reconciled 2026-06-04 19:32 — main 99f21ae. ★★ SHARED-LOWERING
PHASE COMPLETE ★★ #231 landed **Q-list-range-pattern-shared** (the last shared item) — `pattern_needs_ifchain` recognizes
`ListPat`/`RangePat`; ts/go gained list/range binding; pattern-lab ts FAIL→PASS. **Examples matrix now js 16 · ts 9→11 · py
13 · rust 10 · go 9 / 20 — 57→59 runtime-working (49→59 across this whole session).** This completes the shared-lowering
core (#224 exprpos + #226–#229 guard-let/let-shadow/propagate + #231 list/range). **NEXT = Q-examples-baseline-ratchet** (lock
the 59/100 floor à la #221) + a fan-out over the residual per-backend FOUND bugs: Q-ts-match-narrowing, Q-go-pow-operator,
Q-go-list-method-typing, Q-py-matcharm-lambda-binding, Q-plainrecord-valpos-match, Q-go-valpos-bind-match,
Q-go-nested-optional-match, Q-rust-str-literal-match (+ LOW Q-propagate-exprpos-shared). ↓ —
PRIOR: Last reconciled 2026-06-04 17:30 — main fdb16d9. ★ PER-BACKEND
FAN-OUT LANDED (#226 js · #227 ts · #228 py · #229 go) — 4 file-disjoint sessions, each owning ONE emitter (generator.rs /
bock-air / bock-types untouched in all); combined-state verified locally (conformance REQUIRE=all, 0 failed) BEFORE merge,
then re-confirmed on merged main (0 failed). Cleared **Q-guard-let-shared + Q-let-shadow-const + Q-propagate-operator-noop**
across js/ts/py/go (rust was already done for the first two). **Examples matrix: js 16 · ts 7→9 · py 12→13 · rust 10 · go
8→9 / 20 — 53→57 runtime-working (49→57 across this whole session).** FOUND→queue: Q-propagate-exprpos-shared (nested `?` —
js/ts/go all converged), Q-ts-match-narrowing (task-api ts), Q-go-pow-operator + Q-go-list-method-typing (type-zoo/todo-list
go), Q-py-matcharm-lambda-binding (pattern-lab py). NEXT: Q-list-range-pattern-shared (the shared generator.rs recogniser) +
Q-examples-baseline-ratchet. ↓ —
PRIOR: Last reconciled 2026-06-04 15:55 — main f5543bc. #224 LANDED:
**Q-exprpos-shared-desugar DONE** — the shared match-exprpos core (value-position diverging control-flow), implemented as a
codegen pre-pass `hoist_value_cf` (NOT an AIR desugar — the temp's type is only derivable at codegen; go infers it
structurally). Examples **js 14→16 · ts 7 · py 12 · rust 9→10 · go 7→8**; chat-protocol now runs js+go; conformance 548/0; 0
regressions; all 13 CI checks green incl. windows. **With the shared core landed, the remaining shared-lowering items are
parallelizable by backend again — NEXT = a per-backend fan-out:** Q-guard-let-shared (js/ts/py/go) · Q-let-shadow-const
(ts/py/go) · Q-list-range-pattern-shared (generator match_needs_ifchain + per-backend) · Q-propagate-operator-noop (js/ts/py;
may route to Design on `?` semantics). Plus chores: Q-examples-baseline-ratchet + FOUND follow-ups (Q-conformance-target-race,
Q-chat-protocol-residual). ↓ —
PRIOR: SESSION-END PAUSE (2026-06-03 23:25) — main e1e776d. Next was the shared-lowering phase, Q-exprpos-shared-desugar (now done). ↓ —
Last reconciled: 2026-06-03 23:05 — **MS-examples-hardening: 17 PRs landed (#204–#221).** main e2117ee. Latest: a
**5-WAY PARALLEL FAN-OUT — one cluster-batch per backend (#216 rust · #217 js · #218 py · #219 ts · #220 go), all
file-disjoint, generator.rs untouched in every one.** Combined-state conformance **0 failed across 124 fixtures**
(REQUIRE=all, verified on merged main). **Examples matrix LEAPT: runtime-working js 7→14 · ts 5→7 · py 9→12 · rust 8→9 ·
go 1→7 / 20** (30→49 example-target passes; baseline ratcheted #221). go's all-5 bet is paying off (1→7). **Done this
batch:** Q-js-effect-export, Q-py-circular-import, Q-py-windows-utf8, the rust ownership clusters (#216), the go
Result-payload/Char/int-width/unused-var (#220), per-backend match-exprpos emitter work. **THE FAN-OUT SCOPED THE
REMAINING SHARED WORK** (all backends converged on it): **Q-exprpos-shared-desugar** (HIGH — the real match-exprpos core;
value-position diverging control-flow needs a SHARED AIR temp-hoist; go-blocking; NON-parallel) · **Q-propagate-operator-noop**
(HIGH — `?` is a no-op on js/ts/py, drops the unwrap; maybe Design) · Q-list-range-pattern-shared · Q-guard-let-shared
(js/ts/py/go; rust done) · Q-let-shadow-const (ts/py/go; js done). **NEXT focused phase = the shared-lowering session**
(generator.rs/AIR — NOT parallelizable). 0 regressions across the workstream. — Earlier
2026-06-03 13:44: **EXAMPLES-EXEC AUDIT COMPLETE + operator decisions** (see audit.md 2026-06-03 13:44). The full 20×5 audit (built in /tmp, project mode) gives the TRUE matrix: js 10/20 compile·2/10 run,
ts 2/20·2/2, py 15/20·7/15, **rust 3/20·2/3 (in-repo 0/20 — workspace bug masks), go 1/20·1/1** — hello-world the only
all-5. Worse than the digest's 6-example sample, and **rust/go fail on REAL codegen, not just the env bug** (proven:
fizzbuzz-rust passes in /tmp, fails in-repo). **~9 evidence-confirmed root-cause clusters** filed below:
Q-list-method-codegen (A, HIGH, all 5 — receiver dup'd as first arg), Q-list-concat-codegen (B), Q-const-enum-naming
(C), Q-match-exprpos (D — UN-DEFERRED, broadened; subsumes the now-diagnosed Q-chat-protocol-allfail),
Q-go-enum-return-boxing (E), Q-rust-move-codegen (F), Q-rust-string-num-methods (G), Q-js-effect-export (J),
Q-py-circular-import (K), Q-examples-codegen-misc (minor); plus Q-rust-cargo-workspace (L, masking-only) +
Q-examples-exec-coverage (M, the gate). **OPERATOR DECIDED:** v1.0 = **leverage-order, ALL 5 targets at the
'examples green' bar** (not tiered; go/rust long poles accepted); gate = **informational-first → blocking**. → see
MS-examples-hardening. gitignore cleanup → **PR #202** (merging). NEXT: fix A first (engineer session) + build the
informational gate (parallel, disjoint files). — Earlier 2026-06-03: (**★ ItemB COMPLETE — MS-projectmode DONE (S0–S8) ★** — per-module native
output on all 5 [DV13]; project mode real [scaffolder-owned manifests/configs/README + transpiled @test files per
framework], source mode bare [DV18]; config tables parsed; core.error fixed ×5 [#193]. 430 exec pairs / 0 failed
REQUIRE=all. ItemB (the ProjectMode milestone) complete + js/ts/python CI-certified [#196] + rust/go formatter-clean
[#198]. **⚠ BUT (FOUND 2026-06-03): an examples-compile audit shows the conformance fixtures are TOO NARROW — the
real-world examples largely DON'T compile in project mode (ts 0/6, rust 0/6, go 0/6; js/py "OK" = syntax-only).**
Root causes: **Q-list-method-codegen** (List `.map()`-with-closure mislowered, all 5, §20.4) · **Q-rust-cargo-workspace**
· chat-protocol. Meta gap: **Q-examples-exec-coverage** (examples not exec-tested ×5). **v1.0 is further out than
the green-conformance picture implied** — an "examples-hardening" workstream is needed before release. **OPERATOR
DIRECTION PENDING** (recommended: examples-exec audit first → fix clusters). Release actions [escalate]; ItemD unblocked
but escalates. Q-formatter-clean-tree: rust/go DONE [#198], js/ts/python deferred. Plan: `plans/2026-06-02-itemB-per-module-projectmode-plan.md`. Quality-sweep Wave 1 also landed: **Q-conformance-clean-rebuild + Q-time-int64
[#175]**; **Q-r2-codegen-residue (c) builtin-vs-user-method shadowing [#176, ×5]** + pinned Q-go-list-literal /
Q-r2-(b) / Q-ts-generic-impl (verified already-fixed). New FOUND triaged: Q-allcaps-record-parse (parser),
Q-arch-doc-drift (ARCHITECTURE.md/compiler-CLAUDE.md/CONTRIBUTING.md crate-name drift). Q-match-exprpos still
deferred (deep). — earlier: D4 [#172]; ★ v1 STDLIB COMPLETE 11/11 ×5 ★. #123-#176 merged; repo wins). See audit.md._

---

## Ready

- **[Q-vscode-langclient-v10] migrate VS Code extension to vscode-languageclient v10 API** — chore/bug · **DONE (#290)** ·
  `extensions/vscode/` (`tsconfig.json`, `test/tsconfig.json`, `package.json`, lockfile) · — · links #285, #290 · note: **DONE
  2026-06-08 (#290).** Root cause was NOT the imports — v10 added an `exports` map and the extension's `module:commonjs` tsconfig
  defaulted to `node10` resolution (ignores `exports`), so `vscode-languageclient/node` stopped resolving (5× `TS2307`). Fix:
  `module: preserve` (⇒ `moduleResolution: bundler`) in `tsconfig.json` + a `ts-node` `commonjs` override in `test/tsconfig.json`;
  **no `.ts` source changed**. Bumped the dep to ^10; `npm run compile`/`lint`/`test` clean; the `vscode extension` CI job passes;
  **dependabot #285 auto-closed**. ⚠ **USER-FACING:** required `engines.vscode` ^1.75→^1.91 (v10's floor, VS Code Jun-2024) —
  a modest, well-justified minimum-version bump. ORIG FOUND 2026-06-08 (#285's blocker).
- **[Q-xmod-bounds-codegen] ts/go don't re-emit the generic-param trait constraint for an IMPORTED generic fn** — bug · **DONE (#291)** ·
  `compiler/crates/bock-codegen/` (generator.rs fold helper + ts/go emitters) · — · links #286, #291, Q-xmod-bounds, §4.6/§6.5 ·
  note: **DONE 2026-06-08 (#291).** New `merge_where_bounds_into_generics` helper folds a `where`-clause bound onto the generic
  param at the `FnDecl` emission site, so ts emits `<T extends Ranked>` and go `[T Ranked[T]]`; `xmod_where_bound_dispatch` now
  runs on all 5. **FOUND broader than #286's note:** the bound was dropped for LOCALLY-defined `where (T: Ranked)` fns too —
  inline `[T: Ranked]` worked (lands on `GenericParam.bounds`) but `where`-clause bounds land in a separate field the ts/go
  renderers never read; the one helper fixes both local + imported. ORIG: OPEN from #286 (checker enforces the bound on all 5;
  only ts/go codegen dropped it).
- **[Q-fmt-doccomment-indent] `bock fmt` flattens doc-comment prose indentation** — bug · **DONE (#304)** · LOW ·
  `compiler/crates/bock-fmt/src/emit.rs` · — · links #287, #304 · note: **DONE 2026-06-09 (#304).** Fixed via a ZERO-RIPPLE
  seam entirely inside bock-fmt — `format_module` re-derives each `//!` line's content from the RAW comment stream
  (`comments.rs` already extracts it verbatim), stripping only the marker + ≤1 space and trimming trailing ws, so indentation
  is preserved; **the lexer was not touched** (parser/`bock doc`/LSP unaffected — the feared ripple avoided). `///` item docs
  already preserved indentation (only `//!` was broken). 4 new round-trip+idempotence tests; stdlib-fmt-check stays clean. ORIG:
  FOUND 2026-06-08 (#287) — root cause was the lexer's per-line `.trim()`, but the fix didn't need to change it.
- **[Q-bockfmt-cfarm-comma] `bock fmt` appends an illegal trailing comma after a control-flow match arm** — bug · **DONE (#287)** ·
  `compiler/crates/bock-fmt/` · — · links #272, #287, Q-stdlib-fmtcheck · note: **DONE 2026-06-08 (#287)** — value-less
  `break`/`continue`/`return` arm bodies no longer emit a trailing comma (value-bearing forms like `return f(x),` correctly
  keep it); `iter.bock` now folds into the stdlib-fmt gate. ORIG FOUND 2026-06-07 (#272). `bock fmt` rewrote a
  control-flow match-arm body like `None => break` to `None => break,`; the parser then rejects the formatted file (`E2020
  expected expression, found ','`). Caught by the #272 stdlib-fmt behavior-equivalence check (it mangled `iter.bock`). Blocks
  folding `iter.bock` into the `stdlib-fmt` gate. Suppress the trailing comma when an arm body is a control-flow statement
  (`break`/`continue`/`return`/loop tail).
- **[Q-bockfmt-utf8-panic] `bock fmt` panics on long multi-byte (UTF-8) comment lines** — bug · **DONE (#287)** ·
  `compiler/crates/bock-fmt/src/emit.rs` (`find_break_point`/`wrap_long_lines`) · — · links #272, #287, Q-stdlib-fmtcheck ·
  note: **DONE 2026-06-08 (#287)** — line-wrap now snaps to a char boundary via a `floor_char_boundary` polyfill (MSRV 1.82
  predates std's method); `collections.bock` now folds into the stdlib-fmt gate. ORIG FOUND 2026-06-07 (#272). A box-drawing
  divider comment (81 chars / 200+ bytes) panicked the formatter — `end byte index
  100 is not a char boundary` — the line-wrap slices at a byte offset that lands inside a multi-byte char. Blocks folding
  `collections.bock` into the `stdlib-fmt` gate. Slice on char boundaries (char indices / `floor_char_boundary`).
- **[Q-blanket-into-codegen] derived blanket `.into()` is unexecutable on compiled targets (JS confirmed)** — bug · **DONE (#288)** ·
  `compiler/crates/bock-codegen/` + `compiler/crates/bock-air/src/lower.rs` · — · links #273, #288, Q-interp-enum,
  Q-xmod-impl, Q-prim-assoc · note: **DONE 2026-06-08 (#288)** — a `.into()` resolving to a derived blanket is rewritten to
  `Target.from(self)` in a **post-typecheck codegen pre-pass** (`generator.rs`, NOT the lowerer — a pre-typecheck rewrite
  clobbered the `E4012` unrelated-target diagnostic); exec-verified ×5 (js/ts/py/rust/go). En route it FOUND+fixed user-type
  associated-fn codegen (`Type.from`/`Type.origin`), broken on all 5 targets — the user-type half of Q-prim-assoc. ORIG FOUND
  2026-06-07 (#273): the bodyless blanket lowered `m.into()` to `m.into(m)` on JS but only `Source.prototype.from` was defined →
  `m.into is not a function`; codegen/AIR gap, not interpreter-only. Pairs with Q-xmod-impl (cross-module `.into()` resolution).
- **[Q-list-mutation-dq18] List `push`/`append` mutation + Map `contains` reject** — impl/design · **DONE (#269 — DQ18 + DQ22)** ·
  `compiler/crates/bock-types`, `compiler/crates/bock-codegen`, `spec §18.3`, `docs/.../core-collections.md` · links DQ18, DQ22,
  #269 · note: DONE 2026-06-06. `push`/`append` → `mut self` Void mutators (mut-receiver enforced, `E5004`); codegen ×5 (rust/js/ts
  `.push`, py `.append`, go `recv = append(recv, x)`). Map `contains` rejected (`E4013` → `contains_key`); `contains` stays
  Set-only. Spec §18.3 + changelog. `pop`/`insert`/`remove`/`reverse` left value-returning → Q-list-mut-pop-insert-remove.
- **[Q-checker-unknown-method-concrete] unknown method on a concrete type → checker error, not fresh-var** — bug · **DONE (#274)** ·
  `compiler/crates/bock-types/src/checker.rs` · — · links DQ22, #269, #274 · note: **DONE 2026-06-07 (#274).** An unknown
  method on a concrete receiver now errors **E4013** + nearest-name (Levenshtein) suggestion instead of resolving to a fresh
  var. Gated to closed-method-set receivers (primitives, built-in List/Map/Set, Optional/Result, in-scope user records/classes)
  via `method_is_resolvable` (intrinsics + canonical primitive trait conformances + user inherent/trait impls + record
  field-closures + conversion hooks + inherited trait defaults); §4.9 `Flexible`/sketch + `TypeVar`/`Error`/out-of-scope types
  EXEMPT. The fix surfaced + closed a trait-default-method false-positive (`Eq::not_equals` inherited by a concrete type).
  Verified: full conformance REQUIRE=all 0 failed + examples-exec 100/100 non-red. ORIG: FOUND 2026-06-06 (DQ22) — the general
  form of the DQ22 Map-`contains` trap.
- **[Q-list-operator-gating-user-types] §18.5 operator-gating for user types not wired** — bug · **DONE (#296)** ·
  `compiler/crates/bock-types/` · — · links DQ10, §18.5, #296, Q-user-comparison-codegen, Q-equatable-gating-user-types · note:
  **DONE 2026-06-08 (#296)** — `<`/`>`/`<=`/`>=` now require `impl Comparable` on a user (Named) operand (**E4005** + suggestion
  when absent; accepted when present) via a `require_comparable_operand` probe in `infer_binop`; also enforces §18.5's "Bool is
  not Comparable" (`true < false` now errors). Conservative — bounded generics (`T: Comparable`), inference/Flexible/Error
  skipped; no false-positives. No stdlib/example impls needed (well-written code already had them); examples 20/20 ×5, no
  regressions. `==`/`!=` (Equatable) gating deferred → Q-equatable-gating-user-types. **FOUND:** user-type comparison *lowering*
  is broken ×5 → Q-user-comparison-codegen (a `.skip` exec fixture is parked). ORIG: FOUND 2026-06-06 (DQ10 ratification, flagged
  out-of-scope by Design); §18.5's rule (implementing the trait gates the operator) worked for primitives only.
- **[Q-user-comparison-codegen] user-type `<`/`>`/`<=`/`>=` lowering emits native operators (broken ×5)** — bug · **DONE (#299)** ·
  `compiler/crates/bock-codegen/` + `compiler/crates/bock-types/` · — · links #296, #299, Q-list-operator-gating-user-types,
  Q-rust-enum-variant-import, §18.5 · note: **DONE 2026-06-08 (#299).** New `USER_COMPARE_META_KEY` checker stamp (on an ordering
  `BinaryOp` whose operands are a user `Comparable` type — comparison arm only) + per-backend lowering routing through
  `compare()` (`<`⇒`==Less`, `>`⇒`==Greater`, `<=`⇒`!=Greater`, `>=`⇒`!=Less`), reusing each target's `Ordering` rep. Parked
  fixture flipped (`opgate_comparison_user_type_impl`) + 2 new; conformance 814/0 ×5. Primitives/`T: Comparable`/`==`/`!=`
  untouched. **FOUND** → Q-rust-enum-variant-import (rust `use core.compare.{Less,Equal,Greater}` lowered to a non-existent free
  import). ORIG: FOUND 2026-06-08 (#296) — the codegen half of the operator-gating story.
- **[Q-rust-enum-variant-import] rust import lowering emits `use crate::…::{Variant}` for enum variants (E0432)** — bug · **DONE (#303)** · LOW ·
  `compiler/crates/bock-codegen/src/rs.rs` (`emit_cross_module_uses`) · — · links #299, #303, Q-py-enum-variant-import · note:
  **DONE 2026-06-09 (#303).** A braced named import resolving to a registered enum variant (`self.enum_variants`) is now replaced
  by its enum TYPE under the same module path (`use crate::core::compare::{Comparable, Ordering};` instead of the E0432 `{Less,
  Equal, Greater}`); rust reaches variants as `Ordering::Less`. New `enumvarimport_braced_variants` fixture builds+runs (js/ts/
  rust/go). **FOUND** → Q-py-enum-variant-import (Python has the SAME class of bug). ORIG: FOUND 2026-06-08 (#299).
- **[Q-py-enum-variant-import] python import lowering emits `from … import <Variant>` but the variant is class `Enum_Variant`** — bug · **DONE (#306)** · LOW ·
  `compiler/crates/bock-codegen/src/py.rs` (import lowering) · — · links #303, #306 · note: **DONE 2026-06-09 (#306).** The
  `ImportItems::Named` arm now filters out **unaliased** braced leaf names that resolve to a registered user enum variant
  (`user_variant_for_name`, which excludes built-in Optional/Result) before rendering `from {module} import …`; the variant is
  reached at its use site as the `{Enum}_{Variant}` dataclass (`Ordering_Less`), which the implicit-import pass pulls in — exactly
  mirroring the js/ts `Named` filter and the #303 rust fix. The enum TYPE `Ordering` (a real module-level `Union` alias) and all
  non-variant leaves are kept; an *aliased* variant (`{Less as L}`) is left untouched (separate, unexercised). A list filtered
  to empty emits nothing (only a genuinely-empty `{}` keeps the bare `import {module}`). `python` re-added to the
  `enumvarimport_braced_variants` fixture targets → now green ×5 (conformance 824/0/0). ORIG: FOUND 2026-06-09 (#303). No
  follow-ups. **NB:** mirror complete — all of js/ts/python/rust now drop braced enum-variant items; go was never affected
  (package-level types, no import).
- **[Q-rust-host-sleep-tokio-dep] rust no-handler host `sleep` needs a tokio scaffold dep** — bug · **DONE (#301)** ·
  `compiler/crates/bock-codegen/src/scaffold.rs` · — · links #297, #301, Q-clock-handler-routing, Q-time-shim-path · note: **DONE
  2026-06-08 (#301).** The rust scaffold's tokio trigger keyed only on `bock_runtime.rs` presence, so the host-sleep crate (which
  emits `tokio::time::sleep` + `#[tokio::main]` into `main.rs` but no runtime file) got no `tokio` dep → `E0433`. Broadened to a
  CONTENT scan of emitted `.rs` for `tokio::` (`rust_emits_tokio`) — one check covers both the concurrency runtime and host-sleep;
  programs using neither stay dep-free. Features `["rt-multi-thread","macros","sync","time"]`, pinned `"1"`. New
  `hostsleep_no_handler` fixture runs ×5; conformance 819/0. NOTE: the scaffold lives in **bock-codegen** (not bock-build as
  originally filed). ORIG: FOUND 2026-06-08 (#297).
- **[Q-equatable-gating-user-types] gate `==`/`!=` on user types behind Equatable** — bug · **blocked · escalated → Design (DQ29)** · LOW ·
  `compiler/crates/bock-types/` · — · blocked-by DQ29 · links #296, #300, DQ29, §18.5, Q-list-operator-gating-user-types · note:
  **ESCALATED 2026-06-08 (DQ29).** The wave-6 investigation (PR #300, doc-only — NOT merged) confirmed scenario (B): records/enums
  have FREE structural `==` at codegen but NO checker-visible `Equatable` conformance (only primitives are registered), and
  `@derive` is v1.x-reserved — so a strict `require_equatable_operand` gate would reject idiomatic `record == record` with no v1
  escape. That's a design decision, not impl-completeness → **DQ29** (candidate resolutions R1 auto-conform / R2 defer-to-derive /
  R3 strict-reject[rejected]). Un-block + implement once Design rules. Same `infer_binop` mechanism as #296 once ruled.
- **[Q-list-mut-pop-insert-remove] `pop`/`insert`/`remove`/`reverse` mutating-method semantics** — impl/design · **blocked · escalated → Design (DQ30)** ·
  `compiler/crates/bock-types`, `compiler/crates/bock-codegen` · — · blocked-by DQ30 · links DQ18, DQ30, #269 · note:
  **ESCALATED 2026-06-09 (DQ30).** DQ18 ruled `push`/`append` (`mut self` Void); these four were left value-returning
  (checker.rs:4607-4620 still type `pop`/`insert`/`remove`/`reverse` as the placeholder receiver `List[T]`). Applying the
  `mut self` model needs the RETURN CONTRACT decided first, and §18.3 is **silent** on it — the contested axes (`remove` by-index
  return type [`Optional[T]` vs `T`], out-of-bounds behavior, `pop`-on-empty) are a Design call, not impl-completeness (CLAUDE.md:
  "undecided behavior → Design"). Surfaced to the owner 2026-06-09; owner deferred ("will circle back with the design decision").
  Un-block + implement once DQ30 rules; the codegen is then a direct extension of the DQ18 mut-self lowering table ×5. ORIG: FOUND
  2026-06-06 (#269).
- **[Q-py-collections-builtin-shadow] type-zoo python locals named `list`/`map`/`set` shadow builtins** — bug · **DONE (#262 — py codegen renames builtin-shadowing `let`s to `list__bN`)** ·
  `examples/spec-exercisers/type-zoo/` + `compiler/crates/bock-codegen/src/py.rs` · — · links #259 · note: FOUND 2026-06-05,
  surfaced (not caused) by #259 — the py statement-`match` fix de-masked type-zoo py, which then hits `keys = list(map.keys())`
  → `TypeError: 'list' object is not callable` because the example binds locals `list`/`map`/`set`. Rename in the example, or
  guard builtin-shadowing in py codegen for collection lowering. Blocks type-zoo py (run-FAIL).
- **[Q-checker-method-generic-call-infer] checker can't infer a method's own type param at a call (`b.map(dbl)` for `Box[T].map[U]`)** — bug · **DONE (#292)** ·
  `compiler/crates/bock-types/` · — · links #256, DQ28, #292 · note: **DONE 2026-06-08 (#292).** A new `method_generic_params`
  map (type → method → param names, populated in `collect_sig`) + a shared `freshen_method_type_params` helper substitutes the
  method's own params with fresh inference vars at both method-resolution paths (the `Call(FieldAccess)` desugar and the
  FieldAccess-callee inference for `Named`/`Generic` receivers); the receiver still pins the type's own params (`T`), only the
  method's own (`U`) are freshened. `b.map(dbl)` (`U=Int`) and `b.map(to_str)` (`U=String`) check AND **execute ×5** — checker-only,
  no codegen gap. ORIG FOUND 2026-06-05 (#256): the call failed `U` inference on all targets, so type-zoo only declared `Box.map`.
- **[Q-calculator-ts-eval] calculator ts emits `TS1215: Invalid use of 'eval'`** — bug · **DONE (#262 — ts.rs `ts_value_ident` escapes `eval`/`arguments`)** · LOW ·
  `compiler/crates/bock-codegen/src/ts.rs` · — · links #260 · note: FOUND 2026-06-05 (honest audit). Pre-existing (not a
  regression): `calculator` fails `bock build -t ts` with TS1215. Blocks calculator ts (build FAIL). Low (one example/target).
- **[Q-systems-allocator-go-build] systems-allocator go build error** — bug · **DONE (#262 — go.rs `obj.field` type inference sizes `.map` element type)** ·
  `compiler/crates/bock-codegen/src/go.rs` · — · links examples-exec · note: FOUND 2026-06-05 (honest audit). systems-allocator
  fails `bock build -t go` (build FAIL) while passing js/ts/py/rust. Investigate + fix the go codegen gap. Blocks
  systems-allocator go.
- **[Q-int-div-semantics] Normative Int/Int division (§3.6) + Bool interpolation spelling** — impl · **DONE (#264 — Option A truncating-toward-zero)** ·
  `compiler/crates/bock-types/src/checker.rs` (`int_arith` + `bool_stringify` `BinaryOp` stamps) + `compiler/crates/bock-codegen/` ·
  links DQ23, #264 · note: **DONE 2026-06-06 (DQ23 ruled Option A).** Checker `int_arith` stamp (both operands integer) +
  `bool_stringify` stamp; js/ts/py division+modulo arms emit toward-zero truncation, dividend-sign modulo, and a zero-divisor
  abort; rust/go already conformant (no change). Bool interpolation/`to_string` → lowercase `true`/`false`. Spec §3.6/§3.5 +
  changelog. Acceptance fixtures green ×5: negative operands (div+mod), zero-divisor abort, large-int precision (py/rust/go —
  js/ts `Int` is IEEE `number`, a representation ceiling orthogonal to DQ23), Bool spelling. ORIG: `17/5` → `3` on rust/go vs `3.4` on js/ts/py — a cross-target
  divergence. Read-only probe confirmed operand type is NOT available at the codegen `/` site (checker side-table dropped); a
  checker stamp is the prerequisite, but it mirrors the existing `list_concat`/`string_concat` stamps (cheap). **Option A
  (truncating-Int):** js/ts emit `Math.trunc(a/b)`, py `math.trunc` (toward-zero, NOT `//` floor) gated on the stamp; result
  type stays Int. **Option B (always-Float):** change `infer_binop` Div result→Float (ripples through inference, breaks
  `let n: Int = a/b`, shifts `.expected`). One engineer session once DQ23 is ruled. Bundle the Bool-interpolation spelling
  (py `True`/`False`→`true`/`false` — same stamp-in-py-interpolation shape).
- **[Q-todo-guessing-game-disposition] todo() semantics + guessing-game stub-showcase recat** — design/chore · **DONE (this PR)** ·
  `spec/bock-spec.md §18.2` + `spec/changelogs/` + `tools/scripts/examples-exec-audit.sh` + `tools/examples-exec-baseline.txt` ·
  links Design ruling 2026-06-05 · note: Design ruled `todo()` = Never-typed, Panic-effect abort, optional message (§18.2
  normative + changelog `20260605-todo-semantics.md`); `guessing-game` = compile-verified stub showcase (its `todo()` stubs
  need v1.x RNG/stdin), recategorized in the audit as **STUB** (non-red). Examples now 95/100 run + 5 stub = 100/100 non-red.
- **[Q-import-reject] Reject bare module-qualified import** — bug · **DONE (#274)** ·
  `compiler/crates/bock-types/` · — · links DQ8, #274 · note: **DONE 2026-06-07 (#274).** Bare `use core.error`
  (`ImportItems::Module`, neither brace-list nor wildcard) was silently skipped; `check_module` now rejects it with **E4014**
  pointing at the braced form `use core.error.{ ... }`. Braced/wildcard imports unaffected; spec §12.2 already mandated the
  rejection (no spec edit). Decided by DQ8; module-qualified access deferred to v1.x.
- **[Q-interp-enum] interpreter execution gaps for stdlib dispatch** — bug ·
  **DONE 2/3 (#273)** · interpreter crate · — · links #104, #110, #121, #273, Q-blanket-into-codegen · note: **DONE 2026-06-07
  (#273), 2 of 3 residual gaps closed:** user associated-fn dispatch (`Target.from(source)` — was "undefined variable") +
  user-impl `to_string` shadowing the universal builtin (test-harness matcher names reserved so `expect()` keeps builtin
  dispatch). The 3rd — the bodyless blanket `.into()` — was split out: it's a cross-cutting codegen/AIR gap (JS target crashes
  too), not interpreter-only → **Q-blanket-into-codegen**. ORIG: #121 (defect #5) closed the #104 `Ordering.Less` case
  (globals-bearing method-body env).
- **[Q-self-subst] checker: `Self` not substituted in impl method sigs** — bug ·
  **DONE (verified already-fixed #141; re-confirmed #274)** · `compiler/crates/bock-types/` · — · links #141, #274 · note:
  **VERIFIED 2026-06-07 (#274) — already resolved by #141.** `Self`→target substitution happens at impl-sig registration;
  `a.combine(b)` / `fn compare(self, other: Self)` check clean, covered by existing exec fixtures (`self_return`,
  `self_in_plain_impl`, `trait_self_typing`). No change needed. ORIG found #104.
- **[Q-xmod-bounds] Cross-module where-bound enforcement** — bug · **DONE (#286)** ·
  `compiler/crates/bock-types/` (export ABI) · — · links #108, #286, Q-xmod-bounds-codegen · note: **DONE 2026-06-08 (#286).**
  A generic fn's where-bounds are now encoded into its exported `TypeRef` (keyed by type-var id), decoded in `seed_imports`, and
  reconstructed into `FnSig.where_clause` so `check_trait_bounds_at_call` enforces an imported bound exactly like a local one
  (`ExportedSymbol`/`ExportDetail` live in bock-air → threaded via the existing TypeRef string channel, not new fields).
  RESIDUAL → Q-xmod-bounds-codegen (ts/go don't re-emit the constraint for an imported generic fn). ORIG: bounds on imported
  generic fns were dropped; locally-defined bounds enforce (#108). Paired with Q-xmod-impl (DV7/DV8 theme).
- **[Q-xmod-impl] Cross-module trait-impl resolution for `.into()`** — bug ·
  **DONE (#286)** · `compiler/crates/bock-types/` · — · links #110, DV8, #286, Q-blanket-into-codegen · note: **DONE 2026-06-08
  (#286).** User trait-impls over `Named` targets are now exported as synthetic `__bock_impl__` marker symbols; `seed_imports`
  scans every imported module for them (coherence is module-scoped, not name-gated) and `check_module` folds them into the
  impl-table (local+canonical first, local wins) + re-runs blanket-`Into` synthesis — so an `impl From[A] for B` in module X is
  visible to `.into()` in module Y at CHECK time. Canonical/primitive-target impls excluded. The CODEGEN/runtime side is
  Q-blanket-into-codegen (#288). ORIG: the impl-table wasn't seeded across modules. Paired with Q-xmod-bounds.
- **[Q-prim-assoc] Primitive associated calls (`Float.from(3)`)** — bug · **DONE (#294 — primitive half; user-type half #288)** ·
  `compiler/crates/bock-types/` + `compiler/crates/bock-codegen/` (all 5) · — · links #110, #274, #288, #294 · note: **DONE
  2026-06-08 (#294).** Primitive associated conversions now check + execute ×5: `Float.from(Int|Float32)`, `Int.from(<sized
  signed>)`, `String.from(Char)`, `Int.try_from(String)`/`Float.try_from(String)` → `Result[_, ConvertError]` — the
  already-registered `register_canonical_conversions` matrix (NO new semantics; lossy/narrowing still `E4012`). Coupled checker
  resolution + per-target lowering (py `float(..)`/`int(..)`, not `.from`; rust `f64::from`/`i64::try_from`; go native casts).
  FOUND+fixed a pre-existing Rust bug: `core.convert`'s `From`/`TryFrom` trait decls emitted associated methods with a spurious
  `&self` (`E0186`) → any Rust program importing `core.convert` was uncompilable (now omits the receiver + `where Self: Sized`).
  **OPEN §18.3** (normative primitive-conversion *matrix* ratification) is the EXISTING Design item (design-questions.md,
  parallels DQ10) — #294 implemented the floor, did not ratify/extend. ORIG history ↓ — **UPDATE 2026-06-08
  (#288):** the USER-type associated-fn codegen half (`Type.from(x)`/`Type.origin()` — a no-`self` impl method was emitted as an
  instance method, and `Type.method(..)` calls lower/camel-cased the type name into a non-existent value) was FOUND+fixed ×5 in
  #288. The PRIMITIVE half (`Float.from(3)`/`Int.try_from(s)`) REMAINS — still checker+codegen-coupled. **RE-SCOPED 2026-06-07
  (#274).** The checker fix is straightforward, but #274 implemented it and confirmed `Float.from(3)`/`Int.try_from(s)` then
  emit BROKEN codegen on all 5 (`float.from(3)` JS; `from`-keyword Python; no-such-type Rust/Go) — the associated
  primitive-conversion lowering isn't wired in bock-codegen, so enabling the check alone converts a clean `E4002` into garbage
  output (the exact anti-pattern Q-checker-unknown-method-concrete fixed). #274 reverted it. Needs a COUPLED checker+codegen
  change (`Type.from`/`Type.try_from` lowering ×5) in one session. ORIG: the resolver doesn't treat a primitive type name as an
  expression value (`.into()` is the working primitive path).
- **[Q-match-exprpos] Expression-position control-flow lowering — PER-BACKEND done; SHARED value-position desugar remains** —
  impl · **DONE (#218/#219/#220 per-backend emitters + #224 shared core)** · `compiler/crates/bock-codegen/` ·
  — · links #121, #176, #218, #219, #220, MS-examples-hardening, Q-exprpos-shared-desugar · note: the 5-backend fan-out
  (#217–#220) lowered the **tractable** expr-position cases per-backend (ts ValueSink `let r; if{ r=… } else { return }`;
  py statement-form hoist; go value-IIFE + loop_expr_depth) — context-audit now runs on ts/py/go, guessing-game/pattern-lab
  advanced. **BUT all four sessions independently confirmed the genuinely-shared case** (`let x = loop {…}` / a value-position
  match/if whose arms DIVERGE) **needs a SHARED AIR temp-hoist desugar** (it currently emits `/* unsupported */` on the
  backends lacking the per-emitter workaround). That shared desugar is split out → **Q-exprpos-shared-desugar** (the real
  remaining core). This item now tracks only the per-backend emitter work (done); the shared desugar is the next focused
  (NON-parallel, generator.rs/AIR) session. Remaining example barriers routing through it: chat-protocol (early-return
  trapped in value-IIFE on go/ts), inventory map/fold.
- **[Q-exprpos-shared-desugar] Shared temp-hoist desugar for value-position diverging control-flow** — impl · **DONE (#224)** ·
  `compiler/crates/bock-codegen/src/generator.rs` (+ js/ts/py/rs/go.rs) · — · links
  Q-match-exprpos, #217–#220, #224 · note: **DONE (#224, 2026-06-04) — NOT an AIR desugar: implemented as a shared codegen
  pre-pass `hoist_value_cf` (generator.rs), run atop every backend's generate_module/_project, chosen over the S-AIR layer
  because the synthesised temp's type is only derivable at codegen (go infers it structurally from the relocated node).
  Splices a declare-only temp before the consumer, relocates the CF to statement position rewriting value-tails to
  `temp = <v>` and keeping diverging tails verbatim, reads the temp. Covers let/return/assign/call-arg/const/fn-tail.
  Examples js 14→16 · rust 9→10 · go 7→8; chat-protocol runs js+go; conformance 548/0; 0 regressions.** ORIG (FOUND
  2026-06-03, the 5-backend fan-out converged on this): A value-position
  control-flow expression whose arms DIVERGE (`let x = loop { … break v … }`, `let x = match s { A => v  B => return }`)
  has no clean per-backend IIFE lowering — needs a shared temp-hoist desugar (introduce a temp, lower the control-flow as
  statements assigning the temp, replace the expression with the temp) in the AIR/lowering layer so ALL backends emit valid
  code uniformly. The per-backend sessions each did the easy cases + reported this as the shared blocker. Do as ONE focused
  session (conflicts with all backend emitters → not parallelizable). Unblocks the last go/ts/chat-protocol barriers.
- **[Q-examples-baseline-ratchet] Ratchet examples-exec baseline after the #224 gains** — chore · **DONE (this PR — 63/100)** ·
  `tools/examples-exec-baseline.txt` · — · links #221, #224 · note: FOUND 2026-06-04. #224 raised runtime-working js 14→16,
  rust 9→10, go 7→8 (chat-protocol js+go). Re-run `BOCK_EXAMPLES_UPDATE_BASELINE=1 tools/scripts/examples-exec-audit.sh` and
  commit the refreshed baseline (à la #221) to lock the gains as the regression floor; also drops the stale
  `guessing-game/rust` build entry (benign value-less tail-loop `/* unsupported */`, byte-identical on main).
- **[Q-conformance-target-race] Conformance exec test races on shared CARGO_TARGET_DIR (rust fixtures)** — bug · **DONE (#242)** ·
  `compiler/crates/bock-test-harness/` · — · links #224, #242 · note: **DONE 2026-06-04 (#242) — per-process private temp
  target dir (`OnceLock<TempDir>`) for the rust exec path, set on the process env + the `bock build` command; validated 3×
  under default-parallel `cargo test`. Shared-within-process → incremental cache preserved (no cold-rebuild-per-fixture).**
  ORIG: FOUND 2026-06-04 (#224 verify) — concurrent `cargo run` against one CARGO_TARGET_DIR cross-contaminated stdout.
- **[Q-perf-gate-ci] Informational performance-regression CI gate** — chore · **DONE (#248)** · `.github/workflows/`, `tools/` ·
  — · links #248 · note: **DONE 2026-06-05 (#248, operator-requested) — `perf-measure.sh` times build/clippy/conformance-exec,
  `tools/perf-baseline.txt` records the floor, `perf-gate.yml` is informational (`continue-on-error`), ratchet-to-blocking
  documented (mirrors examples-exec.yml). FOLLOW-UP: a criterion micro-benchmark corpus on hot compiler paths (needs a benches
  crate = manifest change) for stable per-op numbers — deferred.**
- **[Q-class-codegen] `class` construction + method dispatch across backends** — impl · **DONE (#249–#252 + #258 — react-components runs on all 5)** ·
  `compiler/crates/bock-codegen/` · blocked-by: Q-method-collision-inherent-trait (js/ts) · links #249, #250, #251, #252,
  react-components · note: **DONE 2026-06-05 — js/ts class literals now `new T(positional)` (#249, js/ts-local `class_fields`
  map, not the shared record set); py attaches class impl/trait methods + base-before-subclass ordering (#250); go exports
  method names (no self-recursive forwarder) + `Fn()->Void`→`func()` (#251); rust capturing-`Fn` alias→`impl Fn` + move clone
  (#252). react-components now passes py/rust/go.** REMAINING: js/ts run-FAIL on the inherent-vs-trait method collision →
  Q-method-collision-inherent-trait (DQ27).
- **[Q-method-collision-inherent-trait] Inherent + same-named trait method → infinite recursion (js/ts; interpreter too)** — design · **DONE (#258 — single-method-namespace; the delegating impl is now an E4012 duplicate)** ·
  `compiler/crates/bock-codegen/` (js/ts) + spec §6.4/traits · blocked-by: DQ27 · links #249, react-components, DQ27,
  escalations 2026-06-05 · note: FOUND 2026-06-05 (#249). `impl Component for Button { fn render = self.render() }` + inherent
  `render` collide on one name on overload-less targets → infinite recursion (reference interpreter also stack-overflows).
  AWAITING Design ruling (recommend: inherent auto-satisfies a same-signature trait requirement). Blocks react-components js/ts.
- **[Q-go-method-generics] Go forbids type params on methods (`Box[T].map[U]`)** — design · **DONE (#256 — go free-fn lowering; residual Q-checker-method-generic-call-infer)** ·
  `compiler/crates/bock-codegen/src/go.rs` · blocked-by: DQ28 · links #220, #246, type-zoo, DQ28, escalations 2026-06-05 ·
  note: FOUND 2026-06-03, confirmed 2026-06-05 the last type-zoo/go blocker. Needs monomorphization or free-fn lowering — a
  design/architecture call. AWAITING Design.
- **[Q-go-chained-combinator-typing] Go `.filter(..).map(..)` chained-combinator element typing** — bug · **DONE (#256)** ·
  `compiler/crates/bock-codegen/src/go.rs` · — · links #246, #251 · note: FOUND 2026-06-05. A `.map` over a `.filter(..)`
  *call* receiver keeps `func(n interface{})` (doesn't recover `[]int64`). The second remaining type-zoo/go blocker
  alongside method-generics. Combinator-receiver element inference.
- **[Q-nested-compose-jstsgo] Nested compose `f >> g >> h` mis-lowers on js/ts/go** — bug · **DONE (#256 go · #257 js/ts — callee-parenthesization)** ·
  `compiler/crates/bock-codegen/` (js/ts/go) + maybe bock-air/lower.rs · — · links #247 · note: FOUND 2026-06-05 (#247 rust
  session). A nested `>>` compose: js emits the closure source as a string; ts produces no output; go uses `interface{}`
  typing in the compose closures. py/rust handle it (py via `emit_callee` parens; rust via `emit_callee_rs`). Shared-desugar
  (lower.rs) × per-backend interaction; mirror the py/rust callee-parenthesization per backend.
- **[Q-interp-method-collision] Reference interpreter stack-overflows on inherent+trait same-name method** — bug · **DONE-by-rejection (#258 — the duplicate is now an E4012 check error, unreachable pre-exec; standalone interp hardening optional)** · LOW ·
  `compiler/crates/bock-interp/` · — · links DQ27, react-components · note: FOUND 2026-06-05 (#249). Independent of the
  codegen DQ27 question — the interpreter itself infinite-recurses on `self.render()` when inherent + trait `render` collide.
  Fix the interpreter's method resolution regardless of the DQ27 ruling.
- **[Q-chat-protocol-residual] chat-protocol still fails ts/python/rust at runtime (unrelated to exprpos)** — bug · **DONE (py #259 stmt-match-return · ts #260 toolchain `.ts`-specifier; rust already-fixed at base — stale)** ·
  `compiler/crates/bock-codegen/` (rust/py/ts) · — · links #224 · note: FOUND 2026-06-04 (#224). After the exprpos desugar
  chat-protocol runs on js+go but still fails the other three for distinct reasons: **rust** `@concurrent`→tokio wiring + an
  `E0507` move in `serialize`; **python** forward-reference ordering (`Serializable` used before defined); **ts**
  `--experimental-strip-types` `.js`-import resolution. Three separable residual codegen gaps; split when picked up.
- **[Q-propagate-operator-noop] The `?`/Propagate operator is a no-op on js/ts/python (drops the unwrap)** — bug · **DONE (#226 js · #227 ts · #228 py · #229 go)** ·
  `compiler/crates/bock-codegen/` (js/ts/py/go) · — · links #219, #226, #227, #228, #229,
  MS-examples-hardening · note: **DONE 2026-06-04 (per-backend fan-out) — lowered `?` to unwrap-or-early-return on all 4
  (js: pre-stmt hoist `const __tryN; if _tag===Err/None return __tryN` then read `._0`; ts: hoist + `return __propN as never`
  typed by the enclosing fn's return container; py: `_bock_try` unwrap + a `try/except _BockPropagate` envelope on the fn;
  go: `emit_try_unwrap` tag-test + zero-value/err early-return). Standard Rust-like semantics — NO Design escalation needed
  (DQ20's deferral resolved by implementation). RESIDUAL → Q-propagate-exprpos-shared (a nested `?` inside a larger
  expression `f(g()?)` has no expression-form early-return; js/ts/go all independently converged on this; no v1 example
  hits it).** ORIG: FOUND 2026-06-03 (#219, ts session). `expr?` (Result/Optional propagation) lowers to
  a no-op on js/ts/python — it does NOT unwrap the payload nor early-return the error, so a `BockResult<T,E>` flows where a
  `T` is expected (type-zoo, task-api remaining errors all trace here). Real semantics bug, not just codegen-shape. Lower
  `?` to each target's unwrap-or-early-return. Verify rust/go too. (DQ20 had deferred `expr?`; this re-opens it as v1.0
  example-blocking — may need a Design check on the exact semantics.)
- **[Q-list-range-pattern-shared] `match` over list/range patterns mis-lowered (shared)** — bug · **DONE (#231)** ·
  `compiler/crates/bock-codegen/src/generator.rs` (+ ts/go/py) · — · links #216, #217, #218, #231, MS-examples-hardening ·
  note: **DONE 2026-06-04 (#231) — `pattern_needs_ifchain` now returns true for `ListPat`/`RangePat` so the shared recogniser
  routes them to the if-chain uniformly. Routing-change risk was contained to ts+go (the only backends that consult
  `match_needs_ifchain`; rust uses native slice/range `match`, py native `case`, js was already `A||A`). ts/go `emit_match_ifchain`
  gained list/range binding (length test + element/`..rest` bind; range `>=lo && <hi` excl / `<=hi` incl per §Range); go
  expr-position `match` now routes through a typed-IIFE if-chain. py value-position ternary path fixed directly. Companion
  fixes the routing surfaced: ts self-binding skip (TS2448), go plain-record field access. pattern-lab ts FAIL→PASS (+1 other
  ts example via the companions: ts 9→11); list/range output verified correct on all 5 via new `list_pat_*`/`range_pat_*`
  fixtures; conformance REQUIRE=all 0 failed. ★ SHARED-LOWERING PHASE COMPLETE.** ORIG: FOUND 2026-06-03 (fan-out).
- **[Q-plainrecord-valpos-match] Plain-record value-position `match` arm doesn't route to the if-chain (py/go)** — bug · **DONE (#233 go · #235 py)** ·
  `compiler/crates/bock-codegen/` (py/go) · — · links #231, Q-match-exprpos, MS-examples-hardening · note: FOUND
  2026-06-04 (#231). A bare-bind record arm (`Point { x, .. } => …`) in value position doesn't take the if-chain path → py
  `get_x` NameError; go `GetX` emits `case interface{}` / undefined `x`. Blocks pattern-lab on py+go. (rust/ts unaffected.)
- **[Q-go-valpos-bind-match] Go value-position bind / string-literal `match` → `case interface{}`** — bug · **DONE (#233)** ·
  `compiler/crates/bock-codegen/src/go.rs` · — · links #231, MS-examples-hardening · note: FOUND 2026-06-04 (#231). Go
  value-position `match` on a bare bind (`EchoBinding`) or string literal (`classify_string`) emits `case interface{}` /
  undefined bind. Distinct from the list/range path (those now route correctly). Blocks pattern-lab on go.
- **[Q-go-nested-optional-match] Go nested-Optional value-position `match` drops nested payload binds** — bug · **DONE (#233)** ·
  `compiler/crates/bock-codegen/src/go.rs` · — · links #231, MS-examples-hardening · note: FOUND 2026-06-04 (#231).
  `match opt { Some(Ok(n)) => … }` — `emit_optional_match_expr` drops the nested payload bind. Blocks pattern-lab on go.
- **[Q-rust-str-literal-match] Rust `String`-vs-`&str` literal `match` → E0308** — bug · **DONE (#236)** ·
  `compiler/crates/bock-codegen/src/rs.rs` · — · links #231, MS-examples-hardening · note: FOUND 2026-06-04 (#231). Matching
  a `String` scrutinee against `&str` literals (`classify_string`) emits an E0308 mismatch (needs `.as_str()` / deref).
  Blocks pattern-lab on rust.
- **[Q-guard-let-shared] `guard (let Pat = expr)` binding dropped on js/ts/python/go** — bug · **DONE (#226 js · #227 ts · #228 py · #229 go; rust #216)** ·
  `compiler/crates/bock-codegen/` (js/ts/py/go) · — · links #216, #226, #227, #228, #229, MS-examples-hardening · note:
  **DONE 2026-06-04 (fan-out) — guard-let binds the pattern payload into the enclosing scope on all 4 (rust was #216 via
  `let-else`; ts/go hoist the scrutinee into `__guardN`, test the tag with a diverging else, bind the payload). js/py: the
  real guessing-game blocker was a value-less tail-position loop falling through to `return /* unsupported */` / `# unsupported`
  — fixed alongside. guessing-game now builds clean ×5 (its run is gated only by its own `todo()` placeholder stubs).** ORIG:
  FOUND 2026-06-03 (fan-out). #216 fixed guard-let on RUST (lowered to `let-else`); js/ts/python/go still drop the bound names.
- **[Q-let-shadow-const] `let` shadowing emitted as repeated `const`/`let` collision (ts/py/go; js done)** — bug · **DONE (#227 ts · #228 py · #229 go; js #217)** ·
  `compiler/crates/bock-codegen/` (ts/py/go) · — · links #217, #227, #228, #229, MS-examples-hardening · note: **DONE
  2026-06-04 (fan-out) — mirrored the js #217 per-block let-scope tracking: ts emits `let`-first / assign-after (fixes
  TS2451 — todo-list build→pass+run); py renames a shadowing inner-block binding to a fresh alias (`{name}__sN`, committed
  after the RHS so `let y = y + 10` still reads the outer `y`); go turns a colliding `:=` into reassignment.** ORIG: FOUND
  2026-06-03 (fan-out). A shadowing `let` emits a second `const`/binding → ts `TS2451`, etc. (todo-list). #217 fixed JS.
- **[Q-propagate-exprpos-shared] Nested `?` inside a larger expression not hoisted (shared)** — impl · ready ·
  `compiler/crates/bock-codegen/src/generator.rs` (a codegen pre-pass like `hoist_value_cf`) · — · links #226, #227, #229,
  Q-propagate-operator-noop, Q-exprpos-shared-desugar · note: FOUND 2026-06-04 (the per-backend fan-out CONVERGED — js, ts,
  AND go all independently reported it). The #226–#229 `?` lowering handles statement-adjacent positions (`let x = e?`, bare
  `e?`, tail); a `?` nested inside a larger expression (`f(g()?)`, `Ok(f()? + 1)`) has no expression-form early-return, so
  it's left un-hoisted. Same shape as Q-exprpos-shared-desugar → a shared pre-pass that hoists the `?` to a statement before
  the consumer. **No current v1 example hits it** (LOW priority; do when the exprpos-shared machinery is next touched).
- **[Q-ts-match-narrowing] TS `match` over Result/Optional doesn't narrow the payload binding** — bug · **DONE (#234)** ·
  `compiler/crates/bock-codegen/src/ts.rs` · — · links #227, MS-examples-hardening · note: FOUND 2026-06-04 (#227). In a
  statement-position `match` switch-lowering, the payload bind `const x = scrutinee._0` is typed `T | E` inside `case "Ok"`
  (no narrowing) → `TS2345` (e.g. `formatTask(task)`). Sole remaining ts blocker for task-api. Narrow the binding per arm
  (cast/guard) in `emit_match`.
- **[Q-go-pow-operator] Go `**` power operator not lowered** — bug · **DONE (#233)** · `compiler/crates/bock-codegen/src/go.rs` · — ·
  links #229, MS-examples-hardening · note: FOUND 2026-06-04 (#229). `a ** b` emits `(a /* pow */ b)` → go `syntax error:
  unexpected literal`. Lower to `math.Pow` (float) / an int-pow helper. Blocks type-zoo on go.
- **[Q-go-list-method-typing] Go `.map`/lambda element typing uses `interface{}`** — bug · **DONE (#233)** ·
  `compiler/crates/bock-codegen/src/go.rs` · — · links #229, Q-list-method-codegen, MS-examples-hardening · note: FOUND
  2026-06-04 (#229). `.map`-with-closure emits `func(t interface{})` + `[]interface{}` where concrete `Todo`/`[]Todo` are
  required (`t.Done undefined`, `cannot use …[]interface{} as []Todo`). Blocks todo-list on go; likely related to the older
  Q-list-method-codegen cluster. Thread the element type through the lambda + result slice.
- **[Q-py-matcharm-lambda-binding] Python match-arm lambda doesn't bind the pattern payload** — bug · **DONE (#235)** ·
  `compiler/crates/bock-codegen/src/py.rs` · — · links #228, Q-match-exprpos, MS-examples-hardening · note: FOUND 2026-06-04
  (#228). A match arm whose body is a lambda mis-binds the pattern payload — `(lambda __v: f"x={x}")(p)` raises `NameError:
  name 'x'`. Match-arm pattern-binding/scope defect in the value-position match lowering. Blocks pattern-lab on py.
- **[Q-examples-ts-tsc-gate] examples-exec ts audit uses strip-types (no type-check) — add `tsc`** — chore · **DONE (#255)** ·
  `tools/scripts/examples-exec-audit.sh` · — · links #234, MS-examples-hardening · note: FOUND 2026-06-04 (#234). The ts row
  of the examples audit runs `node --experimental-strip-types main.ts`, which does NOT type-check — so `tsc`-rejecting output
  (e.g. the TS2345 #234 fixed) passes the audit silently, and the ts "ran" count can overstate type-safety. The real gate is
  `tsc` (the conformance harness + `bock build -t ts` use it). Add a `tsc --noEmit` step to the ts audit path so the matrix
  reflects type-safety. (Same "syntax-check ≠ correct" trap as the broader conformance-vs-examples gap.)
- **[Q-py-valpos-stmt-arms] Python value-position `match` with statement arms below tail drops leading statements** — bug ·
  **DONE (#271)** · `compiler/crates/bock-codegen/src/py.rs` · — · links #235, #271 · note: **DONE 2026-06-07 (#271).** New
  `match_arm_drops_leading_stmts` predicate (mirroring the lambda-chain's bail set) routes a value-tail-plus-leading-statement
  arm to the existing statement-form `match`/`case` (wired into both let-bound + tail-position paths); simple-let/bare-call/
  tail-only arms stay on the lambda chain. Fixture exercises an observable side effect (outer-counter mutation: `steps=0`→`3`).
  ORIG FOUND 2026-06-04 (#235).
- **[Q-rust-str-mixed-binding] Rust `String` `match` mixing `&str` literal + whole-scrutinee binding arm** — bug ·
  **DONE (#271)** · `compiler/crates/bock-codegen/src/rs.rs` · — · links #236, #271 · note: **DONE 2026-06-07 (#271).** Keep
  the `match (s).as_str()` wrap in the mixed case AND re-bind each whole-scrutinee bind to owned `String` at the arm-body top
  (`let other = other.to_string();` — always sound). Extracted a shared `emit_match_scrutinee_prefix` for stmt- and
  expr-position matches; removed dead `scrutinee_matches_str_literal`. Fixture covers literal + guarded bind + plain bind.
  ORIG FOUND 2026-06-04 (#236).
- **[Q-stdlib-fmtcheck] Enable `fmt --check` on stdlib `.bock`** — chore · **DONE (#272)** ·
  `.github/workflows/ci.yml`, `stdlib/`, `tools/scripts/stdlib-fmt-check.sh` · — · links #119, #272, Q-bockfmt-cfarm-comma,
  Q-bockfmt-utf8-panic · note: **DONE 2026-06-07 (#272).** 8/10 stdlib core files `bock fmt`'d (whitespace/trailing-comma
  normalization); behavior-equivalence PROVEN (full test suite + conformance REQUIRE=all 0 failed on the reformatted, rebuilt
  `bock`). New blocking `stdlib-fmt` CI job runs `tools/scripts/stdlib-fmt-check.sh` (stages files into a temp tree since
  `bock fmt` has no path flags). **`collections.bock` + `iter.bock` EXCLUDED** — `bock fmt` corrupts them → split out as
  Q-bockfmt-cfarm-comma + Q-bockfmt-utf8-panic; fold them back in once those land.
- **[Q-go-list-literal] Go `for x in [literal]` element typing** — bug · **DONE (#176)** · note: verified
  already-fixed — Go emits `for _, x := range []int64{...}` (typed slice + typed range var); pinned by the existing
  `go_typed_list_iter.bock` fixture. (No code change; #176 confirmed + pinned.)
- **[Q-ts-generic-impl] TS generic impl-target `self` typing** — bug · **DONE (#176)** · note: verified
  already-fixed — TS emits `self: Box<T>` / `-> Box<T>`, compiles `--strict` clean; pinned by new
  `ts_generic_impl_self.bock` fixture. (No code change; #176 confirmed + pinned.)
- **[Q-iter-interp-mutself] Interpreter hangs on a `mut self` iterator drive** — bug · **DONE (#273)** ·
  interpreter crate · — · links #151, #152, #273 · note: **DONE 2026-06-07 (#273).** `register_impl` now records the
  `mut self` marker (`MethodEntry.self_is_mut`); `try_call_impl_method` returns the post-call `self` (`MethodOutcome`); both
  dispatch sites write it back to the receiver lvalue (variable or record-field path) via new `write_back_receiver`. The
  `loop { match it.next() }` drive over a `ListIterator` now terminates (`sum=6` EXIT=0 vs timeout EXIT=124 before); fixtures
  carry a wall-clock guard so a regression asserts rather than hangs CI. Same family as Q-interp-enum (also #273).
- **[Q-effect-op-node-lowering] Unhandled bare effect-op surfaces E1001, not E8020** — bug/diagnostic-quality ·
  ready (low-pri) · `compiler/crates/bock-air/` (lower.rs / verify_capabilities.rs) · — · links DV16, #155 · note:
  a genuinely-unhandled bare op (no handler, no `with`) surfaces resolver **E1001** "undefined name" rather than the
  capability-pass **E8020** "effect operation has no handler" — because `EffectOp` AIR nodes are constructed ONLY in
  test code, so the E8020 check (`verify_capabilities.rs:476`) never fires on surface bare-op `Call`s. #155 kept
  E1001 for v1 (correct compile-time error per §10.3; the code is non-normative). To unify: lower recognized bare
  unhandled op `Call`s into `EffectOp` nodes so E8020 fires with the proper message. Non-urgent UX polish.
- **[Q-effect-import-unused] Imported effect used only in `handling`/`with` position flagged W1001 unused** — bug ·
  ready (cosmetic, low-pri) · `compiler/crates/bock-air|bock-types/` · — · links #155 · note: when an imported
  effect (`use m.{Log}`) is referenced only in an effect position (`handling (Log with …)` / `fn … with Log`), the
  import binding isn't marked used → cosmetic `W1001 unused import`. Doesn't fail check/exec. Mark effect-position
  references as uses.
  (DONE this block → #155: Q-effect-interp-rust [Rust interpolation effect-op rewrite] + Q-effect-conformance-wiring
  [the inert effects/ suite now executes ×5]; DV16 RESOLVED.)
- **[Q-interp-effect-op-collision] Interpreter flat op-name→effect map can't disambiguate same-named ops** — bug ·
  **deferred v1.x (evaluated #273 — #157 sufficient for v1)** · interpreter / `bock-cli/src/run.rs` · — · links #157, #273 ·
  note: **EVALUATED 2026-06-07 (#273), left as-is** — #157's deterministic dependency-order shadowing (user effects shadow
  core) is correct + sufficient for v1; full effect-qualified dispatch needs call-site effect info threaded from the checker
  into the AIR (a bare `log(msg)` carries no qualifier) — a v1.x item, not cheap. ORIG: the interpreter resolves bare effect
  ops through a FLAT op-name→effect-name map, so two effects sharing an op name (e.g. a user `effect Logger { fn log }`
  + the embedded `core.effect.Log { fn log }`) collide — only last-writer-wins. #157 made registration deterministic
  (topological order → user effects shadow core), which is correct + sufficient for v1, but full qualification (a
  program using BOTH same-named ops) is unsupported on the interpreter. Codegen (all 5 targets) is UNAFFECTED (each
  program compiles in isolation with proper module scoping). Low-pri interpreter-only limitation.
- **[Q-go-error-message] Go: `core.error.SimpleError` field/method name collision** — bug · **DONE (#191)** ·
  `bock-codegen/src/go.rs` · note: fixed in S6b — `go_method_name` disambiguates a public method colliding with a
  same-named record field to `<Name>Method` (applied at trait interface + receiver + call sites; field stays
  `Message`). Locked by a `go.rs` unit test + `conformance/exec/exec_core_error.bock` (rust+go). The js/ts/python
  variants of the same collision split out → **Q-error-message-jstspy** below.
- **[Q-error-message-jstspy] `core.error.message()` field/method collision also breaks js/ts/python** — bug ·
  **DONE (verified already-fixed #193; fixture strengthened #271)** · `bock-codegen/src/{js,ts,py}.rs` · — · links #191, #193,
  #271 · note: **VERIFIED 2026-06-07 (#271) — already fixed at base by #193.** The shared
  `generator::disambiguate_method_name`/`collect_record_field_names` mechanism is wired on js/ts/py/go and `exec_core_error`
  already ran unrestricted ×5; #271 confirmed BOTH the field (`e.message`) and the renamed method (`e.message()`) are reachable
  on every target and STRENGTHENED `exec_core_error.bock` to read both (output `boom/boom;again: boom`), locking the invariant.
  No js/ts/py source change was warranted. ORIG FOUND in S6b.
- **[Q-clock-handler-routing] `Instant.now`/`sleep` bypass the Clock effect handler** — bug · **DONE (#297)** · `bock-codegen` ·
  — · links #160, #297, Q-rust-host-sleep-tokio-dep, Q-time-shim-path · note: **DONE 2026-06-08 (#297)** —
  `Instant.now`/`sleep`/`elapsed` now dispatch through the installed `Clock` handler (`current_handler_vars["Clock"]`) when one
  is in scope, falling through to the host primitive as the no-handler default; verified ×5 with a self-contained user `Clock`
  handler (interception confirmed) + a no-handler default fixture. So `MockClock`-style virtual time (§18.4) is now achievable.
  FOUND (pre-existing, orthogonal) → Q-rust-host-sleep-tokio-dep. ORIG: the time host primitives were inlined per-target and
  bypassed the installed `Clock` handler, so `sleep` always hit the real host. Pairs with Q-time-shim-path.
- **[Q-conformance-clean-rebuild] Conformance harness doesn't force a clean `bock` rebuild** — chore/test-infra ·
  **DONE (#175)** · note: `run-conformance.sh` now `touch`es `compiler/crates/bock-cli/build.rs` + runs
  `cargo build -p bock --bin bock` before the tests, forcing a stdlib re-embed so `execution.rs::bock_binary()` can't
  reuse a stale sibling binary. Root cause confirmed: the build.rs `rerun-if-changed` on the stdlib tree misses a
  newly-added nested subdir. Local-verification false-REDs resolved.
- **[Q-r2-codegen-residue] R2 surfaced minor codegen/parser gaps** — bug · **mostly DONE** · links #163, #176 · note:
  (b) `List[String]` RECORD FIELD on Go → **DONE** (already-fixed by #168; pinned by `record_field_collection_concat.bock`
  in #176); (c) built-in `len`/`is_empty` lowering shadowing same-named user-record methods → **DONE (#176, ×5)** — was
  genuinely broken on all 5; root cause was `desugared_list_method` matching by name alone, fixed by gating on the
  checker's `recv_kind` stamp (+ `raw_recv_kind` reader, 2 unit tests, `user_method_shadows_builtin.bock`). (a) split out
  → **Q-allcaps-record-parse** (parser, separate). (d) String `reverse`/`char_at`/`slice` remain design-deferred (no
  cross-target char primitive; `s.reverse()` checks clean today) — tracked here, → DQ.
- **[Q-time-int64] §18.3.1 `Int64` realized as `Int`** — docs/spec · **DONE (#175)** · note: §18.3.1 prose now
  clarifies the time surface uses `Int` (i64-backed, full `Int64` range; no separate `Int64` surface type), reconciling
  the storage-width wording with the `Int` signatures. Verified wording-only (not a behavioral divergence). Changelog
  `spec/changelogs/20260601-1940-impl-changes.md`.
- **[Q-allcaps-record-parse] ALLCAPS record name not parsed as struct literal** — bug · ready ·
  `compiler/crates/bock-parser/` · — · links #163, #176 · note: an ALLCAPS (≥2-letter) record name in struct-literal
  position (`SB { ... }`) is not parsed as a struct literal → `E1001`. Split from Q-r2-codegen-residue (a); confirmed
  still present by #176 (out of that PR's codegen scope). Parser fix.
- **[Q-arch-doc-drift] ARCHITECTURE.md / compiler-CLAUDE.md / CONTRIBUTING.md crate-name drift** — docs/chore · ready ·
  `ARCHITECTURE.md`, `compiler/CLAUDE.md`, `CONTRIBUTING.md` · — · links #174 · note: D5 (#174) found the root
  `ARCHITECTURE.md` and `compiler/CLAUDE.md` name crates that **don't exist** (`bock-checker`, `bock-codegen-{js,ts,py,rs,go}`)
  and omit the real ones (type-checking is `bock-types`; all codegen is the single `bock-codegen`). Root `CONTRIBUTING.md`
  also describes conformance as `<name>.bock`/`<name>.expected` pairs, but the harness is `// TEST:`/`// EXPECT:`
  directive-driven. The D5 docs page documents reality + notes the divergence; reconcile these three source files to the
  real 17-crate workspace. (CLAUDE.md files are orchestrator/merge-coordinator territory.)

## v1-blocking

- **[Q-codegen-completeness] Codegen completeness across all 5 backends** — impl ·
  **v1-BLOCKING MILESTONE** (operator-decided 2026-05-30 "proceed comprehensive fix"; ~10-15 PRs, phased,
  mostly `compiler/crates/bock-codegen/` → SEQUENTIAL per crate-granularity) · links DV12-DV15, DV10/DV11,
  DQ14/DQ15/DQ18, #129, the 3-agent audit (audit.md 2026-05-30 18:00) · note: the audit established the v1
  codegen substrate is materially incomplete for the stdlib's real needs (all-5-green slice is narrow).
  PHASES: **P0 foundations DONE** — tail-`if`-in-loop (#131, DV15); cross-module `use` via single-file
  bundling of reachable modules (#132, DV13); user-enum codegen / variant registry (#133, DV14). [§20.6.1
  bundling-divergence → DQ19/Design.] **P1 stdlib types DONE** (#135 Python lambdas/generics · #136 Go/TS/Rust generics [DV12 resolved] · #137
  recv_kind annotation + primitive-bridge · #138 Result runtime + Optional/Result methods; `expr?` deferred → DQ20). **P2 traits+match DONE** (#140 trait self/defaults/bounded-dispatch — `use core.compare` runs ×5 · #141
  Self-subst · #142 match guards/or/nested/tuple). **P3 Go collection
  typing DONE** (#144 Go List/Map/Set element typing + record-spread + Self-in-plain-impl · #145 Map/Set method
  dispatch + literals + range()). Collections work ×5.
  **P4 polish** — tuple `.N` parser; Optional-interp; Int/Int + Bool-interp harmonize; mutating-List guard
  (DQ18). SUBSUMES prior codegen follow-ups (Q-match-exprpos, Q-go-list-literal, Q-ts-generic-impl,
  Q-self-subst, Q-prim-assoc). Q-list-codegen READ-ONLY methods DONE (#129); mutating → P4. **Phases 0-3 + P4-codegen DONE (#131-#149); the codegen
  substrate is essentially built (cross-module, enums, generics incl. container/trait, Optional/Result, traits,
  match, collections, primitive-bridge; ~275 exec ×5).** P4-codegen landed: #147 tuple-`.N` diagnostic, #148 TS
  Self-in-plain-impl + expr-position match, #149 generic-container/trait residue (GAP-A/B/C/D — the 4 gaps
  core.iter's v5 STOP exposed; the systematic audit under-covered them). **6th PROBE CLOSED (#152):** core.iter's
  real generic-combinator surface exposed Rust/Go codegen residue (transitive `T: Clone`, Go generic-record-construct
  / concat-arg typed literals / generic-trait interface header / lambda specialization) — fixed, ~300 exec ×5. The
  codegen substrate is now exercised by a full generic stdlib module on all 5. **REMAINING:** (a) ~~core.iter~~ DONE
  (#151/#152); (b) **Q-codegen-completeness P4-hygiene** (bock-types: mutating-collection guarding diagnostic
  [DQ18 v1-floor] + bare-`m.contains` [DQ22] — both checker.rs); (c) design-gated → Design: DQ23 (Int/Int §3.6 NEW),
  DQ18 (mutating lowering), DQ20 (`expr?`), DQ22, DQ21, Bool-interp spelling; (d) Go nested-runtime-payload arith
  [#142 residual] + Rust by-value-reuse [#149 OPEN]. NONE of these gate the R1 effect floor.
- **[Q-stdlib] Implement the core standard library** — impl ·
  **★ DONE — v1 STDLIB COMPLETE, 11/11 modules ×5 ★** (was v1-BLOCKING; now satisfied). R1: iter [#151/#152],
  effect-foundation [#155], effect [#157]. R2: option [#159/#162/#165], result [#161/#165], string [#162/#163], time
  [#160 builtin]. **R3: test [#169 — both free + fluent assert APIs, DQ26], collections [#170 — SortedSet + utils].**
  All ×5. Enabling codegen across the batch: #162 (String methods + keyword escaping + Optional-T:Clone + bundle
  determinism), #164 (dep_graph determinism), #165 (Go generic Optional/Result), #167 (bock test core-loading),
  #168 (generic List[T]-over-user-types + sealed-trait bounds on primitives), #170 (collections Go/Rust residue).
  405 exec pairs ×5. **UNBLOCKS D4** (stdlib reference docs). NO further stdlib work for v1 ·
  `stdlib/`, `compiler/tests/conformance/stdlib/` · — · links DV1, MS-stdlib, DQ5,
  #100 · note: v1 = **11 core modules** at minimum-useful surface (option, result,
  collections, string, iter, compare, convert, error, effect, time, test). Each =
  `stdlib/core/<m>/` source + per-target shims + conformance fixtures, compile/run
  on every target. **Landed:** loading mechanism + `core.error` (#103); `core.compare`
  (#104); the primitive-conformance bridge (#108); `core.convert` + parameterized
  traits (#110); **`core.iter`** (#151 generic `Iterator[T]`/`Iterable[T]` + concrete `ListIterator[T]`
  + 6 eager List-returning combinators + the for→Iterable checker desugar; #152 Rust/Go codegen — all 5×5);
  **`core.effect`** (#157 `Log` effect + `ConsoleLog` handler + `console_log()`; the effect foundation #155 + the
  `effect`-keyword module-path parser fix + the interpreter determinism fix — all 5×5);
  **`core.option`** (#159 utilities; #162 keyword-escape + Rust T:Clone; #165 Go — ×5); **`core.result`** (#161
  utilities; #165 Go — ×5); **`core.string`** (#162 String-method codegen layer; #163 utilities + StringBuilder — ×5);
  **`core.time`** (already a compiler builtin — Duration/Instant/Clock/sleep; #160 conformance floor pins §18.3.1 ×5).
  **Codegen gate CLEARED:** Q-fconf execution conformance (#114/#115)
  + Q-codegen-fixes (#121, DV9) + the codegen-completeness milestone (#131-#152) — 5-target parity real + tested.
  **R1+R2+R3 ALL COMPLETE — v1 stdlib DONE (11/11 ×5).** R3: test #169 (DQ26 both-API floor), collections #170
  (SortedSet + utils). No remaining stdlib work for v1. Plans (all executed): `plans/2026-05-31-core-iter-r1-plan.md`,
  `plans/2026-05-31-effect-foundation-plan.md`, `plans/2026-05-31-core-effect-r1-plan.md`.
  `core.types/math/memory/concurrency` Reserved for v1.x.
  Plans: `plans/2026-05-29-stdlib-loading-error-pilot-plan.md`,
  `plans/2026-05-30-primitive-conformance-bridge-plan.md`,
  `plans/2026-05-30-codegen-correctness-conformance-plan.md` (done).

## Blocked

- **[D4] Stdlib reference docs** — docs · **DONE → #172** · `docs/src/reference/` · note: shipped the v1 stdlib
  reference — landing (`reference/stdlib.md`, replacing the outdated `std.*` stub) + 11 per-module pages
  (`reference/stdlib/core-*.md`) generated from the `///`/`//!` comments via `bock doc stdlib/core` then curated to
  user-facing prose; `core.time` (builtin) hand-written from §18.3.1. SUMMARY wired; `mdbook build docs` clean.
- **[D5] Contributor docs + cleanup** — docs · **DONE → #174** · `docs/src/contributing/` · note: shipped a proper
  nested Contributing section — `index` (overview/where-to-look/reviews), `architecture` (real 17-crate workspace +
  pipeline), `workflow` (canonical 4-command pre-PR gate + directive-driven conformance), `spec-changes` (spec process +
  generated changelog/STATUS/ROADMAP). Replaced the thin flat `contributing.md`; SUMMARY rewired; `mdbook build docs`
  clean. FOUNDs filed → Q-arch-doc-drift. **D5 was the last gate before ItemB → ItemB now UNBLOCKED.**
- **[D2-polish] D2 language-reference final polish** — docs · blocked ·
  `docs/src/language/` · blocked-by: (D2-FOUND mostly resolved — verify)
  · note: most D2-FOUND rows resolved per spec revision; confirm residue.
- **[ItemB] Per-module output + project-mode codegen + config tables** — impl · **★ DONE — MS-projectmode COMPLETE
  (S0–S8: #181/#182/#184/#185/#186/#188/#190/#191/#193/#194 + S8 close) → DV13+DV18 CLOSED; project mode real on all 5 ★** ·
  `compiler/crates/bock-codegen/`,
  `bock-cli/src/build.rs`, `bock-build/src/toolchain.rs`, `compiler/tests/execution.rs` · — · links #28, #132,
  DV13, DQ19, MS-projectmode · plan: `plans/2026-06-02-itemB-per-module-projectmode-plan.md` · note: **v1.0's last
  engineering milestone.** Owner decided (eyes-open) the v1 output is the **per-module native tree** (DQ19 →
  re-opens DV13: native per-target cross-file imports that compile+run) AND **config tables pulled into v1**.
  Staged **S0–S8** (sequential through S0→S4; S6 fans out by target):
  - **S0** — spec/tracking reconcile (DQ19 resolved, config tables un-reserved). **DONE → #181.**
  - **S1** — native imports + harness multi-file run, **PILOT = python**. **DONE → #182** (425 exec pairs / 0
    failed under REQUIRE=all; python emits a per-module native-import tree + runs as a multi-file project via the
    `emits_per_module_tree(target)` harness predicate [python-only]; js/ts/rust/go unchanged/bundling). Notes for
    fan-out: python run plan needed NO change (PEP 420 namespace pkgs resolve from build-dir root) — js/ts need an
    ESM run affordance, rust/go need a manifest; output paths key on the declared `module` path (not source-mirrored);
    per-module emission loses bundling's single-context visibility (re-seed via `seed_effect_registries` /
    `implicit_imports_for`).
  - **S2** — js then ts native ESM imports. **DONE → #184** (js: per-module ESM + minimal `package.json
    {"type":"module"}` run affordance; ts: `tsc→node`, no toolchain.rs change).
  - **S3** — rust + go native imports + minimal manifest. **DONE → #185** (rust: `src/`-rooted cargo crate +
    `mod`/`use crate::`, run `cargo run`; go: flat `package main` + `go.mod`, run `go run .`; run-plans reworked
    to validate/run at project level). FOUND → **Q-go-error-message** below.
  - **S4** — retire dead bundling code (**DV13 CLOSED**). **DONE → #186** — removed the multi-module bundling
    concatenator (trait-default `generate_project`, `bundle_output_path`, `append_entry_invocation`,
    `go::generate_bundle`, the always-true `emits_per_module_tree` predicate; ~170 net lines). KEPT (load-bearing,
    NOT bundling): the single-module self-contained emit (`generate_module` + `per_module` flag) used by ~250 unit
    tests — reframed terminology. **All 5 targets now emit per-module native trees as the sole path.**
  - **S5** — scaffolding framework + `bock.project` config parsing. **DONE → #188** — `Scaffolder` trait in
    `bock-codegen/src/scaffold.rs`; project-mode hook in `build.rs` gated on `!source_only`; `[targets.<T>]` /
    `[targets.<T>.scaffolding]` parsing + validation against the §20.6.2 v1 matrix (unknown value → error naming
    options; 26 unit tests); per-target bodies STUBBED (placeholder README) for S6. Flagged **DV18** (below).
  - **S6** — per-target scaffolders. **DONE** (split S6a/S6b):
    - **S6a → #190** — project-mode output ARCHITECTURE + **DV18 CLOSED**: codegen emits only per-module source;
      the `Scaffolder` owns the manifests (project mode only); `--source-only` is now bare; the conformance harness
      builds in project mode + runs the project. (NOTE: orchestrator finished this PR — the engineer session stalled
      after doing the work; I re-ran the gate, fixed a fmt drift, committed/merged.)
    - **S6b → #191** — enriched per-target scaffolders ×5 (rich manifests w/ framework refs + defaults, formatter
      configs, opt-in linter configs, README first-contact w/ package-manager hints; 41 unit tests) + **fixed
      Q-go-error-message** (go field/method collision via `go_method_name`; locked by `exec_core_error.bock`).
      Required side-fix: TS run plan `tsc main.ts` → `tsc -p .` (scaffolded tsconfig). 427 exec pairs / 0 failed.
      Deep-config that changes CODE (test-file codegen per framework) → S7.
  - **S7** — transpiled tests + formatter-clean gate. **DONE → #194** — Bock `@test` fns transpile to per-target
    test files (Vitest|Jest / pytest|unittest / cargo test / go test), framework-branched, wired into the scaffolded
    project; assertion lowering. **rust+go RUN-verified** (`cargo test`/`go test` pass on the emitted project);
    js/ts/python **compile-verified** (`tsc`/`node --check`/`py_compile`) — their runners (vitest/jest/pytest) +
    formatters (prettier/black) are absent on host/CI. Formatter-clean gate enforced for **rust (`rustfmt --check`)
    + go (`gofmt -l`)** + 2 codegen-hygiene fixes. 430/0. FOUND → Q-ci-projectmode-tooling, Q-go-gofmt-listclosure
    (below). Q-error-message-jstspy was done standalone (#193).
  - **S8** — internal docs + close. **DONE → this PR** — fixed `docs/src/getting-started.md` stale build-output
    path (`.bock/build/` → `build/<target>/`) + documented project-mode default (scaffolded project w/ manifest +
    transpiled tests); tooling.md/project-schema.md already updated by S5–S7. mdbook clean. Tracking closed (this PR).
  INVARIANT (held every PR): `run-conformance.sh REQUIRE=all` green (**430/430**). `--deliverable`/`--no-tests`
  stay v1.x. **★ ItemB COMPLETE (S0–S8) — DV13 + DV18 CLOSED; project mode real on all 5. ★** Remaining for v1.0
  release-readiness: Q-ci-projectmode-tooling **DONE (#196 — js/ts/python project-mode CI-certified)**; remaining =
  Q-formatter-clean-tree (full emitted tree formatter-clean ×5 per §20.6.2). **ItemD now UNBLOCKED** (external — escalates).
- **[ItemD] /get-started project-mode evolution** — docs · **READY-but-ESCALATES (UNBLOCKED 2026-06-03 — ItemB done)** ·
  `docs/`, `website/` · — · note: external-facing copy (website get-started) — **escalate for approval before any
  website change**; do not action autonomously. Now that project mode is real, the website get-started can evolve to
  show the scaffolded-project flow (`bock build` → `npm test`/`cargo run`).
- **[Q-ci-projectmode-tooling] CI provisions js/ts/python test+format tooling** — chore/test-infra · **DONE (#196)** ·
  `.github/workflows/ci.yml` · note: CI ubuntu lane now installs prettier/black/ruff/pytest + node and sets
  **`BOCK_PROJECTMODE_REQUIRE=all`** so the transpiled-test verification RUN-verifies + formatter-gates **all 5**
  (macos/windows stay skip-if-absent). ★ **Key finding: js/ts/python transpiled tests PASS as-emitted** — NO
  execution-codegen bugs; the only fixes were formatter-cleanliness of the emitted *test files* (js/ts tag-predicate
  parens; py blank-line spacing). Also added the missing `rustfmt` component to the test toolchain (surfaced by
  require=all on beta). **js/ts/python project-mode is now CI-certified.** Remaining formatter gap → Q-formatter-clean-tree.
- **[Q-formatter-clean-tree] Full emitted tree formatter-clean (§20.6.2)** — bug · **rust/go DONE (#198); js/ts/python
  DEFERRED** · `compiler/crates/bock-codegen/` · — · links #194, #196, #198, §20.6.2 · note: §20.6.2 mandates **rust+go**
  formatter-cleanliness as the universal baseline → **DONE (#198)**: project-mode build runs a post-emit `gofmt -w`
  (go) / `rustfmt` (rust) pass over the full tree (their formatters ship with the toolchain; go has no source-map
  conflict); full-tree `gofmt -l`/`rustfmt --check` gates added. **js/ts/python full-clean DEFERRED**: prettier/black
  *reflow* long lines (not hand-matchable in codegen) AND post-emit prettier would break the js/ts **source maps**;
  those formatters are user-OPTIONAL per §20.6.2 (not the baseline). Pursue later via either cheap codegen wins
  (redundant parens, py blank-lines) and/or post-emit formatting with source-map regeneration. v1.x-leaning.
- **[Q-list-method-codegen] List `.map()`/`.filter()` method-with-closure mislowered (all 5)** — bug ·
  **DONE → #205 (all 5)** · `compiler/crates/bock-codegen/` · — · links §20.4, MS-examples-hardening, #205,
  Q-impl-body-typecheck · note: FIXED by #205 — new `FUNCTIONAL_LIST_METHODS` + `desugared_list_functional_method`
  recogniser in generator.rs wired into each backend's Call arm; native idioms per target (JS/TS array methods; py
  builtins + gated runtime prelude; rust iter-adapter chains; go for-range func literals). 5 new conformance fixtures
  (×5, 25 exec pairs). **CAVEAT (reaches free-fn call sites; method-body sites bounded by Q-impl-body-typecheck —
  the checker doesn't type-check impl/class method bodies so the recv_kind stamp isn't applied there).** Original detail:
  EXACT root cause was
  a List functional METHOD with a closure is lowered with the **free-function calling convention** — the receiver is
  emitted as an explicit first argument: `data.map(data, (dp) => …)` (verified in TS output). Effect per target: **TS**
  array-not-assignable-to-callback + implicit-any params; **rust** `no method 'map'/'filter' on Vec` (needs
  `.iter().map().collect()`); **go** `found 'map'` syntax-error (`map` keyword) + `.filter` undefined; **js** runtime
  "object is not a function" / "nodes.map is not a function"; **python** `'list' object has no attribute 'map'/'filter'`.
  BROADEST single bug — ~10 examples (data-pipeline, markdown-parser, task-api, inventory-system, ownership-demo,
  ml-data-prep, react-components, systems-allocator, type-zoo, todo-list). Distinct from `core.iter`'s FREE functions
  (conformance-tested + pass) — which is why conformance is 430/0 green while real programs fail. Checks clean ⇒ §20.4
  transpiler bug. Fix the method-call lowering to use each target's native chain (no dup receiver, typed closure params).
- **[Q-rust-cargo-workspace] Generated `Cargo.toml` doesn't opt out of a parent workspace** — bug ·
  **DONE → #210** · `compiler/crates/bock-codegen/src/scaffold.rs` · — · links MS-examples-hardening, #210 · note:
  FIXED by #210 — the rust scaffolder now emits an empty `[workspace]` table in the generated `Cargo.toml`. Verified
  in-repo (fizzbuzz built inside the gitignored `temp/` — which is inside the repo's cargo workspace — now succeeds).
  Was masking-only; recovers the rust examples that failed solely on this.
- **[Q-examples-exec-coverage] Exec-test all ~20 examples on all 5 targets in CI (the gate)** — chore/test-infra ·
  **DONE (informational) → #204; ratchet-to-blocking pending** · `tools/scripts/examples-exec-audit.sh`,
  `tools/examples-exec-baseline.txt`, `.github/workflows/examples-exec.yml` · — · links MS-examples-hardening, #204 ·
  note: LANDED #204 — a script (out-of-tree build ×5 + run) + a `continue-on-error` CI job + a checked-in baseline that
  warns on regression (strict mode `BOCK_EXAMPLES_REQUIRE` exits 1). **FOLLOW-UP (ratchet step): refresh the baseline now
  that A/B/C landed** (post-fix matrix 15:24: js ran 7/20·ts 4/20·py 9/20·rust 2·go 1, +7 vs baseline, 0 regressions) so
  the newly-passing pairs are protected; flip to required per-target as more clusters land. [historical detail below] ·
  — · links milestones (MS-examples-hardening, v1.0 acceptance) · note: FOUND 2026-06-03; the 20×5 audit (13:44) is the
  prototype. The 20 `examples/` aren't built+run on all 5, so real-world-pattern codegen bugs slipped past the narrow
  conformance fixtures (430/0 green while real programs fail). Build the gate: for each example × target, project-mode
  `bock build` (compile) + run where possible (ts via `node --experimental-strip-types`; rust `cargo run`; go `go run .`;
  js `node`; py `python3`). **Land NON-BLOCKING (reports the matrix per PR), then ratchet per-target pass-thresholds
  upward to required as clusters land** (operator decision). Can run parallel to the cluster fixes (disjoint files).
  Note the in-repo cargo-workspace interaction (Q-rust-cargo-workspace) — fix it or build rust examples out-of-tree.
- **[Q-list-concat-codegen] List `+` concatenation emitted as native `+` (ts/rust/go)** — bug ·
  **DONE → #205** · `compiler/crates/bock-codegen/` (+ `bock-types/checker.rs` stamp) · — · links MS-examples-hardening,
  §20.4, #205, Q-impl-body-typecheck · note: FIXED by #205 — checker stamps `LIST_CONCAT_META_KEY` on a List `+`
  (`infer_binop`, checks result OR either operand for a concrete `List` to close the open-result-var case);
  `generator::is_list_concat` reads it + a list-literal syntactic fallback; per-target concat idioms (`[...a,...b]` js/ts,
  clone+extend rust, append-helper go, native `+` py). **CAVEAT: a bare `self.a + self.b` list concat INSIDE an impl
  method won't get the checker stamp (Q-impl-body-typecheck); the syntactic fallback covers the common `xs + [..]` shape.**
  Original: FOUND 2026-06-03 (audit). Bock list
  append/concat via `+` (`(self.items + [todo])`) lowers to a native `+` op: **ts** `Operator '+' cannot be applied to
  T[]`, **rust** E0369 `cannot add Vec<T> to Vec<T>`, **go** `operator + not defined on []T`. js silently does the wrong
  thing (string-concat), python coincidentally works (list `+`). Lower to each target's concat idiom (spread / `extend` /
  `append`). Examples: todo-list, expense-tracker, ownership-demo, systems-allocator.
- **[Q-const-enum-naming] Const / enum-variant identifier def↔use mangling mismatch (all 5)** — bug ·
  **CONST part DONE → #205; enum-variant/trait-name residue now RUNTIME (not build)** · `compiler/crates/bock-codegen/` ·
  — · links MS-examples-hardening, #205, Q-py-circular-import · note: #205 fixed the **const** def↔use mismatch
  (`collect_const_names` registry; consts emitted verbatim at def + use across all backends) — fizzbuzz now compiles on
  js/ts/py/rust. **POST-FIX MATRIX (15:24): the enum-variant (`Category_Electronics`) + trait/protocol-name
  (`Allocatable`) cases now BUILD but RUN-FAIL** on js/py (inventory-system, systems-allocator moved from build-error to
  runtime-error), folded into Q-py-circular-import (K) + a trait-symbol-not-emitted residue — **no remaining BUILD-level
  work here**. Original: FOUND 2026-06-03 (audit). A constant or
  enum-variant name is emitted with one casing at the DEFINITION and another at the USE site: TS defines `FIZZ_NUM` but
  references `fizzNUM` (`Did you mean 'FIZZ_NUM'?`); `Category_Electronics`/`Allocatable` referenced-but-undefined; python
  references `FIZZ_NUM` but never emits the def at module scope. Normalize the identifier transform so def and use agree
  (and ensure module-scope consts are emitted). Examples: fizzbuzz, inventory-system, systems-allocator. Likely cheap.
- **[Q-impl-body-typecheck] Checker does not type-check impl/class method BODIES** — bug ·
  **DONE → #207** · `compiler/crates/bock-types/` (checker.rs) · — · links #205, #207, Q-list-method-codegen,
  Q-list-concat-codegen, MS-examples-hardening, Q-go-error-message/Q-error-message-jstspy · note: FIXED by #207 —
  `check_item` now recurses into `ImplBlock`/`ClassDecl`, type-checking each method body as a function with `self` bound
  to the target + impl generics/`Self` substituted (`build_impl_context`). **Measure-then-fix blast radius was small +
  fully resolved:** turning on body-checking surfaced exactly two latent issues — (1) a **REAL pre-existing bug** in
  `core.error` (`impl Error for SimpleError { fn message(self)->String { self.message } }`: a `FieldAccess` to a field
  whose name collides with a method resolved the METHOD in value position → E4001; affected ALL core modules transitively
  + user-facing; fixed by preferring the same-named field in value position, method *calls* re-resolve via new
  `resolve_user_method_fn_type`), and (2) a `Self`-in-plain-impl return-type **false positive** (the `TypeSelf` arm now
  consults `gp_map["Self"]`). Conformance **455→460** (REQUIRE=all; +5 new `exec_method_body_list_ops` ×5). Negative
  diagnostics fixtures added (impl + class method-body type errors now caught). **HONEST PAYOFF:** the value is the
  **correctness** dimension (catching method-body type errors + the latent core.error bug) — NOT new codegen reach:
  example output (todo-list ×5) is **byte-identical** before/after because codegen already had robust syntactic fallbacks
  for method-body list ops. NEW residue OPENs surfaced (pre-existing, codegen-crate) → folded into Q-examples-codegen-misc
  (h)/(i). [The core.error checker-resolution fix is distinct from the codegen field/method collision work in
  Q-go-error-message/#191 + Q-error-message-jstspy/#193 — same pain point, different layer.]
- **[Q-go-enum-return-boxing] Go: enum variant not boxed into sealed-trait interface on return** — bug ·
  **DONE → #209** · `compiler/crates/bock-codegen/src/go.rs` · — · links MS-examples-hardening, #168, #209,
  Q-string-num-jstspygo, Q-match-exprpos · note: FIXED by #209 (4 root causes: block-in-expr-position closure dropped
  its statements + hardcoded `func() interface{}`; if/match IIFEs didn't propagate the concrete type into branch/arm
  bodies; untyped `let m = if{…}` over variants typed its closure from the fn return; void-call arm tails emitted
  `return println(..)` → the `(int,error)` arity error). Conformance +5 (`exec_enum_return_boxing` ×5). **HONEST: cleared
  the boxing/arity barrier on all 4 go examples but go examples STILL fail (matrix go 1/20 unchanged) — each now hits a
  NEXT barrier** (chat-protocol→early-return-trapped-in-IIFE = Q-match-exprpos; microservice→String.slice = Q-string-num-
  jstspygo + expr-position type-switch payload; calculator/effect-showcase→a single Result-payload type-assert on go).
  E was a necessary prerequisite, not sufficient — go needs the full chain (string-methods + match-exprpos + Result-payload).
- **[Q-rust-move-codegen] Rust: codegen produces borrow/move violations** — bug ·
  **DONE → #210** · `compiler/crates/bock-codegen/src/rs.rs` · — · links MS-examples-hardening, #149, #210 · note:
  FIXED by #210 — clone-on-reuse extended to fn/method params (`seed_reused_params`, skips Copy scalars), the
  desugared-self-call / MethodCall / bare-effect-op arg paths, `for x in coll` iterables, and closure-captured bindings
  (E0507); plus an adjacent effect-handler double-borrow fix (E0277 `&impl T: T`) via a `borrowed_handler_effects` set.
  New fixtures `exec_rust_move_reuse` (×5) + `exec_rust_effect_forwarding` (×5). Recovered rust examples (see Q-list-method
  matrix). [the #149 by-value-reuse follow-up is subsumed.]
- **[Q-rust-string-num-methods] Rust: String / numeric method-lowering gaps** — bug ·
  **DONE (rust) → #210; cross-backend split → Q-string-num-jstspygo** · `compiler/crates/bock-codegen/src/rs.rs`
  (+ `bock-types/checker.rs` string_concat stamp) · — · links MS-examples-hardening, #210, Q-string-num-jstspygo · note:
  FIXED on RUST by #210 — lowered String `slice`/`substring`/`char_at`/`index_of`/`repeat`/`reverse`/`trim_*` + numeric
  `to_float`/`to_int`/`abs`/`min`/`max`/`clamp`/`floor`/`ceil`/`round`/`sqrt`/… to native rust; new checker `string_concat`
  stamp lowers `String + String` to `format!`. Fixture `exec_rust_string_num_methods` (rust-only). **The same lowerings
  are MISSING on js/ts/python/go → split out to Q-string-num-jstspygo (below).**
- **[Q-string-num-jstspygo] String/numeric method lowering missing on js/ts/python/go (§18.3)** — bug ·
  **DONE → #213 (hotfix #214)** · `compiler/crates/bock-codegen/` (js/ts/py/go) · — · links MS-examples-hardening, #210,
  #213, #214, §18.3, Q-py-windows-utf8 · note: FIXED by #213 — String + numeric/Char/Bool §18.3 methods now lower to each
  target's native idiom on js/ts/python/go (was rust-only #210), gating on `recv_kind = "Primitive:<Ty>"` via
  `try_emit_string_method`/`try_emit_numeric_method`; `generator.rs` untouched. Fixture promoted to all-5
  (`string_num_methods.bock`); conformance 476→480. **microservice ts FAIL→PASS** (the `slice` 3-arg fix); go advanced
  past `String.slice` (now hits the deeper chain — match-binding + Result-payload). **INCIDENT: #213 merged with a
  failing windows-python lane** (the all-5 fixture printed multibyte slice output; Windows-Python stdout = locale codepage,
  not UTF-8 → mismatch). Hotfix **#214** made the fixture ASCII-output; main green. Root product issue → Q-py-windows-utf8.
- **[Q-py-windows-utf8] Bock-generated Python should force UTF-8 stdout (cross-platform unicode)** — bug ·
  **DONE → #218** · `compiler/crates/bock-codegen/` (py) · — · links #213, #214, #218, MS-examples-hardening · note:
  FIXED by #218 — entry-only `sys.stdout/stderr.reconfigure(encoding="utf-8")` in `main.py` (py3.7+; verified not emitted
  in non-entry modules). Re-enables a multibyte-rune fixture later. Original: FOUND 2026-06-03 (#214
  incident). Windows-Python defaults stdout to the locale codepage, so a Bock program that `print`s multibyte/unicode
  emits mismatched/garbled bytes on Windows (passes on Linux/macOS). Emit a stdout UTF-8 reconfigure at the Python entry
  point (`sys.stdout.reconfigure(encoding="utf-8")`, py3.7+, entry module only) so unicode output is cross-platform. Real
  product correctness gap; surfaced when the string_num_methods fixture printed a multibyte slice. Re-enables a
  multibyte-rune-correctness fixture (currently ASCII-only per #214).
- **[Q-js-effect-export] JS: effect-group/stack export referenced but not emitted** — bug ·
  **DONE → #217** · `compiler/crates/bock-codegen/` (js) · — · links MS-examples-hardening, #155, #157, #217 · note:
  FIXED by #217 — a public composite effect now emits a `const X = Object.freeze({__composite:[…]})` binding so the ESM
  export resolves (effect-showcase, task-api, microservice js — all build+run). [Part of the js-backend batch #217.]
- **[Q-py-circular-import] Python: multi-module emit produces a circular import** — bug ·
  **DONE → #218** · `compiler/crates/bock-codegen/` (python) · — · links MS-examples-hardening, #182, #218 · note: FIXED
  by #218 — ROOT CAUSE was the implicit-import scan matching record/enum/class **field-label** tokens in the AIR debug
  dump as cross-module references (`InventorySummary.total_value` field ↔ `service.total_value` fn). Fixed by counting
  field-label occurrences across all label positions and subtracting them from the scan; `models.py` no longer imports
  `service`. inventory-system python now runs (the lone py example that flipped fail→pass in the matrix). [batch #218.]
- **[Q-examples-codegen-misc] Examples audit: minor / per-example codegen + stub-quality gaps** — bug · ready (low-pri,
  triage individually) · `compiler/crates/bock-codegen/`, `examples/` · — · links MS-examples-hardening · note: FOUND
  2026-06-03 (audit). Smaller items surfaced: (a) `todo`/unimplemented expression in return position → `return throw …`
  (js) / `return raise …` (py), invalid syntax — partly example **stub-quality** (guessing-game has unfinished bodies);
  (b) reserved-word / identifier collisions — `eval` (calculator js, `Invalid use of 'eval'`), redeclared `list`
  (todo-list js); (c) `Char` type unmapped on ts/rust/go (type-zoo); (d) go unused-var strictness `declared and not used`
  (guessing-game); (e) local `step2` binding not emitted (calculator go/py `undefined: step2`); (f) **[from #205]**
  `.for_each` with a BLOCK / mutating / `println` closure body fails on go/python (the pre-existing
  statement-closure-body gap — `for_each` lowering itself is correct on rust/js/python; excluded from the all-5 fixture);
  (g) **[from #205]** chained `.map(..).reduce(..)` over a record-field projection mislowers on go (nested-IIFE inference
  gap; binding the projection to a typed `let` first works ×5); (h) **[from #207]** go: a `list.map(...)` result returned
  DIRECTLY lowers its element type to `[]interface{}` (fails `go build`) — go generic-element-typing residue of cluster A
  at the free-fn level; the typed-`let` pattern avoids it; (i) **[from #207]** js/ts: a `let` binding that is REASSIGNED
  (`let list = …; list = list.add(…)`) is emitted as `const` → Node `Identifier 'list' has already been declared` (this
  precisely diagnoses the audit's "redeclared `list`", item (b) — a `let`-reassignment-vs-`const` lowering bug; affects
  todo-list js); (j) **[from #210]** rust: guard-`let` pattern lowered to a boolean guard → E0600/E0425 unbound `val`/`val2`
  (ownership-demo); (k) **[from #210]** rust: `mut <param>` not emitted as `mut` → E0384 (ownership-demo); (l) **[from #210]**
  rust: list-pattern emitted as a slice pattern → E0529 (ownership-demo); (m) **[from #209]** go: Result-payload
  type-assert error after the boxing fix (calculator, effect-showcase go); (n) **[from #213]** go: `Char.to_string()`/
  `display` emits `fmt.Sprintf("%v", rune)` → prints the code-point integer (`65`) not the char (`A`); pre-existing
  primitive-*bridge* path (not the method lowering), compounded by the boxed-Optional Char payload. Triage each as its
  own fix or example correction.
  **RESOLVED in the 5-backend fan-out (#216–#220):** (a) py todo-expr → #218; (i) js let-rebind-const → #217 (ts/py/go
  residue → Q-let-shadow-const); (j) rust guard-let → #216 (other backends → Q-guard-let-shared); (k) rust mut-param →
  #216; (l) rust list-pattern → #216 (shared → Q-list-range-pattern-shared); (m) go Result-payload → #220; (n) go
  Char-display → #220 (`string(rune)`); (d) go unused-var → #220; go int/int64 width → #220; (b) js `eval` reserved-word
  → #217. REMAINING: (c) `Char` type unmapped on rust/go (ts done #219 → string); (e) `step2` local-binding (re-check;
  likely fixed by go batch). Still grab-bag for residual one-offs.
- **[Q-chat-protocol-allfail] `chat-protocol` fails build on all 5 — DIAGNOSED → folded into Q-match-exprpos** — bug ·
  **RESOLVED-AS-DUP (diagnosed 2026-06-03 13:44)** · — · links Q-match-exprpos · note: the all-5 failure (js `Unexpected
  token ')'`, py `'(' was never closed`, ts `Expression expected`, go enum-return) is the **expression-position
  control-flow lowering** producing unbalanced parens on js/py + the go-enum-return cluster — NOT a distinct root cause.
  Folded into **Q-match-exprpos** (D, un-deferred) and **Q-go-enum-return-boxing** (E). No separate work item.

## Deferred

- **[ItemC] /get-started AI configuration section** — docs · deferred ·
  trigger: real-world AI-usage characterization (post-launch).

---

## Dependency graph

```
[LANDED: … #121 (DV9) · #123 vscode-CI · #124 TS codegen · #125 changelog ·
 #126 Py-Optional+Go-typed-payload · #127 Go match-in-loop · #129 read-only List methods]
Q-codegen-completeness (MILESTONE: cross-module + user-enums + generics + Result + traits + Go-typing + …
  — v1-BLOCKING, phased P0→P4, mostly bock-codegen → SEQUENTIAL) ──┐ gates ↓
Q-stdlib R1 (iter ✓ #151/#152 · effect NEXT) → R2 (option/result/string/time) → R3 (collections/test) ──→ D4 ──→ D5 ──→ ItemB (P1 → P2-5 → P6) ──→ ItemD
  ⮑ codegen-completeness milestone #131-#152 essentially DONE — substrate complete + now EXERCISED by a full generic stdlib module (core.iter) on all 5
  ⮑ iter DONE on all 5: module + for→Iterable checker desugar (#151) + Rust/Go generic-combinator codegen (#152), ~300 exec ×5
(decided-ready: Q-import-reject [DQ8])
(subsumed by Q-codegen-completeness: Q-self-subst, Q-prim-assoc, Q-match-exprpos, Q-go-list-literal, Q-ts-generic-impl)
(separate bugs: Q-xmod-bounds, Q-xmod-impl, Q-interp-enum)
```

**Critical path to v1.0 (2026-05-30, updated):** the Optional-payload codegen family is CLOSED across all 5
(#124/#126/#127) and the for→Iterable desugar is PROVEN — but `core.iter` (a sensitive probe) exposed that
the v1 codegen substrate is materially incomplete: a **3-agent audit** found **cross-module `use` and
user-defined enums broken on ALL 5**, and Result/generics/closures/Optional-methods broken on 3-4/5
(audit.md 2026-05-30 18:00). The "5-target parity" #114-#121 restored was real only for a narrow slice; the
3 "landed" stdlib modules are **check-only, never executed cross-module**. Operator decided (2026-05-30): a
**codegen-completeness MILESTONE** (`Q-codegen-completeness`, v1-BLOCKING, ~10-15 PRs, phased P0-P4, mostly
bock-codegen → sequential) — fix comprehensively, THEN resume the stdlib. Updated path:
**Q-codegen-completeness (P0 cross-module+enums+tail-`if` → P1 stdlib-types → P2 traits+match → P3 Go-typing
→ P4 polish) → Q-stdlib R1 (iter, effect) → R2 → R3 → D4 → D5 → ItemB**. Phase-0 design in flight.
