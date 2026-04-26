#!/bin/bash
# Sync compiler-owned vocabulary and spec markdown into the VS Code extension.
#
# Produces:
#   extensions/vscode/assets/vocab.json — `bock-dump-vocab --pretty` output
#   extensions/vscode/assets/spec/      — copy of spec/sections/
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

mkdir -p "$ASSETS_DIR"

echo "==> Building bock-dump-vocab (release)"
cargo build --release -p bock-vocab --bin bock-dump-vocab

echo "==> Writing $VOCAB_PATH"
./target/release/bock-dump-vocab --pretty --output "$VOCAB_PATH"

echo "==> Syncing $SPEC_DIR"
rm -rf "$SPEC_DIR"
cp -r spec/sections "$SPEC_DIR"

echo "==> Done"
echo "    vocab: $(wc -c <"$VOCAB_PATH") bytes"
echo "    spec files: $(find "$SPEC_DIR" -type f | wc -l)"
