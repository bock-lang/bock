#!/bin/bash
# Sync compiler-owned vocabulary and documentation markdown into the artifacts
# that ship it: the VS Code extension (reads its copies at runtime) and the
# `bock` CLI crate (compiles its copies into the binary with `include_str!`,
# so `bock mcp` can serve them as MCP resources).
#
# Produces:
#   extensions/vscode/assets/vocab.json     — `bock-dump-vocab --pretty` output
#   extensions/vscode/assets/spec/          — copy of the single-file spec (spec/bock-spec.md)
#   compiler/crates/bock-cli/assets/spec/   — same spec, for `include_str!`
#   compiler/crates/bock-cli/assets/context-pack/ — copy of context-pack/BOCK-CONTEXT-PACK.md
#   compiler/crates/bock-cli/assets/stdlib/ — copy of docs/src/reference/stdlib/core-*.md
#
# Why in-tree copies for the CLI: `cargo publish` packages only files inside
# the crate directory, so `include_str!("../../../../spec/bock-spec.md")`
# builds locally and produces a broken published crate; and reading from disk
# at runtime is wrong for `cargo install bock` users, who have no checkout.
# The generated copies make the served content version-locked to the binary by
# construction. Drift is guarded by the "vocab + spec assets in sync" CI job.
#
# Run after any change to the language, stdlib, diagnostics, or CLI surface.
# The extension consumes these files at runtime; the compiler is the single
# source of truth.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

ASSETS_DIR="extensions/vscode/assets"
VOCAB_PATH="$ASSETS_DIR/vocab.json"
SPEC_DIR="$ASSETS_DIR/spec"
SPEC_SOURCE="spec/bock-spec.md"

mkdir -p "$ASSETS_DIR"

echo "==> Building bock-dump-vocab (release)"
cargo build --release -p bock-vocab --bin bock-dump-vocab

# Honor CARGO_TARGET_DIR (the worktree session pattern sets it per-branch);
# fall back to ./target when unset. `cargo build` just wrote the binary there.
TARGET_DIR="${CARGO_TARGET_DIR:-target}"

echo "==> Writing $VOCAB_PATH"
"$TARGET_DIR/release/bock-dump-vocab" --pretty --output "$VOCAB_PATH"

echo "==> Syncing $SPEC_DIR/bock-spec.md"
# The spec was consolidated into a single file (spec/bock-spec.md) by the K04
# spec-consolidation; the pre-K04 sectioned spec/sections/ layout no longer
# exists. The extension's spec panel (src/features/spec-panel.ts) reads
# assets/spec/bock-spec.md, so copy that one file in. Clear any stale
# per-section files left over from the old layout while preserving the tracked
# .gitkeep that keeps the directory in git.
mkdir -p "$SPEC_DIR"
find "$SPEC_DIR" -type f ! -name '.gitkeep' -delete
cp "$SPEC_SOURCE" "$SPEC_DIR/bock-spec.md"

CLI_ASSETS_DIR="compiler/crates/bock-cli/assets"
PACK_SOURCE="context-pack/BOCK-CONTEXT-PACK.md"
STDLIB_DOCS_DIR="docs/src/reference/stdlib"

echo "==> Syncing $CLI_ASSETS_DIR (MCP resource payloads)"
# Regenerate from scratch so a deleted source file cannot linger as a stale
# asset. `include_str!` then turns any rename/removal into a build error.
rm -rf "$CLI_ASSETS_DIR/spec" "$CLI_ASSETS_DIR/context-pack" "$CLI_ASSETS_DIR/stdlib"
mkdir -p "$CLI_ASSETS_DIR/spec" "$CLI_ASSETS_DIR/context-pack" "$CLI_ASSETS_DIR/stdlib"
cp "$SPEC_SOURCE" "$CLI_ASSETS_DIR/spec/bock-spec.md"
cp "$PACK_SOURCE" "$CLI_ASSETS_DIR/context-pack/BOCK-CONTEXT-PACK.md"
cp "$STDLIB_DOCS_DIR"/core-*.md "$CLI_ASSETS_DIR/stdlib/"

echo "==> Done"
echo "    vocab: $(wc -c <"$VOCAB_PATH") bytes"
echo "    spec files: $(find "$SPEC_DIR" -type f ! -name '.gitkeep' | wc -l)"
echo "    cli assets: $(find "$CLI_ASSETS_DIR" -type f | wc -l) files, \
$(find "$CLI_ASSETS_DIR" -type f -exec cat {} + | wc -c) bytes"
