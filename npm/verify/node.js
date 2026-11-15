// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

// Node entry. The wasm is loaded synchronously on import; `init()` exists
// so code written for the browser entry runs unchanged.

import { readFileSync } from "node:fs";
import * as raw from "./pkg/tensorseed_wasm.js";
import { makeApi } from "./api.js";

export { WELL_KNOWN_CLAIM, WELL_KNOWN_REVOCATIONS, claimUrl, revocationsUrl } from "./api.js";

raw.initSync({ module: readFileSync(new URL("./pkg/tensorseed_wasm_bg.wasm", import.meta.url)) });

export async function init() {}

const api = makeApi(raw, () => true);
export const {
  checkClaim,
  checkClaimWithCatalog,
  openCatalog,
  verifyResponse,
  checkEndpoint,
  verifyFollowList,
  fetchFollowList,
  claimSigningMessage,
  revocationSigningMessage,
  recoverWallet,
} = api;
export const CatalogVerifier = raw.CatalogVerifier;
