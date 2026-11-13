// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

import init, {
  CatalogVerifier,
  check_coin_rpc,
  parse_manifest as wasm_parse_manifest,
} from "../pkg/tensorseed_wasm.js";
import wasmUrl from "../pkg/tensorseed_wasm_bg.wasm?url";

let ready: Promise<void> | null = null;

export function initWasm(): Promise<void> {
  if (!ready) {
    ready = init({ module_or_path: wasmUrl }).then(() => undefined);
  }
  return ready;
}

export { CatalogVerifier, check_coin_rpc, wasm_parse_manifest };
