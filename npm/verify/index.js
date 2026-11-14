// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

// Browser and bundler entry. Call `await init()` once before anything
// else; it fetches pkg/tensorseed_wasm_bg.wasm next to this file, or takes
// a URL, Response, or the wasm bytes.

import initWasm, * as raw from "./pkg/tensorseed_wasm.js";
import { makeApi } from "./api.js";

export { WELL_KNOWN_CLAIM, WELL_KNOWN_REVOCATIONS, claimUrl, revocationsUrl } from "./api.js";

let ready = false;

export async function init(wasm) {
  if (ready) return;
  await initWasm(wasm === undefined ? undefined : { module_or_path: wasm });
  ready = true;
}

const api = makeApi(raw, () => ready);
export const {
  checkClaim,
  checkClaimWithCatalog,
  openCatalog,
  verifyResponse,
  checkEndpoint,
  claimSigningMessage,
  revocationSigningMessage,
  recoverWallet,
} = api;
export const CatalogVerifier = raw.CatalogVerifier;
