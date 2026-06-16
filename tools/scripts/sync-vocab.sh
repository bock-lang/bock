#!/bin/bash
# Sync compiler-owned vocabulary and spec markdown into the VS Code extension.
#
# Produces:
#   extensions/vscode/assets/vocab.json — `bock-dump-vocab --pretty` output
#   extensions/vscode/assets/spec/      — copy of the single-file spec (spec/bock-spec.md)
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

echo "==> Done"
echo "    vocab: $(wc -c <"$VOCAB_PATH") bytes"
echo "    spec files: $(find "$SPEC_DIR" -type f ! -name '.gitkeep' | wc -l)"
