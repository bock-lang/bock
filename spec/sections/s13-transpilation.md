# Spec Excerpt: Transpilation Pipeline

## Pipeline Stages
Source → Parse → Type Check → Context Resolve → Target Analyze
→ Code Generate → Verify → Target Compile → Assemble Deliverable

## Three-Tier Generation (AI involvement, §17.2)
- Tier 1 (default): AI generates idiomatic target code from AIR,
  invoked selectively at capability gaps (§17.6) — not every node
- Tier 2 (fallback): Rule-based deterministic transpilation,
  handles the common case and AI fallback
- Tier 3 (optional): AI optimization pass

## Output Modes (output completeness, §20.6.2)
Distinct concept from generation tiers. Selected by `bock build` flag:
- **Source mode** (`--source-only`): bare target source files,
  source-mirrored structure, no scaffolding
- **Project mode** (default): source files + target-ecosystem
  scaffolding (package.json / Cargo.toml / pyproject.toml /
  go.mod / etc.) + transpiled tests in target test framework
  (Vitest, pytest, cargo test, etc.). Runnable in target's
  normal toolchain
- **Deliverable mode** (`--deliverable`): final runnable artifact
  (bundled JS, compiled binary, container, mobile package, etc.);
  see §17.5 for deliverable types

Default is project mode. Output filename mirrors source: `src/<path>.bock`
→ `build/<target>/<path>.<ext>`. Per-target scaffolding contents
documented in each target's codegen package.

Project mode specifics:
- bin/lib/both inferred from `src/main.bock` and `src/lib.bock`;
  `[project] type` overrides
- per-target naming defaults to language conventions
  (kebab-case for npm, snake_case for Python/Rust, placeholder
  for Go modules); `[targets.<T>]` sections override
- lockfiles NOT generated (target package manager owns this);
  scaffolded README instructs users on first install
- transpiled tests included by default; `--no-tests` opts out
- §20.4 Tier 2 transpilation tests are the validation gate for
  project mode trustworthiness

## Target Profile
```
TargetProfile {
  id, capabilities: {
    memory_model: GC | ARC | Manual
    null_safety: Bool
    algebraic_types: Native | Emulated | None
    async_model: EventLoop | GreenThread | OSThread
    generics: Reified | Erased | Monomorphized
    pattern_matching: Native | SwitchBased | Emulated
    traits: Native | InterfaceBased | Emulated
  },
  conventions: { naming, error_handling, indent }
}
```

## Capability Gap Resolution
| AIR Construct  | Gap Example      | Synthesis             |
|----------------|------------------|-----------------------|
| Algebraic types| JS (no ADTs)     | Tagged objects+switch |
| Pattern match  | Go (no match)    | if/else chains        |
| Ownership/Move | JS, Python (GC)  | Erase annotations     |
| Effects        | All targets      | Parameter passing     |

## Effect Transpilation (Universal)
Effects → extra function parameters.
```
Bock: fn process(data) with Log, Clock
JS:   function process(data, { log, clock })
Rust: fn process(data: Data, log: &dyn Log, clock: &dyn Clock)
Go:   func Process(data Data, log Logger, clock Clock)
```

## Decision Manifest
Every AI decision logged: module, target, decision key, choice,
alternatives, reasoning, model, confidence (float 0.0–1.0),
pinned status. Default acceptance threshold: 0.75 (configurable).

Split by lifecycle:
- Build decisions (`.bock/decisions/build/`): codegen choices,
  committed to VCS
- Runtime decisions (`.bock/decisions/runtime/`): adaptive handler
  selections (§10.8), environment-local, not committed
`bock inspect` shows build by default; `--runtime` for runtime;
`--all` for both. `bock override --promote` moves runtime pin
to build.

## AI Provider Interface
The compiler calls AI models through a provider-agnostic interface
with four interaction modes:
- **Generate:** AIR + target profile → target code (Tier 1)
- **Repair:** failing code + compiler error + AIR → fixed code + optional rule (§17.7)
- **Optimize:** working code + AIR → improved code (Tier 3)
- **Select:** closed option set + context → choice identifier (§10.8 adaptive handlers)

Select returns a classification from a provided set, never
generated code. Trait return type enforces the closed-set constraint.

Verification (§17.3) is separate — always deterministic, never
involves the AI provider.

Two built-in providers: OpenAI-compatible (Chat Completions format,
covers most providers including local runtimes) and Anthropic
Messages API (native, enables extended thinking and structured
content blocks). Configured via `[ai]` section in bock.project.
