#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build crates/tensorseed-wasm and emit the wasm-bindgen glue into pkg/.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "${ROOT}/../.." && pwd)"
OUT="${ROOT}/pkg"
TARGET="${REPO}/target/wasm32-unknown-unknown/release/tensorseed_wasm.wasm"

if ! command -v rustup >/dev/null || ! rustup target list --installed | grep -q '^wasm32-unknown-unknown$'; then
  rustup target add wasm32-unknown-unknown
fi
if ! command -v wasm-bindgen >/dev/null; then
  echo "wasm-bindgen is not on PATH. Install wasm-bindgen-cli, for example:" >&2
  echo "  cargo install wasm-bindgen-cli --locked --version 0.2.130" >&2
  exit 1
fi

cargo build -p tensorseed-wasm --target wasm32-unknown-unknown --release --manifest-path "${REPO}/Cargo.toml"
rm -rf "${OUT}"
mkdir -p "${OUT}"
# One build for every runtime: the web target's ES module takes the wasm
# bytes in Node (node.js) and fetches them in a browser (index.js).
wasm-bindgen "${TARGET}" --out-dir "${OUT}" --target web --no-typescript
if command -v wasm-opt >/dev/null; then
  wasm-opt -O --enable-bulk-memory -o "${OUT}/tensorseed_wasm_bg.wasm" "${OUT}/tensorseed_wasm_bg.wasm"
fi
# The published package carries the repository license.
cp "${REPO}/LICENSE" "${ROOT}/LICENSE"
echo "built ${OUT} ($(wc -c <"${OUT}/tensorseed_wasm_bg.wasm") bytes of wasm)"
