# Handoff — model-bench fleet validation (→ Windows-side agent / operator)

**Filed:** 2026-08-31 06:22 UTC
**Queue item:** Q-model-bench
**Blocks:** first scored benchmark run; R8 dogfooding measurement quality

## What this is

`tools/model-bench/` (PR #497, design PR #496) benchmarks locally-served
models as the Claude Code backend for Bock dogfooding. The code is
written, unit-tested, and gate-clean — but it has **never spoken to a
real model.** Every test runs against a mock upstream.

This handoff exists because the split is environmental, not editorial.
The session that wrote the harness runs in a Docker container that
cannot reach llama-server: `/.dockerenv` present, no `/mnt/c`, no
Windows interop, 172.17.0.0/16 bridge. `lls_up tiny` succeeded through
the MCP broker while HTTP to port 8175 failed from `127.0.0.1`, the
default gateway, and `10.255.255.254` alike. Everything below needs a
machine that can actually reach port 8160.

## Source documents (read these first)

- `.claude/specs/2026-08-31-local-model-agent-benchmark-design.md` — the
  design, including the safety finding that motivates it (PR #496)
- `.claude/plans/2026-08-31-local-model-agent-benchmark.md` — the
  implementation plan (PR #496)
- `tools/model-bench/README.md` — how to run it (PR #497)

## Already decided (context, NOT for re-litigation)

- **Backend per model, measured, not uniform:** `rocm` for
  `qwopus-coder`, `vulkan` for `flash-next-c`.
- **Sampling per model card.** Not tuned, not equalised. `qwopus-coder`
  carries `presence_penalty 1.5`; that is the card value and stays.
- **Three runs per model per task.** This fleet has shown a 3-point
  spread on identical prompts; n=1 is not reportable.
- **Sandboxing is not optional.** Throwaway clone, no credentials in the
  environment. Runs use `--dangerously-skip-permissions`, which is
  exactly what makes the disposition axes meaningful.
- **Context equalised at `-c 65536`** for both contenders (see ask 1).
- **Destruction and overclaiming are vetoes, not deductions.**

## The ask

Six items. (1)–(4) block any scored run; (5)–(6) block a *trustworthy*
one.

### 1. Set `-c 65536` on both entries — VERIFY it takes

Both `qwopus-coder` and `flash-next-c` are currently at `-c 32768`.
That is unworkable against this repo: Claude Code's system prompt plus
tool schemas is ~12–17k tokens and `CLAUDE.md` another ~3.7k, so ~16–21k
of the 32k window is consumed before any work begins.

**Verify:** `/props` on each server reports the context actually in
effect. `--fit off` is already required on `flash-next-c` because
llama.cpp mis-sizes that architecture; confirm the raised context did
not silently get re-fitted.

### 2. Confirm `flash-next-c`'s KV fits at 65536 — VERIFY, do not assume

At 56.7 GB resident in a 96 GB carve-out there is ~39 GB of headroom,
which should be ample, but this is the machine where a 103.7 GB model
spilled ~24 GB into system RAM and WSL would not start. **Anything that
touches the system-RAM ledger is disqualified regardless of quality.**

If it does not fit, say so and stop — do not quietly drop to a lower
context for one model only. Unequal context is a 2× working-memory
handicap being measured as model quality.

### 3. Pin `flash-next-c`'s backend to `vulkan`

Its entry currently sets no backend, despite Vulkan measuring 18.21 t/s
settled against ROCm's 14.96.

### 4. Re-measure throughput at 65536 for both — the old numbers are dead

KV size moves decode speed, so every figure taken at 32768 is invalid at
65536. Re-measure and record: **decode t/s, prefill t/s, MTP acceptance
rate, and mean draft length.**

These feed `harness/run.py` via `--decode-tps`, `--prefill-tps`,
`--mtp-acceptance`, `--backend`, `--engine-build`, `--quant`,
`--context`, which stamp every run record. They are per-run rather than
per-model on purpose: a mid-campaign re-measurement must not
retroactively rewrite older rows.

Note for context: `qwopus-coder` accepts ~11 points more MTP draft on
ROCm than Vulkan on identical settings. That is unexplained and may be a
numerical difference between the draft and verification paths. Not a
blocker; worth watching if acceptance moves at the new context.

### 5. Run the pre-flight tool-call gate and READ THE WIRE LOG

**This is the item most likely to change the project's conclusions.**

For each model, run one trivial task (add a doc comment to a named
function) and inspect `shim.jsonl`. Confirm that `Read` → `Edit` →
`Bash` round-trips with arguments intact — specifically that `Edit`
calls arrive with `old_string` complete and verbatim.

If a tool call is malformed or dropped, **the model is not ready to
benchmark.** Fix transport first. Otherwise a translation bug gets
recorded as model quality, which is the single confound this entire
design exists to rule out.

Two places can mangle a call, and they fail identically from outside:

1. **llama.cpp's built-in template/grammar parser.** The Qwen3.6-family
   chat-template problem lives here — tool-call schema formatting and
   reasoning-tag leakage. One publisher in that family recommends a
   replacement chat template specifically for this. Whether llama.cpp's
   parser handles the format correctly under Claude Code is **unknown
   and untested.**
2. **The shim's own translation** of `tool_use`/`tool_result` blocks.
   Unit-tested against a mock, never against a real model.

First suspect if `qwopus-coder` specifically fails: `presence_penalty
1.5` penalises token reuse, and an `Edit` call must repeat long verbatim
`old_string` arguments. Report it rather than changing the card value.

### 6. Decide the background-model route

Every fleet entry except `tiny` uses port 8160, so the contenders cannot
run concurrently without an override. Serial runs are correct for clean
t/s anyway — but Claude Code fires a cheap background/`haiku`-class call
that, unrouted, lands on the model under test and corrupts every
wall-clock number.

The shim already detects and routes this class. Either give it somewhere
to go (`--background-upstream` / `--background-alias`, e.g. `gemma-26b`
on an overridden port), or accept the distortion and **record that
choice alongside the results** so the numbers are read correctly.

If routing to `gemma-26b`: its entry sets no sampling flags and no
`--spec-type`, so it may be running unrecorded defaults. It also has a
known thinking-runaway requiring
`chat_template_kwargs.enable_thinking=false`. Resolve both before using
it for anything measured.

## One open decision for the operator, not the Windows agent

The design chose claude-code-router with a logging tee; the
implementation built a hand-written shim instead, because the router
could not be verified from the container (no upstream reachable → no
wire log → nothing to judge it by). The harness talks to a URL, so the
router remains a drop-in swap. **Confirm or reverse on PR #497.**

## What comes back

Per model: the `/props` context actually in effect, the four throughput
figures at 65536, a verdict on the pre-flight gate with the wire-log
evidence, and the background-route decision. That is enough to start
scored runs.
