#!/usr/bin/env bash
# Build tensorseed-wasm for the explorer and emit wasm-bindgen web bindings.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "${ROOT}/.." && pwd)"
OUT="${ROOT}/src/pkg"
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
mkdir -p "${OUT}"
wasm-bindgen "${TARGET}" --out-dir "${OUT}" --target web --typescript

if command -v wasm-opt >/dev/null; then
  wasm-opt -O --enable-bulk-memory -o "${OUT}/tensorseed_wasm_bg.wasm" "${OUT}/tensorseed_wasm_bg.wasm"
fi

# Integrity hashes for first-party glue (logged; the page uses same-origin hashed Vite assets).
if command -v sha384sum >/dev/null; then
  echo "SRI (sha384):"
  sha384sum "${OUT}"/tensorseed_wasm.js "${OUT}"/tensorseed_wasm_bg.wasm | awk '{print "  sha384-" $1, $2}'
fi
