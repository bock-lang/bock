# Spec Alignment Assessment (D1)

**Scratch file. Drives the design-chat handoff for spec/impl
divergence. Deleted in D5 alongside `docs/INVENTORY.md` once
D2–D4 have absorbed every row.**

Worked through `docs/INVENTORY.md` cross-reference matrix rows
C01–C05, F01–F22, and K04 — 28 rows total. Each row is
classified for downstream resolution per the scheme described
in the D1 session prompt. Verified against `main` at the
session base commit; `spec/bock-spec.md` is treated as
authoritative per the inventory's Spec Authority Note.

## Summary

| Classification                              | Count | Routing |
|---------------------------------------------|-------|---------|
| **A.** Spec amendment to match impl         | 7     | Design chat |
| **B.** Implementation planned; spec forward | 0     | — (no fits) |
| **C.** Defer or remove from spec            | 16    | Design chat |
| **D.** Implementation bug                   | 0     | — (no fits) |
| **E.** Resolved by Item B Phase 1           | 3     | Closed by Item B |
| **F.** Naming/organization                  | 1     | Design chat |
| Reclassified to aligned (no action)         | 1     | FOUND: tag |
| **Total**                                   | **28**| |

Design-chat-routed rows: **24** (A + C + F). Closed without
design chat: **4** (E + reclassified-aligned).

**FOUND:** F21 (`[registries]`) was marked drift in the matrix
but is actually aligned — the impl reads `[registries].default`
and named registries via `bock-pkg/network.rs::parse_registries`.
Inventory error; no spec or impl change needed. Surfaced in this
session's commit message.

## Classification scheme (recap)

- **A.** Spec gap or drift where impl made a deliberate choice;
  update the spec to describe it.
- **B.** Spec is forward-looking; impl is planned but not yet
  scoped into a specific session.
- **C.** Spec describes functionality outside v1 scope; trim or
  mark "Reserved" in the spec.
- **D.** Impl is wrong; file as bug.
- **E.** Closed by Item B Phase 1 (F01–F03 are pre-classified
  per the inventory and Item B implementation plan).
- **F.** Structural/organizational decision that doesn't fit the
  spec/impl axis.

The recommendations below are implementation chat's read.
Design chat decides.

---

## Per-row entries

### C01 — `bock build --strict`

- **Inventory row:** C01 (spec gap)
- **Classification:** A — Spec amendment to match impl
- **Spec says:** Implied by §10.7 (graduated strictness — production
  requires pinning) and §17.4 (production strictness replays pinned
  decisions). Flag not named in §20.1 build flag list.
- **Impl does:** `bock build --strict` forces production strictness
  for the build regardless of the project's configured default; fails
  if any build-scope decision is unpinned
  (`compiler/crates/bock-cli/src/main.rs:62-65`).
- **Recommendation:** Add `--strict` to §20.1's `bock build` flag
  list with a one-line description. Behavior is already consistent
  with §10.7's strictness model; this is a naming gap, not a
  behavioral one.

### C02 — `bock build --pin-all`

- **Inventory row:** C02 (spec gap)
- **Classification:** A — Spec amendment to match impl
- **Spec says:** Implied by §17.4 (every AI decision is recorded;
  decisions can be pinned). Flag not named in §20.1.
- **Impl does:** After a successful build, pins every build-scope
  decision in `.bock/decisions/build/`. Intended development → ship
  workflow: build with `--pin-all`, commit pins, then ship with
  production strictness
  (`compiler/crates/bock-cli/src/main.rs:67-72`).
- **Recommendation:** Add `--pin-all` to §20.1's `bock build` flag
  list. The workflow it enables (`pin-all` in development →
  `--strict` in production) is worth documenting alongside §17.4.

### C03 — `bock build --source-map` / `--no-source-map`

