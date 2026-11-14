# Draft: verified serving

Status: **draft for discussion**. Two pieces are built for 0.3.0:
`tensorseed reference-logprobs` and `tensorseed check-endpoint` (section
5.7). Everything else beyond the unsigned `ts.served-weights/0`
placeholder already described in SPEC section 19.1 and
[SELL-INFERENCE.md](../SELL-INFERENCE.md) is not implemented. Field
names, command names, header names, and the EAS schema string below are
proposals. They are not a released schema; the two built file formats
are versioned `/1` and may still change before release.

Publisher: Tuned Chaos. License: AGPL-3.0-or-later.
Date: 2026-11-13. Revised the same day: the primary live check is signed
responses plus rare logprob challenges (section 5). Full-generation
spot checks are a dispute tool (section 6).

This draft is meant to be read with a marketplace partner. Tensorseed's
part is deliberately small: a signed claim, response signing on the
seller's machine, helpers that never hold wallet or marketplace keys,
offline reference generation, and a check client. Stake custody,
adjudication, offer routing, and buyer-facing badges belong on the
marketplace side. Tensorseed is token-neutral: it has no dependency on
any marketplace token, does not hold or promote one, and does not
require one to write, register, or check a claim.

---

## 1. Goal

A buyer or a marketplace should be able to answer, with increasing
strength:

1. What does the seller *claim* to be serving (which catalog, which
   revision, which file or quantization, which endpoint)?
2. Is that claim public, permanent, and revocable on a chain the
   marketplace already uses, and does the named manifest hash really
   appear in a signed Tensorseed catalog for that model?
3. Can a given answer be attributed to that seller (signed responses)?
4. Do rare, precomputed logprob challenges show that the live endpoint
   behaves like those hashed weights?
5. (Dispute only.) Do longer, full-generation checks still agree, if a
   challenge is contested?
6. (Optional, premium.) Does a hardware handshake at connection setup
   show that a measured guest, on a confidential GPU, loaded those
   weights?

A catalog hash match is necessary and cheap. It proves the seller
pointed at a real, signed revision. It does not prove those bytes are
what answers the buyer: a seller could hold the real weights, pass a
hash check, and still route traffic to a cheaper model.

A chain cannot see inside a seller's server, so it cannot close that
gap by itself. Signed responses make misrepresentation attributable.
Rare logprob challenges, whose expected values are computed once
offline and reused, test what is actually answering without a second
live model running in parallel. Hardware attestation is the only true
handshake: the check happens when the connection opens, not on every
request.

Even a signed claim proves what the seller asserts, not what actually
runs. That limit is already stated in SPEC 19.1. Layers 5 and 8 are
how the gap is narrowed.

## 2. What exists today (unreleased 0.3.0)

`tensorseed serve-inference` (SPEC 19.1, `tensorseed_app::inference`)
verifies a local copy against the signed catalog, gates on the commercial
serving mark (SPEC 11.1), starts llama.cpp or vLLM, waits for `/v1/models`,
and writes `.tensorseed/served-weights.json` with schema
`ts.served-weights/0`. Version 0 is unsigned and provisional. The file
records the catalog name, the manifest key, the SHA-256 of the signed
manifest value, the repo, the revision, the license and mark, the runtime,
the entrypoint (a GGUF path or the safetensors directory), the served
model id, the local and public base URLs, every verified file hash, and
`created_at`. Readers must not treat that file as proof.

