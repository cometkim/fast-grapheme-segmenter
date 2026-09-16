#!/usr/bin/env bash
# Build the WebAssembly module.
#
#   ./build.sh             raw C ABI module -> grapheme-segmenter.wasm (~41 KB)
#   ./build.sh --bindgen   additionally builds the wasm-bindgen pkg/ and
#                          pkg-node/ targets (typed JS/TS API)
#
# Profile overrides ride in through the environment so the workspace's
# normal release profile — which the benchmarks measure against — stays put.
set -euo pipefail
cd "$(dirname "$0")"

export CARGO_PROFILE_RELEASE_LTO=true
export CARGO_PROFILE_RELEASE_PANIC=abort
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1

WASM=target/wasm32-unknown-unknown/release/fast_grapheme_segmenter_wasm.wasm

cargo build --release --target wasm32-unknown-unknown
cp "$WASM" grapheme-segmenter.wasm

# Optional size pass; the module works either way.
if command -v wasm-opt >/dev/null; then
  wasm-opt -Oz --strip-debug grapheme-segmenter.wasm -o grapheme-segmenter.wasm.tmp
  mv grapheme-segmenter.wasm.tmp grapheme-segmenter.wasm
fi

if [[ "${1:-}" == "--bindgen" ]]; then
  cargo build --release --target wasm32-unknown-unknown --features bindgen
  wasm-bindgen --target web --out-dir pkg "$WASM"
  wasm-bindgen --target nodejs --out-dir pkg-node "$WASM"
  echo "bindgen pkg:"
  ls -l pkg pkg-node | grep -v '^d\|^total'
fi

ls -l grapheme-segmenter.wasm
