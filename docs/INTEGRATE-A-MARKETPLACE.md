<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->

# Integrate Tensorseed into a marketplace or gateway

This guide is for the other side of
[SELL-INFERENCE.md](SELL-INFERENCE.md): an inference marketplace, a
router, or an API gateway that lists sellers and wants to check what they
claim to serve. It covers what ships in Tensorseed 0.3.0, plus the
0.3.1 additions marked **0.3.1** (`check-claim`, the calibrated
`check-endpoint` set decision, claim expiry and revocation, the
verify-only `tensorseed-verify` crate, and the `@tensorseed/verify` npm
package; not released yet, build from `main`).
Every command, field, header, and path below was checked against the
code, and the example outputs come from real runs against the official
catalog and a llama.cpp seller behind `serve-inference --sign`. Formats
are versioned `/1` and may still change before 1.0. Ideas that are not
built yet are listed under [Planned](#13-planned-not-in-030).

Tensorseed has no marketplace code, holds no wallet keys, and has no
dependency on any payment rail. You keep listings, routing, billing,
deposits, and dispute decisions. Tensorseed gives you formats and tools
to check four things: that a catalog is genuine, that a seller made a
specific signed claim about the weights behind an endpoint, that a given
answer came from the holder of that claim's key, and that the endpoint
behaves like those weights on rare spot challenges.

Contents:

1. [What you can verify, and what you cannot](#1-what-you-can-verify-and-what-you-cannot)
2. [The pieces](#2-the-pieces)
3. [Trust catalogs](#3-trust-catalogs)
4. [Check a seller's served-weights claim](#4-check-a-sellers-served-weights-claim)
5. [Verify signed responses](#5-verify-signed-responses)
6. [Run logprob spot challenges](#6-run-logprob-spot-challenges)
7. [Suggested trust tiers](#7-suggested-trust-tiers)
8. [Health checks, `/v1/models`, and upstream keys](#8-health-checks-v1models-and-upstream-keys)
9. [Desktop and seeding](#9-desktop-and-seeding)
10. [Security notes](#10-security-notes)
11. [Integration checklist](#11-integration-checklist)
12. [Reference: exit codes and limits](#12-reference-exit-codes-and-limits)
13. [Planned (not in 0.3.0)](#13-planned-not-in-030)

## 1. What you can verify, and what you cannot

| Question | Can you check it in 0.3.0? | How |
|---|---|---|
| Is this catalog revision really from the publisher I pinned, and is it the latest one anchored on chain? | Yes | Minisign signatures, the file catalog hash chain, and the Chia anchor ([section 3](#3-trust-catalogs)) |
| Does the model, revision, and file a seller names exist, with these exact hashes, in a catalog I trust? | Yes | Match the claim's `manifest_sha256` and `files` to the signed manifest ([4.5](#45-match-the-claim-to-a-catalog-you-trust)) |
| Did the seller (and optionally the seller's wallet) put their name to that claim? | Yes | `claim_sha256` and an optional EIP-191 wallet signature ([4.3](#43-the-canonical-hash-claim_sha256), [4.4](#44-the-wallet-signature)) |
| Did this specific answer come from the holder of the claim's response key, unmodified? | Yes | Ed25519 signed responses ([section 5](#5-verify-signed-responses)) |
| Does the live endpoint behave like those weights? | Statistically, on rare challenges | Logprob spot challenges with tolerances ([section 6](#6-run-logprob-spot-challenges)) |
| Is every request served by those weights, all the time? | No | Nothing short of hardware attestation can show that, and Tensorseed 0.3.0 has none |
| Is the publisher of a catalog honest? | No | A catalog proves what *that* publisher signed. Trust in the files comes from the recorded Hugging Face commit and hashes, which anyone can re-check against the Hub |
| Is a license safe for your use? | No | The commercial serving mark is derived from the license id only and is **not legal advice** |

Be honest with buyers about the gap. A claim is a statement the seller
makes and can be held to, not proof of what runs. A seller can hold the
real weights, pass every hash check, and still route traffic to a
cheaper model. Signed responses make that attributable to the seller's
key. Logprob challenges make it detectable at a low, tunable rate. They
do not make it impossible.

## 2. The pieces

| Format or path | What it is | Who writes it | Who checks it |
|---|---|---|---|
| Catalog files (`head.json`, `deltas/`, `snapshots/`, `chain-proof.json`) | The signed, anchored list of mirrored revisions | A catalog publisher | Everyone ([section 3](#3-trust-catalogs)) |
| `ts.signed/1` entry with a `ts.manifest/1` value | One signed catalog document, for example a revision's manifest | A catalog publisher | Everyone |
| `ts.served-weights/1` | The seller's claim: catalog, manifest hash, repo, revision, file, engine, model id, endpoint, response key, optional wallet | The seller (`serve-inference --sign` or `claim-serving`) | You ([section 4](#4-check-a-sellers-served-weights-claim)) |
| `/.well-known/tensorseed/served-weights.json` | Where the signing proxy serves the claim | The seller's proxy | You |
| `X-Tensorseed-*` headers and the SSE trailer (`ts.response-sig/1`) | Per response Ed25519 signature | The seller's proxy | You ([section 5](#5-verify-signed-responses)) |
| `ts.reference-logprobs/1` | Secret challenge prompts and expected logprobs for one file | You, from your own verified copy | You ([section 6](#6-run-logprob-spot-challenges)) |
| `ts.reference-logprobs-commitment/1` | The public part of a reference set: `set_hash` and bindings, no prompts | You | Anyone, after you reveal the set |
| `ts.check-endpoint/1` | JSON report of a challenge run, without prompts or keys | `tensorseed check-endpoint --json` | You |
| `ts.claim-check/1` | **0.3.1.** JSON report of every check on a claim | `tensorseed check-claim --json` | You |
| `ts.claim-revocations/1` | **0.3.1.** Signed revocations of claims, one entry per claim ([4.8](#48-expiry-and-revocation)) | The seller (`revoke-claim`); you may keep your own list | You |
| `/.well-known/tensorseed/revocations.json` | **0.3.1.** Where the signing proxy serves the seller's revocation list | The seller's proxy | You |

The `tensorseed` CLI does all of this. Rust code can call the same
functions in the `tensorseed-app` crate. **0.3.1:** every check on a
claim, a revocation, a signed response, a catalog head, or a manifest
entry lives in the verify-only `tensorseed-verify` crate (no networking,
no torrent, no serving code, no key files), which the CLI, the desktop
app, and the WebAssembly build share. The `tensorseed-wasm` crate (used
by the browser explorer) verifies catalogs and chain anchors from
JavaScript, and in 0.3.1 also claims and signed responses, packaged as
`@tensorseed/verify` ([5.6](#56-verify-in-javascript)). Every format is
also specified below precisely enough to reimplement in another
language.

## 3. Trust catalogs

### 3.1 Pin the official catalog

A catalog is identified by four values. Pin them in your own config.
Never take them from a seller, a claim, or a catalog file.

| Value | Official Tensorseed catalog |
|---|---|
| Catalog name (bound into every signature) | `tensorseed` |
| Publisher public key (minisign) | `RWS4/z1UPQ4ezmjrcpKqDYpqyn5h1WUS2oDs1B73Dh91kVzzNIZd03xd` (key id `CE1E0E3D543DFFB8`) |
| Chia DataLayer store id | `3dcd43dced47a32f8e38e8ec77f583d8d1bafe1ab24e381538e50a36034b7a76` |
| File catalog base URL | `https://catalog.tensorseed.dev/` |

The same values are in `config/tensorseed.example.toml`. A minimal
verifier config (no publishing keys, no Chia software):

```toml
[catalog]
backend = "file"
name = "tensorseed"
publisher_pubkey = "RWS4/z1UPQ4ezmjrcpKqDYpqyn5h1WUS2oDs1B73Dh91kVzzNIZd03xd"
chain_check = "strict"

[file]
base_url = "https://catalog.tensorseed.dev/"

[chia]
catalog_store_id = "3dcd43dced47a32f8e38e8ec77f583d8d1bafe1ab24e381538e50a36034b7a76"
chain_source = "light"
coin_api_urls = ["https://api.coinset.org"]   # add more endpoints, or your own full node
```

### 3.2 Verify a catalog head with the CLI

`catalog info` syncs (head, snapshot, deltas, every entry signature)
and runs the chain check. `catalog chain-status` runs the chain check
whatever `chain_check` says, prints every value, and exits non-zero on a
mismatch:

```text
$ tensorseed --config verifier.toml catalog info
catalog:   tensorseed
publisher: RWS4/z1UPQ4ezmjrcpKqDYpqyn5h1WUS2oDs1B73Dh91kVzzNIZd03xd (key id CE1E0E3D543DFFB8)
backend:   file
revision:  4
root:      206d7047c1f0e7cdb3abd8cf097c6bb7979f8bcd2e8471bbb460e9c6b164e101
entries:   28
snapshot:  revision 1 (snapshots/0000000001.json)
sources:   https://catalog.tensorseed.dev/, https://mirror.tensorseed.dev/
chain:     Strict
anchored:  revision 4 in Chia store 3dcd43dced47a32f8e38e8ec77f583d8d1bafe1ab24e381538e50a36034b7a76 (coin current, checked 2026-11-14T21:46:53Z)
published by: Tuned Chaos
contact:   none published (catalog.publisher_contact)
policy:    none published (catalog.publisher_policy_url)

$ tensorseed --config verifier.toml catalog chain-status
mode:       Strict
source:     light
store:      3dcd43dced47a32f8e38e8ec77f583d8d1bafe1ab24e381538e50a36034b7a76
file:       revision 4 root 206d7047c1f0e7cdb3abd8cf097c6bb7979f8bcd2e8471bbb460e9c6b164e101
chain:      revision 4 root 206d7047c1f0e7cdb3abd8cf097c6bb7979f8bcd2e8471bbb460e9c6b164e101
datalayer:  root 16876aad99eb60d2fc5038af1a6cdb341df4c27f73a67e973b8a30c08b77c636 coin 870eb4fc8dc261d25dffa3a45be7cae0a23ca7af0d49fc9053b5aa4cd177ac28
coin:       Current (created at height 9568842)
proof from: https://catalog.tensorseed.dev/tensorseed/chain-proof.json
checked by: https://api.coinset.org
result:     OK, the file catalog matches the chain
```

(Log lines on stderr are omitted.) `contact` and `policy` are the
publisher's takedown contact and policy URL from the signed
`ts/v1/catalog` entry; "none published" means the publisher left them
empty (0.3.1 names missing fields instead of printing a blank). The verified state is kept in
`<file.state_dir>/<catalog>/state.json` and is re-verified on load. A
client never accepts a head below the revision it already accepted, so
run your verifier with a persistent state directory.

Signed heads expire (`expires_unix`; the official catalog uses one week)
and the publisher re-signs them on a schedule. A strict client refuses
an expired head. Treat "head expired" as "catalog unavailable", not as
"seller failed".

To get every verified entry as one file for your own tooling:

```sh
tensorseed --config verifier.toml catalog export --out catalog.ts-export.json
```

The `ts.export/1` bundle has `catalog`, `publisher_pubkey`
(informational), `source_kind`, `source_id`, and `entries`, a list of
`ts.signed/1` envelopes sorted by key. It is not signed as a whole;
every entry already is, and the export only contains entries that
verified against the key pinned in your config.

### 3.3 Accept other publishers' catalogs

Anyone can publish an independent catalog
([PUBLISH-A-CATALOG.md](PUBLISH-A-CATALOG.md)). To follow one next to
the official catalog, add a `[[catalogs]]` subscription with that
publisher's four values:

```sh
tensorseed catalog add --name example-catalog \
  --publisher-pubkey RW... --base-url https://catalog.example.org/ \
  --store-id <64 hex> --label "Example publisher" --chain-check strict --priority 20
```

which writes:

```toml
[[catalogs]]
name = "example-catalog"
label = "Example publisher"
publisher_pubkey = "RW..."
store_id = "<64 hex>"
base_url = "https://catalog.example.org/"
chain_check = "strict"
priority = 20
```

`tensorseed catalog list` shows each subscription with its chain mode
and head, and `catalog remove <name>` drops one. The same name with a
different key is a config error, so one catalog cannot silently replace
another. `list` and `show` report when catalogs agree on a revision's
hashes, and a hash conflict is an error unless you pick a catalog with
`--catalog <name>`.

Two things to decide as a marketplace:

- **Which publishers you accept.** A claim names its catalog by `name`
  and `publisher_key_id`. Only accept claims whose catalog is one you
  pinned. The key id in a claim is what the seller wrote, so always
  check the claim against your own verified copy of that catalog, never
  against a key the seller supplies.
- **Whether agreement matters to you.** When several independent
  catalogs list the same Hugging Face commit with the same file hashes,
  that is extra evidence. Requiring it is your policy, not Tensorseed's.

`catalog export` exports the catalog in the `[catalog]` block. To export
another publisher's catalog, give it its own config file (its four
values in `[catalog]`, `[file]`, and `[chia]`) and pass `--config`.

Tensorseed 0.3.0 has no signed list of trusted catalog publishers. You
configure the publishers you accept yourself. A neutral, signed follow
list is a proposal under [Planned](#13-planned-not-in-030).

### 3.4 Check catalog signatures without Tensorseed code

Every catalog signature is a standard
[minisign](https://jedisct1.github.io/minisign/) signature, so you can
check it with the stock `minisign` tool or reimplement it.

**Key format.** The public key line (`RW...`) is base64 of 42 bytes:
`"Ed"` (2 bytes), the key id (8 bytes), and the Ed25519 public key (32
bytes). The key id is shown as 16 uppercase hex digits of those 8 bytes
read as a little endian number: the official key's id bytes are
`b8ff3d543d0e1ece`, shown as `CE1E0E3D543DFFB8`.

**Signature format.** A `.minisig` file, or an entry's `sig` string, has
four lines:

```text
untrusted comment: signature from tensorseed key CE1E0E3D543DFFB8
<base64 of "ED" || key id[8] || ed25519 signature[64]>
trusted comment: <context line>
<base64 of global signature[64]>
```

- The signature on line 2 is Ed25519 over **BLAKE2b-512 of the signed
  bytes** (minisign's prehashed `ED` mode). Tensorseed writes `ED` only.
  When verifying it also accepts legacy `Ed` (Ed25519 over the raw
  bytes), as minisign does.
- The global signature on line 4 is Ed25519 over the 64 signature bytes
  from line 2 followed by the trusted comment text (without the
  `trusted comment: ` prefix and without the newline).
- The key id on line 2 must equal the pinned key's id.
- The untrusted comment is not covered by anything. Ignore it.

**What is signed, and the required trusted comment.** A valid signature
is not enough: also compare the trusted comment with the exact string
below. That is what binds a document to its catalog, key, revision, and
root.

| Artifact | Signed bytes | Trusted comment, exactly |
|---|---|---|
| Catalog entry (`ts.signed/1`) | The UTF-8 bytes of the envelope's `value` string | `ts.entry/1 catalog=<name> key=<key> sha256=<SHA-256 of value, 64 lowercase hex>` |
| `head.json`, a delta, or a snapshot | The exact file bytes | `ts.file/1 kind=<head\|delta\|snapshot> catalog=<name> rev=<n> root=<64 hex>` |

An entry envelope looks like
`{"key":"ts/v1/...","schema":"ts.signed/1","sig":"<four line minisig>","value":"<canonical JSON document as a string>"}`.
Tensorseed also rejects an entry whose `value` is not canonical JSON, a
`schema` other than `ts.signed/1`, and a key that does not start with
`ts/` or contains whitespace. Every entry inside a delta or snapshot is
verified on its own, so a correctly signed delta cannot carry an entry
signed by another key.

**Canonical JSON** (used for entry values, catalog files, and the
claim hash in [4.3](#43-the-canonical-hash-claim_sha256)): UTF-8; object
keys sorted by byte order at every level; no whitespace between tokens;
strings escaped as `serde_json` does (`\"`, `\\`, `\b`, `\f`, `\n`,
`\r`, `\t`, other control characters as `\u00XX`, everything else,
including `/` and non-ASCII, written as is); integers in plain decimal;
floats in shortest round trip form (for example `0.0` or `-0.553258`).
Python's
`json.dumps(obj, sort_keys=True, separators=(",", ":"), ensure_ascii=False)`
produces the same bytes for these documents. So does a recursive key
sort plus `JSON.stringify` in JavaScript, as long as keys are ASCII
(true for every Tensorseed schema) and integers stay below 2^53.

Example with the stock tool, on the live head and on the manifest entry
used later in this guide:

```text
$ curl -sO https://catalog.tensorseed.dev/tensorseed/head.json
$ curl -sO https://catalog.tensorseed.dev/tensorseed/head.json.minisig
$ minisign -Vm head.json -P RWS4/z1UPQ4ezmjrcpKqDYpqyn5h1WUS2oDs1B73Dh91kVzzNIZd03xd
Signature and comment signature verified
Trusted comment: ts.file/1 kind=head catalog=tensorseed rev=4 root=206d7047c1f0e7cdb3abd8cf097c6bb7979f8bcd2e8471bbb460e9c6b164e101

# value.json holds the manifest entry's "value" string, value.json.minisig its "sig"
$ minisign -Vm value.json -P RWS4/z1UPQ4ezmjrcpKqDYpqyn5h1WUS2oDs1B73Dh91kVzzNIZd03xd
Signature and comment signature verified
Trusted comment: ts.entry/1 catalog=tensorseed key=ts/v1/repo/model/Qwen/Qwen2.5-1.5B-Instruct-GGUF/rev/91cad51170dc346986eccefdc2dd33a9da36ead9 sha256=7abb51d7f81299d068ddf9bc5c723f4a414857f74c5a11f4a32fa97f070509a2
$ sha256sum value.json
7abb51d7f81299d068ddf9bc5c723f4a414857f74c5a11f4a32fa97f070509a2  value.json
```

`minisign` checks both signatures; comparing the trusted comment with
the expected string is your job. The file layout, the `state_hash` and
`root` hash chain, and the client rules (rollback, fork, gap, expiry)
are specified in [SPEC.md](SPEC.md) section 6.8, key names in section
6.3, and document schemas in section 6.4.

### 3.5 Check catalogs from JavaScript (`tensorseed-wasm`)

`crates/tensorseed-wasm` wraps the same Rust verifier for JavaScript.
0.3.0 is not published to npm; build it from a tagged checkout (in
0.3.1, `npm/verify` packages the same build as `@tensorseed/verify`,
with a typed wrapper; see [5.6](#56-verify-in-javascript)):

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --locked --version 0.2.130
cargo build -p tensorseed-wasm --target wasm32-unknown-unknown --release
wasm-bindgen target/wasm32-unknown-unknown/release/tensorseed_wasm.wasm \
  --out-dir pkg --target nodejs     # or --target web, as the explorer does
```

It exports `CatalogVerifier` (`verify_head`, `load_snapshot`,
`apply_delta`, `verify_chain_proof`, `get_entry`, `list_keys`, and the
`catalog`, `revision`, `root`, and `entry_count` getters),
`check_coin_rpc`, `coin_id_hex`, `parse_manifest`, `commercial_serving`,
and `commercial_disclaimer`. It does **not** verify served-weights
claims or signed responses in 0.3.0. **0.3.1** adds
`CatalogVerifier.check_claim`, which does the whole match below and the
claim's own checks in one call, and the free functions `check_claim`,
`verify_response`, `claim_signing_message`, `revocation_signing_message`,
and `recover_wallet` ([5.6](#56-verify-in-javascript)). A Node example that verifies the official catalog and its
Chia anchor, then matches a claim against it
([4.5](#45-match-the-claim-to-a-catalog-you-trust)):

```js
import { createRequire } from "node:module";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
const require = createRequire(import.meta.url);
const { CatalogVerifier, check_coin_rpc } = require("./pkg/tensorseed_wasm.js");

// What you pin: never take these from the seller.
const PIN = {
  name: "tensorseed",
  pubkey: "RWS4/z1UPQ4ezmjrcpKqDYpqyn5h1WUS2oDs1B73Dh91kVzzNIZd03xd",
  baseUrl: "https://catalog.tensorseed.dev/",
  storeId: "3dcd43dced47a32f8e38e8ec77f583d8d1bafe1ab24e381538e50a36034b7a76",
  coinApi: "https://api.coinset.org",
};
const dir = new URL(`${PIN.name}/`, PIN.baseUrl);
const pad = (n) => String(n).padStart(10, "0");
const bytes = async (rel) => {
  const r = await fetch(new URL(rel, dir));
  if (!r.ok) throw new Error(`${rel}: HTTP ${r.status}`);
  return new Uint8Array(await r.arrayBuffer());
};
const signed = async (rel) => ({ body: await bytes(rel), sig: new TextDecoder().decode(await bytes(`${rel}.minisig`)) });
const rpc = async (method, body) =>
  (await fetch(`${PIN.coinApi}/${method}`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) })).json();

const v = new CatalogVerifier(PIN.name, PIN.pubkey);
const head = await signed("head.json");
const info = JSON.parse(v.verify_head(head.body, head.sig, Date.now() / 1000));
if (info.snapshot) {
  const s = await signed(info.snapshot.path);
  v.load_snapshot(s.body, s.sig);
}
for (let rev = Number(v.revision) + 1; rev <= info.revision; rev++) {
  const d = await signed(`deltas/${pad(rev)}.json`);
  v.apply_delta(d.body, d.sig);
}
// A persistent verifier must also refuse a head below the revision it
// accepted last time (keep revision and root between runs).
const proof = JSON.parse(v.verify_chain_proof(await bytes("chain-proof.json"), PIN.storeId));
const rec = await rpc("get_coin_record_by_name", { name: `0x${proof.coin_id}` });
let peak;
if (rec.coin_record?.spent) peak = BigInt((await rpc("get_blockchain_state", {})).blockchain_state.peak.height);
const coin = JSON.parse(check_coin_rpc(JSON.stringify(proof), JSON.stringify(rec), peak, 192n));
console.log(`catalog ${v.catalog} revision ${v.revision} root ${v.root}, ${v.entry_count} entries`);
console.log(`anchored: ${JSON.stringify(coin.state)} (expires ${new Date(info.expires_unix * 1000).toISOString()})`);

// Match a claim (already checked for claim_sha256 and its wallet signature).
const claim = JSON.parse(readFileSync(process.argv[2], "utf8"));
const value = v.get_entry(claim.catalog.manifest_key);
if (!value) throw new Error("manifest_key is not in the pinned catalog");
const sha = createHash("sha256").update(value, "utf8").digest("hex");
if (sha !== claim.catalog.manifest_sha256) throw new Error("manifest_sha256 does not match the signed manifest");
if (v.get_entry(`ts/v1/repo/${claim.type}/${claim.repo}/tombstone`)) throw new Error("taken down");
const m = JSON.parse(value);
const files = new Map(m.files.map((f) => [f.path, `${f.size}:${f.sha256}`]));
for (const f of claim.files) {
  if (files.get(f.path) !== `${f.size}:${f.sha256}`) throw new Error(`${f.path} differs from the signed manifest`);
}
console.log(`claim matches ${m.repo.id}@${m.hf_commit} (${claim.entrypoint.path ?? "directory"})`);
```

```text
$ node verify-catalog.mjs served-weights.json
catalog tensorseed revision 4 root 206d7047c1f0e7cdb3abd8cf097c6bb7979f8bcd2e8471bbb460e9c6b164e101, 28 entries
anchored: "Current" (expires 2026-11-20T13:53:03.000Z)
claim matches Qwen/Qwen2.5-1.5B-Instruct-GGUF@91cad51170dc346986eccefdc2dd33a9da36ead9 (qwen2.5-1.5b-instruct-q2_k.gguf)
```

With a claim whose GGUF hash was changed, the same script stops with
`qwen2.5-1.5b-instruct-q2_k.gguf differs from the signed manifest`.
`check_coin_rpc` takes the same grace window as the CLI (192 blocks).

### 3.6 What the chain anchor proves

Every Chia update of a catalog store carries a signed anchor naming the
file catalog revision and root. Clients verify the anchor signature and
the DataLayer inclusion proof locally, and ask coin endpoints only
whether the store's coin exists and whether it is spent. So:

- A host, mirror, or seeder cannot get a validly signed head accepted
  unless the chain anchors it. That blocks split views, and blocks a
  stolen publisher signing key from publishing without the store
  owner's wallet.
- A lying coin endpoint can hold you on an older but genuine revision,
  or refuse to answer. List several independent endpoints, or your own
  node.
- The anchor proves the latest state of *that publisher's* catalog. It
  says nothing about whether the publisher is honest.

Details and limits: [SPEC.md](SPEC.md) section 6.14.

## 4. Check a seller's served-weights claim

### 4.1 Where to find the claim

A seller running `serve-inference --sign` or `sign-responses` serves
the claim at a fixed path on the same host as the API:

```text
<scheme>://<host>/.well-known/tensorseed/served-weights.json
```

Tensorseed's tools build that URL from a base URL by removing a trailing
`/` and then a trailing `/v1`, and appending the path. So
`https://llm.example.com/v1` maps to
`https://llm.example.com/.well-known/tensorseed/served-weights.json`.
The proxy reads the file from disk on each request, so an attached
wallet signature appears without a restart, and it answers 503 if the
file no longer hashes to the claim it signs under. The claim holds no
secrets, so sellers expose this path without the API key
([SELL-INFERENCE.md](SELL-INFERENCE.md) section 6). A seller can also
hand you the file directly when listing.

Fetch it with a size cap. The CLI caps claims at 4 MiB, follows at most
3 redirects, refuses URLs with credentials in them, and times out after
10 seconds.

### 4.2 Fields

A real claim, written by `serve-inference --sign --wallet ...` and then
signed with a throwaway wallet:

```json
{
  "schema": "ts.served-weights/1",
  "signed": true,
  "catalog": {
    "name": "tensorseed",
    "publisher_key_id": "CE1E0E3D543DFFB8",
    "manifest_key": "ts/v1/repo/model/Qwen/Qwen2.5-1.5B-Instruct-GGUF/rev/91cad51170dc346986eccefdc2dd33a9da36ead9",
    "manifest_sha256": "7abb51d7f81299d068ddf9bc5c723f4a414857f74c5a11f4a32fa97f070509a2"
  },
  "type": "model",
  "repo": "Qwen/Qwen2.5-1.5B-Instruct-GGUF",
  "revision": "91cad51170dc346986eccefdc2dd33a9da36ead9",
  "license": {
    "id": "apache-2.0",
    "class": "open",
    "commercial_serving": "allowed",
    "rights_asserted": false
  },
  "runtime": "llama-cpp",
  "engine_version": "version: 1.0.0-dev (build 12366, commit 31953bcf7)",
  "entrypoint": {
    "kind": "gguf",
    "path": "qwen2.5-1.5b-instruct-q2_k.gguf"
  },
  "served_model_id": "qwen2.5-1.5b-instruct",
  "base_url": "http://127.0.0.1:18090/v1",
  "public_base_url": "https://llm.example.com/v1",
  "files": [
    {
      "path": "LICENSE",
      "size": 11343,
      "sha256": "832dd9e00a68dd83b3c3fb9f5588dad7dcf337a0db50f7d9483f310cd292e92e"
    },
    {
      "path": "README.md",
      "size": 4856,
      "sha256": "57c72d4010fb11a98fc8e62dc8ef112b23a7a7525b71e571ca978d35cb3b2eec"
    },
    {
      "path": "qwen2.5-1.5b-instruct-q2_k.gguf",
      "size": 752880160,
      "sha256": "5ede348e91ce1e7a330926ec5b202c27b864d065149dc463257fde1f98865b3a"
    }
  ],
  "seller_wallet": "0x6c72521c15d87eedebe623c63e5ae10dc05da50d",
  "response_key": {
    "alg": "ed25519",
    "pubkey": "b263675c2621c515f3e8299ec1bea216c4c6b9269a6bf83f109e0af86a084bb5"
  },
  "logprobs": "required",
  "started_at": "2026-11-14T21:48:28Z",
  "created_at": "2026-11-14T21:48:28Z",
  "claim_sha256": "0a00a5079b7176d65a5c0a33b5ee43c2daab4f8c00b2f44307bebc51177c7185",
  "sig": {
    "alg": "eip191",
    "value": "0x0827ede1b1a4eafb86ae2c3c05d6902ec07d04bc1d94f974f3275fdcc4f947c807419753006d117e1e90d706790b0b2b907f1d9e8cd6460a01bd04023f857c421b"
  }
}
```

| Field | Meaning | Always present |
|---|---|---|
| `schema` | `ts.served-weights/1`. Reject anything else. `ts.served-weights/0` is an unsigned placeholder written by `serve-inference` without `--sign`; never treat it as evidence | yes |
| `signed` | `true` exactly when `sig` is present | yes |
| `catalog.name` | Catalog the seller verified against | yes |
| `catalog.publisher_key_id` | That catalog's key id (16 uppercase hex), or `null` when unknown. Informational: check against your own pin | yes, may be `null` |
| `catalog.manifest_key` | Catalog key of the revision's manifest | yes |
| `catalog.manifest_sha256` | SHA-256 of the signed manifest's `value` string, which is also the `sha256=` in that entry's trusted comment | yes |
| `type` | `model` or `dataset` | yes |
| `repo`, `revision` | Hugging Face repo id and 40 hex commit | yes |
| `license.id` | License id from the manifest, or `null` | yes, may be `null` |
| `license.class` | `open`, `attribution`, `restricted`, or `unknown` | yes |
| `license.commercial_serving` | `allowed`, `restricted`, `forbidden`, or `unknown`, derived on the seller's machine | yes |
| `license.rights_asserted` | `true` when the seller passed `--i-have-rights` to serve a `forbidden` license | yes |
| `runtime` | `llama-cpp` or `vllm` | yes |
| `engine_version` | The version string the engine reports, or what the seller passed with `--engine-version` | yes |
| `entrypoint` | `{"kind":"gguf","path":"<file>"}` for one GGUF file, or `{"kind":"directory"}` for a safetensors checkpoint | yes |
| `served_model_id` | The model id on `/v1/models` and in `X-Tensorseed-Served-Model` (1 to 256 printable ASCII, no spaces) | yes |
| `base_url` | The local signing proxy address, usually loopback. Not for you to call | yes |
| `public_base_url` | The public base URL buyers call. Should be `https` | no |
| `files` | Every file the seller verified, each with `path`, `size`, `sha256` | yes |
| `seller_wallet` | `0x` and 40 lowercase hex, as plain data | no |
| `response_key` | `{"alg":"ed25519","pubkey":"<64 lowercase hex>"}` | yes |
| `logprobs` | `required` (the endpoint returns logprobs) or `none` | yes |
| `started_at`, `created_at` | RFC 3339 UTC | yes |
| `expires_at` | **0.3.1.** RFC 3339 UTC; the claim is not valid after it, and must be later than `created_at` ([4.8](#48-expiry-and-revocation)) | no |
| `claim_sha256` | See [4.3](#43-the-canonical-hash-claim_sha256) | yes |
| `sig` | `{"alg":"eip191","value":"0x<65 bytes as hex>"}` | no |

Unknown fields are rejected: Tensorseed refuses a claim with any field
that is not in this table.

### 4.3 The canonical hash (`claim_sha256`)

The hash covers the claim as received, minus three fields:

1. Parse the claim as JSON.
2. Remove the top-level fields `claim_sha256`, `sig`, and `signed`.
3. Serialize the rest as canonical JSON
   ([3.4](#34-check-catalog-signatures-without-tensorseed-code)): keys
   sorted at every level, no whitespace, `serde_json` escaping.
4. `claim_sha256` is the SHA-256 of those UTF-8 bytes, as 64 lowercase
   hex.

For that to be unambiguous, the hashed fields have one allowed form,
and since 0.3.1 Tensorseed rejects a claim that uses any other
(`malformed ts.served-weights/1: ...`):

- `public_base_url`, `seller_wallet`, and `expires_at` are either
  present with a value or left out. An explicit `null` is rejected, so
  "absent" and "null" can never hash differently. A claim without
  `expires_at` hashes exactly as it did in 0.3.0.
- `catalog.publisher_key_id` and `license.id` are always present, as
  `null` when unknown. Leaving them out is rejected.
- No unknown fields at any level, including inside `catalog`,
  `license`, `entrypoint`, and `files` entries. (0.3.0 ignored unknown
  fields inside those objects, so they were shown but not hashed.)
- Numbers are integers as written by `serde_json`.

Every claim Tensorseed has written already has this form, so no hash
changes and every 0.3.0 claim still verifies. `sig` is outside the hash,
so `"sig": null` is accepted and means "not signed"; attaching the
wallet signature does not change `claim_sha256`. If you write claims
with other software, compute the hash over exactly the bytes you
publish, following the rules above. This is
`ServedClaim::compute_hash` plus the form check in `parse_claim`.

Worked example: for the claim above, the hashed bytes are 1425 bytes
(wrapped here at field boundaries; the real bytes have no line breaks):

```text
{"base_url":"http://127.0.0.1:18090/v1","catalog":{"manifest_key":"ts/v1/repo/model/Qwen/Qwen2.5-1.5B-Instruct-GGUF/rev/91cad51170dc346986eccefdc2dd33a9da36ead9","manifest_sha256":"7abb51d7f81299d068ddf9bc5c723f4a414857f74c5a11f4a32fa97f070509a2","name":"tensorseed","publisher_key_id":"CE1E0E3D543DFFB8"},
"created_at":"2026-11-14T21:48:28Z","engine_version":"version: 1.0.0-dev (build 12366, commit 31953bcf7)","entrypoint":{"kind":"gguf","path":"qwen2.5-1.5b-instruct-q2_k.gguf"},
"files":[{"path":"LICENSE","sha256":"832dd9e00a68dd83b3c3fb9f5588dad7dcf337a0db50f7d9483f310cd292e92e","size":11343},{"path":"README.md","sha256":"57c72d4010fb11a98fc8e62dc8ef112b23a7a7525b71e571ca978d35cb3b2eec","size":4856},{"path":"qwen2.5-1.5b-instruct-q2_k.gguf","sha256":"5ede348e91ce1e7a330926ec5b202c27b864d065149dc463257fde1f98865b3a","size":752880160}],
"license":{"class":"open","commercial_serving":"allowed","id":"apache-2.0","rights_asserted":false},"logprobs":"required","public_base_url":"https://llm.example.com/v1","repo":"Qwen/Qwen2.5-1.5B-Instruct-GGUF",
"response_key":{"alg":"ed25519","pubkey":"b263675c2621c515f3e8299ec1bea216c4c6b9269a6bf83f109e0af86a084bb5"},"revision":"91cad51170dc346986eccefdc2dd33a9da36ead9","runtime":"llama-cpp","schema":"ts.served-weights/1",
"seller_wallet":"0x6c72521c15d87eedebe623c63e5ae10dc05da50d","served_model_id":"qwen2.5-1.5b-instruct","started_at":"2026-11-14T21:48:28Z","type":"model"}
```

Their SHA-256 is
`0a00a5079b7176d65a5c0a33b5ee43c2daab4f8c00b2f44307bebc51177c7185`.
Reproduce it in Python:

```python
import hashlib, json
c = json.load(open("served-weights.json"))
for k in ("claim_sha256", "sig", "signed"):
    c.pop(k, None)
for k in ("public_base_url", "seller_wallet", "expires_at"):
    if k in c and c[k] is None:
        del c[k]
body = json.dumps(c, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
print(hashlib.sha256(body.encode()).hexdigest())
# 0a00a5079b7176d65a5c0a33b5ee43c2daab4f8c00b2f44307bebc51177c7185
```

When Tensorseed reads a claim (`claim_from_bytes` in Rust, used by
every command that takes one), it also requires: `claim_sha256` and
`catalog.manifest_sha256` are 64 lowercase hex; `response_key` is
`ed25519` with a valid 32 byte public key; `served_model_id` is valid;
`seller_wallet`, if present, is already lowercase; `signed` agrees with
`sig`; `expires_at`, if present (0.3.1), is RFC 3339 and later than
`created_at`; and the wallet signature, if present, recovers to
`seller_wallet`. Reading a claim does not look at the clock; expiry is
checked by `check-claim` and `verify-response`. A missing or non `https` `public_base_url` is only a
warning, so check it yourself ([4.6](#46-listing-checks)).

### 4.4 The wallet signature

`seller_wallet` and `sig` are optional. When present, `sig.value` is an
Ethereum `personal_sign` (EIP-191) signature by `seller_wallet` over
this exact message:

```text
tensorseed served-weights/1:
<claim_sha256>
```

That is `tensorseed served-weights/1:` and a newline, then the 64 hex
characters, with no trailing newline. The signature is 65 bytes
(`r || s || v`, with `v` of 27, 28, 0, or 1). Tensorseed accepts only
low `s` values (EIP-2) and compares the recovered address in lowercase.
With ethers v6:

```js
import { verifyMessage } from "ethers";
const ok = verifyMessage(`tensorseed served-weights/1:\n${claim.claim_sha256}`, claim.sig.value)
  .toLowerCase() === claim.seller_wallet;
```

ethers does not enforce low `s`; Tensorseed does. Whether a wallet must
be present, and whether it must match a seller account you already
know, is your policy.

### 4.5 Match the claim to a catalog you trust

The claim's own checks prove only that it is internally consistent and
who signed it. The step that ties it to real weights is yours: look the
manifest up in your own verified catalog copy. `verify-response` does
not do this.

**0.3.1: `tensorseed check-claim`** does every check in this section and
the URL and license checks of [4.6](#46-listing-checks) and
[4.7](#47-license-marks-a-marketplace-should-act-on) in one command.
`tensorseed check-claim https://llm.example.com/v1` checks the claim
that endpoint publishes. The test seller has no public host, so this
run checked its claim file against the URL the claim names:

```text
$ tensorseed check-claim served-weights.json --expect-url https://llm.example.com/v1
claim      served-weights.json
  sha256   0a00a5079b7176d65a5c0a33b5ee43c2daab4f8c00b2f44307bebc51177c7185
  weights  Qwen/Qwen2.5-1.5B-Instruct-GGUF@91cad51170dc346986eccefdc2dd33a9da36ead9
  file     qwen2.5-1.5b-instruct-q2_k.gguf
  model id qwen2.5-1.5b-instruct
  endpoint https://llm.example.com/v1
  wallet   0x6c72521c15d87eedebe623c63e5ae10dc05da50d (verified)
  catalog  tensorseed
  license  apache-2.0 -> allowed (from the catalog; the claim says allowed)

  ok   claim_sha256     0a00a5079b7176d65a5c0a33b5ee43c2daab4f8c00b2f44307bebc51177c7185 is the SHA-256 of the canonical JSON without claim_sha256, sig, and signed
  ok   claim_fields     schema, response key, model id, wallet, and expires_at formats are valid
  warn expiry           no expires_at; the claim stays valid until it is revoked or replaced
  ok   wallet_signature EIP-191 signature over claim_sha256 recovers to 0x6c72521c15d87eedebe623c63e5ae10dc05da50d
  ok   public_url       https://llm.example.com/v1 is the expected URL
  skip revocation       read from a file; pass --revocations, or check the URL, to check revocations
  ok   catalog          catalog tensorseed is one you trust
  ok   publisher_key    publisher key CE1E0E3D543DFFB8 is the key pinned for tensorseed
  ok   manifest         Qwen/Qwen2.5-1.5B-Instruct-GGUF@91cad51170dc346986eccefdc2dd33a9da36ead9 is in tensorseed with manifest sha256 7abb51d7f81299d068ddf9bc5c723f4a414857f74c5a11f4a32fa97f070509a2
  ok   files            3 file(s) match the manifest by path, size, and sha256
  ok   takedown         no takedown covers this revision
  ok   freshness        catalog synced during this check at revision 4 (an expired head is refused); chain check (strict) anchored revision 4, coin current, 0s ago
  ok   license          apache-2.0: allowed (recomputed from the catalog)

verdict: pass (11 pass, 0 fail, 1 warn, 1 skip)
note: a valid claim says what the seller serves; logprob challenges (check-endpoint) test that the endpoint really serves it
```

- SOURCE is the endpoint base URL (the claim is read from
  `/.well-known/tensorseed/served-weights.json` under it, without a
  trailing `/v1`), a claim URL ending in `.json`, or a local file. At
  most 3 redirects are followed and the final URL is the one compared.
- `claim_sha256` is recomputed under the canonical rule of
  [4.3](#43-the-canonical-hash-claim_sha256). The wallet signature
  ([4.4](#44-the-wallet-signature)) is checked when present; a claim
  without one is a `warn`, or a `fail` with `--require-wallet-sig`.
  `--expect-wallet 0x...` fails a claim that names another wallet.
- `expiry` (0.3.1): a claim past its `expires_at` fails. A claim
  without one is a `warn` (every 0.3.0 claim), or a `fail` with
  `--require-expiry`. See [4.8](#48-expiry-and-revocation).
- `revocation` (0.3.1): for a claim read from its well-known path, the
  seller's `/.well-known/tensorseed/revocations.json` next to it is
  fetched (404 means the seller publishes none, a `pass`).
  `--revocations <file|URL>` (repeatable) adds your own lists. A verified
  revocation in effect fails the claim; one dated in the future is a
  `warn`. When the seller's list cannot be fetched or does not parse the
  check is a `warn` and a run that would pass is `inconclusive` (exit 2),
  because the claim may be revoked, even when your own lists were read;
  a revocation in the lists that were read still fails the claim.
  `--no-revocations` skips the seller's list, the way to accept one that
  cannot be read. A `--revocations` list that cannot be read is an
  error. For a file without `--revocations` the check is skipped.
- `public_url`: the claim must have been fetched from
  `public_base_url`'s well-known path. The same valid claim served by
  another host fails. For a file, pass `--expect-url`, or the check is
  skipped.
- `catalog`: the catalog the claim names must be one in your config
  (the default catalog or one added with `tensorseed catalog add`), and
  its pinned publisher key id must equal the claim's. `--catalog NAME`
  picks the configured catalog to compare with; the names must still
  agree.
- `manifest`, `files`, `takedown`: the signed manifest for the claimed
  revision is read from that catalog (signature checked against the
  pinned key), and `manifest_key`, `manifest_sha256`, repo, revision,
  every claimed file (path, size, hash), and the entrypoint must match.
  A tombstone covering the revision fails.
- `freshness`: the catalog is synced during the check (an expired head
  is refused; the chain check runs as configured). `--no-wait` uses the
  local copy and reports a `warn`, or a `fail` with `--require-fresh`.
- `license`: the commercial serving mark is recomputed from the
  catalog's license; a claim whose mark, class, or license id differs
  fails, even if it was resealed.
- Exit status 0 pass, 1 fail (any failed check), 2 inconclusive (the
  claim or the catalog could not be read). `--json` prints
  `ts.claim-check/1` with the claim fields, `wallet_verified`,
  `catalog`, `freshness`, the recomputed `license`, `expires_at` and
  `revoked` (0.3.1), every check with its
  `status` (`pass`, `fail`, `warn`, `skip`) and `detail`, `verdict`, and
  `counts`.

A claim edited after sealing fails on its hash, and the wallet
signature is then not counted, because it covers the stated hash:

```text
$ tensorseed check-claim edited-claim.json --no-wait
...
  FAIL claim_sha256     the claim says 0a00a5079b7176d65a5c0a33b5ee43c2daab4f8c00b2f44307bebc51177c7185 but its contents hash to f75829515af7a16d1d476a553f7db61558e8de0a674cc8f34116560d2123f5da; it was changed after it was written
  skip claim_fields     not checked: claim_sha256 does not match
  skip wallet_signature not checked: a signature over claim_sha256 says nothing about contents that hash differently
...
verdict: fail (6 pass, 1 fail, 1 warn, 5 skip)
```

Without Tensorseed (or on 0.3.0), do the catalog match yourself. With a
`catalog export` ([3.2](#32-verify-a-catalog-head-with-the-cli)):

```python
import hashlib, json, sys

claim = json.load(open(sys.argv[1]))            # ts.served-weights/1
export = json.load(open(sys.argv[2]))           # ts.export/1 from your own pinned config

def fail(msg):
    print("no match:", msg); sys.exit(1)

if claim["catalog"]["name"] != export["catalog"]:
    fail("claim names another catalog")
entries = {e["key"]: e for e in export["entries"]}
entry = entries.get(claim["catalog"]["manifest_key"])
if entry is None:
    fail("manifest_key is not in this catalog")
if hashlib.sha256(entry["value"].encode()).hexdigest() != claim["catalog"]["manifest_sha256"]:
    fail("manifest_sha256 differs from the signed manifest")
manifest = json.loads(entry["value"])
if manifest["repo"]["id"] != claim["repo"] or manifest["hf_commit"] != claim["revision"]:
    fail("repo or revision differs")
signed = {f["path"]: (f["size"], f["sha256"]) for f in manifest["files"]}
for f in claim["files"]:
    if signed.get(f["path"]) != (f["size"], f["sha256"]):
        fail(f"file {f['path']} is not in the signed manifest with that size and hash")
ep = claim["entrypoint"]
if ep["kind"] == "gguf" and ep["path"] not in {f["path"] for f in claim["files"]}:
    fail("entrypoint is not among the claimed files")
tomb = entries.get("ts/v1/repo/%s/%s/tombstone" % (claim["type"], claim["repo"]))
if tomb is not None:
    fail("the repo is taken down in this catalog")
print("match:", claim["repo"], claim["revision"], ep.get("path", "(directory)"))
```

```text
$ python3 match_claim.py served-weights.json catalog.ts-export.json
match: Qwen/Qwen2.5-1.5B-Instruct-GGUF 91cad51170dc346986eccefdc2dd33a9da36ead9 qwen2.5-1.5b-instruct-q2_k.gguf
```

The JavaScript example in [3.5](#35-check-catalogs-from-javascript-tensorseed-wasm)
does the same against a `CatalogVerifier`. Notes:

- For a very large repo the manifest's file list can be split into
  pages (`file_pages` above 0, keys `.../rev/<commit>/files/<page:06>`).
  Merge the pages before comparing.
- A seller may verify and claim only the files it pulled (here one GGUF
  of the nine in the repo). Every claimed file must be in the manifest
  with the same size and hash. For `{"kind":"directory"}` (vLLM) the
  claim should list the config, the tokenizer files, and every weight
  shard.
- `check-endpoint` gives you an indirect match as well: it refuses a
  claim whose `manifest_sha256` or `entrypoint` differs from the
  reference set you generated from your own verified copy
  ([section 6](#6-run-logprob-spot-challenges)).

### 4.6 Listing checks

`check-claim` (0.3.1) covers the first point when you give it the
listed base URL. The rest are yours:

- `public_base_url` equals the base URL the seller listed with you, and
  uses `https`. Fetch the claim from that URL's well-known path, not
  from a URL the seller gives separately.
- `served_model_id` equals the model id in the listing and appears in
  the endpoint's `/v1/models`.
- `logprobs` is `required` if the listing is to be spot checked.
- The claim stays the same. Re-fetch it on your health check schedule.
  A new `claim_sha256` means a new statement (a new file, model id,
  endpoint, engine version, wallet, or response key) and should be
  checked like a new listing. A seller restarting with the same settings
  keeps the same claim, wallet signature included.
- A 0.3.0 claim has no expiry and no revocation. `started_at` only says
  when this serving period began. **0.3.1** adds both
  ([4.8](#48-expiry-and-revocation)); decide whether you require
  `expires_at` for new listings.

### 4.7 License marks a marketplace should act on

The mark in a claim is computed on the seller's machine, and the seller
could edit it before sealing the claim. Recompute it from the signed
manifest's license (`license.spdx` and `license.class`) with your own
copy: `check-claim` (0.3.1) does this and fails a claim whose mark
differs, `tensorseed list --json` gives `commercial_serving` and
`commercial_note` per entry, and `tensorseed-wasm` has
`commercial_serving(license, class)`. The table lives in
`crates/tensorseed-core/src/commercial.rs`.

| Mark | Meaning | Suggested marketplace action |
|---|---|---|
| `allowed` | Permissive license without field of use limits | List. Show the license and keep its notices available |
| `restricted` | Commercial use with conditions (for example the Llama licenses' user cap, acceptable use policy, and attribution; Gemma and OpenRAIL use restrictions; AGPL network source duties) | List only if your terms pass the conditions on to buyers. Show the conditions |
| `forbidden` | Non-commercial or research only | Do not list for paid access. `rights_asserted: true` means the seller says it holds a separate license; accept that only through your own review |
| `unknown` | Missing, `other`, or custom | Hold for manual review |

These marks come from the license id only and are not legal advice.

### 4.8 Expiry and revocation

**0.3.1.** A claim can carry `expires_at`, and a seller can revoke a
claim with a signed entry in a `ts.claim-revocations/1` list. Both are
optional and 0.3.0 claims keep working. The full design is in
[verified-serving.md](plans/verified-serving.md) section 3.4.

**Expiry.** `expires_at` is inside the hashed body, so a seller cannot
extend it without a new `claim_sha256` (and a new wallet signature). A
claim is valid until that instant and fails `check-claim` after it. A
response is checked against it by its signed `timestamp`, not by when
you check: a response signed after `expires_at` fails with
`claim_expired`, while one signed before it stays verifiable later.
Sellers set it with `claim-serving --expires-in 30d` or
`serve-inference --sign --expires-in 30d` and renew by writing a new
claim. A claim with `expires_at` is rejected by 0.3.0 binaries, which do
not know the field.

**Revocation lists.** The seller's signing proxy serves its list at
`/.well-known/tensorseed/revocations.json`, next to the claim (404 when
it has none):

```json
{
  "schema": "ts.claim-revocations/1",
  "revocations": [
    {
      "claim_sha256": "0a00a5079b7176d65a5c0a33b5ee43c2daab4f8c00b2f44307bebc51177c7185",
      "revoked_at": "2026-11-25T00:00:00Z",
      "reason": "superseded",
      "response_sig": "<128 lowercase hex>"
    }
  ]
}
```

An entry is checked against the claim it names, so you need not trust
whoever hosts the list, and you can merge lists or keep your own. Each
entry carries `response_sig` (Ed25519 by the claim's `response_key`),
`wallet_sig` (`{"alg":"eip191","value":"0x..."}` by the claim's
`seller_wallet`), or both, over this message (a newline after every
line, including the last):

```text
ts.claim-revocation/1
claim_sha256=<claim_sha256>
revoked_at=<revoked_at, exactly as in the entry>
reason=<reason>
```

Every signature present must verify, or the entry is ignored and the
check reports it. Either key alone can revoke; nothing undoes a
revocation. `reason` is `[a-z0-9_]{1,32}`; Tensorseed writes
`superseded`, `stopped`, `key_compromised`, and `other`. What it means
for you:

| Situation | `check-claim` | `check-endpoint` | Responses (`verify-response`) |
|---|---|---|---|
| `revoked_at` in the past | `revocation` fails | The run fails (`revoked`); fresh responses are signed after it | Signed at or after `revoked_at`: `claim_revoked`. Signed before: still valid |
| `revoked_at` in the future | `revocation` warns | A warning (`pending`); the responses verify | Same rule, so they verify until then |
| `reason` `key_compromised` | `revocation` fails | The run fails (`revoked`) | Every response under the claim fails with `claim_revoked`, whenever it was signed |
| An entry whose signature does not verify | Ignored, reported as a `warn` | Ignored, counted in `ignored` | Ignored |
| An entry for another claim | Not counted | Not counted | Not counted |
| No list (404) | `revocation` passes | Reported (`no_list`); no effect | Nothing to apply |
| The list cannot be read or does not parse | `revocation` warns; a passing run is `inconclusive` | A passing run is `inconclusive` (`unreadable`) | `verify-response` stops with an error |

All three commands take `--revocations <file|URL>` (repeatable) for
lists you keep, and `--no-revocations` to skip the seller's list (the
lists you pass are still read). In `check-claim` and `check-endpoint` a
seller list that cannot be read makes a passing run inconclusive even
when your lists were read, unless they revoke the claim; a list you pass
that cannot be read is an error (exit 2) in all three. `check-claim` and `verify-response`
fetch the seller's list next to a claim read from its well-known path;
`check-endpoint` does the same, and otherwise fetches it from the
endpoint it checks.

Policy suggestions: re-fetch the list on your health check schedule
with the claim; stop routing to a claim as soon as a revocation is in
effect; keep a `key_compromised` revocation on file and treat responses
under that claim as unattributable in disputes; require `expires_at` for
new listings once your sellers run 0.3.1 (`check-claim
--require-expiry`). A list holds at most 10,000 entries; the CLI and the
npm package read at most 4 MiB.

## 5. Verify signed responses

### 5.1 Headers

Send a request id with every call you want to verify:

| Request header | Value |
|---|---|
| `X-Tensorseed-Request-Id` | 1 to 128 characters of `A-Z a-z 0-9 . _ ~ : -`. A UUID or 64 hex works. The proxy answers HTTP 400 to an invalid id, and mints (64 hex) and echoes one when it is missing |

The proxy adds these to every JSON response, whatever the status,
including `/v1/models`:

| Response header | Value |
|---|---|
| `X-Tensorseed-Request-Id` | The request id, echoed byte for byte |
| `X-Tensorseed-Claim-SHA256` | The claim's `claim_sha256` |
| `X-Tensorseed-Served-Model` | The claim's `served_model_id` |
| `X-Tensorseed-Timestamp` | RFC 3339 UTC time of signing |
| `X-Tensorseed-Body-SHA256` | SHA-256 of the exact body bytes, 64 lowercase hex |
| `X-Tensorseed-Signature` | Ed25519 signature, 128 lowercase hex |

Other content types pass through unsigned, with the request id echoed.

### 5.2 The signed message

The signature is raw Ed25519 (not minisign) by the claim's
`response_key.pubkey`, over the UTF-8 bytes of exactly these six lines,
each ending in `\n`, the last one included:

```text
ts.response-sig/1
claim_sha256=<X-Tensorseed-Claim-SHA256>
request_id=<X-Tensorseed-Request-Id>
served_model_id=<X-Tensorseed-Served-Model>
timestamp=<X-Tensorseed-Timestamp>
body_sha256=<X-Tensorseed-Body-SHA256>
```

Verification, in the order Tensorseed does it (`verify_fields`):

1. Header formats: both hashes 64 lowercase hex, the signature 128
   lowercase hex, a valid request id, a valid model id.
2. `X-Tensorseed-Claim-SHA256` equals the claim you verified.
3. `X-Tensorseed-Served-Model` equals the claim's `served_model_id`, and
   the model you requested.
4. The request id equals the one you sent.
5. The timestamp is within the skew window (300 s either way by
   default; see [Skew window](#skew-window)).
6. The SHA-256 of the body bytes equals `X-Tensorseed-Body-SHA256`.
7. The Ed25519 signature verifies.

Replay detection (rejecting a request id you already accepted for the
same claim) needs state and is left to you.

### 5.3 Streams

For `"stream": true`, the proxy passes events through as they arrive.
The response headers carry the request id, the claim hash, the served
model, and `X-Tensorseed-Signature-Mode: sse-trailer`. After the engine
ends the stream, the proxy appends one SSE comment line, which SSE
clients ignore. The end of a real stream:

```text
data: [DONE]

: ts.response-sig/1 timestamp=2026-11-14T21:49:54Z body_sha256=d492ec7b384a07e137aa1c2d2779d89507fde92caadd76ffbd6da6778d2d108b signature=33adb604f1138c10e401efad5e22a2bce24c458379cdf4397dcc59fede4997641f30e0ccc772bc19ec7f6a95dd2680c2ee5a75822378410f0e05540f3e3c0701

```

`body_sha256` covers every byte before the `: ts.response-sig/1 ` line.
The signature is over the same six line message, with the timestamp and
body hash taken from the trailer. A stream that was cut short, or whose
upstream failed, has no trailer and is unsigned. Keep the raw bytes (for
example `curl -N -o`) to verify.

### 5.4 With the CLI

```text
$ curl -s -D h.txt -o b.json https://llm.example.com/v1/chat/completions \
    -H "Authorization: Bearer $KEY" -H 'content-type: application/json' \
    -H 'X-Tensorseed-Request-Id: mkt-test-1' \
    -d '{"model":"qwen2.5-1.5b-instruct","messages":[{"role":"user","content":"hello"}],"max_tokens":8}'
$ grep -i '^x-tensorseed' h.txt
x-tensorseed-request-id: mkt-test-1
x-tensorseed-claim-sha256: 0a00a5079b7176d65a5c0a33b5ee43c2daab4f8c00b2f44307bebc51177c7185
x-tensorseed-served-model: qwen2.5-1.5b-instruct
x-tensorseed-timestamp: 2026-11-14T21:49:48Z
x-tensorseed-body-sha256: dc79e3b7502e715dd755afbe5e8051ef73992a5d16f29b5d2617c796c623f7bf
x-tensorseed-signature: 2cd07428f81823dfe5a5134e210ee7e7d302311b196ab12db360624842a3be7dafa66c29484d2378e671fc6de05206fef98436b5c726be69c1b1fea66f30a605
$ tensorseed verify-response --claim https://llm.example.com/v1 \
    --headers h.txt --body b.json --request-id mkt-test-1 --model-id qwen2.5-1.5b-instruct
valid: signed by the response key of claim 0a00a5079b7176d65a5c0a33b5ee43c2daab4f8c00b2f44307bebc51177c7185
  model      qwen2.5-1.5b-instruct
  request id mkt-test-1
  timestamp  2026-11-14T21:49:48Z
  wallet     0x6c72521c15d87eedebe623c63e5ae10dc05da50d (claim signature verified)
```

`--claim` takes a file, a URL ending in `.json`, or a base URL (which
fetches the well-known path). **0.3.1:** when the claim comes from its
well-known path, the seller's revocation list next to it is fetched too
(404 means none), and `--revocations <file|URL>` (repeatable) adds
lists you keep. The claim's `expires_at` and any verified revocation are
applied by the response's signed timestamp
([4.8](#48-expiry-and-revocation)), and the JSON gains
`claim_expires_at` and `revocation_lists`.

**URL checks (0.3.1).** A claim given as a URL must be the one published
at its own `public_base_url`, the `public_url` rule of `check-claim`
([4.5](#45-match-the-claim-to-a-catalog-you-trust)); a claim fetched
from anywhere else fails with `url_mismatch`. `--response-url <URL>`
names the URL you sent the request to, which must be under the claim's
`public_base_url` (same scheme, host, and port, and under its path), or
the response fails with `url_mismatch`. A claim read from a file is
not tied to a URL unless you pass `--response-url`, so after checking a
claim once, keep it as a file and pass `--response-url` for each
response. In the JavaScript package these are the `originUrl` and
`responseUrl` options of `verifyResponse`. `--json` prints a result
object:

```json
{
  "body_sha256": "dc79e3b7502e715dd755afbe5e8051ef73992a5d16f29b5d2617c796c623f7bf",
  "claim_sha256": "0a00a5079b7176d65a5c0a33b5ee43c2daab4f8c00b2f44307bebc51177c7185",
  "claim_source": "https://llm.example.com/.well-known/tensorseed/served-weights.json",
  "request_id": "mkt-test-1",
  "seller_wallet": "0x6c72521c15d87eedebe623c63e5ae10dc05da50d",
  "served_model_id": "qwen2.5-1.5b-instruct",
  "timestamp": "2026-11-14T21:49:48Z",
  "valid": true
}
```

A failure has `"valid": false`, an `error` label, and a `detail`. With
one body byte changed:

```json
{
  "claim_sha256": "0a00a5079b7176d65a5c0a33b5ee43c2daab4f8c00b2f44307bebc51177c7185",
  "claim_source": "served-weights.json",
  "detail": "the body does not match the signed body hash",
  "error": "body_hash_mismatch",
  "valid": false
}
```

Error labels: `absent`, `missing_header`, `malformed_header`,
`claim_mismatch`, `request_id_mismatch`, `model_mismatch`,
`timestamp_skew`, `body_hash_mismatch`, `bad_signature`, and in 0.3.1
`claim_expired`, `claim_revoked`, and `url_mismatch`. Exit status is
0 valid, 1 invalid or unsigned, and 2 for an error (for example a claim
that does not verify). For a stream, pass the raw saved stream as
`--body`.

#### Skew window

The skew check compares the signing time with the checking time, by
default now, and allows 300 s either way. Set your own default with
`[verify] max_skew_secs` in the config (0.3.1) and override it for one
run with `--max-skew`; both take 1 to 604800 seconds (7 days), and a
value outside that range is an error (exit 2). The same window applies
to `check-endpoint`. To check a response you stored, do not widen the
window: pass the time you received it with `verify-response --at
<RFC 3339>` (0.3.1), and the JSON gains `checked_at`. Expiry and
revocation are judged by the signing time either way. In the
JavaScript package, pass `now` and `maxSkewSecs` the same way.

### 5.5 In Rust

`tensorseed-app` is not on crates.io. Depend on the release tag:

```toml
[dependencies]
anyhow = "1"
tensorseed-app = { git = "https://github.com/TunedChaos/TensorSeed", tag = "v0.3.0" }
```

```rust
use tensorseed_app::signing::{Expect, WalletStatus, claim_from_bytes, extract_signed, verify_fields};

/// Check one response. `headers` are (name, value) pairs as received;
/// `body` is the exact response bytes, before any gateway edits.
fn check(
    claim_json: &[u8],
    headers: &[(String, String)],
    body: &[u8],
    sent_request_id: &str,
    requested_model: &str,
) -> anyhow::Result<()> {
    // Schema, claim_sha256, response key, and the wallet signature if present.
    let (claim, check) = claim_from_bytes(claim_json)?;
    match &check.wallet {
        WalletStatus::Verified(addr) => println!("claim signed by wallet {addr}"),
        WalletStatus::NotSigned => println!("claim is not wallet-signed"),
    }
    let (fields, signed_bytes) = extract_signed(headers, body)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as i64;
    verify_fields(
        &fields,
        signed_bytes,
        &Expect {
            pubkey: claim.response_pubkey()?,
            claim_sha256: &claim.claim_sha256,
            served_model_id: &claim.served_model_id,
            request_id: Some(sent_request_id),
            requested_model: Some(requested_model),
            now_unix: now,
            max_skew_secs: 300,
        },
    )
    .map_err(|e| anyhow::anyhow!("{} ({})", e, e.label()))?;
    println!("valid response {} at {}", fields.request_id, fields.timestamp);
    Ok(())
}
```

Run against a fresh response, and then with a different requested model:

```text
claim signed by wallet 0x6c72521c15d87eedebe623c63e5ae10dc05da50d
valid response mkt-test-3 at 2026-11-14T21:51:57Z

claim signed by wallet 0x6c72521c15d87eedebe623c63e5ae10dc05da50d
Error: served model "qwen2.5-1.5b-instruct" is not the claimed or requested model (model_mismatch)
```

`extract_signed` reads the headers, or the headers plus the trailer for
a stream, and returns the bytes the signature covers. `tensorseed-app`
is the library the CLI and Desktop share, so it brings their full
dependency tree.

**0.3.1: depend on `tensorseed-verify` instead.** It is the verify-only
crate (no networking, no torrent, no serving, no async runtime; with
`default-features = false` it builds for `wasm32-unknown-unknown`).
The same check, with expiry and
revocations:

```toml
[dependencies]
anyhow = "1"
tensorseed-verify = { git = "https://github.com/TunedChaos/TensorSeed", tag = "v0.3.1" }
```

```rust
use tensorseed_verify::claim::{WalletStatus, parse_claim};
use tensorseed_verify::response::{Expect, extract_signed, verify_fields};
use tensorseed_verify::revocation::Revocations;

/// Check one response against a claim and the revocation lists you hold.
fn check(
    claim_json: &[u8],
    revocations_json: Option<&[u8]>,
    headers: &[(String, String)],
    body: &[u8],
    sent_request_id: &str,
    requested_model: &str,
) -> anyhow::Result<()> {
    let claim = parse_claim(claim_json)?;
    // Schema, claim_sha256, field formats, and the wallet signature if present.
    match claim.verify()?.wallet {
        WalletStatus::Verified(addr) => println!("claim signed by wallet {addr}"),
        WalletStatus::NotSigned => println!("claim is not wallet-signed"),
    }
    let revoked = match revocations_json {
        Some(raw) => Revocations::parse(raw)?.revoked_from(&claim),
        None => None,
    };
    let (fields, signed_bytes) = extract_signed(headers, body)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as i64;
    verify_fields(
        &fields,
        signed_bytes,
        &Expect {
            pubkey: claim.response_pubkey()?,
            claim_sha256: &claim.claim_sha256,
            served_model_id: &claim.served_model_id,
            request_id: Some(sent_request_id),
            requested_model: Some(requested_model),
            now_unix: now,
            max_skew_secs: 300,
            claim_expires_unix: claim.expires_unix()?,
            claim_revoked_unix: revoked,
        },
    )
    .map_err(|e| anyhow::anyhow!("{} ({})", e, e.label()))?;
    println!("valid response {} at {}", fields.request_id, fields.timestamp);
    Ok(())
}
```

`tensorseed_verify::claim_check` has the full `check-claim` logic as pure
functions (`check_document`, `check_origin`, `check_revocation`,
`build_report`) for callers that fetch the claim and the catalog
themselves. The `tensorseed_app::signing` paths above still work in
0.3.1; they re-export these types.

### 5.6 Verify in JavaScript

**0.3.1: `@tensorseed/verify`.** `npm/verify` packages the WebAssembly
build of `tensorseed-verify` for Node and browsers (ES module, typed),
so a JavaScript service runs the same code as the CLI. Build it from a
checkout with `pnpm --dir npm/verify test` (it builds `pkg/` and runs the
Node tests); the package name is reserved for a release.

```js
import { init, openCatalog, checkEndpoint, verifyResponse } from "@tensorseed/verify";

await init(); // a no-op in Node; fetches the .wasm next to the module in a browser

// Listing: verify your pinned catalog (signed snapshot or deltas, then the
// signed head), then check the seller's claim and revocation list.
const { verifier } = openCatalog({ name: "tensorseed", pubkey: PIN.pubkey, snapshot, deltas, head });
const report = await checkEndpoint("https://llm.example.com/v1", {
  catalog: verifier,
  requireWalletSig: true,
  requireExpiry: false, // true once your sellers run 0.3.1
});
// report is ts.claim-check/1, exactly as `check-claim --json` prints it.
if (report.verdict !== "pass") throw new Error(JSON.stringify(report.checks));

// Per response: the exact bytes, the headers, and the request id you sent.
const result = verifyResponse({ claim, headers: res.headers, body, requestId, requestedModel, revocations });
if (!result.valid) console.warn(result.error); // body_hash_mismatch, claim_revoked, ...
```

`checkClaim(claim, options)` runs the checks that need no catalog (best
verdict `inconclusive`), `checkClaimWithCatalog(verifier, claim,
options)` runs all of them, and `claimSigningMessage`,
`revocationSigningMessage`, and `recoverWallet` give the raw pieces.
`checkEndpoint` is the only function that touches the network, through
the `fetch` you pass (or the global one). Like `check-claim`, it makes a
passing verdict `inconclusive` when the seller's revocation list cannot
be fetched or does not parse (`noRevocations: true` skips it). The
package README has the details.

Without the package (or on 0.3.0), here is a direct implementation of the format above with `node:crypto` (Node 18
or newer, no dependencies). It verified the same responses as the CLI,
streams included, matched Tensorseed's `claim_sha256` (with the form
rules in [4.3](#43-the-canonical-hash-claim_sha256)), and rejected
the changed body:

```js
import { createHash, createPublicKey, verify } from "node:crypto";

const sha256 = (b) => createHash("sha256").update(b).digest("hex");

// Canonical JSON as Tensorseed writes it: keys sorted, no whitespace.
function canonical(v) {
  if (Array.isArray(v)) return `[${v.map(canonical).join(",")}]`;
  if (v && typeof v === "object") {
    return `{${Object.keys(v).sort().map((k) => `${JSON.stringify(k)}:${canonical(v[k])}`).join(",")}}`;
  }
  return JSON.stringify(v);
}

export function claimSha256(claim) {
  const body = { ...claim };
  delete body.claim_sha256;
  delete body.sig;
  delete body.signed;
  // The form rules of section 4.3 (unknown fields are not checked here).
  for (const k of ["public_base_url", "seller_wallet", "expires_at"]) {
    if (body[k] === null) throw new Error(`${k} is null; it must be left out`);
  }
  if (!("publisher_key_id" in body.catalog) || !("id" in body.license)) {
    throw new Error("catalog.publisher_key_id and license.id must be present (null when unknown)");
  }
  return sha256(Buffer.from(canonical(body), "utf8"));
}

// headers: a Headers object or a plain object with lowercase names.
// body: the exact response bytes (Buffer).
export function verifyResponse(claim, headers, body, { requestId, model, maxSkewSecs = 300 } = {}) {
  const h = (n) => (typeof headers.get === "function" ? headers.get(n) : headers[n]) ?? null;
  if (claim.schema !== "ts.served-weights/1") throw new Error("not a ts.served-weights/1 claim");
  if (claimSha256(claim) !== claim.claim_sha256) throw new Error("claim_sha256 does not match the claim");

  let f = {
    claim: h("x-tensorseed-claim-sha256"),
    requestId: h("x-tensorseed-request-id"),
    model: h("x-tensorseed-served-model"),
    timestamp: h("x-tensorseed-timestamp"),
    bodySha256: h("x-tensorseed-body-sha256"),
    signature: h("x-tensorseed-signature"),
  };
  let signed = body;
  if (h("x-tensorseed-signature-mode") === "sse-trailer") {
    const marker = Buffer.from(": ts.response-sig/1 ");
    const at = body.lastIndexOf(marker);
    if (at < 0) throw new Error("stream has no signature trailer (cut short or unsigned)");
    const line = body.subarray(at + marker.length).toString("utf8").trim();
    const kv = Object.fromEntries(line.split(" ").map((p) => p.split("=")));
    f = { ...f, timestamp: kv.timestamp, bodySha256: kv.body_sha256, signature: kv.signature };
    signed = body.subarray(0, at);
  }
  for (const [k, v] of Object.entries(f)) if (!v) throw new Error(`missing ${k}`);
  if (f.claim !== claim.claim_sha256) throw new Error("response names a different claim");
  if (f.model !== claim.served_model_id || (model && model !== f.model)) throw new Error("served model mismatch");
  if (requestId && requestId !== f.requestId) throw new Error("request id was not echoed");
  const skew = Date.parse(f.timestamp) / 1000 - Date.now() / 1000;
  if (!(Math.abs(skew) <= maxSkewSecs)) throw new Error(`timestamp outside the window (${Math.round(skew)} s)`);
  if (sha256(signed) !== f.bodySha256) throw new Error("body does not match the signed body hash");

  const msg =
    `ts.response-sig/1\nclaim_sha256=${f.claim}\nrequest_id=${f.requestId}\n` +
    `served_model_id=${f.model}\ntimestamp=${f.timestamp}\nbody_sha256=${f.bodySha256}\n`;
  const key = createPublicKey({
    key: { kty: "OKP", crv: "Ed25519", x: Buffer.from(claim.response_key.pubkey, "hex").toString("base64url") },
    format: "jwk",
  });
  if (!verify(null, Buffer.from(msg, "utf8"), key, Buffer.from(f.signature, "hex"))) {
    throw new Error("signature does not verify with the claim's response key");
  }
  return { requestId: f.requestId, timestamp: f.timestamp, claim: f.claim };
}
```

This sketch skips the header format checks of step 1 in
[5.2](#52-the-signed-message), and the expiry and revocation rules of
[4.8](#48-expiry-and-revocation); add them if you accept responses at
scale. Check the wallet signature as in [4.4](#44-the-wallet-signature).

### 5.7 Rules for a gateway on the request path

- **Verify the seller's bytes first, then change anything.** The body
  hash covers the engine's body exactly. Hash and verify before you add
  billing fields, rename the model, or re-encode the JSON. Verifying
  after a change breaks honest signatures.
- **Compression.** The proxy asks the engine for the identity encoding,
  and sellers are told not to compress these paths at their reverse
  proxy. If your HTTP client decompresses transparently, verify the
  decoded bytes.
- **Forward streams untouched**, including the final comment line, if
  your buyers verify too.
- **Request ids.** Send a fresh, unpredictable id per call, require the
  echo, and keep recent ids per claim to reject replays.
- **A missing or invalid signature on a listing that advertised signing
  fails that response.** Whether it pauses the listing is your policy.
- Signed responses prove who answered and that the answer was not
  changed. They do not prove which model produced it. That is what
  [section 6](#6-run-logprob-spot-challenges) is for.

## 6. Run logprob spot challenges

A signed response proves who answered. A logprob challenge tests what
answered: you send short prompts whose next-token log probabilities you
measured on a verified copy of the claimed file, and compare the
endpoint's answers. Design and math: [verified-serving.md](plans/verified-serving.md)
section 5.

### 6.1 Make a reference set from your own verified copy

Pull the claimed revision yourself (`tensorseed pull`, which verifies
every file), then:

```text
$ tensorseed reference-logprobs dl/Qwen/Qwen2.5-1.5B-Instruct-GGUF \
    --file qwen2.5-1.5b-instruct-q2_k.gguf --count 16 \
    --out ref-q2_k.json --commitment ref-q2_k.commit.json
verifying dl/Qwen/Qwen2.5-1.5B-Instruct-GGUF against the signed manifest
verified 3 files of Qwen/Qwen2.5-1.5B-Instruct-GGUF@91cad51170dc346986eccefdc2dd33a9da36ead9 (manifest sha256 7abb51d7f81299d0)
starting llama.cpp: .../llama-server --model dl/Qwen/Qwen2.5-1.5B-Instruct-GGUF/qwen2.5-1.5b-instruct-q2_k.gguf --alias Qwen/Qwen2.5-1.5B-Instruct-GGUF --host 127.0.0.1 --port 37053
  8/16 challenges
  16/16 challenges
wrote ref-q2_k.json (16 challenges, ts.reference-logprobs/1)
  entrypoint qwen2.5-1.5b-instruct-q2_k.gguf
  engine     llama.cpp version: 1.0.0-dev (build 12366, commit 31953bcf7)
  set_hash   600fa5c51f8d22d6ce7c0e7f62fb0dba58292570eee0f951d1392741fcebeb0b
  commitment ref-q2_k.commit.json (public)
note: ref-q2_k.json holds the challenge prompts and expected values; keep it secret until the set is retired, and publish only set_hash or the commitment file
```

The default is 32 prompts from the `ts.challenge-gen/1` generator with a
random seed; `--prompts` takes your own. Other options: `--top-logprobs`
(default 5), `--max-tokens` (default 1), `--sampling-seed`, `--runtime`,
and `--endpoint` to use a server you already run for that copy. Make one
set per GGUF file (quantization) you list.

**0.3.1:** `--samples N` (default 5) makes N whole passes over the set
and records the per-token median, so one unlucky response cannot become
the expected value (the main cause of 0.3.0's false fails, see
[6.4](#64-tolerances-and-what-a-fail-means)). The run takes N times as
many requests; progress shows `requests (challenges x samples)`. The
file records `generator.samples`. `--samples 1` gives the 0.3.0 shape. Use the same engine family
the seller claims; a llama.cpp set says little about a vLLM endpoint.

Each run draws a fresh seed and salt, so it gets a new `set_hash`. The
set used in the rest of this section came from an earlier run; its
public commitment:

```json
{
  "schema": "ts.reference-logprobs-commitment/1",
  "set_hash": "6782e9273b0df285d26c15ab25722694ef415418eecd601b5d918c9f3f5c0851",
  "manifest_sha256": "7abb51d7f81299d068ddf9bc5c723f4a414857f74c5a11f4a32fa97f070509a2",
  "manifest_key": "ts/v1/repo/model/Qwen/Qwen2.5-1.5B-Instruct-GGUF/rev/91cad51170dc346986eccefdc2dd33a9da36ead9",
  "repo": "Qwen/Qwen2.5-1.5B-Instruct-GGUF",
  "revision": "91cad51170dc346986eccefdc2dd33a9da36ead9",
  "entrypoint": {
    "kind": "gguf",
    "path": "qwen2.5-1.5b-instruct-q2_k.gguf"
  },
  "entrypoint_sha256": "5ede348e91ce1e7a330926ec5b202c27b864d065149dc463257fde1f98865b3a",
  "sampling": {
    "temperature": 0.0,
    "seed": 1,
    "max_tokens": 1,
    "logprobs": true,
    "top_logprobs": 5
  },
  "generator": {
    "tool": "tensorseed reference-logprobs",
    "tensorseed_version": "0.3.0",
    "runtime": "llama-cpp",
    "engine_version": "version: 1.0.0-dev (build 12366, commit 31953bcf7)",
    "request_extra": {
      "cache_prompt": false
    },
    "note": "A reference run, not a bit-identical guarantee."
  },
  "generation_method": "ts.challenge-gen/1",
  "prompt_count": 16,
  "created_at": "2026-11-14T21:52:10Z"
}
```

`set_hash` is the SHA-256 of the canonical JSON
([3.4](#34-check-catalog-signatures-without-tensorseed-code)) of the
whole reference file with `set_hash` removed. The secret file holds a
random salt, so the hash reveals nothing about the prompts.

### 6.2 Commit, rotate, reveal

1. Publish `set_hash` or the commitment file before you use the set.
   Sellers cannot learn the prompts from it.
2. Keep the reference file private. Anyone who has it can answer those
   exact prompts from a table.
3. Retire sets on a schedule and make new ones.
4. Reveal a retired file when you want to show your checks were fair,
   or in a dispute. Anyone can check it against the published hash
   (`check-endpoint --expect-set-hash`, or recompute it), and anyone with
   a verified copy can regenerate the expected values with
   `reference-logprobs --prompts`.

0.3.0 has no reveal command and no format for publishing commitments;
host the commitment file wherever your sellers can read it.

**0.3.1: calibrate the set before you publish it.** Run
`calibrate-reference` against a server you run yourself for the same
verified copy (for example `reference-logprobs --endpoint` or
`serve-inference` on loopback). It sends the whole set `--runs` times
(default 10), measures how far honest answers drift, and writes a
calibrated copy whose `calibration` block holds the observed spread and
the tolerances `check-endpoint` will use:

```text
$ tensorseed calibrate-reference http://127.0.0.1:8090/v1 -r ref-q2_k.json \
    -o ref-q2_k.cal.json --commitment ref-q2_k.cal.commit.json
calibrating against http://127.0.0.1:8090/v1 (model Qwen/Qwen2.5-1.5B-Instruct-GGUF): 10 runs of 32 challenges
  run  1: median 0.179  max 0.308 nats
  run  2: median 0.189  max 0.417 nats
  run  3: median 0.170  max 0.367 nats
  run  4: median 0.174  max 0.338 nats
  run  5: median 0.152  max 0.334 nats
  run  6: median 0.140  max 0.356 nats
  run  7: median 0.159  max 0.310 nats
  run  8: median 0.160  max 0.367 nats
  run  9: median 0.163  max 0.337 nats
  run 10: median 0.198  max 0.479 nats

observed: per challenge mean p50 0.167, p90 0.278, p99 0.384, max 0.479; token max 0.892; run median max 0.198; first token match 84%
calibrated tolerance: mean <= 0.5, token <= 1.15, median <= 0.35, at most 25% failing
wrote ref-q2_k.cal.json
  set_hash   db4a338534d1b4acf25c1897bc5e28683b15d3ffbe6b8eeecdbfae52321dc71a (was ed77ccd7cad5056e0579f5316dc345cb04bea4cdd366bed5f64845285dad69b6)
  commitment ref-q2_k.cal.commit.json (public)
note: the calibration is part of the sealed set, so set_hash changed; publish the new set_hash (or commitment), not the old one
```

The calibration is inside the sealed set, so the set gets a new
`set_hash`; publish that one (the commitment gains a `tolerance`
field). How the tolerances are derived is in
[verified-serving.md](plans/verified-serving.md) section 5.7.1.
`--margin` (default 1.5) scales the p90 and run median terms. Calibrate
on the same engine family and, ideally, hardware class as your sellers;
GPU drift can be larger than this CPU run.

### 6.3 Check an endpoint

0.3.1, against the calibrated set above (the endpoint URL is shown as
the claim's public URL):

```text
$ tensorseed check-endpoint https://llm.example.com/v1 -r ref-q2_k.cal.json \
    --model-id qwen2.5-1.5b-instruct --api-key-env SELLER_KEY --sample 8
checking https://llm.example.com/v1 (model qwen2.5-1.5b-instruct) against Qwen/Qwen2.5-1.5B-Instruct-GGUF@91cad51170dc346986eccefdc2dd33a9da36ead9 qwen2.5-1.5b-instruct-q2_k.gguf
  set_hash db4a338534d1b4acf25c1897bc5e28683b15d3ffbe6b8eeecdbfae52321dc71a, 8 of 32 challenges, sample seed 47109d829dcbb5a2d778194488e1ba685328ef10f47aa4a9a664c9eb2d718b42
  tolerance mean <= 0.5 nats, token <= 1.15 nats, 3 attempt(s) (calibration)
  set rule: at most 25% of challenges outside tolerance, median deviation <= 0.35 nats
  signatures required: claim 0a00a5079b7176d6 from https://llm.example.com/.well-known/tensorseed/served-weights.json
  lp-0006  pass           1x  mean  0.145  max  0.274  top same  sig ok
  lp-0011  pass           1x  mean  0.265  max  0.647  top differs  sig ok
  lp-0015  pass           1x  mean  0.222  max  0.567  top same  sig ok
  lp-0022  pass           1x  mean  0.100  max  0.228  top same  sig ok
  lp-0023  pass           1x  mean  0.085  max  0.131  top same  sig ok
  lp-0027  pass           1x  mean  0.193  max  0.313  top differs  sig ok
  lp-0030  pass           1x  mean  0.288  max  0.474  top differs  sig ok
  lp-0032  pass           1x  mean  0.222  max  0.405  top same  sig ok
verdict: pass (8 pass, 0 fail, 0 inconclusive, 0 unsupported)
  0 of 8 scored challenges outside tolerance (max 25%), median deviation 0.207 nats (max 0.35)
signatures: 8 verified, 0 invalid, 0 absent, 0 unverifiable (claim signed by wallet 0x6c72521c15d87eedebe623c63e5ae10dc05da50d)
```

That endpoint was honest: the same file and the same llama.cpp build.
The same seller checked with 0.3.0 against its own single-sample set
passed 1 run in 11; see [6.4](#64-tolerances-and-what-a-fail-means).

What `check-endpoint` does:

- Loads the reference file and refuses it if its contents no longer
  match its own `set_hash`, or (with `--expect-set-hash`) the published
  one.
- Fetches the claim from the endpoint's well-known path (or `--claim`).
  If there is one, it refuses to run when the claim's `manifest_sha256`
  or `entrypoint` differs from the reference, or when `--model-id`
  differs from the claim's `served_model_id`, and it then requires a
  valid signature on every challenge response. `--require-signatures`
  fails when the endpoint serves no claim.
- **0.3.1:** with a claim, fetches the seller's revocation list
  (`/.well-known/tensorseed/revocations.json` next to a claim read from
  its well-known path, else at the endpoint) and any `--revocations`
  lists, verifies each entry against the claim as `check-claim` does
  ([4.8](#48-expiry-and-revocation)), and applies the result: a
  revocation in effect fails the run, `key_compromised` voids every
  response, one dated in the future is a warning and responses signed
  before it verify, a 404 is reported and changes nothing, and a list
  that cannot be read or does not parse turns a passing run into
  `inconclusive` (`--no-revocations` skips the seller's list). A
  response signed after the claim's `expires_at` fails its challenge.
- Sends `--sample` challenges (default 8; 0 for all), chosen by a
  printed `--sample-seed` so a run can be repeated.
- Re-sends a challenge outside tolerance up to `--repeats` times
  (default 3). It is `fail` only if every attempt fails. A pass after a
  failed attempt, or a transport or HTTP error, is `inconclusive`. A
  response without logprobs is `unsupported`.
- Decides over the set (0.3.1, `ts.set-decision/1`): any unsigned or
  badly signed response under a claim fails the run; fewer than half the
  sampled challenges scored (answered with logprobs) is `inconclusive`;
  all `unsupported` is `unsupported`; the run fails when more than
  `--max-fail-fraction` of the scored challenges are outside tolerance
  (default 0.25) or when their median mean deviation is above
  `--max-median` (default 0.35 nats); otherwise it passes. One honest
  outlier no longer fails a run; a different file fails most challenges
  and shifts the median. `--max-fail-fraction 0` restores the 0.3.0
  rule (any `fail` is `fail`). Exit status 0, 1, 2.
- Tolerances come from the flags, else the set's calibration, else the
  defaults; the `tolerance` line names the source.

The API key is read from the variable named by `--api-key-env` and
never printed. Do not pass `--no-prompt-cache` to a seller: it sends a
llama.cpp specific field that marks the request as a challenge. It is
for checking your own server.

`--json` prints `ts.check-endpoint/1`. 0.3.1 adds `decision` (`rule`,
`scored`, `failed`, `fail_fraction`, `median_mean_abs_dev`,
`signature_failures`, `reason`), `tolerance_source` (`default`,
`calibration`, `flags`, or `calibration+flags`), and
`max_fail_fraction` and `max_median_mean_abs` inside `tolerance`, and,
under a claim, `revocation` (`status`: `not_revoked`, `no_list`,
`pending`, `revoked`, `unreadable`, or `not_checked`; `detail`; and when
found `lists`, `revoked_at`, `reason`, `revoked_by`, and `ignored`);
existing fields are unchanged, and the 0.3.0 `verdict` field holds the
set decision. Its top level from a 0.3.0 run:

```json
{
  "schema": "ts.check-endpoint/1",
  "tensorseed_version": "0.3.0",
  "base_url": "https://llm.example.com/v1",
  "model_id": "qwen2.5-1.5b-instruct",
  "reference": {
    "set_hash": "6782e9273b0df285d26c15ab25722694ef415418eecd601b5d918c9f3f5c0851",
    "manifest_sha256": "7abb51d7f81299d068ddf9bc5c723f4a414857f74c5a11f4a32fa97f070509a2",
    "repo": "Qwen/Qwen2.5-1.5B-Instruct-GGUF",
    "revision": "91cad51170dc346986eccefdc2dd33a9da36ead9",
    "entrypoint": {
      "kind": "gguf",
      "path": "qwen2.5-1.5b-instruct-q2_k.gguf"
    },
    "runtime": "llama-cpp",
    "engine_version": "version: 1.0.0-dev (build 12366, commit 31953bcf7)",
    "prompt_count": 16
  },
  "sampling": {
    "temperature": 0.0,
    "seed": 1,
    "max_tokens": 1,
    "logprobs": true,
    "top_logprobs": 5
  },
  "tolerance": {
    "max_mean_abs": 0.5,
    "max_token_abs": 2.0
  },
  "repeats": 3,
  "concurrency": 1,
  "timeout_secs": 60,
  "sample": {
    "requested": 0,
    "sent": 16,
    "seed": "6486829a3fa4fcd02b5ee7bd592b25e1dee50514e4fa45f5d351d22e511ceba7"
  },
  "verdict": "fail",
  "counts": {
    "pass": 15,
    "fail": 1,
    "inconclusive": 0,
    "unsupported": 0
  },
  "signatures": {
    "claim_source": "https://llm.example.com/.well-known/tensorseed/served-weights.json",
    "claim_sha256": "0a00a5079b7176d65a5c0a33b5ee43c2daab4f8c00b2f44307bebc51177c7185",
    "response_pubkey": "b263675c2621c515f3e8299ec1bea216c4c6b9269a6bf83f109e0af86a084bb5",
    "seller_wallet": "0x6c72521c15d87eedebe623c63e5ae10dc05da50d",
    "verified": 16,
    "invalid": 0,
    "absent": 0,
    "unverifiable": 0
  },
  "checked_at": "2026-11-14T22:04:33Z",
  "challenges": [
    "..."
  ]
}
```

and one entry of `challenges`:

```json
{
  "id": "lp-0001",
  "verdict": "fail",
  "attempts": 3,
  "comparison": {
    "compared": 5,
    "positions": 1,
    "mean_abs_dev": 0.588886,
    "max_abs_dev": 1.376341,
    "missing": 1,
    "first_token_match": true
  },
  "attempt_mean_abs_dev": [
    0.588886,
    0.588886,
    0.588886
  ],
  "reason": "outside tolerance on all 3 attempt(s) (last mean 0.589, max 1.376 nats)",
  "signature": {
    "status": "verified"
  },
  "elapsed_ms": 93
}
```

The report has no prompts or expected values, so you can share it with
the seller without leaking the set.

### 6.4 Tolerances and what a fail means

| Option | Default | Meaning |
|---|---|---|
| `--max-mean-abs` | 0.5, or calibrated | Largest mean absolute logprob deviation per challenge, in nats |
| `--max-token-abs` | 2, or calibrated | Largest deviation of any single token, in nats |
| `--max-fail-fraction` | 0.25, or calibrated | 0.3.1. Largest share of scored challenges outside tolerance before the run fails |
| `--max-median` | 0.35, or calibrated | 0.3.1. Largest median of per-challenge mean deviations, in nats |
| `--repeats` | 3 | Attempts before a challenge fails |
| `--sample` | 8 | Challenges per run (0 for all) |
| `--concurrency` | 2 | Challenges in flight |
| `--timeout` | 60 | Seconds per request |
| `--max-skew` | `[verify] max_skew_secs`, else 300 | Signature timestamp skew, seconds (1 to 604800) |

Each reference token (the chosen one plus its top alternatives) is
looked up in the endpoint's list. A listed token adds its absolute
difference. An unlisted one adds the lower bound `max(0, expected -
floor)`, where the floor is the lowest logprob the endpoint listed.
Comparison stops at the first position where the chosen token differs.

**Why 0.3.0 failed honest sellers.** On one CPU host, an honest endpoint
(same q2_k file, same llama.cpp build, reference made from a verified
copy) checked with the full 16 prompt set passed 1 run in 11. Measured
causes: llama.cpp logprobs on a multi-thread CPU server change from run
to run and with server state (prompt cache, KV slots, batch neighbours),
so the same request does not give the same numbers; the reference held a
single sample, so an unlucky draw made one challenge sit over tolerance
for every honest run; q2_k's flat distributions amplify this; repeats
did not help because the drift is correlated; and one failing challenge
failed the run. Decoding parameters and token alignment were not the
cause.

**0.3.1 results on the same host** (real runs; details and per
quantization numbers in [verified-serving.md](plans/verified-serving.md)
section 5.7.1):

| Setup | Runs | Result |
|---|---|---|
| Honest seller, calibrated q2_k set, `--sample 8` | 25 | 25 pass |
| Honest seller, calibrated set, all 32 challenges | 10 | 10 pass |
| Honest seller, uncalibrated 5 sample set, defaults | 10 | 10 pass |
| Honest seller, the 0.3.0 single-sample set, defaults | 10 | 10 pass |
| q3_k_m, q4_k_m, or Qwen2.5-0.5B served under the claimed name with a valid claim and signatures, `--sample 8` | 30 | 30 fail |
| Logprobs altered after signing | 5 | 5 fail (signatures) |
| Logprobs altered, signatures and claim removed | 5 | 5 fail (deviation) |

So:

- Calibrate every set ([6.2](#62-commit-rotate-reveal)). Tighter,
  calibrated limits catch more: a calibrated q4_k_m set had honest runs
  at medians 0.08 to 0.16 and a limit of 0.3.
- The closest dishonest case is the neighbouring quantization (q3_k_m
  against a q4_k_m set failed 38 to 50% of challenges at `--sample 8`).
  Use `--sample 16` or more when a seller could swap in an adjacent
  quantization, and look at `decision.fail_fraction` and
  `decision.median_mean_abs_dev` across runs, not one verdict.
- Treat a single failing run as a reason to check again; a substitute
  file fails run after run.
- `unsupported` means the endpoint returned no logprobs, so it cannot
  be checked this way. A claim with `"logprobs": "required"` promises
  them.
- A signature failure inside a challenge is a hard fact and fails the
  run by itself.
- 0.3.0 binaries reject reference files written with `--samples` above
  1 or calibrated (unknown fields); 0.3.0 files keep working in 0.3.1.

### 6.5 How often, and keeping challenges ordinary

Tensorseed does not set a schedule. Things that help:

- Use `--sample` so each run sends a few prompts from a large secret
  set, and run at random times rather than on a fixed clock.
- Send challenges with the same client, headers, API key, and network
  path as real buyer traffic. Do not send `--no-prompt-cache`.
- Re-run after any change of `claim_sha256`, and after a fail.
- Rotate sets ([6.2](#62-commit-rotate-reveal)).

A determined seller can still try to tell challenges from real traffic
(the generated prompts have a recognizable style, and `max_tokens: 1`
with `top_logprobs` is unusual). Your own `--prompts` in the style of
your traffic make that harder. Perfect indistinguishability is not
claimed.

### 6.6 Doing it in your own code

`tensorseed_app::challenge` exposes the pieces `check-endpoint` uses:
`ReferenceFile::load` (checks `set_hash`), `request_body` (builds the
exact request for a prompt), `parse_logprobs`, `compare`,
`Comparison::within`, and `check_signature`; 0.3.1 adds `median_steps`,
`decide` (the set decision), `calibration_stats`, and
`calibrated_tolerance`. `tensorseed_app::claim_check` has the
`check-claim` checks (`check_document`, `check_origin`,
`resolve_catalog`, `check_catalog`, `check_claim_bytes`). Same crate and dependency
note as [5.5](#55-in-rust).

## 7. Suggested trust tiers

Tensorseed does not rank sellers. These tiers are one way to turn the
checks above into listing labels. Rename, merge, or ignore them. Each
tier includes the ones above it.

| Suggested tier | What you checked | What a buyer can rely on |
|---|---|---|
| Listed | Base URL, model id, `/v1/models` healthy | Nothing beyond your usual terms |
| Claimed | A `ts.served-weights/1` claim at the well-known path that passes `check-claim` (0.3.1) or loads in Tensorseed and matches a catalog you trust ([4.5](#45-match-the-claim-to-a-catalog-you-trust)), and passes the listing checks ([4.6](#46-listing-checks)) | The seller states which exact files it serves, and the files exist in a signed catalog |
| Signed | Claimed, plus responses verify against the claim's response key, ideally on every proxied call | Answers came from the holder of that key and were not changed on the way |
| Wallet bound | Signed, plus a valid `sig` from a `seller_wallet` you link to the seller account | The statement is tied to a wallet the seller controls |
| Spot checked | Signed, plus regular `check-endpoint` runs with secret sets and calibrated tolerances that keep passing | The endpoint behaves like the claimed file on your prompts, within your tolerances |

Show the evidence date (last claim fetch, last challenge run) next to
any label. Claims expire only when you stop accepting them, and a pass
today says nothing about tomorrow, so downgrade on stale evidence.

None of these tiers proves that every answer came from the claimed
weights. That needs hardware attestation, which 0.3.0 does not have
([section 13](#13-planned-not-in-030)).

## 8. Health checks, `/v1/models`, and upstream keys

- **Health.** Tensorseed adds no health endpoint. Use the engine's
  OpenAI compatible `GET <base_url>/models`, which must list
  `served_model_id`. Through the signing proxy that response is signed
  like any other JSON response, so a health check can also verify a
  signature and catch a changed claim cheaply.
- **Claim file.** Fetch `/.well-known/tensorseed/served-weights.json`
  with your health checks. A 503 from the proxy there means the file on
  disk no longer matches the claim the proxy signs under (the seller
  must restart), or no longer verifies.
- **Upstream keys.** Sellers usually require a bearer key at their TLS
  proxy (`TS_UPSTREAM_KEY` in [SELL-INFERENCE.md](SELL-INFERENCE.md)
  section 5) and give it to you to send on every call. The signing proxy
  forwards `Authorization` to the engine unchanged and never sees or
  needs your marketplace credentials. Some sellers leave `GET /v1/models`
  open for health checks; the claim path never needs the key.
- **Status codes from the proxy.** 400 for an invalid
  `X-Tensorseed-Request-Id`, 413 for a request body over 64 MiB, 502
  when the engine is down or its body cannot be read, 503 for the claim
  problems above. These errors come from the proxy itself, as JSON with
  `"type": "tensorseed_proxy"`, and are not signed. Error responses
  from the engine (JSON with any status) are signed like any other.
- **Limits.** Request and signed response bodies are capped at 64 MiB.

## 9. Desktop and seeding

- **Tensorseed Desktop** pulls, verifies, and seeds catalog files. It
  does not serve inference or sign responses, so sellers you list run
  the CLI (`serve-inference --sign` or `sign-responses`).
- **Getting weights to check against.** `tensorseed pull` downloads
  from Hugging Face, mirrors, and the swarm, and verifies every byte
  against the signed manifest, so the source does not matter. You need
  this copy to make reference sets.
- **Seeding.** If you seed catalog files to help sellers download
  faster, note that 0.3.0 runs one seeder per machine: `serve`,
  `tensorseed seed`, and Desktop share a lock (`seeder.lock`), and
  Desktop attaches to a running `serve` instead of starting another. The `serve` control endpoint
  (`/control/status`, `/control/action`) listens on loopback only and
  needs a token. Seeding never exposes inference.

## 10. Security notes

- **Seller keys never leave the seller.** The response key, the wallet
  key, and the engine all stay on the seller's machine. You verify with
  public data only: the claim, the response headers, and the body.
- **The marketplace never needs seller private keys.** Do not ask for
  them, and do not accept a design that needs them. The only seller
  secret you hold is the upstream API key the seller chose to share.
- **Tensorseed holds no wallet keys.** It does not sign with, store, or
  move funds from any wallet. Wallet signatures on claims are made by
  the seller's own wallet software over a message Tensorseed prints
  (`claim-serving`). Chia anchoring is the catalog publisher's wallet,
  not yours or the seller's.
- **Pin publishers out of band.** Take catalog public keys and store ids
  from a source you trust, never from a claim, a mirror, or a seller.
  The claim's `catalog.publisher_key_id` is a hint only.
- **Treat claims as untrusted input.** Cap their size, refuse
  credentials in URLs, and verify before you show anything from them.
  A claim with any unknown field is invalid.
- **Keep reference sets secret** until you reveal them, and keep
  `--api-key-env` keys out of logs and shell history.
- **Replay and staleness are yours to handle.** Use fresh request ids
  and a skew window that fits your use.
- **Revocations need no trusted host, but they need fetching.** An
  entry is only as current as your last fetch of the seller's list
  (0.3.1). Fetch it with the claim, keep your own copy, and never treat
  an unreadable list as "not revoked" for disputes. A leaked response key
  can revoke the seller's claim but cannot un-revoke one.

## 11. Integration checklist

1. Pin the official catalog (and any others you accept) by name, key,
   and store id, with several coin endpoints.
2. Sync and verify with `catalog info` and `chain-status`; refresh
   on a schedule.
3. On listing, run `tensorseed check-claim <listed base URL>` (0.3.1):
   it fetches the claim from the well-known path and checks the hash,
   fields, wallet signature, expiry, the seller's revocation list, URL,
   catalog match, takedowns, freshness, and the recomputed license mark.
   From JavaScript, `checkEndpoint` in `@tensorseed/verify` does the
   same ([5.6](#56-verify-in-javascript)). On 0.3.0, load the claim and match
   it to your catalog copy yourself ([4.5](#45-match-the-claim-to-a-catalog-you-trust)).
4. Check `served_model_id` and `logprobs` against the listing. Decide
   on wallet policy (`--require-wallet-sig`, `--expect-wallet`).
5. Health check `/v1/models`, the claim, and its revocation list;
   re-check on any new `claim_sha256`, and stop routing to an expired or
   revoked claim ([4.8](#48-expiry-and-revocation)).
6. Send `X-Tensorseed-Request-Id` on every call; verify signatures on
   the seller's exact bytes before changing anything.
7. Make secret reference sets from your own verified copies, calibrate
   them with `calibrate-reference` (0.3.1), publish the calibrated
   `set_hash`, and run sampled `check-endpoint` runs at random times.
   Act on the set decision across runs, not on one challenge.
8. Label listings with the evidence you hold and its date.

## 12. Reference: exit codes and limits

| Command | 0 | 1 | 2 |
|---|---|---|---|
| `verify-response` | valid | invalid or unsigned | error (for example the claim does not verify) |
| `check-endpoint` | pass | fail | inconclusive, unsupported, or an error that prevents a verdict |
| `check-claim` (0.3.1) | every check passed (warnings allowed) | any check failed | the claim, the catalog, or the seller's revocation list could not be read (`inconclusive`), or a `--revocations` list could not be read |
| `calibrate-reference` (0.3.1) | calibrated copy written | error | |
| `revoke-claim` (0.3.1) | revocation written (or message printed) | error | |

| Limit | Value |
|---|---|
| Claim size when fetched | 4 MiB |
| Claim fetch | 10 s timeout, at most 3 redirects, no credentials in the URL |
| Request id | 1 to 128 of `A-Z a-z 0-9 . _ ~ : -` |
| Served model id | 1 to 256 printable ASCII, no spaces |
| Signature timestamp skew | 300 s by default; 1 s to 7 days (`--max-skew`, `[verify] max_skew_secs`); stored responses: `verify-response --at` |
| Signed body | 64 MiB |
| Coin anchor grace | 192 blocks (`[chia] chain_grace_blocks`) |
| Revocation list | 4 MiB when fetched, at most 10,000 entries (0.3.1) |
| Claim lifetime (`--expires-in`) | 1 hour to 400 days (0.3.1) |

## 13. Planned (not in 0.3.0)

Not in 0.3.0. Design notes are in
[verified-serving.md](plans/verified-serving.md); none of this has a
release date.

- **On-chain serving attestations** (`attest-serving`, EAS) and stake,
  so a claim can be tied to a bonded identity.
- **Evidence bundles and dispute tooling** (`ts.challenge-evidence/1`
  and a full generation dispute mode for `check-endpoint`).
- **Tolerance data from GPUs and vLLM.** The 0.3.1 calibration numbers
  come from one CPU host running llama.cpp. `calibrate-reference` works
  against any engine, but no published per-quantization defaults exist
  for GPUs yet.
- **`check-claim` against the listing.** It does not compare
  `served_model_id` with `/v1/models` or your listing, and it does not
  store results; schedule it with your health checks.
- **Hardware attestation** (confidential computing) to show which
  weights produced each answer.
- **A follow list for trusted catalog signers.** Nothing exists today;
  each marketplace keeps its own `[[catalogs]]` list. A possible format,
  proposed here for discussion only and not implemented:

  ```json
  {
    "schema": "ts.follow-list/1",
    "name": "example-marketplace",
    "rev": 1,
    "catalogs": [
      {
        "name": "tensorseed",
        "pubkey": "RWS4/z1UPQ4ezmjrcpKqDYpqyn5h1WUS2oDs1B73Dh91kVzzNIZd03xd",
        "store_id": "3dcd43dced47a32f8e38e8ec77f583d8d1bafe1ab24e381538e50a36034b7a76",
        "base_url": "https://catalog.tensorseed.dev/",
        "note": "official catalog"
      }
    ]
  }
  ```

  It would be published as canonical JSON with a minisign signature
  whose trusted comment reads
  `ts.follow-list/1 name=<name> rev=<n> sha256=<hex>`, so it can be
  checked with the same tools as a catalog, and a command such as
  `tensorseed catalog follow-list <url>` could turn it into
  `[[catalogs]]` entries after showing the changes.