The guide in [SELL-INFERENCE.md](../SELL-INFERENCE.md) shows how to put a
TLS reverse proxy in front of the runtime and list the endpoint on an
OpenAI compatible marketplace. [Kompute](https://kompute.gg/docs) is the
worked example there: a seller lists a model id, a public `base_url`, and
a price; the marketplace health-checks the seller's `/models` endpoint
and, when several sellers list the same id, routes to a healthy offer.
Buyers get one base URL. Prepaid funds sit in on-chain escrow (USDC on
Base, and a separate rail on Robinhood Chain); agents can also pay
per call with x402 on Base. Tensorseed has no marketplace code, no
wallet, and no token in that flow, and this draft keeps it that way.

Version 1, defined below, replaces the placeholder. Version 0 remains
readable so existing `serve-inference` output does not break. Anything
that needs a claim as evidence must require `schema` equal to
`ts.served-weights/1` and a valid signature.

## 3. Layer 1: the signed served-weights claim (`ts.served-weights/1`)

### 3.1 Fields

The document lives at `.tensorseed/served-weights.json` (same path as
version 0) or is copied next to a listing. Proposed fields, extending the
version 0 record:

```json
{
  "schema": "ts.served-weights/1",
  "signed": true,
  "catalog": {
    "name": "tensorseed",
    "publisher_key_id": "CE1E0E3D543DFFB8",
    "manifest_key": "ts/v1/repo/model/<org>/<name>/rev/<commit>",
    "manifest_sha256": "<sha256 of the signed manifest value>"
  },
  "type": "model",
  "repo": "<org>/<name>",
  "revision": "<hf commit, 40 lowercase hex>",
  "license": {
    "id": "apache-2.0",
    "class": "open",
    "commercial_serving": "allowed",
    "rights_asserted": false
  },
  "runtime": "llama-cpp",
  "engine_version": "<runtime-reported version string>",
  "entrypoint": { "kind": "gguf", "path": "<file>.gguf" },
  "served_model_id": "<id listed on /v1/models>",
  "base_url": "http://127.0.0.1:8090/v1",
  "public_base_url": "https://llm.example.com/v1",
  "files": [ { "path": "...", "size": 0, "sha256": "..." } ],
  "seller_wallet": "0x<40 lowercase hex>",
  "response_key": { "alg": "ed25519", "pubkey": "<32-byte public key, 64 lowercase hex>" },
  "logprobs": "required",
  "started_at": "2026-11-13T21:01:42Z",
  "created_at": "2026-11-13T21:01:42Z",
  "claim_sha256": "<sha256 of the canonical unsigned body, 64 lowercase hex>",
  "sig": { "alg": "eip191", "value": "0x<65-byte signature, hex>" }
}
```

Notes on names, so this stays aligned with 19.1:

- `catalog.manifest_sha256` is the manifest hash: the SHA-256 of the
  signed manifest value already stored in version 0. A marketplace
  should check that this hash is the signed catalog entry for `repo`
  and `revision` before treating the claim as verified.
- `entrypoint` is the quantization or file: `kind` is `gguf` with a
  relative `path`, or `kind` is `directory` for a safetensors checkpoint
  (same tagged enum `serve-inference` already writes).
- `runtime` is `llama-cpp` or `vllm`. `engine_version` is new in version 1
  and should be the version string the binary reports, not a Tensorseed
  version.
- `public_base_url` is the endpoint buyers and the marketplace call. It
  must be an `https` URL ending in `/v1` (or the exact public base the
  seller listed). `base_url` may stay loopback, as in version 0.
- `seller_wallet` is an Ethereum address, stored lowercase (no checksum
  mixed in) so the canonical bytes are unique. Verifiers accept an EIP-55
  checksum and normalize.
- `response_key` is a dedicated Ed25519 public key used only to sign
  responses (section 5.1). It is named in the claim so it is bound to
  `seller_wallet`. It is not the catalog publisher key and not a
  marketplace key. The matching secret stays on the seller's machine.
- `logprobs` is `required` for the logprob-verified tier, or `none` if
  the engine cannot return logprobs. A `none` offer may still carry a
  signed claim and signed responses, but it is not in the verifiable
  tier (section 5.6).
- `started_at` is when this serving period began. `created_at` is when
  this document was written. They are often the same instant.
- The catalog publisher key does not sign this document. The seller's
  wallet does, so the claim binds to the same address the marketplace
  already uses as the seller identity.

### 3.2 Canonical form

The unsigned body is the JSON object with every field above except
`claim_sha256` and `sig`. Canonicalization is the same rule as SPEC 6.4:
UTF-8, object keys sorted at every nesting level, no insignificant
whitespace. Tensorseed already implements this in `tensorseed_core`
(`to_canonical_vec`). Timestamps are RFC 3339 UTC. Hex hashes, the
wallet address, and `response_key.pubkey` are lowercase.

```
unsigned          = canonical JSON of the object without claim_sha256 or sig
claim_sha256      = SHA-256(unsigned), 32 bytes, stored as 64 lowercase hex
eip191_message    = "tensorseed served-weights/1:\n" || claim_sha256
sig.value         = Ethereum personal_sign (EIP-191) of eip191_message
                    by seller_wallet
```

Verification:

1. Reject any document whose `schema` is not `ts.served-weights/1`.
2. Rebuild the unsigned body, canonicalize it, and check `claim_sha256`.
3. Recover the signer of `sig.value` over `eip191_message` and require it
   to equal `seller_wallet` (after lowercase normalization).
4. Re-check `catalog.manifest_sha256` against a locally verified catalog
   manifest for the same `repo` and `revision`, and check that `files`
   match that manifest.
5. Require `public_base_url` to use `https` when the claim will be
   published.
6. Require `response_key.alg` to be `ed25519` and the public key to be
   32 bytes.

EIP-191 is the proposed default because Tensorseed can print the exact
message and an external wallet can sign it, with no Tensorseed-held key.
EIP-712 (a typed "ServedWeights" struct the wallet can display) is an
open alternative; if it is chosen, `sig.alg` becomes `eip712` and the
domain and types will be written down here before anyone ships a signer.
Do not accept both algorithms in one schema revision.

### 3.3 What the claim does not prove

The signature binds the seller wallet to a statement about a catalog
revision, a file, an endpoint, and a response-signing key. A catalog
lookup can confirm that `manifest_sha256` is genuine. None of that
proves that the process listening on the endpoint loaded those bytes,
that it still does so an hour later, or that every request is served
by the same weights. Changing the weights, the file, the endpoint,
the served model id, or the response key requires a new claim. The
previous claim should be revoked on chain (section 4.3).

---

## 4. Layer 2: on-chain registration on Base (EAS)

The [Ethereum Attestation Service](https://docs.attest.org/) is a pair of
contracts (a schema registry and an attestation contract) that already
exist on Base as OP Stack predeploys. A seller posts a compact record
that points at the off-chain claim. Anyone can read it, and the seller
can revoke it.

### 4.1 Contracts on Base mainnet

These addresses are the official EAS deployments on Base, also listed in
the Base contract reference:

| Contract | Address | Sources |
|---|---|---|
| EAS (attest, revoke, getAttestation) | `0x4200000000000000000000000000000000000021` | [EAS contracts](https://github.com/ethereum-attestation-service/eas-docs-site/blob/main/docs/quick--start/contracts.md), [Base contracts](https://docs.base.org/specifications/reference/base-contracts), [Basescan](https://basescan.org/address/0x4200000000000000000000000000000000000021) |
| SchemaRegistry | `0x4200000000000000000000000000000000000020` | same sources, [Basescan](https://basescan.org/address/0x4200000000000000000000000000000000000020) |

Official EAS documents this as Version 1.0.1 on Base. The same
predeploy addresses also appear on Base Sepolia, which is the suggested
place to trial a schema before mainnet. EAS also documents later
Version 1.2.0 helper contracts on Base (`EIP712Proxy`, `Indexer`); this
draft uses only the predeployed EAS and SchemaRegistry.

Schemas already registered on Base can be browsed at
[base.easscan.org/schemas](https://base.easscan.org/schemas).

### 4.2 Proposed schema string

EAS schemas are Solidity ABI type lists
([Create a Schema](https://github.com/ethereum-attestation-service/eas-docs-site/blob/main/docs/tutorials/create-a-schema.md)).
The on-chain payload should stay small. The full claim stays off chain;
the attestation carries a hash and the fields a marketplace index needs.

Proposed schema string (exact characters, including spaces after commas):

```
bytes32 claimSha256,string catalog,string repo,string revision,string servedModelId,string publicBaseUrl,string runtime,string entrypoint,bytes32 responsePubkey,uint64 startedAt
```

Proposed registration flags:

- resolver: the zero address (no on-chain hook in this draft)
- revocable: `true`

`claimSha256` is the 32-byte SHA-256 of the canonical unsigned claim
(the same value as `claim_sha256`). `entrypoint` is a short string such
as `gguf:qwen2.5-1.5b-instruct-q4_k_m.gguf` or `directory`.
`responsePubkey` is the 32-byte Ed25519 public key from `response_key`.
`startedAt` is a Unix timestamp in UTC, equal to `started_at`.

The attester of the EAS attestation must be `seller_wallet`. A reader
rejects the pair if they differ. `refUID` is zero for a bare serving
claim. A later hardware attestation (section 8) may point at this
attestation's UID through `refUID`.

The schema UID is assigned at registration time (it depends on the
schema string, the resolver, and the revocable flag). This draft does
not invent a UID. The marketplace, or a one-time registration signed by
an agreed wallet, should register the schema once on Base and publish
the UID. Tensorseed then embeds that UID as a well-known constant.

How EAS records and revokes attestations is described in
[How EAS Works](https://github.com/ethereum-attestation-service/eas-docs-site/blob/main/docs/core--concepts/how-eas-works.md):
the attestation is stored on chain; a revocation changes its status
without deleting the original record.

### 4.3 Revocation and replacement

The schema is registered as revocable so a seller can call `revoke` when:

- they stop serving the endpoint;
- they change weights, quantization, runtime, model id, public URL, or
  the response-signing key (those changes need a new claim and a new
  attestation);
- they rotate the serving host but keep the same weights (new claim,
  new attestation, old one revoked).

Marketplaces should treat a revoked attestation as inactive immediately
after the revoke transaction confirms, and should not route buyers to
that offer. An expired attestation (`expirationTime` set at attest time)
should be treated the same way. This draft recommends `expirationTime = 0`
(no automatic expiry) plus an operational habit of revoking promptly,
rather than silent expiry, so a forgotten claim does not vanish without
a trace. That choice is open (section 13).

A seller must not leave two unrevoked attestations for the same
`publicBaseUrl` and `servedModelId` with different `claimSha256` values.
The marketplace may refuse to list the newer offer until the older
attestation is revoked.

### 4.4 Robinhood Chain

Base is the default chain for this design. Kompute's public docs already
settle prepaid USDC and x402 on Base, so a Base attestation sits next to
the money rail most agents already use.

Whether EAS is deployed on Robinhood Chain is not established by the
official EAS or official Robinhood Chain documentation:

- The official EAS deployment list
  ([contracts.md](https://github.com/ethereum-attestation-service/eas-docs-site/blob/main/docs/quick--start/contracts.md))
  names Ethereum, Optimism, Base, Arbitrum One, Arbitrum Nova, Polygon,
  Scroll, zkSync, Celo, Blast, and Linea (plus testnets). It does not
  name Robinhood Chain.
- Official Robinhood Chain network documentation
  ([Connecting to Robinhood Chain](https://docs.robinhood.com/chain/connecting/))
  gives chain id 4663 for mainnet and 46630 for testnet, and does not
  mention EAS.

A third-party project, Sherwood, documents an EAS and SchemaRegistry
deployment *it* made on Robinhood Chain mainnet (4663), and notes that
easscan does not index that chain
([Sherwood EAS page](https://docs.sherwood.sh/reference/integrations/eas),
[Sherwood deployments](https://docs.sherwood.sh/reference/deployments)).
Those addresses are not an official EAS predeploy, and this draft has
not independently verified the bytecode against the official EAS
contracts. They are therefore **not** adopted as a Tensorseed default.

If the marketplace later wants the same claim on Robinhood Chain, it
should first confirm a canonical EAS deployment (official, or a
bytecode-verified copy whose operator is agreed), then register the
same schema string there. Until that is done, treat EAS-on-Robinhood as
unknown and keep registration on Base.

### 4.5 What Tensorseed will and will not send

`tensorseed attest-serving` (section 11) prints the EAS `attest` calldata
(to, data, and a human decode of the fields) for an external wallet or
a marketplace helper to submit. Tensorseed does not hold a private key,
does not talk to a wallet service, and does not depend on any token to
build that calldata. Gas is paid by the seller's own wallet.

---

## 5. Primary live check: signed responses and rare logprob challenges

This is the main approach. It has two parts that work together.

- **Signed responses** prove which seller answered a given request, and
  make a false claim a public, attributable offence. They do not prove
  which model produced the answer.
- **Rare logprob challenges** test whether the live endpoint matches
  the claimed weights. Expected token logprobs are computed once,
  offline, from a verified copy of that exact manifest revision and
  entrypoint, then reused by every marketplace. There is no second
  model running in parallel with production traffic.

Together they are much cheaper than running a shadow copy of the model
on every (or even many) buyer requests. Cost arithmetic is in section 7.
Full-generation comparisons are reserved for disputes (section 6).

### 5.1 Signed responses

`serve-inference` starts llama.cpp or vLLM and is not, today, on the
request path after the runtime is up. The proposed place to sign is a
small signing proxy that Tensorseed can start in front of the runtime,
on loopback, with the TLS reverse proxy in front of that:

```
client or marketplace
  -> TLS reverse proxy
  -> tensorseed signing proxy (loopback)
  -> llama.cpp or vLLM (loopback)
```

The signing key never leaves the seller's machine. Tensorseed does not
hold marketplace keys, and the marketplace does not hold the seller's
response key. Rotating the key means a new claim and a new attestation.

#### Headers

Proposed request header, set by the buyer or by the marketplace before
it forwards the call:

| Header | Value |
|---|---|
| `X-Tensorseed-Request-Id` | Opaque id unique to this call (a UUID, or 32 random bytes as 64 lowercase hex) |

Proposed response headers, set by the signing proxy:

| Header | Value |
|---|---|
| `X-Tensorseed-Claim-SHA256` | `claim_sha256` (64 lowercase hex) |
| `X-Tensorseed-Request-Id` | Echo of the request id |
| `X-Tensorseed-Served-Model` | `served_model_id` from the claim |
| `X-Tensorseed-Timestamp` | RFC 3339 UTC when the signature was made |
| `X-Tensorseed-Body-SHA256` | SHA-256 of the exact response body bytes the runtime returned, 64 lowercase hex |
| `X-Tensorseed-Signature` | Ed25519 signature over the canonical digest, 64 bytes as 128 lowercase hex |

If the request had no `X-Tensorseed-Request-Id`, the proxy mints one and
echoes it. Verifiers that sent an id must require a byte-for-byte echo.

#### Canonical digest

The proxy signs the UTF-8 bytes of this exact message (field order is
fixed, no extra whitespace, hex lowercase, a newline after each line
including the last):

```
ts.response-sig/1
claim_sha256=<64 hex>
request_id=<echoed id>
served_model_id=<id>
timestamp=<RFC 3339 UTC>
body_sha256=<64 hex>
```

The signature is raw Ed25519 (the algorithm already used inside
Tensorseed's minisign implementation) over those message bytes, with
`response_key`.

`body_sha256` covers the runtime body only: the OpenAI-compatible JSON
as returned by llama.cpp or vLLM, before any marketplace annotation.
A gateway that adds its own fields (for example a billing object) must
hash and verify the seller body first, then add fields. Verifying after
mutation would break honest signatures.

#### Who verifies, and how

A buyer or a marketplace:

1. Loads the `ts.served-weights/1` claim for `X-Tensorseed-Claim-SHA256`
   (from the listing, or by fetching a published copy) and verifies it
   as in section 3.2, including the catalog hash match.
2. Checks that `X-Tensorseed-Served-Model` equals the claimed
   `served_model_id` and the model id that was requested.
3. Checks that `X-Tensorseed-Request-Id` equals the id that was sent.
4. Checks that the timestamp is within a skew window (proposed: 5
   minutes) and not unreasonably in the future.
5. Recomputes SHA-256 of the seller response body and checks
   `X-Tensorseed-Body-SHA256`.
6. Rebuilds the canonical digest and verifies `X-Tensorseed-Signature`
   with `response_key.pubkey`.
7. Rejects a repeated `X-Tensorseed-Request-Id` for the same claim
   (replay). Marketplaces should record recent ids.

A missing or invalid signature on an offer that advertised signing is a
hard failure. Signed responses are cheap (one Ed25519 signature per
reply, on the CPU) and should be on by default for any verified listing.

What this does not prove: a seller can still run a different model and
sign that output. The value is attribution. Once logprob challenges
(or a dispute check) show a mismatch, the signed replies are evidence
tied to that seller's key and wallet, not an anonymous host.

### 5.2 Logprob challenges

The marketplace (or another checker it trusts) occasionally sends the
seller a challenge that looks like an ordinary `chat/completions`
request, but whose expected next-token logprobs are already known.

Properties:

- The expected values are generated once per catalog revision and
  entrypoint, offline, from a verified copy of the weights, with
  `tensorseed reference-logprobs` (section 11). They are reused by
  every marketplace and for the life of that set. There is no live
  parallel runner sitting beside production traffic.
- Each challenge asks for a short completion (`max_tokens` 1, or a
  few) with `temperature` 0, a fixed `seed`, `logprobs` true, and
  `top_logprobs` set (proposed: 5). That is one forward pass, on the
  order of a prefill plus one or a few output tokens, not a long
  generation.
- Challenges are sent as additional requests in the seller's stream,
  not by replacing a buyer's prompt. The buyer always gets their own
  answer.
- At a rate such as 1 in 1000 seller requests, the added inference
  cost is well under 1% (section 7).

Proposed expectation file, `ts.reference-logprobs/1`:

```json
{
  "schema": "ts.reference-logprobs/1",
  "manifest_sha256": "<same as the claim>",
  "repo": "<org>/<name>",
  "revision": "<commit>",
  "entrypoint": { "kind": "gguf", "path": "<file>.gguf" },
  "sampling": {
    "temperature": 0,
    "seed": 1,
    "max_tokens": 1,
    "logprobs": true,
    "top_logprobs": 5
  },
  "generator": {
    "tool": "tensorseed reference-logprobs",
    "runtime": "llama-cpp",
    "engine_version": "<string>",
    "note": "A reference run, not a bit-identical guarantee."
  },
  "set_hash": "<sha256 of the canonical prompts and expected values>",
  "prompts": [
    {
      "id": "lp-0001",
      "messages": [ { "role": "user", "content": "..." } ],
      "expected": [
        {
          "token": "...",
          "logprob": 0.0,
          "top_logprobs": [ { "token": "...", "logprob": 0.0 } ]
        }
      ]
    }
  ]
}
```

Reference values are per entrypoint. Two GGUF quantizations of the same
repo are two files, two `set_hash` values, and must not be compared to
each other. A safetensors directory is its own entrypoint.

#### Keeping the set secret, then revealing it

A public, fixed prompt set can be special-cased. The live challenge set
is therefore held by the marketplace (or by a shared checker the
marketplace names), not published with the mirror.

To keep the checker honest, use commit-reveal:

1. When a set is created, publish only `set_hash` (the SHA-256 of the
   canonical `ts.reference-logprobs/1` object, or a Merkle root of the
   prompt records) and the manifest hash it is bound to. The prompts
   and expected logprobs stay secret.
2. Use that set for a limited period, then retire it and switch to a
   new set with a new published hash.
3. After retirement, or when a dispute needs it, reveal the file.
4. Anyone can then run `tensorseed reference-logprobs` against a
   verified copy of the same revision and entrypoint and check that
   the revealed expectations match what the published tool produces
   (and that `set_hash` matches). The tool is part of Tensorseed, so
   the method is auditable after reveal.

Rotation should happen often enough that a revealed set is no longer
the live set. Who holds the secret copy, and whether several
marketplaces share one checker, is an open question (section 13).

#### Indistinguishability from normal traffic

If every challenge were the only request that set `logprobs: true` and
`max_tokens: 1`, a cheating seller could detect the pattern and run the
real weights only then. Challenges should therefore:

- use the same `served_model_id`, the same URL, and the same upstream
  credential as ordinary traffic;
- vary message shape and length, and not always use `max_tokens: 1`;
- carry no checker-only header the seller can see (the request id is
  fine, because honest clients send one too);
- be mixed with cover: the marketplace may also request `logprobs` on
  a small fraction of real buyer calls, so logprobs are not a signal
  by themselves. Asking an engine for logprobs on a request it is
  already serving does not take a second forward pass; it may only
  enlarge the response.

Perfect indistinguishability is not realistic. The practical bar is
that a seller cannot cheaply filter challenges out of the stream.

#### Tolerances and engines that cannot return logprobs

The same numeric caveats as any temperature 0 comparison still apply.
Thinking Machines' September 2025 note
([Defeating Nondeterminism in LLM Inference](https://thinkingmachines.ai/blog/defeating-nondeterminism-in-llm-inference/))
shows that temperature 0 APIs, and even self-hosted stacks such as
vLLM, still vary in practice, mainly because kernels are not
batch-invariant. Different engines, GPU models, and driver versions
will not be bit-identical even on the same unquantized checkpoint.

So a challenge compares logprobs with a stated tolerance, and a failure
has to be repeatable:

1. Look up the expected logprob of each reference token (and the
   top-k list when present) in the seller's `logprobs` payload.
2. Proposed starting threshold: a mean absolute deviation of 0.5 nats
   or less on the reference-token logprobs for that challenge, with a
   cap on per-token spikes. This is a starting point for calibration,
   not a claim about any one model.
3. Repeat a failing challenge a small number of times (proposed:
   three) before it counts as evidence.
4. Hard failures, no tolerance: wrong model id, HTTP error, missing
   `logprobs` on a listing that claimed `logprobs: "required"`, or a
   distribution that is clearly a different model (for example a 1B
   stand-in for a 70B).

An engine or listing that cannot return logprobs cannot be in the
logprob-verified tier. It may still be listed with a signed claim and
signed responses (`logprobs: "none"`). Buyers should see that as a
weaker, "unverified" (or "claim only") mark. There is no honest way to
run this primary check without logprobs.

A swapped model, a different quantization, or a much smaller stand-in
is expected to fail the logprob distance quickly. An honest seller on a
different GPU than the reference machine should usually pass after
calibration. That last sentence needs real measurements before any
deposit is at risk.

### 5.3 Challenge flow

1. At a low rate, the marketplace picks a secret prompt from the
   current committed set for that `manifest_sha256` and entrypoint,
   and sends it to the seller's `base_url` as an ordinary
   `chat/completions` call, with the request id and sampling above.
2. It verifies the signed response (section 5.1), then compares
   logprobs (section 5.2).
3. On a repeatable failure it stores evidence (section 5.4), may
   pause the offer, and starts whatever adjudication it has defined.
4. On a formal dispute it may also run a full-generation check
   (section 6) and, after the set is retired, reveal the challenge
   file so others can audit the expectations.

Health checks (liveness of `/models`) and logprob challenges (weight
identity) are different. A live endpoint can still be serving the
wrong weights.

### 5.4 Evidence format

Proposed bundle, `ts.challenge-evidence/1`, as canonical JSON:

- the claim (`ts.served-weights/1`) and its EAS UID and transaction;
- the `set_hash` and prompt ids (not necessarily the secret prompts
  until reveal);
- each request (URL, headers with secrets removed, body) and each
  response (status, headers, body, time), including the signature
  headers;
- the comparison results, including repeat counts;
- the challenger identity and a UTC timestamp;
- a signature over the canonical bundle by the challenger.

API keys and upstream keys never appear in evidence.

### 5.5 Stake custody and adjudication

Tensorseed does not hold deposits, does not slash, and does not decide
disputes. If a marketplace wants a financial backstop, it defines that
separately from its existing buyer prepaid escrow and seller settlement
of earnings. Kompute's public docs today describe those buyer and
settlement rails, not a seller listing bond. A seller deposit for
verified serving would be new marketplace work, not a change to those
rails, and not something Tensorseed can do on its own.

This draft does not require any particular token. A stable prepaid rail
the marketplace already uses is the obvious choice if a bond is added.

### 5.6 Tiers, in short

| Tier | What is required | What a buyer can believe |
|---|---|---|
| Claim only | Valid `ts.served-weights/1` and catalog hash match; optional signed responses | The seller claimed these weights. The catalog hash is real. |
| Logprob-verified | The above, `logprobs: "required"`, signed responses, and passing rare challenges | In addition, the live endpoint has been matching precomputed logprobs of those weights, statistically. |
| Hardware-attested | The above (or at least the claim), plus section 8 | A measured confidential guest reported loading those weights at handshake time. |

### 5.7 As built in 0.3.0 (`reference-logprobs`, `check-endpoint`)

Code: `tensorseed_app::challenge` (shared with Desktop) and
`crates/tensorseed-cli/src/challenge.rs`. Choices made where the draft
was open:

**Reference file.** `ts.reference-logprobs/1` has the fields shown in
5.2 plus: `manifest_key`; `entrypoint_sha256` (the GGUF file hash from
the signed manifest, absent for a directory); `generator.tensorseed_version`;
`generator.request_extra` (engine specific request fields, see below);
`generation` (`method` is `ts.challenge-gen/1` or `file`, the generator
`seed` when generated, and `count`); a random 32-byte `salt`; and
`created_at`. A prompt has either `messages` (sent to
`/chat/completions`) or `prompt` (sent to the legacy `/completions`), and
`expected` holds one entry per generated position. Logprobs are rounded
to 6 decimals. The copy is verified against the signed manifest before
any request, with the same `verify_copy` that `serve-inference` uses,
and the runtime is started with the same `LaunchPlan` on a private
loopback port (or `--endpoint` names a running server).

**`set_hash`.** SHA-256 of the canonical JSON (SPEC 6.4) of the whole
reference object with the `set_hash` field removed. That covers the
bindings, decoding, prompts, expected values, and the salt. The salt
makes the hash a hiding commitment even for guessable prompts. A
verifier re-serializes with the same canonical rule; float formatting is
serde_json's shortest round trip, so a verifier in another language
should parse and re-serialize the same way or use Tensorseed. No Merkle
root is used yet (open question 16).

**Secret and public parts.** `--out` writes the whole file, owner-only on
Unix. `--commitment` writes `ts.reference-logprobs-commitment/1`:
`set_hash`, manifest hash and key, repo, revision, entrypoint and its
hash, decoding, generator, generation method, prompt count, and
`created_at`. It has no prompts, expected values, salt, or generator
seed. `check-endpoint --expect-set-hash` refuses a file whose hash does
not match a published commitment, and every load rejects a file whose
contents no longer match its own `set_hash`.

**Prompt generator `ts.challenge-gen/1`.** For prompt `i` (0 based), the
byte stream is `SHA-256("tensorseed challenge-gen/1\n" || seed || be32(i) || be32(block))`
for `block = 0, 1, ...`. Byte 0 picks one of 8 templates, byte 1 adds a
fixed system message when odd, byte 2 sets the word count (`3 + b % 18`),
and one byte per word indexes a fixed 256-word list. Ids are `lp-0001`
onward. The templates and word list are constants in
`tensorseed_app::challenge`, pinned by a test vector. A secret set uses
a random seed, stored only in the secret file.

**llama.cpp prompt cache.** A live run showed that llama.cpp's prompt
cache changes logprobs between identical requests (the first request
and its repeats differ by tenths of a nat). Reference runs on llama.cpp
therefore send `cache_prompt: false` and record it in
`generator.request_extra`, so every value comes from a full prefill.
`check-endpoint` does not send it by default, so challenges stay
ordinary requests; `--no-prompt-cache` sends it for self-checks.

**Tolerance math.** At each position, each reference token (the chosen
token plus its top-k) is looked up in the endpoint's chosen token plus
top-k. A present token contributes `|actual - expected|`. An absent
token's logprob can be no higher than the smallest value the endpoint
listed (the floor), so it contributes the lower bound
`max(0, expected - floor)`. Comparison stops after the first position
whose chosen token differs, because later positions follow different
text. A challenge passes when the mean deviation is at most
`--max-mean-abs` (default 0.5 nats) and no single deviation exceeds
`--max-token-abs` (default 2 nats). Both are uncalibrated starting points.

**Verdicts and repeats.** A challenge outside tolerance is re-sent up to
`--repeats` attempts in total (default 3) and is `fail` only when every
attempt fails. A pass after a failed or errored attempt is
`inconclusive`. Transport errors, timeouts, and HTTP errors are
`inconclusive`; a response without logprobs is `unsupported`. Overall:
any `fail` is `fail`; all `pass` is `pass`; all `unsupported` is
`unsupported`; anything else is `inconclusive`. Exit codes are 0, 1, and 2
(2 also for setup errors such as a changed reference file). The model id
defaults to the reference file's `repo`. `--sample N` picks challenges
by `SHA-256(sample_seed || id)` order, so a run is reproducible from the
printed seed.

**Report.** `--json` prints `ts.check-endpoint/1`: the base URL, model
id, the reference bindings and `set_hash` (no prompts or expected
values), decoding, tolerances, repeats, concurrency, timeout, the
sample request and seed, the overall verdict and counts, and per
challenge the verdict, attempts, comparison (compared tokens, positions,
mean and max deviation, missing tokens, whether the first token
matched, divergence position), per-attempt means, reason, and elapsed
time. API keys are read from the variable named by `--api-key-env` and
never appear in arguments, output, or reports. This is not yet the
`ts.challenge-evidence/1` bundle of 5.4.

**First measurements (one CPU host, llama.cpp build 12366, not a
calibration).** Reference: 32 generated prompts from
`Qwen/Qwen2.5-1.5B-Instruct-GGUF` q2_k, one token, top 5. Against the
same file served by `serve-inference`: all challenges passed, with
per-challenge mean deviations up to about 0.36 nats from batching and
KV layout, even with the prompt cache off. Against q3_k_m of the same
revision: overall fail, 19 of 32 challenges failed, mean deviation about
0.81 nats. Against Qwen2.5-0.5B-Instruct q4_k_m served under the 1.5B
model id: overall fail, 23 of 32 failed, mean deviation about 0.83 nats.
Individual challenges of a wrong model can still pass, which is why a
verdict needs several challenges. The gap between honest noise and a
different file is narrower for low-bit quantizations, so open question 7
still stands.

---

## 6. Secondary tool: full-generation checks for disputes

Longer reference-answer comparisons (many output tokens, token
agreement plus logprobs over a full completion) are useful, but they
are the wrong default for live traffic. They cost more per check, and
they are easy to mistake for "a second model running all the time."

They are not that, even when used. A `ts.reference-answers/1` file is
also generated once, offline, from the verified copy, and reused. The
cost is amortized. A live shadow model that answers every buyer prompt
in parallel would roughly double inference cost; this design does not
do that.

Use full-generation checks:

- when a logprob challenge is contested and needs a richer comparison;
- when calibrating tolerances for a new engine or GPU;
- optionally, as a rare audit, far less often than the 1-in-1000
  logprob probes.

The file shape is `ts.reference-answers/1`, bound to `manifest_sha256`
and the entrypoint, with fixed sampling (`temperature` 0, a fixed seed,
`logprobs` true, a larger `max_tokens`). `tensorseed reference-answers`
generates it. `tensorseed check-endpoint` can run this mode with a
flag, defaulting to logprob challenges.

Comparison rules (token agreement, logprob distance, required repeats,
hard failures) are the same idea as section 5.2, applied to a longer
sequence. The same numeric caveats apply. These files may be published
with the mirror, because they are not the secret live challenge set;
the secret set stays in `ts.reference-logprobs/1` until reveal.

---

## 7. Cost

This section is arithmetic under stated assumptions. It is not a
benchmark. No measured tokens-per-second or dollar figures are claimed.

Let:

- `C` be the average billed cost of one ordinary buyer request (input
  tokens plus output tokens, at the offer price);
- `C_ch` be the cost of one logprob challenge (a short prefill plus
  `max_tokens` of 1 or a few);
- `p` be the fraction of seller requests that are challenges;
- `N` be the number of prompts in a challenge set;
- `C_pre` be the one-time cost to generate that set offline
  (`N * C_ch`, once per revision and entrypoint);
- `C_sign` be the cost of one Ed25519 signature over a short message.

Then:

- Added inference cost on the live path is `p * C_ch` per ordinary
  request, or `(p * C_ch) / C` as a fraction of buyer inference.
- If we take a conservative upper bound `C_ch = C` (a challenge costs
  as much as an average buyer request, which will usually be an
  overestimate, because challenges are short), then `p = 1/1000`
  adds 0.1% to inference cost.
- If a typical buyer request is much longer than a one-token
  challenge, `C_ch` is a fraction of `C`, and the overhead is
  correspondingly lower (for example 0.01% if `C_ch` is `C/10`).
- Cover traffic that asks for logprobs on a real buyer request does
  not add a second forward pass. It may add some response bytes. That
  is not counted as `p * C_ch`.
- Precomputation `C_pre` is paid once per revision and entrypoint,
  then shared by every marketplace and every seller of that
  quantization for the life of the set. It is not a per-request cost.
- `C_sign` is a CPU signature per response. Relative to a GPU forward
  pass it is treated as negligible.
- A full-generation dispute check costs more than `C_ch` (it may be
  on the order of `C` or several times `C`, depending on
  `max_tokens`). It is incurred only when a dispute or rare audit
  needs it, not at rate `p`.
- A live parallel runner that generated a reference for every buyer
  prompt would add about 100% to inference cost. That is the path
  this design avoids.
- Hardware attestation (section 8) is paid at connection setup, not
  per request.

The 1-in-1000 rate is a starting policy, not a measured optimum. If
challenges are cheap relative to `C`, a marketplace can raise `p`
and still stay well under 1%.

---

## 8. Hardware attestation (premium tier)

This is the only true handshake in the design: the relying party
checks a hardware-signed report when the connection (or session)
opens, and does not need a per-request inference challenge to
establish what was loaded. It is optional. It needs specific hardware
and a measured software stack that most sellers will not have.

### 8.1 What the hardware actually provides

NVIDIA Confidential Computing runs the GPU in a trusted execution
environment with a hardware root of trust. When the GPU boots in
confidential computing mode it produces a cryptographically signed
attestation report. The relying party is expected to check that report
before trusting the device.

Sources:

- NVIDIA describes confidential computing on H100, including the signed
  attestation report, CC-On mode, and the need for a CPU confidential
  VM, in
  [Confidential Computing on NVIDIA H100 GPUs](https://developer.nvidia.com/blog/confidential-computing-on-h100-gpus-for-secure-and-trustworthy-ai/).
- Protected PCIe (PPCIE) for multi-GPU HGX H100 and H200, generally
  available with CUDA 12.8, and the requirement for a CPU TEE (AMD
  SEV-SNP or Intel TDX), are described in
  [Announcing NVIDIA Secure AI General Availability](https://developer.nvidia.com/blog/announcing-nvidia-secure-ai-general-availability/).
- NVIDIA's current platform list for confidential containers includes
  H100, H200, Blackwell B200, HGX B300, and RTX Pro 6000 BSE, with
  Intel TDX and AMD SEV-SNP host CPUs, in
  [Supported Platforms](https://docs.nvidia.com/confidential-computing/containers/supported-platforms).
- NVIDIA's own threat model says a successful GPU attestation does not
  validate the CPU TEE or the workload, and a CPU attestation does not
  establish the state of the GPUs. Both must be checked before secrets
  are released
  ([Trust and Threat Model](https://docs.nvidia.com/confidential-computing/virtual-machines/threat-model),
  [NVIDIA Confidential Computing overview](https://docs.nvidia.com/confidential-computing/overview)).

So a complete report for this tier is: CPU TEE evidence (TDX or
SEV-SNP) plus GPU evidence (H100, H200, or Blackwell in CC or PPCIE
mode), appraised against current reference values and certificate
revocation, plus a binding to the weights.

### 8.2 Binding the weights hash

NVIDIA's report measures the GPU identity, firmware, driver, and
confidential computing mode. It does not, by itself, name a Tensorseed
manifest hash. The binding has to be built in the measured guest:

1. The guest image and inference stack are measured (CPU RTMRs or
   equivalent, plus GPU evidence).
2. At start, the stack verifies the weights against the signed
   Tensorseed manifest (the same check `serve-inference` already does)
   and refuses to serve on mismatch.
3. The guest includes `catalog.manifest_sha256` and the entrypoint in a
   guest-issued statement that is bound to the attestation (for
   example, a hash of those values placed in a measured register, a
   nonce, or a report extension the appraiser checks).
4. Only then is the endpoint advertised under the layer-1 claim.

Without step 3, a seller could attest a genuine confidential GPU and
still load different weights inside it. With it, hash matching is
real: the protected machine hashes the weights it actually loaded,
and that hash is compared to the catalog.

### 8.3 Posting a digest on chain

The full CPU and GPU reports are too large and too sensitive to put in
an EAS payload. Proposed second schema, also revocable, registered on
the same Base contracts:

```
bytes32 claimSha256,bytes32 reportDigest,string cpuTee,string gpuModel,string ccMode,bytes32 measuredWeightsHash,uint64 createdAt
```

`reportDigest` is the SHA-256 of the concatenated, canonicalized CPU
and GPU evidence the seller published off chain. `measuredWeightsHash`
is the manifest hash the guest bound in step 3, and must equal
`claimSha256`'s underlying `catalog.manifest_sha256` (or a clearly
documented derivative). The attestation's `refUID` is the UID of the
layer-2 serving claim. Verifiers fetch the full evidence from a URL
the seller publishes, recompute the digest, and only then trust the
row.

### 8.4 Who does this today

This draft only names operators when a public source confirms the
relevant piece.

Phala Cloud documents GPU TEE instances (H200 and B300) with
`nvidia-smi conf-compute` showing `CC State: ON` and Intel TDX, and
NVIDIA local verification tools for the GPU report
([Deploy and Verify GPU TEE](https://docs.phala.com/phala-cloud/confidential-ai/confidential-gpu/deploy-and-verify)).
Phala's Trust Center documents automated verification of Intel TDX and
NVIDIA Confidential Computing evidence, and a comparison of application
measurements against on-chain records
([Trust Center Verification](https://docs.phala.com/phala-cloud/attestation/trust-center-verification)).

That is evidence that confidential GPU plus CPU TEE attestation is in
production use, and that at least one platform already checks
measurements against a chain. It is not a recipe for posting a
Tensorseed weights digest on Base, and this draft does not claim any
inference marketplace already does that for listed offers.

---

## 9. Weight fingerprinting (not used for mirrors)

A publisher who trains a model themselves can, in principle, embed a
hidden trigger so the model gives a known odd reply, or otherwise
watermark the weights, and later use that as a fingerprint. That only
applies to models the publisher trains and releases (for example a
first-party fine-tune). It does not help for ordinary Hugging Face
mirrors, which is most of a Tensorseed catalog, and it is not part of
the primary check. This draft does not rely on it.

---

## 10. zkML is not a path for this design today

Zero-knowledge proofs of inference (often called zkML) would, in
principle, prove that a given output came from a named model without a
trusted GPU. The costs are still too high to use per request for
LLM-sized models in a marketplace.

The 2024 ACM CCS paper zkLLM (Sun, Li, and Zhang) reports a correctness
proof for a 13-billion-parameter model in under 15 minutes, with a
proof under 200 kB, on a data-center GPU
([arXiv:2404.16109](https://arxiv.org/abs/2404.16109),
[ACM CCS 2024](https://dl.acm.org/doi/10.1145/3658644.3670334)).
That is a real result, and later systems have improved throughput, but
even optimistic recent figures remain many times slower than native
inference and cover only some model families. A marketplace that
charges per chat completion cannot wait minutes (or spend a large
multiple of GPU time) to prove each answer.

This design therefore does not depend on zkML. If proving becomes cheap
enough later, a proof digest could hang off the same claim through
another EAS `refUID`, the same way hardware reports would.

---

## 11. What Tensorseed implements, and what the marketplace implements

Tensorseed stays small, offline-verifiable, and token-neutral. It never
holds a seller or marketplace private key. Response-signing secrets
stay on the seller's machine.

### 11.1 Tensorseed (proposed commands; two built in 0.3.0)

| Command | Role |
|---|---|
| `tensorseed claim-serving <dir\|repo_id>` | After a verified copy exists, write the unsigned `ts.served-weights/1` body (including `response_key`), compute `claim_sha256`, and print the exact EIP-191 message. `--wallet` sets `seller_wallet`. `--sig 0x...` verifies the signature and writes the signed file. `--response-key` names a local public key; Tensorseed can also generate a key pair on the seller's machine and refuse to print or upload the secret. |
| `tensorseed attest-serving <claim.json>` | Check the claim, then print EAS calldata for Base (contract `0x4200...0021`, encoded `attest` data, and a human summary). `--broadcast` is out of scope; an external wallet or the marketplace submits the transaction. |
| `tensorseed reference-logprobs <dir>` | **Built in 0.3.0** (section 5.7). From a verified local copy, generate `ts.reference-logprobs/1` for that entrypoint: the secret (or later revealed) challenge expectations. Prints `set_hash` so a marketplace can commit to the set without publishing the prompts yet. |
| `tensorseed reference-answers <dir>` | Generate the longer `ts.reference-answers/1` file used for disputes and calibration, also offline and amortized. |
| `tensorseed check-endpoint` | **Built in 0.3.0** for logprob challenges only (section 5.7); signed response checks, the dispute mode, and evidence bundles are not built. Default: verify signed responses and run logprob challenges against a claim and a `ts.reference-logprobs/1` file (or a revealed set). A flag runs the full-generation dispute mode instead. Writes an optional `ts.challenge-evidence/1` bundle. No stake is moved. |
| `tensorseed serve-inference` (extension) or `tensorseed sign-responses` | Start (or run standalone) the loopback signing proxy in front of the runtime, using the local response key. The secret key never leaves the seller host and is never sent to Tensorseed developers or to a marketplace. |

`serve-inference` can later gain a flag to write a version 1 body and
pause for a wallet signature, but version 0 stays the default until
this schema is accepted.

All of these use only the local catalog, the local copy, local keys,
and public HTTP. None of them take a marketplace private key, talk to
a chain RPC with a signer, or mention a token.

### 11.2 Marketplace

The marketplace, not Tensorseed, would:

- register the EAS schema on Base and publish the UID;
- accept a claim and an attestation UID when a seller lists an offer,
  and check that the attester, model id, and `public_base_url` match
  the listing, and that `manifest_sha256` is in the signed catalog;
- verify signed responses on the proxy path (hash the seller body
  before adding any marketplace fields);
- hold the secret logprob set, publish `set_hash`, rotate and reveal,
  and inject rare challenge requests into the seller's stream without
  altering buyer prompts or the existing billing ledger;
- show the tier (claim only, logprob-verified, hardware-attested) on
  offers, and hide or down-rank revoked or failing ones;
- decide whether routing may prefer, or require, a verified tier when
  several sellers list the same model id;
- store evidence and adjudicate; run section 6 checks on dispute;
- if it wants a financial backstop, design seller deposits without
  reworking existing buyer escrow or settlement rails;
- optionally, accept a section 8 hardware attestation as a higher tier.

Kompute is the intended first partner for that work. The public product
shape this draft assumes is the one in [kompute.gg/docs](https://kompute.gg/docs):
sellers list model id, `base_url`, and price; offers are health-checked
at `/models`; the router picks a healthy offer; buyers call
`https://kompute.gg/v1`. Source-level hook points are out of scope for
this public document and will be discussed directly with the
marketplace developers.

---

## 12. Threat model

| Threat | What stops it | What remains |
|---|---|---|
| Seller lists a model id that does not match the weights they run | Catalog hash match, plus rare logprob challenges; hardware handshake if present | Challenges are statistical; a seller who cheats only on rare requests may be missed, which is why the rate `p` is a policy choice |
| Seller holds the real weights but serves a cheaper model | Logprob challenges (and dispute checks). Hash match alone does not catch this | Same statistical limit |
| Seller signs a claim for weights they never loaded | Same as above. The claim alone does not catch this | Buyers who trust only layer 2 are exposed |
| Seller changes weights after attesting | Revoke-and-replace rule; ongoing challenges; hardware re-attestation on guest restart | A window between the change and the next challenge |
| Seller special-cases the live challenge set | Secret sets, rotation, commit-reveal, and cover logprobs on real traffic (section 5.2) | Perfect indistinguishability is not claimed |
| Seller runs the real model only when `logprobs` is set | Cover: some real buyer requests also ask for logprobs; challenges vary in shape | A sophisticated filter may still exist; dispute checks and hardware help |
| False failure from GPU or batch nondeterminism | Fixed sampling, tolerances, and required repeats | Tolerances that are too loose miss cheats; too tight punish honest sellers |
| Different quantization served than claimed | Per-entrypoint reference files; logprob distance | None, if the set is present and logprobs are required |
| Engine cannot return logprobs | Offer is claim-only / unverifiable tier, not logprob-verified | Buyers must see the weaker mark |
| Stolen response key or stolen seller wallet | Key lives only on the seller host; rotation is a new claim; marketplace binds offers to the signed-in wallet | A stolen key is still a stolen identity; wallet and host hygiene are out of band |
| Replay of a signed good response on a later request | Unique `X-Tensorseed-Request-Id` bound in the digest; verifiers reject reuse | Callers must actually send unique ids and check the echo |
| Marketplace annotates the response and breaks the signature | Verify and hash the seller body before adding fields | Implementations that hash too late will false-fail |
| Replay of an old attestation after the seller moved | `public_base_url` is inside the signed claim and the EAS payload; revoke on move | A forgotten unrevoked attestation can linger until noticed |
| Marketplace as a dishonest adjudicator | Commit-reveal of `set_hash`; evidence bundles; anyone can regenerate expectations with `reference-logprobs` after reveal | A marketplace that never runs checks cannot be fixed by Tensorseed |
| Griefing challenges | Challenger bond; required repeats; marketplace reproduction | Policy has to be written and enforced |
| Upstream key or buyer key leaks into evidence | Evidence format forbids secrets | Operator error is still possible |
| Forged hardware report or stale TCB | Appraise against NVIDIA / CPU vendor roots and current revocation; require both CPU and GPU evidence | Side channels and unmeasured guest software remain, per NVIDIA's threat model |
| Claim registered on the wrong chain or a fake EAS | Tensorseed defaults to the official Base predeploys in section 4.1 | A copy of EAS on another chain is only useful after a bytecode and operator check |

---

## 13. Open questions for the marketplace discussion

1. Who registers the EAS schema on Base, and which wallet owns that
   transaction? Tensorseed can print the `register` calldata either way.
2. Is Base the only attestation chain for v1, given that official EAS
   documentation does not list Robinhood Chain (section 4.4)?
3. Should routing prefer logprob-verified offers, require them for some
   model ids, or only display a badge and leave price-first routing as
   it is?
4. Does a listing have to include a claim up front, or can a seller add
   one later?
5. Who holds the secret `ts.reference-logprobs/1` set, who publishes
   `set_hash`, and do several marketplaces share one checker?
6. What rotation and reveal cadence is enough, and who may request an
   early reveal in a dispute?
7. What logprob tolerances survive a calibration run across llama.cpp
   and vLLM on at least two GPU models, for both one-token challenges
   and full-generation dispute checks?
8. What fraction of real buyer requests should also ask for logprobs,
   as cover, and is `p = 1/1000` a good starting challenge rate?
9. Is Ed25519 as a dedicated `response_key` acceptable, or is signing
   with the seller wallet (EIP-191 or EIP-712) preferred even on the
   per-response path?
10. Are the proposed `X-Tensorseed-*` header names acceptable, and
    should the body digest cover raw bytes or canonical JSON?
11. How should offers whose engines cannot return logprobs be labelled
    and ranked?
12. Is a seller deposit in scope for a first version, and if so, in
    what amount and with what quiet period after a revoke, without
    changing existing buyer escrow or settlement? Tensorseed will not
    custody it.
13. How should `expirationTime` be used, if at all?
14. Is EIP-191 acceptable for the claim signature, or is EIP-712
    preferred for wallet display?
15. For the hardware tier, is a digest-plus-off-chain-evidence URL
    enough, and which appraisers (NVIDIA NRAS, a local verifier, a
    third party) are acceptable?
16. Should `set_hash` stay a hash of the whole canonical file, or become a
    Merkle root of prompt records so single challenges can be revealed
    in a dispute without revealing the rest of the set?
17. Should generated sets vary `max_tokens` per prompt (the built
    generator uses one value per set), and should the generator gain
    more templates or a larger word list to make challenges harder to
    filter?
18. Should a mean deviation threshold depend on the quantization (low-bit
    files are noisier), and should the overall verdict tolerate a small
    fraction of failing challenges instead of failing on any one?
19. Should a checker that knows the engine (llama.cpp) send
    `cache_prompt: false` on challenges, given that it reduces honest
    noise but is a field ordinary clients rarely send?

---

## 14. Sources

- Tensorseed SPEC sections 6.4 (canonical JSON), 6.9 (signatures), 11.1
  (commercial serving marks), 19.1 (`serve-inference` and
  `ts.served-weights/0`).
- [SELL-INFERENCE.md](../SELL-INFERENCE.md) and
  [Kompute docs](https://kompute.gg/docs).
- EAS contracts and concepts:
  [contracts.md](https://github.com/ethereum-attestation-service/eas-docs-site/blob/main/docs/quick--start/contracts.md),
  [How EAS Works](https://github.com/ethereum-attestation-service/eas-docs-site/blob/main/docs/core--concepts/how-eas-works.md),
  [Create a Schema](https://github.com/ethereum-attestation-service/eas-docs-site/blob/main/docs/tutorials/create-a-schema.md).
- [Base contract addresses](https://docs.base.org/specifications/reference/base-contracts).
- [Robinhood Chain network configuration](https://docs.robinhood.com/chain/connecting/).
- Third-party EAS-on-Robinhood claim (not adopted here):
  [Sherwood EAS](https://docs.sherwood.sh/reference/integrations/eas).
- NVIDIA Confidential Computing:
  [H100 blog](https://developer.nvidia.com/blog/confidential-computing-on-h100-gpus-for-secure-and-trustworthy-ai/),
  [Secure AI GA](https://developer.nvidia.com/blog/announcing-nvidia-secure-ai-general-availability/),
  [overview](https://docs.nvidia.com/confidential-computing/overview),
  [supported platforms](https://docs.nvidia.com/confidential-computing/containers/supported-platforms),
  [threat model](https://docs.nvidia.com/confidential-computing/virtual-machines/threat-model).
- Phala:
  [Deploy and Verify GPU TEE](https://docs.phala.com/phala-cloud/confidential-ai/confidential-gpu/deploy-and-verify),
  [Trust Center](https://docs.phala.com/phala-cloud/attestation/trust-center-verification).
- [zkLLM, arXiv:2404.16109](https://arxiv.org/abs/2404.16109)
  ([ACM CCS 2024](https://dl.acm.org/doi/10.1145/3658644.3670334)).
- [Defeating Nondeterminism in LLM Inference](https://thinkingmachines.ai/blog/defeating-nondeterminism-in-llm-inference/),
  Thinking Machines Lab, September 2025.

---

Copyright 2026 Tuned Chaos. Licensed under AGPL-3.0-or-later.
