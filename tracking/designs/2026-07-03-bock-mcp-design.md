# Design Brief — bock-mcp and the agent-facing ecosystem — 2026-07-03 04:19 UTC

> **Hub integration (orchestrator, 2026-07-03 04:28 UTC):** routed per §8 —
> recorded here verbatim; `queue.md` gains `Q-cli-format-json` (ready,
> prerequisite) + `Q-mcp-pack-resources` (blocked), and `Q-mcp-server` is
> updated (its "scoping pass" is DONE: this brief decides the `bock mcp`
> subcommand delivery, §5). `milestones.md` v1.1 R4 bullet elaborated.
> No spec change (per §7); no design gate (§20.1 non-normative CLI shape,
> DQ1 precedent). Two forward gates recorded in the items: a one-pass Design
> review of the tool schemas, and the MCP protocol-dependency choice
> (escalates — provider/tooling). Positioning hook stays marketing-owned.

**From:** Design chat
**To:** Operator + Orchestrator
**Re:** How a Bock MCP server fits the ecosystem offerings, relative to the context pack (elaborates audit R4)
**Status:** Design sketch for discussion → roadmap/queue routing; not a spec change

---

## 1. The fit: two halves of agent enablement

The context pack and an MCP server solve different halves of the same problem, and each is weak without the other:

- **Context pack = knowledge.** Static, universal, zero-infrastructure. Teaches a model *what Bock is* — syntax, idioms, stdlib surface, error codes. Works in any model context, no server required. It closes the model-familiarity gap at generation time.
- **bock-mcp = capability.** Dynamic, verb-shaped. Gives an agent *things it can do* — check, run, test, build, inspect, conformance. It closes the loop at verification time.

A model with the pack but no tools writes plausible Bock it cannot verify. A model with tools but no pack burns loop iterations rediscovering syntax through error messages. Together they produce the actual 2026 agentic workflow — informed generation, grounded verification — running on Bock. The audit's framing applies directly: the pack is training data delivered at prompt time; the MCP is diagnostics-as-affordance delivered at act time.

## 2. The key integration: the MCP server *serves* the pack

MCP has three primitives — tools, resources, prompts — and the resources primitive collapses what would otherwise be two maintained offerings into one. The context pack's contents (spec sections by §-number, per-module stdlib reference, idiom guide, error-code table) become MCP **resources** the agent pulls on demand: the exact slice it needs, when it needs it, instead of the user front-loading the whole pack into context.

One artifact, two access modes:

| Mode | Mechanism | Where it works |
|------|-----------|----------------|
| Static | pack file(s) pasted/attached into context | every model, everywhere, no infra |
| On-demand | MCP resources + an `explain` tool | MCP-capable agents, context-efficient |

The pack remains the single maintained source; the server reads it. No divergence risk, and the pack's versioning (it should version with the compiler) carries over for free.

## 3. Tool surface — v1 sketch

Small and high-value; every tool is a thin wrapper over the CLI:

| Tool | Wraps | Returns | Note |
|------|-------|---------|------|
| `bock_check` | `bock check` | structured diagnostics: code, span, message, suggestion | the workhorse of the repair loop |
| `bock_run` | `bock run` (interpreter) | stdout/stderr/exit | fast semantic ground truth, no target toolchains needed |
| `bock_test` | `bock test` (interpreter) | structured pass/fail per test | |
| `bock_build` | `bock build --target T` | output tree or structured errors | |
| `bock_conformance` | conformance harness | per-target behavior comparison for a file/fixture | **the moat as a verb** — see §4 |
| `bock_inspect` | `bock inspect` | decision-manifest entries | the governance story, agent-native |
| `bock_explain` | pack error-code table | explanation + fix pattern for `E____` | resource-backed |

Plus resources per §2. MCP prompts: skip in v1 (low value relative to tools/resources; add later if demand appears).

## 4. The differentiator tool

`bock_conformance` deserves to exist even though it's the most expensive tool on the list, because it is the one verb no other language can offer an agent: *"does this program behave identically on every target?"* as a single tool call. It operationalizes the 1.0 positioning (provable equivalence) inside the agent loop itself — an agent porting logic into Bock can *demonstrate* equivalence as part of its own workflow, and cite the result in its PR. v1 scope can be modest: single-file/fixture exec-compare across locally configured targets, clearly reporting which targets were exercised. Cost and toolchain-availability caveats go in the tool description so agents budget accordingly.

## 5. Delivery: a `bock mcp` subcommand, not a separate binary

Recommend shipping the server inside the CLI: `bock mcp` (stdio transport). Rationale: zero additional install (agent config points at the binary the user already has), version-locked to the compiler by construction (no server/compiler skew), and no separate release artifact to maintain. §20.1 already states the CLI's command shape is expected to evolve and is non-normative, so this is an orchestrator/operator-track addition (DQ1 precedent), not a design gate. The server layer stays deliberately thin over the CLI so that if the agent-protocol landscape shifts post-MCP, the migration cost is a transport shim, not a rewrite.

## 6. The enabling work (the real engineering in this brief)

The MCP tools want **structured output**, and today the CLI emits human text. The right fix is at the substrate: `--format json` on `check` / `test` / `inspect` (minimum), emitting the diagnostics as data — code, span, message, suggestion. Two consumers beyond MCP make this worth doing once and well: the LSP already needs structured diagnostics internally (share the layer; never parse human text), and CI users have asked for machine-readable output in every language ecosystem eventually. Sequencing: the JSON layer is the prerequisite queue item; the MCP server is a thin second item on top of it; the pack-as-resources wiring is a third, smallest item.

## 7. Guardrails and honest notes

- **Execution safety:** `bock_run`/`bock_test`/`bock_build` execute code and write to the workspace — normal local-dev-tool semantics, same trust envelope as the CLI itself, but the tool descriptions should say so plainly (agent frameworks surface these descriptions for permissioning).
- **Not a design-authority surface:** nothing here changes language semantics; there is no spec change in this brief beyond, eventually, a one-line §20.3-adjacent note when the roadmap item lands (the audit's R4 §20.3 reorientation covers it).
- **Validation status:** like everything agent-facing, the proof is usage. The first consumer should be our own orchestrator/engineer sessions — dogfooding the MCP in this project's own loops (audit R8 spirit) before positioning it externally.

## 8. Routing

| Piece | Owner |
|-------|-------|
| CLI `--format json` layer | Orchestrator → engineer session (non-core CLI shape; no design gate) |
| `bock mcp` subcommand + tool schemas | Orchestrator → engineer session; tool schema review by Design (one pass, cheap) |
| Pack-as-resources wiring | Follows the above; smallest item |
| Positioning ("the language that ships as an agent tool" or similar) | Marketing chat — hook noted here, wording theirs |
| §20.3 note | Design changelog when the roadmap item lands, per audit R4 |