- **Inventory row:** C03 (spec gap)
- **Classification:** A — Spec amendment to match impl
- **Spec says:** §20.5 references source maps generically ("Source
  maps enable debugging transpiled code in target-language
  debuggers"). Flag pair not named in §20.1.
- **Impl does:** `--source-map` (default on) emits source map files
  alongside generated code; `--no-source-map` suppresses them
  (`compiler/crates/bock-cli/src/main.rs:74-80`).
- **Recommendation:** Add `--source-map` / `--no-source-map` to
  §20.1's `bock build` flag list and note default-on. The
  source-map subsystem itself is already described in §20.5; this
  is a flag-surface gap.

### C04 — `bock pkg init`, `tree`, `list`

- **Inventory row:** C04 (spec gap)
- **Classification:** A — Spec amendment to match impl
- **Spec says:** §20.1 lists `pkg` subs as `add`, `remove`,
  `update`, `audit`, `publish`, `search`. The three impl-only verbs
  are absent.
- **Impl does:** `bock pkg init` initializes a `bock.package`
  manifest in the current directory; `bock pkg tree` shows the
  dependency tree; `bock pkg list` enumerates direct dependencies
  (`compiler/crates/bock-cli/src/main.rs:362-393` and
  `compiler/crates/bock-cli/src/pkg.rs`).
- **Recommendation:** Add `init`, `tree`, `list` to §20.1's pkg
  subcommand list. These are standard package-manager verbs
  orthogonal to the spec's existing set; they describe lifecycle
  operations that complement (don't conflict with) `add`/`remove`.

### C05 — `bock pkg cache clear`

- **Inventory row:** C05 (spec gap)
- **Classification:** A — Spec amendment to match impl
- **Spec says:** §20.1 lists `bock cache` (AI/decision/rule
  caches) but does not describe `bock pkg cache` (tarball cache).
- **Impl does:** `bock pkg cache clear` removes downloaded
  package tarballs from `.bock/cache/`
  (`compiler/crates/bock-cli/src/main.rs:395-400`,
  `compiler/crates/bock-pkg/src/install.rs:CACHE_SUBDIR`).
- **Recommendation:** Add `bock pkg cache` as a distinct cache
  surface in §20.1, separate from `bock cache`. The two operate
  on different caches with different lifecycles (AI/decision/rule
  vs. package tarballs); conflating them in docs would mislead
  users about what `bock cache clear` removes.

### F01 — `bock build --deliverable`

- **Inventory row:** F01 (drift: spec only)
- **Classification:** E — Resolved by Item B Phase 1
- **Status:** Spec describes (§20.6.2 "Deliverable mode" with
  `--deliverable` flag, §17.5 "Deliverables"); impl doesn't yet.
  Item B Phase 1 schedules this flag per the
  `20260510-2100-specs-changes.md` changelog ("Phase 1 scope
  additions: `--source-only` and `--deliverable` flags").
- **Routing:** None to design chat. Will close when Item B Phase 1
  lands. Until then, invoking `--deliverable` errors out (not
  silently misbuilds).

### F02 — `bock build --no-tests`

- **Inventory row:** F02 (drift: spec only)
- **Classification:** E — Resolved by Item B Phase 1
- **Status:** Spec describes (§20.6.2 "Test inclusion" — tests
  included by default, `--no-tests` opts out); impl doesn't yet.
  Item B Phase 1 schedules this flag per the same changelog
  ("Phase 1 scope additions: `--no-tests` flag (new)").
- **Routing:** None to design chat. Closes with Item B Phase 1.

### F03 — `bock build --optimize`

- **Inventory row:** F03 (drift: spec only)
- **Classification:** E — Resolved by Item B Phase 1
- **Status:** Spec describes (§17.2 Tier 3 — AI Optimization,
  activated via `bock build --optimize`); impl doesn't yet. Item
  B Phase 1 owns the build-flag surface establishment per the
  `20260510-2100` and `20260510-2300` changelogs.
- **Routing:** None to design chat. Closes with Item B Phase 1.

### F04 — `bock check --context` vs `--no-context`

- **Inventory row:** F04 (drift: polarity)
- **Classification:** A — Spec amendment to match impl
  *(ambiguous: also consider C for the separate `--context`
  facet)*
- **Spec says:** §20.1 lists `bock check` flags as `--types`,
  `--lint`, `--context` — described as "for selective checking"
  (i.e., run only context-system validation in isolation).
- **Impl does:** `bock check --no-context` is a *diagnostic
  display* toggle that hides the source-context snippet in error
  output (default-on); it does not select what's checked. The
  selective-check `--context` mode described in the spec is not
  implemented (`compiler/crates/bock-cli/src/main.rs:108-110`,
  `compiler/crates/bock-cli/src/check.rs:30-48,267-273`).
- **Note on the inventory framing:** The matrix described this as
  a "polarity difference (same capability)". On verification, the
  two flags are different features with confusingly similar names,
  not opposite defaults of the same feature. The impl flag is a
  rendering toggle on diagnostic output; the spec flag is a
  selective-check mode analogous to `--types` and `--lint`.
- **Recommendation:** Two facets, both routed to design chat:
  1. **Spec amendment (A):** add the impl's `--no-context`
     (diagnostic display) to §20.1's `bock check` flag list with a
     name that doesn't collide with the selective-check usage —
     candidates: `--no-source-context`, `--brief`, or rename the
     spec's selective-check flag to `--only=context`.
  2. **Defer the selective-check feature (C):** the spec's
     `--context` (as selective check) is a planned feature impl
     doesn't have. Either implement it as `--only=context`
     (alongside `--only=types`, `--only=lint`) or remove from
     spec until implemented.

### F05 — `bock test` flags impl lacks

- **Inventory row:** F05 (drift: spec ahead)
- **Classification:** C — Defer or trim spec
- **Spec says:** §20.1 lists `bock test` flags as `--target`,
  `--all-targets`, `--smart`, `--coverage`, `--snapshot` in
  addition to `--filter`.
- **Impl does:** Only `--filter` is implemented
  (`compiler/crates/bock-cli/src/main.rs:113-120`).
- **Recommendation:** Defer in spec — mark "v1.x additions: ..."
  in §20.1 or move the unshipped flags to §20.4 (Testing Tiers)
  as forward-looking. `--target` and `--all-targets` come for free
  once Item B is further along (the build surface gains target
  selection); `--smart`, `--coverage`, `--snapshot` are larger
  features that don't need to ship in v1. v1's testing surface
  is "run tests with an optional filter."

### F06 — `bock cache list`, `bock cache prune`

- **Inventory row:** F06 (drift: spec ahead)
- **Classification:** C — Defer or trim spec
- **Spec says:** §20.1 lists `bock cache` subs as `list`, `clear`,
  `prune`, `stats`.
- **Impl does:** `bock cache stats` and `bock cache clear` are
  implemented; `list` and `prune` are not
  (`compiler/crates/bock-cli/src/main.rs:333-358`).
- **Recommendation:** Defer in spec — `list` and `prune` are
  operational conveniences (`stats` already shows aggregate info;
  `clear` is the universal-hammer). v1 ships with `stats` + `clear`;
  `list` (enumerate entries) and `prune` (age-based eviction)
  can land post-v1.

### F07 — `bock pkg update`, `audit`, `publish`, `search`

- **Inventory row:** F07 (drift: spec ahead)
- **Classification:** C — Defer or trim spec
- **Spec says:** §20.1 lists `bock pkg` verbs including `update`,
  `audit`, `publish`, `search`.
- **Impl does:** `init`, `add`, `remove`, `tree`, `list`, `cache`
  exist (see also C04, C05). `update`, `audit`, `publish`,
  `search` are not implemented.
- **Recommendation:** Defer in spec. Most v1 package work to date
  has been the install/resolve loop; the remaining verbs depend on
  registry workflows that aren't v1-blocking:
  - `update` — depends on a stable lockfile format and resolver
  - `audit` — depends on a security-advisory feed (post-v1
    infrastructure)
  - `publish` — depends on a public registry and auth flow
  - `search` — depends on a registry search API
  Mark "Reserved; ships alongside the public registry" in §20.1.

### F08 — `bock model list`, `install`, `use`

- **Inventory row:** F08 (drift: spec ahead)
- **Classification:** C — Defer or trim spec
- **Spec says:** §20.1 lists `bock model` verbs as `list`,
  `install`, `use`.
- **Impl does:** `bock model show` and `bock model set` are
  implemented; the spec's three verbs are not
  (`compiler/crates/bock-cli/src/main.rs:402-414`).
- **Recommendation:** Defer in spec. v1's AI surface is
  remote-only (`[ai] provider`/`endpoint`/`api_key_env`); there's
  no local-model lifecycle to manage. Reshape §20.1 to describe
  what's actually shipped (`show`/`set`) and mark
  `list`/`install`/`use` as post-v1 (paired with local model
  support).

### F09 — `bock fix`

- **Inventory row:** F09 (drift: spec ahead)
- **Classification:** C — Defer or trim spec
- **Spec says:** §20.1 describes `bock fix — Auto-fix lint
  warnings.`
- **Impl does:** Not implemented.
- **Recommendation:** Defer in spec — mark "v1.x". Auto-fix
  semantics ("safe to apply, idempotent, source-preserving")
  warrant a small design pass before v1 lands them. Today's
  `bock check --lint` reports warnings; `bock fix` is its
  ergonomic counterpart and ships once the lint catalog stabilizes.

### F10 — `bock migrate`

- **Inventory row:** F10 (drift: spec ahead)
- **Classification:** C — Defer or trim spec
- **Spec says:** §20.1 describes `bock migrate — AI-assisted
  import from other languages.`
- **Impl does:** Not implemented.
- **Recommendation:** Defer in spec — mark "post-v1". This is a
  full feature requiring a per-source-language frontend plus the
  AI pipeline reverse-direction. v1 ships green-field Bock
  authoring; migration is a separate effort.

### F11 — `bock target`

- **Inventory row:** F11 (drift: spec ahead)
- **Classification:** C — Defer or trim spec
- **Spec says:** §20.1 describes `bock target — Target management
  (list, add, info).`
- **Impl does:** Not implemented. Target selection today happens
  via `bock build --target T`.
- **Recommendation:** Defer in spec — mark "post-v1". Target
  management as a top-level surface (registering custom targets,
  listing supported targets, inspecting target capabilities)
  doesn't have user pull today; the per-target `[targets.<T>]`
  config block in `bock.project` is the v1 surface for target
  customization.

### F12 — `bock ci`

- **Inventory row:** F12 (drift: spec ahead)
- **Classification:** C — Defer or trim spec
- **Spec says:** §20.1 describes `bock ci — Run all CI checks
  in one command.`
- **Impl does:** Not implemented.
- **Recommendation:** Remove from spec, or defer. Users can
  compose `bock fmt --check && bock check && bock test` from
  existing primitives; a packaged `bock ci` is convenience, not
  capability. Either drop from §20.1 or mark "Reserved" — both
  are defensible. Mild preference for keep-and-defer (the verb
  is good shorthand once it exists).

### F13 — `[project] authors`

- **Inventory row:** F13 (drift: spec ahead)
- **Classification:** C — Defer or trim spec
- **Spec says:** Appendix A includes `authors = ["team@example.com"]`.
- **Impl does:** Not read. `bock-cli/src/doc.rs::read_project_meta`
  reads `name` and `version` only.
- **Recommendation:** Defer in spec — mark "Reserved" or remove
  from the Appendix A example. `authors` becomes meaningful once
  publishing exists (F07); v1 doesn't need it.

### F14 — `[strictness.overrides]`

- **Inventory row:** F14 (drift: spec ahead)
- **Classification:** C — Defer or trim spec
- **Spec says:** Appendix A shows `[strictness.overrides]` with
  per-path glob mappings (`"src/experiments/**" = "sketch"`).
- **Impl does:** Reads `[strictness] default` only
  (`bock-cli/src/promote.rs::read_strictness`).
- **Recommendation:** Defer. v1 ships a flat project-level
  strictness default; per-path overrides are a layered concept
  that depends on a glob matcher and a strictness-resolution
  hierarchy that isn't built. Mark "Reserved" in Appendix A.

### F15 — `[paradigm]`

- **Inventory row:** F15 (drift: spec ahead)
- **Classification:** C — Defer or trim spec
- **Spec says:** Appendix A includes `[paradigm] default = "multi"`.
- **Impl does:** Not read.
- **Recommendation:** Remove from Appendix A. Bock is
  multi-paradigm by design (functional + OO + effectful); a
  project-level paradigm switch doesn't have semantic content
  the compiler acts on. If a future linting/style decision needs
  it, reintroduce then.

### F16 — `[targets]` (top-level)

- **Inventory row:** F16 (drift: spec ahead)
- **Classification:** C — Defer or trim spec
- **Spec says:** Appendix A shows `[targets] primary = "web"`
  with `additional = ["ios", "android"]`.
- **Impl does:** The impl reads per-target tooling config
  (`[targets.<T>]`) per §20.6.2 — *not* the top-level `[targets]
  primary/additional` block. The two have different semantics
  (top-level: which targets to build; per-target: how to scaffold
  each one).
- **Recommendation:** Defer in spec — replace the top-level
  `[targets]` example in Appendix A with the `[targets.<T>]`
  pattern that §20.6.2 actually describes. Target selection
  happens via `bock build --target T` / `--all-targets`, not via
  manifest-level primary/additional lists. Mark the
  primary/additional surface "Reserved" if it has future value.

### F17 — `[effects]` + `[effects.overrides.*]`

- **Inventory row:** F17 (drift: spec ahead)
- **Classification:** C — Defer or trim spec
- **Spec says:** Appendix A shows `[effects]` mapping effect names
  to handler symbols, with `[effects.overrides.test]` for test
  contexts.
- **Impl does:** Not read. Effect handlers are resolved via §10.3
  (call-site + module + project layers) which is implemented
  inline in code via `handling` blocks and module-level
  `handle ... with ...` declarations.
- **Recommendation:** Defer in spec — mark "Reserved" or remove
  from Appendix A. Project-level handler defaults are a layered
  feature on top of the inline mechanism; v1 ships the inline
  surface. Either drop the Appendix A example or note that the
  project-level handler defaults are forward-looking.

### F18 — `[plugins]`

- **Inventory row:** F18 (drift: spec ahead)
- **Classification:** C — Defer or trim spec
- **Spec says:** Appendix A shows `[plugins]` entries with version
  and capability grants. Appendix C describes the plugin system in
  detail.
- **Impl does:** Not read. No plugin loading mechanism exists.
- **Recommendation:** Mark consistent with Appendix C — Appendix C
  already implies plugins are forward-looking. Annotate the
  Appendix A `[plugins]` entry to point at Appendix C's
  description and note it's not active in v1. Or remove from
  Appendix A entirely and reintroduce when plugin loading lands.

### F19 — `[testing]`

- **Inventory row:** F19 (drift: spec ahead)
- **Classification:** C — Defer or trim spec
- **Spec says:** Appendix A shows `[testing] smart_target_threshold
  = 0.3`, `always_test = ["js"]`.
- **Impl does:** Not read. v1 testing is single-target via
  `bock test --filter` (see F05).
- **Recommendation:** Defer. The `[testing]` config block is paired
  with the unshipped test flags in F05 (`--smart`,
  `--all-targets`, etc.). Ships together post-v1. Remove from
  Appendix A or mark "Reserved" alongside §20.4's smart-target
  description.

### F20 — `[build]` (incl. `hooks`, `cache.remote`)

- **Inventory row:** F20 (drift: spec ahead)
- **Classification:** C — Defer or trim spec
- **Spec says:** Appendix A shows `[build] min_aura = "1.2.0"`
  (note: pre-rename, should be `min_bock`), `[build.hooks]`
  with pre-build scripts, `[build.cache] remote = "s3://..."`.
- **Impl does:** Not read. v1 has no build hooks and no remote
  cache.
- **Recommendation:** Defer. Three sub-decisions:
  1. **`min_aura` → `min_bock`:** rename in Appendix A regardless
     (§20 rename was 2026-04-20; Appendix A wasn't refreshed).
     This is a copy-edit fix bundled with the deferral.
  2. **`[build.hooks]`:** Defer; mark "Reserved". Hook semantics
     (pre-build vs. post-build, error propagation, sandboxing)
     warrant their own design pass.
  3. **`[build.cache] remote`:** Defer; mark "Reserved" pending
     a v1.x cache-server design.

### F21 — `[registries]`

- **Inventory row:** F21 (originally drift; reclassified)
- **Classification:** *Reclassified — aligned*
- **Spec says:** Appendix A shows `[registries] internal =
  "https://bock.company.internal"`.
- **Impl does:** **Reads** the `[registries]` section. The
  `default` field selects the default registry URL; named
  registries are flattened into a map for per-add overrides
  (`compiler/crates/bock-pkg/src/network.rs:366-400`,
  `parse_registries` / `default_registry_url`).
- **Conclusion:** Inventory error. F21 belongs on the aligned
  list, not the drift list. Surfaced via FOUND: tag in this
  session's commit message; no design chat action, no spec or
  impl change. The Appendix A example is consistent with the
  impl.

### F22 — `[dependencies]` at project root

- **Inventory row:** F22 (drift: spec/impl placement mismatch)
- **Classification:** A — Spec amendment to match impl
- **Spec says:** Appendix A shows `[dependencies] core-http = "^1.0"`
  in the `bock.project` example.
- **Impl does:** Reads `[dependencies]` from `bock.package`, not
  `bock.project` (`compiler/crates/bock-pkg/src/manifest.rs:18-24`).
  §19 (Package Manager) describes `bock.package` as the
  package-manifest file; the spec is internally inconsistent
  between §19 and Appendix A on where dependencies live.
- **Recommendation:** Fix Appendix A — move `[dependencies]` out
  of the `bock.project` example and into a `bock.package` example
  (or a cross-reference to §19). The spec internally treats
  `bock.project` as project config and `bock.package` as package
  manifest; the Appendix A example conflates them.

### K04 — Section file numbering vs `bock-spec.md` numbering

- **Inventory row:** K04 (numbering mismatch)
- **Classification:** F — Naming/organization decision
- **Issue:** `spec/sections/s01-lexical.md` through
  `s16-tooling.md` don't map cleanly to `bock-spec.md`'s §1–§23.
  Examples:
  - `s13-transpilation.md` covers `bock-spec.md` §17 (Transpilation
    Pipeline), not §13 (Concurrency).
  - `s14-stdlib.md` covers §18 (Standard Library), not §14
    (Interop and FFI).
  - `s15-packaging.md` covers §19 (Package Manager).
  - `s16-tooling.md` covers §20 (Tooling).
  - There are no section files for §13 (Concurrency), §14
    (Interop), §15 (Annotations), §16 (AIR), §22 (Target
    Profiles), or §23 (Appendices) — these only exist in
    `bock-spec.md`.
- **Resolution options:**
  - **Option 1: Rename section files to match `bock-spec.md`.**
    `s13-transpilation.md` becomes `s17-transpilation.md`, etc.
    Add missing section files for §13, §14, §15, §16, §22, §23.
    Pro: clean numerical correspondence after rename. Con: every
    changelog (18 of them) references section files by their
    current names; renaming creates link churn in historical
    changelogs that don't merit edits.
  - **Option 2: Document the mapping; don't rename.** Section
    files keep current names; add a translation table to
    `docs/src/reference/spec.md` mapping each `s##-` file to its
    `bock-spec.md` section number. New section files (for §13,
    §14, etc.) use the next free `s##-` number.
    Pro: no link churn, no changelog edits. Con: ongoing mental
    overhead for anyone cross-referencing.
- **Recommendation:** **Option 2**. Section files are excerpts
  per the Spec Authority Note ("`bock-spec.md` is the canonical
  assembled spec and is authoritative when it disagrees with
  section excerpts"); their numbering can be a separate naming
  scheme that doesn't pretend to mirror `bock-spec.md`. A
  documented mapping in `docs/src/reference/spec.md` (created in
  D3) is cheaper than retroactive renames.
- **Routing:** Design chat decision. Affects D2–D5
  cross-reference patterns: D2/D3 cite by `bock-spec.md`
  numbering (authoritative) with section files as reading-aid
  links; D5 may want to consolidate by removing the section
  files entirely if `bock-spec.md` is canonical.

---

## Closed without design-chat action

The following 4 rows are listed here for completeness. They
require no escalation to design chat:

| Row | Status | Disposition |
|-----|--------|-------------|
| F01 | E | Closed by Item B Phase 1 (`--deliverable`) |
| F02 | E | Closed by Item B Phase 1 (`--no-tests`) |
| F03 | E | Closed by Item B Phase 1 (`--optimize`) |
| F21 | Reclassified aligned | FOUND: tag; inventory error, no change needed |

---

## Expected output from design chat

The 24 design-chat-routed rows (7 A + 16 C + 1 F) should resolve
into one or more spec changelogs in `spec/changelogs/`:

- **A-class amendments** add named flags / commands to §20.1 or
  fix Appendix A's `bock.project` example.
- **C-class deferrals** trim §20.1 entries, mark "Reserved" in
  Appendix A, or remove unshipped surfaces from the spec.
- **F (K04)** records the section-file numbering decision and
  shapes how D2–D5 cite the spec.

D2–D5 should not start until those changelogs are merged. D6
(changelog backfill, per `docs/INVENTORY.md` cross-effort
dependencies) follows D1's resolutions.
