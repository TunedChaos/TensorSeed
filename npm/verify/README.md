# @tensorseed/verify

Verify Tensorseed served-weights claims, claim revocations, signed
inference responses, and signed catalogs in a browser or Node. It is the
WebAssembly build of the `tensorseed-verify` Rust crate, the same code the
`tensorseed` CLI runs for `check-claim` and `verify-response`. It holds no
keys and makes no network requests except through `checkEndpoint`, which
uses the `fetch` you give it.

License: AGPL-3.0-or-later. Copyright Tuned Chaos.

## Install

```sh
pnpm add @tensorseed/verify
```

To build from a checkout (needs Rust, the `wasm32-unknown-unknown` target,
and `wasm-bindgen-cli` 0.2.130):

```sh
cd npm/verify
pnpm test        # builds pkg/ from crates/tensorseed-wasm, then runs the Node tests
```

## Usage

In Node the wasm loads on import. In a browser or bundler, `await init()`
once first (it fetches `pkg/tensorseed_wasm_bg.wasm` next to the module, or
takes a URL or the bytes).

```js
import { init, openCatalog, checkEndpoint, verifyResponse } from "@tensorseed/verify";

await init();

// 1. Before listing an endpoint: check its claim against a catalog you trust.
//    The snapshot and head are signed files you fetched from your mirror.
const { verifier } = openCatalog({ name: "main", pubkey: PUBLISHER_KEY, snapshot, head });
const report = await checkEndpoint("https://seller.example/v1", {
  catalog: verifier,
  requireWalletSig: true,
  requireExpiry: true,
});
if (report.verdict !== "pass") throw new Error("claim did not verify");
// Store report.claim_sha256, report.seller_wallet and report.expires_at.

// 2. For each response: send your own X-Tensorseed-Request-Id, read the
//    body as bytes, then verify the signature headers.
const res = await fetch("https://seller.example/v1/chat/completions", {
  method: "POST",
  headers: { "content-type": "application/json", "x-tensorseed-request-id": requestId },
  body: JSON.stringify(request),
});
const body = new Uint8Array(await res.arrayBuffer());
const result = verifyResponse({
  claim,            // the claim JSON you checked in step 1
  headers: res.headers,
  body,
  requestId,
  requestedModel: request.model,
  revocations,      // optional: the seller's revocations.json, merged with yours
});
if (!result.valid) console.warn("unsigned or altered response:", result.error);
```

`checkClaim(claim, options)` runs every check that needs no catalog, so its
best verdict is `inconclusive`. A claim past its `expires_at`, or revoked
by an entry in a `ts.claim-revocations/1` list, fails. A response signed
after the claim expired or was revoked fails with `claim_expired` or
`claim_revoked`; a `key_compromised` revocation fails every response under
that claim.

The report is the `ts.claim-check/1` JSON that `tensorseed check-claim
--json` prints. See `docs/INTEGRATE-A-MARKETPLACE.md` in the repository for
what each check means.
