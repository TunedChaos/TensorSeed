# Draft: verified serving

Status: **draft for discussion**. Nothing in this document is implemented
beyond the unsigned `ts.served-weights/0` placeholder already described in
SPEC section 19.1 and [SELL-INFERENCE.md](../SELL-INFERENCE.md). Field
names, command names, and the EAS schema string below are proposals. They
are not a released schema.

Publisher: Tuned Chaos. License: AGPL-3.0-or-later.
Date: 2026-11-13.

This draft is meant to be read with a marketplace partner. Tensorseed's
part is deliberately small: a signed claim, helpers that never hold wallet
keys, a reference-answer file, and a spot-check client. Stake custody,
adjudication, offer routing, and buyer-facing badges belong on the
marketplace side. Tensorseed is token-neutral: it has no dependency on any
marketplace token, does not hold or promote one, and does not require one
to write, register, or check a claim.

---

## 1. Goal

A buyer or a marketplace should be able to answer, with increasing
strength:

1. What does the seller *claim* to be serving (which catalog, which
   revision, which file or quantization, which endpoint)?
2. Is that claim public, permanent, and revocable on a chain the
   marketplace already uses?
3. When someone challenges the endpoint, do the answers match a published
   reference run of those exact weights, within a stated tolerance?
4. (Optional, premium.) Does hardware attestation show that a measured
   guest, on a confidential GPU, loaded those weights?

A chain cannot see inside a seller's server, so it cannot prove by itself
what is running. What it can do is make the seller's claim public and
bind it to a wallet, and (if the marketplace chooses) attach a deposit
that the seller loses if a challenge succeeds. Even a signed claim proves
what the seller asserts, not what actually runs. Layers 3 and 4 are how
that gap is narrowed. This matches the limit already stated in SPEC 19.1.

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
  "started_at": "2026-11-13T21:01:42Z",
  "created_at": "2026-11-13T21:01:42Z",
  "claim_sha256": "<sha256 of the canonical unsigned body, 64 lowercase hex>",
  "sig": { "alg": "eip191", "value": "0x<65-byte signature, hex>" }
}
```

Notes on names, so this stays aligned with 19.1:

- `catalog.manifest_sha256` is the manifest hash the task called "manifest
  hash": the SHA-256 of the signed manifest value already stored in
  version 0.
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
(`to_canonical_vec`). Timestamps are RFC 3339 UTC. Hex hashes and the
wallet address are lowercase.

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
4. Re-check `catalog.manifest_sha256` against a locally verified manifest
   when one is available, and check that `files` match that manifest.
5. Require `public_base_url` to use `https` when the claim will be
   published.

EIP-191 is the proposed default because Tensorseed can print the exact
message and an external wallet can sign it, with no Tensorseed-held key.
EIP-712 (a typed "ServedWeights" struct the wallet can display) is an
open alternative; if it is chosen, `sig.alg` becomes `eip712` and the
domain and types will be written down here before anyone ships a signer.
Do not accept both algorithms in one schema revision.

### 3.3 What the claim does not prove

The signature binds the seller wallet to a statement about a catalog
revision, a file, and an endpoint. It does not prove that the process
listening on that endpoint loaded those bytes, that it still does so
an hour later, or that every request is served by the same weights.
Changing the weights, the file, the endpoint, or the served model id
requires a new claim. The previous claim should be revoked on chain
(section 4.3).

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
bytes32 claimSha256,string catalog,string repo,string revision,string servedModelId,string publicBaseUrl,string runtime,string entrypoint,uint64 startedAt
```

Proposed registration flags:

- resolver: the zero address (no on-chain hook in this draft)
- revocable: `true`

`claimSha256` is the 32-byte SHA-256 of the canonical unsigned claim
(the same value as `claim_sha256`). `entrypoint` is a short string such
as `gguf:qwen2.5-1.5b-instruct-q4_k_m.gguf` or `directory`. `startedAt`
is a Unix timestamp in UTC, equal to `started_at`.

The attester of the EAS attestation must be `seller_wallet`. A reader
rejects the pair if they differ. `refUID` is zero for a bare serving
claim. A later hardware attestation (section 6) may point at this
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
- they change weights, quantization, runtime, model id, or public URL
  (those changes need a new claim and a new attestation);
- they rotate the serving host but keep the same weights (new claim,
  new attestation, old one revoked).

Marketplaces should treat a revoked attestation as inactive immediately
after the revoke transaction confirms, and should not route buyers to
that offer. An expired attestation (`expirationTime` set at attest time)
should be treated the same way. This draft recommends `expirationTime = 0`
(no automatic expiry) plus an operational habit of revoking promptly,
rather than silent expiry, so a forgotten claim does not vanish without
a trace. That choice is open (section 10).

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

