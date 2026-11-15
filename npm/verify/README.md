# @tensorseed/verify

Verify Tensorseed served-weights claims, claim revocations, signed
inference responses, signed catalogs, and signed follow lists in a
browser or Node. It is the
WebAssembly build of the `tensorseed-verify` Rust crate, the same code the
`tensorseed` CLI runs for `check-claim` and `verify-response`. It holds no
keys and makes no network requests except through `checkEndpoint` and
`fetchFollowList`, which use the `fetch` you give them.

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
  responseUrl: "https://seller.example/v1/chat/completions", // must be under public_base_url
  revocations,      // optional: the seller's revocations.json, merged with yours
});
if (!result.valid) console.warn("unsigned or altered response:", result.error);
```

`checkClaim(claim, options)` runs every check that needs no catalog, so its
best verdict is `inconclusive`. A claim past its `expires_at`, or revoked
by an entry in a `ts.claim-revocations/1` list, fails. A response signed
after the claim expired or was revoked fails with `claim_expired` or
`claim_revoked`; a `key_compromised` revocation fails every response under
that claim. `checkEndpoint` also fetches the seller's revocation list
and merges it with any `revocations` you pass; when it cannot be fetched
or does not parse, a verdict that would pass is `inconclusive`, since the
claim may be revoked. `noRevocations: true` skips it.
`verifyResponse` fails with `url_mismatch` when `responseUrl` is not
under the claim's `public_base_url`, or when `originUrl` (where you
fetched the claim) is not the claim's own well-known URL. `maxSkewSecs`
is 1 to 604800 (default 300); for a stored response, pass the time you
received it as `now` instead of widening the window.

### Follow lists

A follow list (`ts.follow-list/1`) is a signed list of catalogs, each a
name, a publisher key, and a base URL, that someone publishes for others
to follow. Pin the list signer's key yourself, and keep the `rev` and
`sha256` you accepted so an older or swapped list is refused:

```js
const list = await fetchFollowList("https://lists.example.org/trusted.json", {
  pubkey: LIST_SIGNER_KEY,
  seen: stored, // { rev, sha256 } from last time, or undefined
});
if (!list.valid) throw new Error(`follow list: ${list.error}`); // signature, invalid, rollback, fork
stored = { rev: list.rev, sha256: list.sha256 };
for (const c of list.catalogs) {
  // Verify each catalog against its own c.publisher_pubkey with openCatalog.
}
```

`verifyFollowList({ body, sig, pubkey, seen })` does the same for bytes
you already have. A list only says which catalogs to follow; it cannot
sign catalog entries.

The report is the `ts.claim-check/1` JSON that `tensorseed check-claim
--json` prints. See `docs/INTEGRATE-A-MARKETPLACE.md` in the repository for
what each check means.
