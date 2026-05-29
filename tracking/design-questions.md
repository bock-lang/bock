# Design Questions — open decisions for Design

**The one question:** what should the behavior be? (undecided choices
awaiting a Design call)

Not factual mismatches (→ `divergences.md`) and not actionable work
(→ `queue.md`). A divergence whose disposition needs a decision links
here. Migrated from the open D1-refresh / D2-FOUND rows (most are now
decided — see `## Decided`).

Schema: `[ID] question · § · context · status(open | routed-to-Design |
decided→link)`

---

## Open

### DQ1 — `bock check` default strictness
- **Question:** should `bock check` default to the project's configured
  strictness instead of requiring explicit `--strict`?
- **§:** §20.1 · **context:** O1/O2 landed (#87) keeping `--strict`
  explicit, mirroring `bock build`. Parked as a smaller follow-up.
- **Status:** open

### DQ2 — `@performance` budget literal syntax
- **Question:** should `@performance(max_latency: 100, max_memory: 50)`
  accept bare integers, or require unit-suffixed literals (`100.ms`,
  `50.mb`)?
- **§:** §11.4 · **context:** the context-audit example uses bare ints
  → E8003 from the checker. Decide whether the example is wrong or the
  spec/checker should accept bare ints. Blocks `queue.md Q-perf-example`.
- **Status:** open

### DQ3 — §13.3 channels: bounded-channel v1 status
- **Question:** are bounded channels (`Channel.new(buffer: N)`) v1, or
  Reserved for v1.x?
- **§:** §13.3 · **context:** see `divergences.md` DV2 — the spec lists
  channels as plain v1 with no Reserved marker, but the 0449 changelog
  asserts a (never-applied, no-changelog) Reserved decision. Resolve
  the actual decision, then DV2 reconciles spec vs changelog.
- **Status:** open

### DQ4 — §13.4 synchronization primitives: v1 vs Reserved
- **Question:** which of `Mutex/RwLock/Atomic/WaitGroup/OnceCell` ship
  in v1 vs Reserved for v1.x?
- **§:** §13.4 · **context:** same unapplied-0449-claim as DQ3 (DV2).
- **Status:** open

### DQ5 — §18.3 Core Modules: what did D1-refresh route?
- **Question:** confirm whether the D1-refresh §18.3 row was a
  substantive v1-vs-Reserved decision (still open) or merely the
  historical `core.time` expansion (already resolved, 20260408-0900).
- **§:** §18.3 · **context:** uncertain in the reconciliation; resolve
  before treating as open. Related to DV1 (stdlib) / Q-stdlib.
- **Status:** open (pending confirmation)

---

## Decided (D1-refresh / D2-FOUND — resolved by the spec-revision cycle)

22 of the 25 routed rows are resolved (links in `divergences.md`
"Resolved"): §13.5, §14.1/2, §16.3/4, §20.3, §20.5, §10.3/4/6, §18.5,
§19.7, §20.6, §1.3, §15, §17.6, §12.2 (×2), §4.7, §7.6, §6.1, §11
module-level annotations. Evidence: the per-section changelogs + the
20260514-0548 spec-revision artifact (confirmed applied to the live
spec). Only DQ3/DQ4/DQ5 above remain from that cycle.