`tensorseed attest-serving` (section 8) prints the EAS `attest` calldata
(to, data, and a human decode of the fields) for an external wallet or
a marketplace helper to submit. Tensorseed does not hold a private key,
does not talk to a wallet service, and does not depend on any token to
build that calldata. Gas is paid by the seller's own wallet.

---

## 5. Layer 3: stake and spot checks

Layer 2 makes the claim public. Layer 3 tests whether the live endpoint
behaves like the claimed weights. This is statistical, not a proof.

### 5.1 Why exact match is the wrong test

Greedy decoding (`temperature` 0, a fixed seed, `top_k` 1) is
theoretically deterministic, but production inference often is not.
Thinking Machines' September 2025 note
([Defeating Nondeterminism in LLM Inference](https://thinkingmachines.ai/blog/defeating-nondeterminism-in-llm-inference/))
shows that temperature 0 APIs, and even self-hosted stacks such as
vLLM, still vary in practice. The main cause they identify is that
kernels are not batch-invariant: the numeric reduction order changes
with batch size, and a server's batch size changes with load. Floating
point non-associativity is real, but a single matrix multiply on the
same data is bit-identical; the load-dependent batch is what makes
successive requests diverge.

Two further gaps matter for Tensorseed mirrors:

- Different quantizations (for example two GGUF files in the same repo)
  are different entrypoints and will not match each other's tokens or
  logprobs. Each reference file is bound to one entrypoint.
- Different engines (llama.cpp vs vLLM), GPU models, and driver or
  kernel versions will not be bit-identical even on the same
  unquantized checkpoint.

So a challenge compares answers with a stated tolerance, and a failure
has to be repeatable, not a one-off numeric wobble.

### 5.2 Reference answers file

Published per mirrored revision and entrypoint, bound to the same
`manifest_sha256` as the claim. Proposed schema `ts.reference-answers/1`:

```json
{
  "schema": "ts.reference-answers/1",
  "manifest_sha256": "<same as the claim>",
  "repo": "<org>/<name>",
  "revision": "<commit>",
  "entrypoint": { "kind": "gguf", "path": "<file>.gguf" },
  "sampling": {
    "temperature": 0,
    "seed": 1,
    "top_k": 1,
    "max_tokens": 32,
    "logprobs": true,
    "top_logprobs": 5
  },
  "generator": {
    "runtime": "llama-cpp",
    "engine_version": "<string>",
    "note": "A reference run, not a bit-identical guarantee."
  },
  "prompts": [
    {
      "id": "ra-0001",
      "messages": [ { "role": "user", "content": "..." } ],
      "tokens": ["...", "..."],
      "logprobs": [ [ { "token": "...", "logprob": 0.0 } ] ]
    }
  ]
}
```

Sampling is fixed and must be sent unchanged to the seller (OpenAI
compatible `chat/completions` fields: `temperature`, `seed`,
`max_tokens`, `logprobs`, `top_logprobs`; `top_k` where the runtime
accepts it). The prompt set should mix short factual items, a little
code, and a short structured extraction task, so a swapped or heavily
compressed model shows up quickly. A second, held-out set (not
published with the mirror) makes it harder for a seller to special-case
the public prompts; who holds that set is a marketplace decision.

Who produces the file is an open question (section 10). Options: the
catalog publisher, a well-known reference runner, or the first seller,
with later cross-checks. The file should be signed (publisher minisign,
or the same wallet scheme as the claim) so it cannot be quietly
replaced.

`tensorseed reference-answers` (section 8) generates this file from a
verified local copy.

### 5.3 Comparison

For each prompt, send the same messages and sampling to
`<public_base_url>/chat/completions` with the claimed `served_model_id`.
Then:

1. **Token agreement.** Compare the returned token sequence to the
   reference (or the argmax of the returned logprobs, when the text
   tokenizer would otherwise disagree). Allow a small number of token
   mismatches per prompt (proposed starting point: zero mismatches on
   short prompts of 8 tokens or fewer, and at most one mismatch on
   longer ones). If that is too strict across engines, relax by
   documented agreement, not silently.
2. **Logprob distance.** For each position, compare the logprob of the
   reference token, and the top-k list when present. A proposed starting
   threshold is a mean absolute deviation of 0.5 nats or less on the
   reference-token logprobs, averaged over the completion, with a cap
   on per-token spikes. The number is a starting point for calibration,
   not a claim about any one model.
3. **Repeat on failure.** A single failing request is not enough, because
   of section 5.1. The checker repeats the same prompt a small number of
   times (proposed: three) and asks the marketplace to reproduce it
   before a stake is touched.
4. **Hard failures, no tolerance.** Wrong model id, HTTP error, refusal
   to return logprobs when the listing promised them, or an answer
   distribution that looks like a clearly different model (for example
   a 1B stand-in for a 70B) are immediate evidence.

A swapped model, a different quantization, or a much smaller stand-in
is expected to fail (2) quickly even when (1) occasionally passes. A
honest seller on a different GPU than the reference machine should
usually pass (2) after calibration. That last sentence needs real
measurements before any deposit is at risk.

### 5.4 Challenge flow

1. A challenger (the marketplace itself, a buyer, or a third party the
   marketplace allows) selects prompts from the published set and, if
   one exists, the held-out set.
2. The challenger calls the seller's public endpoint, or the
   marketplace's proxy if that is the only reachable URL, with the
   fixed sampling.
3. The challenger compares as in 5.3 and, on a repeatable failure,
   builds an evidence bundle.
4. The marketplace repeats the check from its own infrastructure when
   it can reach the endpoint, then decides.
5. If the challenge is upheld, the marketplace applies whatever stake
   or listing penalty it has defined. If it is rejected, the
   challenger's own bond (if any) may be taken, so griefing is not
   free.

Health checks (liveness of `/models`) and spot checks (weight identity)
are different. A live endpoint can still be serving the wrong weights.

### 5.5 Evidence format

Proposed bundle, `ts.challenge-evidence/1`, as canonical JSON:

- the claim (`ts.served-weights/1`) and its EAS UID and transaction;
- the reference-answers file identity (`manifest_sha256`, entrypoint,
  prompt ids);
- each request (URL, headers with secrets removed, body) and each
  response (status, body, time);
- the comparison results, including repeat counts;
- the challenger wallet or marketplace operator id, and a UTC timestamp;
- a signature over the canonical bundle by the challenger.

API keys and upstream keys never appear in evidence. The marketplace
already stores an optional upstream key for proxying; challenge requests
should use a dedicated checker credential.

### 5.6 Stake custody and adjudication

Tensorseed does not hold deposits, does not slash, and does not decide
disputes. If a marketplace wants a financial backstop, it defines:

- what asset is locked (this draft does not require any particular
  token; a stable prepaid rail the marketplace already uses is the
  obvious choice);
- how much, and whether it scales with volume or is a flat listing
  bond;
- who may challenge, and whether a challenger posts a bond;
- the time window to reproduce a failure;
- who rules, and how a seller appeals;
- what happens on a seller-initiated revoke (the deposit should be
  returnable after a quiet period, not frozen forever).

Kompute's public docs today describe *buyer* prepaid escrow and seller
settlement of earnings, not a seller listing bond or slashing. A seller
deposit for verified serving would be new marketplace work, not an
extension Tensorseed can do on its own.

---

## 6. Layer 4: hardware attestation (premium tier)

This tier is optional. It is the strongest practical path today, and it
needs specific hardware and a measured software stack that most sellers
will not have.

### 6.1 What the hardware actually provides

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

### 6.2 Binding the weights hash

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
still load different weights inside it.

### 6.3 Posting a digest on chain

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

### 6.4 Who does this today

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

## 7. Layer 5: zkML is not a path for this design today

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

## 8. What Tensorseed implements, and what the marketplace implements

Tensorseed stays small, offline-verifiable, and token-neutral. It never
holds a seller or marketplace private key.

### 8.1 Tensorseed (proposed commands, not yet built)

| Command | Role |
|---|---|
| `tensorseed claim-serving <dir\|repo_id>` | After a verified copy exists, write the unsigned `ts.served-weights/1` body, compute `claim_sha256`, and print the exact EIP-191 message. `--wallet` sets `seller_wallet`. `--sig 0x...` verifies the signature and writes the signed file. |
| `tensorseed attest-serving <claim.json>` | Check the claim, then print EAS calldata for Base (contract `0x4200…0021`, encoded `attest` data, and a human summary). `--broadcast` is out of scope; an external wallet or the marketplace submits the transaction. |
| `tensorseed reference-answers <dir>` | Run the verified copy under the chosen runtime with the fixed sampling, and write `ts.reference-answers/1`. |
| `tensorseed check-endpoint` | Fetch or accept a claim and a reference file, call the public endpoint, and exit with a comparison report (and, optionally, a `ts.challenge-evidence/1` bundle). No stake is moved. |

`serve-inference` can later gain a flag to write a version 1 body and
pause for a signature, but version 0 stays the default until this
schema is accepted.

All four commands use only the local catalog, the local copy, and
public HTTP. None of them take a private key flag, talk to a chain RPC
with a signer, or mention a token.

### 8.2 Marketplace

The marketplace, not Tensorseed, would:

- register the EAS schema on Base and publish the UID;
- accept a claim and an attestation UID when a seller lists an offer,
  and check that the attester, model id, and `public_base_url` match
  the listing;
- show a verified-serving mark on offers that have an unrevoked
  attestation, and hide or down-rank ones that are revoked or stale;
- decide whether routing may prefer, or require, a verified claim when
  several sellers list the same model id;
- run or accept spot checks, store evidence, and adjudicate;
- if it wants a financial backstop, design seller deposits and slashing
  (not part of today's public Kompute billing, which is buyer prepaid
  escrow plus settlement of earnings);
- optionally, accept a layer-4 hardware attestation as a higher tier.

Kompute is the intended first partner for that work. The public product
shape this draft assumes is the one in [kompute.gg/docs](https://kompute.gg/docs):
sellers list model id, `base_url`, and price; offers are health-checked
at `/models`; the router picks a healthy offer; buyers call
`https://kompute.gg/v1`. Source-level hook points are out of scope for
this public document and will be discussed directly with the
marketplace developers.

---

## 9. Threat model

| Threat | What stops it | What remains |
|---|---|---|
| Seller lists a model id that does not match the weights they run | Layer 3 spot checks against reference answers; layer 4 if present | Checks are statistical; a seller who cheats only on rare requests may be missed |
| Seller signs a claim for weights they never loaded | Same as above. The claim alone does not catch this | Buyers who trust only layer 2 are exposed |
| Seller changes weights after attesting | Revoke-and-replace rule; periodic spot checks; layer 4 re-attestation on guest restart | A window between the change and the next check |
| Seller special-cases the public prompt set | Held-out prompts; periodic refresh of the public set | Held-out prompts have to stay secret and be rotated |
| False failure from GPU or batch nondeterminism | Fixed sampling, tolerances, and required repeats (section 5.1, 5.3) | Tolerances that are too loose miss cheats; too tight punish honest sellers |
| Different quantization served than claimed | Entrypoint-bound reference files; logprob distance | None, if the reference is present and logprobs are required |
| Stolen seller wallet signs a claim for someone else's host | Marketplace already binds offers to the signed-in wallet; it should also require control of the `base_url` (existing health check, plus TLS) | A stolen key is still a stolen identity; wallet hygiene is out of band |
| Replay of an old attestation after the seller moved | `public_base_url` is inside the signed claim and the EAS payload; revoke on move | A forgotten unrevoked attestation can linger until noticed |
| Marketplace as a dishonest adjudicator | Evidence bundles are public and reproducible with `check-endpoint` | A marketplace that censors evidence or never runs checks cannot be fixed by Tensorseed |
| Griefing challenges | Challenger bond; required repeats; marketplace reproduction | Policy has to be written and enforced |
| Upstream key or buyer key leaks into evidence | Evidence format forbids secrets; checkers use a dedicated credential | Operator error is still possible |
| Forged hardware report or stale TCB | Appraise against NVIDIA / CPU vendor roots and current revocation; require both CPU and GPU evidence | Side channels and unmeasured guest software remain, per NVIDIA's threat model |
| Claim registered on the wrong chain or a fake EAS | Tensorseed defaults to the official Base predeploys in section 4.1 | A copy of EAS on another chain is only useful after a bytecode and operator check |

---

## 10. Open questions for the marketplace discussion

1. Who registers the EAS schema on Base, and which wallet owns that
   transaction? Tensorseed can print the `register` calldata either way.
2. Is Base the only attestation chain for v1, given that official EAS
   documentation does not list Robinhood Chain (section 4.4)?
3. Should routing prefer verified offers, require them for some model
   ids, or only display a badge and leave price-first routing as it is?
4. Does a listing have to include a claim up front, or can a seller add
   one later?
5. Who publishes `ts.reference-answers/1` for official catalog
   revisions, and is there a held-out set?
6. What logprob and token tolerances survive a calibration run across
   llama.cpp and vLLM on at least two GPU models?
7. Should a verified listing be required to expose `logprobs`?
8. Is a seller deposit in scope for a first version, and if so, on
   which existing prepaid rail (if any), in what amount, and with what
   quiet period after a revoke? Tensorseed will not custody it.
9. Who may file a challenge (marketplace only, any buyer, anyone), and
   is there a challenger bond?
10. How should `expirationTime` be used, if at all?
11. Is EIP-191 acceptable for the claim signature, or is EIP-712
    preferred for wallet display?
12. For layer 4, is a digest-plus-off-chain-evidence URL enough, and
    which appraisers (NVIDIA NRAS, a local verifier, a third party) are
    acceptable?
13. Should `check-endpoint` results be accepted from third parties, or
    only from the marketplace's own runners?

---

## 11. Sources

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
