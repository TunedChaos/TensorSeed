# Tensorseed: Project Specification (Revision 4)

> Name: **Tensorseed** (binary and crate prefix `tensorseed`).
> Website: https://tensorseed.dev (primary). Also registered: tensorseed.org, tensorseed.garden.
> Publisher: Tuned Chaos. License: AGPL-3.0-or-later.
> Status: pre-alpha, milestone M0 (spikes). Design approved in principle on 2026-11-12. Initial Rust workspace scaffold exists; no product logic yet.

Revision 4 changes: added a second, chain-free catalog backend (signed static files, section 6.8) next to Chia DataLayer, made every catalog entry a publisher-signed envelope in every backend (6.9), and defined `catalog export` / `catalog import` for moving a catalog between backends with signatures intact (6.10).

Revision 3 changes: renamed the project to Tensorseed, switched key and schema prefixes to `ts`, and made the catalog layer explicitly pluggable through a `CatalogBackend` trait with Chia DataLayer as the first implementation (section 6.0).

---

## 1. Summary

A Rust CLI and daemon that keeps open AI models and datasets from Hugging Face (HF) available even if HF goes away. It does this in two layers:

1. **Catalog layer (pluggable; Chia DataLayer first, signed static files second):** a small, signed, versioned index behind a `CatalogBackend` trait so other chains can be added later without changing the manifest format. Every entry is signed with one publisher key in every backend, so the catalog can move between backends. The `file` backend (6.8) needs no chain at all. In the first backend, one DataLayer store maps each HF repository to its mirrored revisions. Each revision has a manifest: file list, sizes, SHA-256 hashes, license, the HF commit it came from, and how to fetch the bytes. Only the Merkle root of this store goes on chain. Values are kilobytes, not gigabytes.
2. **Content layer (BitTorrent v2 plus HTTP webseeds):** the actual model bytes travel as BitTorrent v2 torrents (BEP 52) with BEP 19 webseeds. A Cloudflare R2 bucket is the primary always-on webseed. Backblaze B2 and/or Sia are optional cold backups. Volunteers seed whatever subset they choose.

A user who knows only the catalog store ID (for the Chia backend) can find any mirrored model, download it from the swarm or the webseed, and verify every byte against hashes that are anchored on Chia and traceable to the original HF commit.

This directly answers the question in the tweet that started this project: models "cryptographically signed, then hosted any arbitrary place if the sigs matched" (https://x.com/ErikVoorhees/status/2120707384071143510).

### 1.1 What changed from Revision 1 and why

| Revision 1 idea | Revision 2 | Reason |
|---|---|---|
| Store model bytes as 4 to 8 MB hex chunks in DataLayer | Store only manifests in DataLayer; bytes go over BitTorrent v2 and HTTP | Subscribers reject any per-generation delta file over `max_delta_file_size: 250` MiB by default, so 1 TiB would need at least 4,195 sequential on-chain generations. Every subscriber also keeps roughly 3 copies (key/value blobs, all delta `.dat` files, latest full `.dat`), and new subscribers replay the whole history. Sources in section 13. |
| "Mirrored across the chain and in multiple plots" | Plots are not involved at all | Chia plots contain no user data (https://www.chia.net/blog/why-chia-plots-dont-contain-real-user-data/). Only the root hash is on chain. |
| One DataLayer store per model plus a master catalog | One catalog store (optionally sharded later) | Manifests are KB-sized, so per-repo stores add on-chain transactions and mirror coins without benefit. |
| `https://huggingface.co<repo_id>` | `/api/models/{repo_id}`, `/api/datasets/{repo_id}`, tree endpoint | Original path was malformed. |
| Poll `get_root` | `get_root` for owned stores; `get_local_root` / `get_sync_status` for subscribed stores | `get_root` returns an invalid hash for subscribed stores (DataLayer RPC reference). |
| Each `batch_update` is visible immediately | Stage with `submit_on_chain=false`, publish with `submit_pending_root` | Gives one atomic generation per publish, so subscribers never see a half-written revision. |
| Built-in DataLayer HTTP server fronted by Nginx/Caddy for model bytes | R2 webseed for bytes; DataLayer mirror only for the small catalog `.dat` files | Chia subscribers fetch whole `.dat` files from one server at a time; they do not use byte ranges or swarm. |

---

## 2. Goals and non-goals

### 2.1 Goals

- G1. Mirror selected HF model and dataset revisions with byte-exact fidelity, identified by HF commit SHA.
- G2. Publish a tamper-evident, versioned catalog that anyone can subscribe to and re-serve: anchored on Chia, and also available as signed static files with no chain dependence.
- G3. Let anyone download a mirrored revision without HF and without trusting any single host, verifying SHA-256 per file.
- G4. Distribute bandwidth across volunteer seeders, with an always-on HTTP webseed so downloads work with zero peers.
- G5. Be idempotent and resumable: re-running does nothing if the HF commit is already published; interrupted work resumes.
- G6. Respect licenses: record each repo's license, default to an open-license allowlist, support takedowns.
- G7. Single static Rust binary for publish, pull, and seed.
- G8. Stay chain agnostic: manifests and key layout do not depend on Chia, and every chain specific operation goes through the `CatalogBackend` trait so a different or additional chain can be adopted later. Entry signatures are backend independent (6.9).

### 2.2 Non-goals

- N1. Storing model bytes on chain or inside DataLayer values.
- N2. Paying seeders or running an incentive token. (Possible future work, see DIG Network in section 14.)
- N3. Hosting gated or restricted-license weights without the license allowing it.
- N4. Replacing HF features (model cards UI, inference, Spaces, discussions).
- N5. Format conversion or quantization (no GGUF/MLX/NVFP4 conversion). We mirror what upstream publishes. Pre-quantized repos can be added as separate targets.
- N6. Anonymity guarantees for seeders. BitTorrent exposes peer IPs.

---

## 3. Architecture

```
              Hugging Face Hub
                    |
                    | (1) model info + tree (sha, sizes, LFS sha256, license)
                    | (2) file download (hf-hub crate, Xet aware)
                    v
 +-------------------------------------------------------------+
 | tensorseed publish (publisher node)                         |
 |  staging dir -> verify sha256 -> build BEP 52 torrent       |
 |   -> upload files to R2 (webseed)  [optional B2 / Sia copy] |
 |   -> start seeding                                          |
 |   -> write manifest + pointers to DataLayer (staged batch)  |
 |   -> submit_pending_root -> wait for confirmation           |
 +-------------------------------------------------------------+
        |                         |                     |
        | DataLayer RPC (mTLS,    | HTTPS (BEP 19)      | BitTorrent v2 swarm
        | localhost:8562)         |                     | (DHT + trackers)
        v                         v                     v
  Chia DataLayer             Cloudflare R2         Volunteer seeders
  catalog store              public bucket         (tensorseed seed)
  (root hash on chain;       (models.tensorseed.dev)
   .dat files served on
   :8575 by any mirror)
        ^                         ^                     ^
        |_________________________|_____________________|
                                  |
                       tensorseed pull
         subscribe catalog -> read manifest -> torrent + webseed
         -> verify sha256 per file -> write to output dir
```

Key property: the catalog says *what* the correct bytes are (hashes) and *where* they may be found (infohash, webseed URLs). Any source can serve bytes, because the client checks them against the catalog.

---

## 4. Threat model

| Threat | Impact | Mitigation |
|---|---|---|
| Malicious or buggy webseed / peer serves wrong bytes | Corrupt or poisoned weights | BitTorrent v2 per-16 KiB-block Merkle verification during transfer, plus full-file SHA-256 check against the manifest before a file is moved out of the temp dir. Failing sources are dropped. |
| Malicious DataLayer mirror serves forged catalog `.dat` | Fake manifests | Chia DataLayer validates downloaded data against the on-chain root before accepting it. Clients pin the catalog store ID. Every entry is also verified against the pinned publisher key (6.9). |
| Malicious file-catalog host or mirror forges, rolls back, or forks the catalog | Fake or outdated manifests | Head, deltas, and snapshots are minisign-signed with the pinned publisher key; every entry is verified again; the revision and `prev_root` hash chain plus the local trust anchor reject rollback, replay, gaps, and forks (6.8). |
| File-catalog host freezes the catalog (serves an old, validly signed head forever) | Users miss new revisions and tombstones | `head.json` carries `expires_unix`; clients refuse expired heads. Publisher re-signs the head before `file.head_ttl_secs` lapses. |
| Publisher shows different file-catalog chains to different users (split view) | Targeted users get different manifests | Not preventable by the file backend alone. Detectable by comparing heads across mirrors; prevented by the Chia backend's on-chain root (6.11). |
| Publisher signing key compromise (any backend) | Attacker signs valid looking entries | Keep the key offline; rotation and revocation design is an open question (section 16). Rollback and fork checks still stop rewriting history already seen by clients. |
| Catalog owner key compromise | Attacker publishes manifests pointing to malicious weights (pickle `.bin`/`.pt` can execute code on load) | Keep the owner key in a cold or Vault-style setup; publish from a CHIP-0035 delegated writer key that the owner can revoke (optional, see 6.6). Manifests record `hf_commit` and HF-reported hashes so anyone can audit against HF while it exists. `pull` warns loudly on pickle-based formats and prefers `safetensors`/`gguf`. `pull` shows the publishing generation and timestamp and supports `--expect-sha256` / pinned revisions. |
| Upstream HF repo itself is malicious or later altered | We mirror bad content, or a force-push rewrites history | We mirror by immutable commit SHA; new commits become new revisions and never overwrite old manifests. Record HF security-scan status if exposed by the API (unverified, M0). |
| Publisher host compromise | Same as key compromise; also R2 credentials | Least-privilege R2 token (write to one bucket), DataLayer RPC never exposed beyond localhost (RPC reference warns port 8562 must not be public). |
| Takedown request (license or legal) | Must stop distributing | See section 11. Honest limit: the DataLayer delta history is permanent for anyone who already synced, and the swarm cannot be recalled. We can tombstone the catalog entry and delete webseed objects. |
| Catalog spam / bloat | Only relevant if write access is delegated | Single writer by default; delegated writers are explicit and revocable. |
| Seeder privacy | Seeder IPs visible in swarm | Documented; seeders may use a VPN or seed only via webseed-style HTTP hosting. |
| Hash algorithm confusion | v2 `pieces root` is not a plain SHA-256 of the file | Manifest stores both: `sha256` (plain, matches HF LFS oid) and `bt_v2_pieces_root` (BEP 52). Both are checked. |

---

## 5. Hugging Face integration

- Model info: `GET https://huggingface.co/api/models/{repo_id}` (datasets: `/api/datasets/{repo_id}`), optionally `/revision/{rev}`. Provides commit `sha`, `gated` status, and `cardData.license` (to be confirmed in M0).
- File tree with sizes and LFS hashes: `GET https://huggingface.co/api/models/{repo_id}/tree/{commit_sha}?recursive=true` (datasets analogous). LFS entries carry `lfs.oid` (SHA-256) and size. Non-LFS small files are hashed locally.
- Downloads: the `hf-hub` crate (1.0.0 supports Xet transfers via `hf-xet`; https://crates.io/crates/hf-hub). Always download by commit SHA, never by branch name, to avoid races.
- Auth: optional HF token from env `HF_TOKEN` for rate limits. Gated repos are refused unless the license allowlist permits redistribution (section 11).
- Bounded concurrency for downloads (default 2 files), with HTTP resume of partial files.

All endpoint field names above must be confirmed against live responses in M0 before M1 code depends on them.

---

## 6. Catalog layer

### 6.0 Catalog backend abstraction (pluggable)

The catalog is defined by its key layout (6.3) and JSON schemas (6.4), not by any one chain. All chain specific work sits behind one Rust trait in the `tensorseed-catalog` crate:

```rust
#[async_trait]
pub trait CatalogBackend: Send + Sync {
    /// Short stable identifier, for example "chia-datalayer".
    fn kind(&self) -> &'static str;
    /// Create a new catalog and return its identifier (Chia: store ID).
    async fn create_catalog(&self) -> Result<CatalogId>;
    /// Read one value by key from the latest synced generation.
    async fn get(&self, catalog: &CatalogId, key: &str) -> Result<Option<Vec<u8>>>;
    /// Read the signed envelope for a key (used by export, 6.10).
    async fn get_signed(&self, catalog: &CatalogId, key: &str) -> Result<Option<SignedEntry>>;
    /// List keys under a prefix.
    async fn keys(&self, catalog: &CatalogId, prefix: &str) -> Result<Vec<String>>;
    /// Stage a batch of upserts and deletes without publishing.
    async fn stage(&self, catalog: &CatalogId, batch: &[CatalogOp]) -> Result<()>;
    /// Publish everything staged as one atomic generation.
    async fn commit(&self, catalog: &CatalogId) -> Result<PendingCommit>;
    /// Wait until a commit is confirmed by the backend.
    async fn wait_confirmed(&self, pending: &PendingCommit) -> Result<Generation>;
    /// Subscribe to a catalog published by someone else.
    async fn subscribe(&self, catalog: &CatalogId, mirror_urls: &[String]) -> Result<()>;
    /// Local versus remote sync state for a subscribed catalog.
    async fn sync_status(&self, catalog: &CatalogId) -> Result<SyncStatus>;
    /// Advertise a mirror for this catalog where the backend supports it.
    async fn add_mirror(&self, catalog: &CatalogId, urls: &[String]) -> Result<()>;
}
```

Rules:
- Two implementations in M1: `chia` (Chia DataLayer over its local RPC with mTLS, the first and canonical backend, sections 6.1 to 6.7) and `file` (signed static files, no chain, section 6.8).
- Backends must provide: an owner controlled writer, atomic multi key generations, a verifiable history (each generation anchored by a root hash or equivalent), and third party subscription or replication.
- Every stored value is a `SignedEntry` (6.9) signed by the one publisher key pinned in `catalog.publisher_pubkey`, whatever the backend. Backends add history and distribution on top; they are never the only source of authenticity.
- Manifests, key names, and canonical JSON are backend independent, so the same manifest bytes can be published to several backends at once. A future migration means publishing the same keys into a new backend and announcing its catalog ID in `ts/v1/catalog` of the old one.
- Config selects the backend with `catalog.backend = "chia" | "file"`. Unknown backends are a config error.
- Candidate future backends (not planned for M1 to M4, only on demand): other chains with a key value store or an anchored Merkle root, for example an EVM layer 2. Migration uses export and import (6.10).


### 6.1 Chia DataLayer backend: facts this design relies on (verified 2026-11-12)

| Fact | Source |
|---|---|
| RPC on `localhost:8562`, mTLS with `config/ssl/data_layer/private_data_layer.crt` / `.key`; must not be exposed publicly | https://docs.chia.net/datalayer-rpc/ and `chia/util/initial-config.yaml` |
| `rpc_server_max_request_body_size: 26214400` (25 MiB of hex JSON, so about 12.5 MiB raw per call) | https://github.com/Chia-Network/chia-blockchain/blob/main/chia/util/initial-config.yaml |
| `max_delta_file_size: 250` MiB per generation on subscribers | same config; enforced in `chia/data_layer/download_data.py` |
| One pending root per store; next write fails with "Already have a pending root waiting for confirmation." | https://docs.chia.net/datalayer-rpc/ |
| `batch_update` has `submit_on_chain` (since 2.3.0); `submit_pending_root` publishes staged batches | https://docs.chia.net/datalayer-rpc/ |
| `get_root` invalid for subscribed stores; use `get_local_root` | https://docs.chia.net/datalayer-rpc/ |
| `add_mirror` takes `id`, `urls`, `amount` (min 0, refunded on delete), `fee`; works for owned or subscribed stores | https://docs.chia.net/datalayer-rpc/ and https://docs.chia.net/guides/datalayer-user-guide/ |
| Subscribers find mirrors from on-chain mirror coins plus subscribe `urls`, shuffle them, and fetch whole `.dat` files from one server at a time | `chia/data_layer/data_layer.py` (`fetch_and_validate`) |
| Default fee from config is 1,000,000,000 mojos (0.001 XCH) if none is passed | https://docs.chia.net/datalayer-rpc/ |
| Transaction blocks average about 52 seconds | https://docs.chia.net/chia-blockchain/coin-set-model/costs/ |

### 6.2 Store topology

- **One catalog store** (`catalog_store_id`), owned by the publisher.
- Optional later: shard by first letter of org or by repo type if the catalog grows large enough that full replay becomes slow for new subscribers. Not needed at launch.

### 6.3 Key naming

All keys are UTF-8 strings, hex-encoded on the wire (DataLayer requires hex keys and values). `{type}` is `model` or `dataset`. `{org}/{name}` is the HF repo id. `{commit}` is the full 40-hex HF commit SHA.

| Key | Value | Notes |
|---|---|---|
| `ts/v1/catalog` | `CatalogInfo` JSON | Schema version, publisher info, webseed base URLs, policy URL. |
| `ts/v1/repo/{type}/{org}/{name}/latest` | `LatestPointer` JSON | Points at the newest mirrored commit. |
| `ts/v1/repo/{type}/{org}/{name}/rev/{commit}` | `Manifest` JSON | Immutable once written. |
| `ts/v1/repo/{type}/{org}/{name}/rev/{commit}/files/{page:06}` | `FilePage` JSON | Only when the file list is too large for one value (see 6.5). |
| `ts/v1/repo/{type}/{org}/{name}/tombstone` | `Tombstone` JSON | Present only if taken down. |
| `ts/v1/index/{type}/{org}/{name}` | `IndexEntry` JSON | Small summary for listing without reading manifests. |

The `ts/v1/` prefix (short for Tensorseed, schema v1) leaves room for a schema v2 to coexist.

### 6.4 JSON schemas

All JSON is UTF-8, keys sorted, no insignificant whitespace (canonical form) so the same content always produces the same bytes and leaf hash. Timestamps are RFC 3339 UTC.

**CatalogInfo**
```json
{
  "schema": "ts.catalog/1",
  "name": "Tensorseed catalog",
  "publisher": { "display_name": "string", "contact": "string", "policy_url": "https://..." },
  "webseed_bases": ["https://models.tensorseed.dev/t/"],
  "trackers": ["udp://tracker.example:1337/announce"],
  "created_at": "2026-11-12T00:00:00Z"
}
```

**LatestPointer**
```json
{
  "schema": "ts.latest/1",
  "commit": "40-hex HF commit sha",
  "manifest_key": "ts/v1/repo/model/org/name/rev/<commit>",
  "updated_at": "2026-11-12T00:00:00Z"
}
```

**Manifest**
```json
{
  "schema": "ts.manifest/1",
  "repo": { "type": "model", "id": "org/name", "hf_url": "https://huggingface.co/org/name" },
  "hf_commit": "40-hex",
  "hf_last_modified": "2026-11-01T00:00:00Z",
  "license": {
    "spdx": "apache-2.0",
    "hf_tag": "apache-2.0",
    "class": "open",
    "license_files": ["LICENSE"],
    "attribution": null,
    "gated_upstream": false
  },
  "totals": { "files": 12, "bytes": 16000000000 },
  "files": [
    {
      "path": "model-00001-of-00004.safetensors",
      "size": 4000000000,
      "sha256": "64-hex",
      "hf_lfs_oid": "64-hex or null",
      "bt_v2_pieces_root": "64-hex or null",
      "format": "safetensors",
      "pickle_risk": false
    }
  ],
  "file_pages": 0,
  "torrent": {
    "infohash_v2": "64-hex",
    "infohash_v1": "40-hex or null",
    "magnet": "magnet:?xt=urn:btmh:1220<64-hex>&dn=...",
    "name": "org__name@<commit12>",
    "piece_length": 4194304,
    "torrent_file_urls": ["https://models.tensorseed.dev/t/<infohash_v2>.torrent"]
  },
  "webseeds": ["https://models.tensorseed.dev/t/<infohash_v2>/"],
  "backups": [ { "kind": "b2", "locator": "b2://bucket/prefix/" } ],
  "published_at": "2026-11-12T00:00:00Z",
  "publisher_tool": "tensorseed 0.1.0"
}
```

Field rules:
- `sha256` is the plain SHA-256 of the whole file and is always present. `hf_lfs_oid` must equal it when present; a mismatch aborts publish.
- `bt_v2_pieces_root` is the BEP 52 per-file Merkle root (empty files have none).
- `piece_length` is a power of two, at least 16 KiB (BEP 52). The 4 MiB example is a placeholder default to tune in M0.
- `pickle_risk` is true for `.bin`, `.pt`, `.pth`, `.pkl`, `.ckpt` and similar formats.
- `files` is empty and `file_pages > 0` when paging is used.

**FilePage**: `{ "schema": "ts.filepage/1", "page": 0, "files": [ ...same objects as above... ] }`

**Tombstone**: `{ "schema": "ts.tombstone/1", "reason": "license|legal|publisher|security", "detail": "string", "effective_at": "RFC3339", "revisions": ["<commit>", "..."] }`

**IndexEntry**: `{ "schema": "ts.index/1", "latest_commit": "40-hex", "license_spdx": "apache-2.0", "total_bytes": 16000000000, "updated_at": "RFC3339", "taken_down": false }`

### 6.5 Value size budget

- Target: each value under 1 MiB raw; hard cap 4 MiB raw (well under the about 12.5 MiB raw per RPC call implied by the 25 MiB hex body default).
- If a manifest's `files` would exceed the target (large datasets with many files), split into `FilePage` values of at most 1 MiB each.
- The `SignedEntry` envelope adds about 350 bytes of signature plus JSON string escaping of the value (roughly 10 to 20 percent for manifests); budgets above apply to the envelope. Confirm in M0.
- One publish (all keys for one revision plus index and latest pointer) must stay far below the 250 MiB per-generation delta cap. In practice it is kilobytes to a few MiB.

### 6.6 Write path rules

1. All DataLayer writes go through a single in-process **write queue** per store. No concurrent writers.
2. A publish stages its keys with one or more `batch_update` calls using `submit_on_chain=false`, then calls `submit_pending_root` once. This produces one generation containing the whole revision (manifest, pages, index, latest pointer) atomically.
3. After submitting, poll until confirmed (owned store: `get_root`, checking that the returned hash matches the expected root and that it is confirmed; confirm exact response fields in M0). Timeout and retry policy in config.
4. Optional: `multistore_batch_update` / `submit_all_pending_roots` if the catalog is sharded across several stores, to publish all shards in one transaction.
5. Optional: CHIP-0035 delegated writers (supported by `chia-sdk-driver` with feature `chip-0035`; https://github.com/xch-dev/chia-wallet-sdk) so co-maintainers or a hot publishing key can write while a cold owner key can revoke. Not in M1.
6. Fees: configurable `fee_mojos`; default 0, relying on Chia's config default only if the user leaves it unset. Document the 0.001 XCH config default.

### 6.7 Catalog mirroring

- The publisher runs the DataLayer HTTP server (port 8575) and calls `add_mirror` once for the catalog store with its public URL(s). `amount` default 0 (refundable, minimum 0 per docs).
- Volunteers who `seed` with `--serve-catalog` subscribe to the catalog (their Chia node then serves the `.dat` files automatically) and may optionally call `add_mirror` themselves to advertise. That costs a transaction fee and requires a reachable port 8575.
- Optionally also publish each catalog generation's full `.dat` file as a small torrent, so the catalog survives even if all DataLayer mirrors vanish (same idea as The Hugging Bay's catalog torrent).

### 6.8 Signed static-file backend (`file`, no chain)

A second backend with zero chain dependence. It carries the same keys (6.3) and the same canonical JSON documents (6.4), wrapped in the same `SignedEntry` envelopes (6.9) as the Chia backend, but publishes them as plain files. Config: `catalog.backend = "file"` and the `[file]` table (section 10). Code: `tensorseed-catalog::file`.

**Layout** under `file.base_url` (default `https://catalog.tensorseed.dev/`, an R2 bucket on a custom domain), one directory per logical catalog name:

```
<base_url>/<catalog>/head.json                       (+ head.json.minisig)    rewritten each revision
<base_url>/<catalog>/deltas/<revision:010>.json      (+ .minisig)             one per revision, never rewritten
<base_url>/<catalog>/snapshots/<revision:010>.json   (+ .minisig)             full state, every file.snapshot_every revisions
```

Everything except `head.json` is append-only: a published delta or snapshot is never modified or deleted, so any static host, mirror, or archive can carry the full history. Each snapshot is also published as a small BitTorrent v2 torrent (magnet listed in `head.json` in M1), so the catalog survives if every HTTPS host vanishes. Extra read mirrors go in `file.mirror_urls`.

**Documents** (canonical JSON, schemas `ts.filehead/1`, `ts.filedelta/1`, `ts.filesnapshot/1`):

- `Delta { catalog, revision, prev_root, state_hash, root, ops: [ {"op":"upsert","entry":SignedEntry} | {"op":"delete","key":...} ] }`
- `Snapshot { catalog, revision, prev_root, state_hash, root, entries: [SignedEntry sorted by key] }`
- `Head { catalog, revision, root, prev_root, snapshot: {revision, path, sha256} | null, expires_unix }`

**Hash chain:**

- `state_hash` = SHA-256 of the canonical JSON array of `[key, sha256(canonical SignedEntry bytes)]` pairs, sorted by key.
- `root(n)` = SHA-256 of canonical `{"schema":"ts.fileroot/1","catalog","revision":n,"prev_root":root(n-1),"state_hash"}`; `root(0)` is 64 zeros.
- Revisions increase by exactly 1. Each delta names the root it builds on, so the chain is a linked list from revision 1 to the head.

**Signatures:** every head, delta, and snapshot has a detached minisign signature (6.9) whose trusted comment is exactly `ts.file/1 kind=<head|delta|snapshot> catalog=<name> rev=<n> root=<hex>`. Every entry inside a delta or snapshot is verified again on its own, so a correctly signed delta cannot smuggle in an entry signed by another key.

**Client rules** (implemented in `ChainState`, unit tested):

| Situation | Result |
|---|---|
| Head revision lower than the highest accepted revision | `Rollback` error |
| Same revision, different root | `Fork` error |
| Delta revision lower than next | `Rollback` (replay) error |
| Delta revision higher than next | `Chain` error (gap): fetch the missing deltas or a newer snapshot |
| Delta `prev_root` differs from the local root | `Fork` error |
| Recomputed `state_hash` or `root` differs | `Chain` error |
| `expires_unix` in the past | `Chain` error (stale head, freeze attack) |
| Wrong key, tampered body, or wrong trusted comment | `Signature` error |

Clients keep a trust anchor `(revision, root)` in `file.state_dir` (persistence is M1) and never go below it. A new client bootstraps from the newest snapshot, then applies later deltas. The publisher re-signs `head.json` (same revision, new `expires_unix`) before `file.head_ttl_secs` lapses.

**What it does not give you:** the file backend proves *who* published each revision and that history was not rewritten for clients that already saw it. It cannot prove to a brand new client that the publisher never showed a different chain to someone else (a split view); only cross-checking with other mirrors or a public ledger can. That is what Chia adds (6.11).

### 6.9 Signed entries and the signature format

Every value stored under a `ts/v1/` key, in every backend, is a `SignedEntry` envelope:

```json
{ "schema": "ts.signed/1", "key": "ts/v1/...", "value": "<canonical JSON document as a string>", "sig": "<minisign signature over the UTF-8 bytes of value>" }
```

The signature's trusted comment is exactly `ts.entry/1 catalog=<name> key=<key> sha256=<sha256 of value>`, which binds the document to its key and to the logical catalog name (`catalog.name`, independent of backend ids such as a Chia store id). One publisher Ed25519 key signs every entry, and its public key is pinned in config as `catalog.publisher_pubkey`. Verification rejects non-canonical values, unknown schemas, and any context mismatch. On Chia the stored value is the canonical JSON of the envelope (hex encoded on the wire as before).

**Format choice: minisign** (https://jedisct1.github.io/minisign/), Ed25519 over a BLAKE2b-512 prehash (`ED` mode).

- *Why minisign:* a single fixed algorithm, so no algorithm negotiation or downgrade surface. Keys are one short base64 line that fits in TOML. The trusted comment is covered by a second signature, which gives us a signed, human readable context line (catalog, key, revision, root) for free. Anyone can check any Tensorseed signature with the stock `minisign -V` tool or `rsign2`, without trusting Tensorseed code; our implementation is checked both ways against the `minisign` tool. The implementation is small (about 200 lines over `ed25519-dalek` and `blake2`, both permissively licensed and accepted by `deny.toml`).
- *Why not ssh-sig (`ssh-keygen -Y sign`):* it accepts many key types (RSA, ECDSA, security keys), needs an allowed_signers file and namespace handling, has no signed comment field for context, and pulls in a much larger parser. Its advantage (reusing an existing SSH key, keys discoverable on GitHub) matters less for a single project key. It could be added later as a second accepted signature type (open question).
- Not yet done: loading minisign's scrypt encrypted secret key file and a `catalog keygen` command (M0). Until then only in-memory keys exist (tests).

### 6.10 Export, import, and migration

Because signatures live in the entries and not in a backend, a catalog can move between backends with signatures intact:

```
tensorseed catalog export --out catalog.ts-export.json [--backend chia|file]
tensorseed catalog import catalog.ts-export.json [--backend chia|file] [--dry-run]
```

- `export` reads every `ts/` key through `CatalogBackend::get_signed`, verifies each entry against the pinned key, and writes an `ExportBundle` (`ts.export/1`: `catalog`, informational `publisher_pubkey`, `source_kind`, `source_id`, `entries` sorted by key). The bundle is not signed as a whole; every entry already is.
- `import` verifies every entry against the key pinned in the *destination* config (never the key inside the bundle), then stages all entries unchanged and publishes them as one new generation (Chia) or one new revision (file). No re-signing, so the destination holds byte-identical signed entries. `--dry-run` only verifies; this mode works today.
- Migration (for example Chia to file, or file to a future chain): export, import into the new backend, then publish a `ts/v1/catalog` update in the old backend announcing the new location. Running both backends side by side from the same publisher key is supported and recommended during a transition.

### 6.11 Backend comparison

| | `chia` (DataLayer) | `file` (signed static files) |
|---|---|---|
| Entry authenticity | publisher signature (6.9) plus store owner key | publisher signature (6.9) |
| History | public, tamper-evident, on-chain root per generation; split views are impossible | hash chain; rollback and forks detected for clients that saw a newer state; split views detectable only by cross-checking mirrors |
| Mirror discovery | permissionless, on-chain `add_mirror` coins | static list in config (`file.mirror_urls`) plus any host copying the files |
| Dependencies | Chia full node, wallet, XCH for fees | none beyond HTTPS or BitTorrent |
| Cost per update | one transaction fee (often 0) | a few R2 writes |
| Best for | the canonical, publicly auditable catalog | offline use, simple mirrors, and insurance against Chia becoming unavailable |

Sia stays an optional cold store for *bytes* (7.3); it has no catalog or signature features and is not a catalog backend. Other chains (for example an EVM layer 2) can be added later as further `CatalogBackend` implementations only if users ask for them; the entry format and export bundles already make that a migration, not a redesign.

---

## 7. Content layer

### 7.1 BitTorrent v2

- One torrent per (repo, commit). Multi-file torrent; `name` = `{org}__{name}@{commit[0:12]}`; files keep their HF relative paths.
- **v2 required** for per-file SHA-256 Merkle trees and `btmh` magnets (BEP 52, https://www.bittorrent.org/beps/bep_0052.html).
- **Hybrid v1+v2 (decision to confirm):** default to hybrid torrents so v1-only clients can still join, at the cost of v1 padding files and a second info hash. Config switch `torrent.hybrid = true|false`.
- Trackers: a short configurable list plus DHT. Private trackers are not used.
- The `.torrent` file itself is uploaded to R2 next to the content and its URL stored in the manifest.

### 7.2 Webseeds (BEP 19)

- Each torrent's `url-list` contains the R2 base, for example `https://models.tensorseed.dev/t/<infohash_v2>/`. Per BEP 19, when the URL ends in `/`, clients append the torrent name and file path, so objects are stored at `t/<infohash_v2>/<name>/<relative path>` (https://bittorrent.org/beps/bep_0019.html).
- R2 bucket is public via a custom domain, read-only, with HTTP range support. Upload uses S3 multipart for large files.
- Optional second webseed on B2 (via its S3 API) for redundancy.

### 7.3 Cold backups (optional)

- Sia stays optional cold storage for bytes only; it is not a catalog backend (6.11).
- Backblaze B2 bucket and/or Sia (via `renterd`) holding the same object layout. Recorded under `backups` in the manifest so a future operator can restore the R2 webseed. Not used by `pull` unless `--allow-backup-sources` is set.

---

## 8. State machines

### 8.1 Publish (per target repo)

```
START
 -> RESOLVE        fetch HF model info + tree at commit; read license, gated status
 -> POLICY_CHECK   license allowlist / attribution rules / gated rules (section 11)
                   fail -> SKIPPED(policy) [logged, no writes]
 -> DIFF           read ts/v1/repo/.../latest via get_value
                   latest.commit == remote commit -> DONE(up_to_date)
 -> DOWNLOAD       hf-hub download by commit into staging/<repo>/<commit>/
                   bounded concurrency; resumable; record per-file state in sqlite
 -> VERIFY         sha256 every file; compare to hf lfs oid where present
                   mismatch -> FAILED(integrity)
 -> BUILD_TORRENT  compute BEP 52 piece layers (+v1 if hybrid); write .torrent
 -> UPLOAD         multipart upload files + .torrent to R2 (and backups if enabled)
                   HEAD-check size; resumable via sqlite part tracking
 -> SEED_START     add torrent to local seeding session (data from staging or seed dir)
 -> STAGE_CATALOG  batch_update(submit_on_chain=false): manifest, pages, index, latest
 -> SUBMIT         submit_pending_root (through the write queue)
 -> CONFIRM        poll until confirmed root == expected; timeout -> RETRY or FAILED(chain)
 -> CLEANUP        if keep_local_seed=false: delete staging after R2 upload confirmed
                   else move to seed_dir
 -> DONE(published)
```

Resumability: every transition is recorded in sqlite (`jobs`, `files`, `upload_parts`, `chain_ops`). On restart, a job resumes at its last completed state. CONFIRM is idempotent: if the expected root is already on chain, it advances.

Failure after STAGE_CATALOG but before SUBMIT: use `clear_pending_roots` and restage, or submit if the staged root matches the expected one.

### 8.2 Pull

```
START (args: --catalog <store_id> <repo_id> [--revision <commit>] | --manifest-file)
 -> ENSURE_SUB     subscribe(catalog, urls from config) if not subscribed
 -> WAIT_SYNC      get_sync_status / get_local_root until local generation == chain generation
                   (or --no-wait to use the current local copy)
 -> READ           get_value latest or rev/<commit>; read pages; check tombstone
                   tombstoned -> STOP with explanation unless --ignore-tombstone
 -> PLAN           compare with existing files in output dir (sha256); skip verified ones
 -> FETCH          torrent session with webseeds; v2 block verification in transit
 -> VERIFY         full-file sha256 vs manifest; also pieces root
 -> FINALIZE       atomic rename into output dir; write license files + NOTICE
 -> optional SEED  keep seeding for --seed-after <duration>
 -> DONE
```

`pull` also works without a local Chia node via `--manifest-file` or `--manifest-url` (verification against chain is then skipped and the CLI says so).

### 8.3 Seed daemon

```
LOOP every poll_interval:
  sync catalog (subscription)
  compute desired set = policy(selection rules, disk budget, license classes)
  for new items: pull (data) then seed
  for removed/tombstoned items: stop seeding; delete if prune=true
  report status (local only: JSON status endpoint on localhost, optional)
```

Selection rules: explicit repo list, glob patterns (`org/*`), "latest revision only" vs "all revisions", max bytes, license classes allowed. Bandwidth caps (upload/download), and connection limits. Optional `--serve-catalog` (section 6.7).

---

## 9. CLI

Single binary `tensorseed`.

| Command | Purpose |
|---|---|
| `doctor` | Check config, pinned publisher key, catalog backend (Chia: DataLayer RPC mTLS; file: head fetch and signature), wallet and node sync, R2 credentials, torrent ports, disk space. |
| `catalog create` | Create the catalog through the configured backend (Chia: `create_data_store`), wait for confirmation, write `CatalogInfo`. |
| `catalog info [--store]` | Show catalog info, generation, root, mirrors. |
| `catalog add-mirror --url ... [--amount] [--fee]` | Advertise a DataLayer mirror for the catalog (Chia only; the file backend lists mirrors in config). |
| `catalog export --out FILE [--backend chia\|file]` | Write every signed entry to a backend neutral `ts.export/1` bundle (6.10). |
| `catalog import FILE [--backend chia\|file] [--dry-run]` | Verify a bundle against the pinned key and publish its entries unchanged as one generation (6.10). |
| `publish <repo_id> [--type model\|dataset] [--revision <commit>]` | Run the publish state machine for one repo. |
| `sync` | Publish every target in config (sequential chain writes, bounded downloads). |
| `pull <repo_id> --catalog <store_id> [--revision] [--out DIR] [--seed-after DUR]` | Download and verify. |
| `seed` | Run the seeding daemon. |
| `verify <dir> --catalog <store_id> <repo_id> [--revision]` | Re-hash a local copy against the manifest. |
| `list [--catalog] [--filter]` | List catalog entries (from `index` keys). |
| `show <repo_id> [--revision]` | Print a manifest. |
| `takedown <repo_id> --reason ... [--revisions ...] [--delete-webseed]` | Write tombstone, optionally delete R2 objects. |
| `jobs [--retry ID] [--clear ID]` | Inspect and manage resumable jobs. |

Additional scaffold command: `config show` prints the effective merged configuration (secrets are never stored in it).

Global flags: `--config`, `--log-format json|pretty`, `-v`.

---

## 10. Configuration (TOML, loaded with figment; env overrides `TENSORSEED_*`, nested keys split on `__`)

```toml
[general]
state_db = "~/.local/share/tensorseed/state.sqlite"
staging_dir = "/srv/tensorseed/staging"
seed_dir = "/srv/tensorseed/seed"
keep_local_seed = true
max_concurrent_downloads = 2

[catalog]
backend = "chia"                 # "chia" | "file"; see sections 6.0 and 6.8
name = "tensorseed"              # logical catalog name, bound into every entry signature
publisher_pubkey = ""            # pinned minisign public key ("RW..."), same for every backend
# publisher_seckey_file = "~/.config/tensorseed/publisher.key"   # only needed to publish

[chia]
rpc_url = "https://localhost:8562"
ssl_cert = "~/.chia/mainnet/config/ssl/data_layer/private_data_layer.crt"
ssl_key = "~/.chia/mainnet/config/ssl/data_layer/private_data_layer.key"
catalog_store_id = ""            # set by `catalog create`
fee_mojos = 0
confirm_poll_secs = 20
confirm_timeout_secs = 1800
catalog_mirror_urls = []         # public URLs of DataLayer HTTP server (port 8575)
mirror_amount_mojos = 0

[file]
base_url = "https://catalog.tensorseed.dev/"   # head.json, deltas/, snapshots/ (+ .minisig)
mirror_urls = []                 # any static host carrying the same files
state_dir = "~/.local/share/tensorseed/catalog"
snapshot_every = 64              # full snapshot every N revisions
head_ttl_secs = 604800           # head expires after 7 days unless re-signed
torrent = true                   # also publish each snapshot as a torrent

[huggingface]
endpoint = "https://huggingface.co"
token_env = "HF_TOKEN"

[torrent]
hybrid = true
piece_length = 4194304           # placeholder, tune in M0
trackers = []
listen_port = 6881
upload_limit_kbps = 0            # 0 = unlimited
dht = true

[webseed.r2]
enabled = true
account_id_env = "R2_ACCOUNT_ID"
access_key_env = "R2_ACCESS_KEY_ID"
secret_key_env = "R2_SECRET_ACCESS_KEY"
bucket = "tensorseed"
public_base_url = "https://models.tensorseed.dev/t/"

[backup.b2]
enabled = false
endpoint = "https://s3.<region>.backblazeb2.com"
bucket = "tensorseed-cold"
key_id_env = "B2_KEY_ID"
app_key_env = "B2_APP_KEY"

[backup.sia]
enabled = false
renterd_url = "http://localhost:9980"
password_env = "RENTERD_PASSWORD"

[policy]
license_allowlist = ["apache-2.0", "mit", "bsd-3-clause", "bsd-2-clause", "cc-by-4.0", "cc0-1.0", "openrail", "openrail++"]
attribution_licenses = ["llama3", "llama3.1", "llama3.2", "llama3.3", "gemma"]   # allowed only with attribution handling
allow_gated = false
allow_pickle_formats = false

[[targets]]
repo = "org/name"
type = "model"
include = ["*.safetensors", "*.json", "LICENSE*", "README.md"]
exclude = []

[seed]
poll_interval_secs = 600
max_bytes = 2_000_000_000_000
select = ["org/*"]
latest_only = true
prune = true
serve_catalog = false
```

Secrets are only read from environment variables or a file outside the repo; never stored in the TOML. Default config path: `~/.config/tensorseed/tensorseed.toml` (override with `--config` or `TENSORSEED_CONFIG`). An example lives at `config/tensorseed.example.toml` in the repository.

---

## 11. Licensing policy

1. **Record:** every manifest stores the HF license tag, a normalized SPDX id where one exists, the license class (`open`, `attribution`, `restricted`, `unknown`), whether the repo was gated upstream, and the repo's license files (which are always mirrored).
2. **Default allowlist:** only `open` licenses are published by default (see `policy.license_allowlist`). Unknown or missing license means skip.
3. **Attribution-style licenses (Llama-style):** the Llama 3.1 Community License allows redistribution if you include a copy of the agreement, keep the required "Notice" text file, and prominently display "Built with Llama" (https://github.com/meta-llama/llama-models/blob/main/models/llama3_1/LICENSE). These are allowed only when listed in `attribution_licenses`; the publisher then (a) mirrors the license and notice files, (b) sets `license.attribution` in the manifest to the required text, and (c) `pull` writes the license and NOTICE into the output directory and prints the attribution. The catalog explorer displays "Built with Llama" for those entries.
4. **Gated repos:** "gated" is an HF access setting, not a license term. Gated repos are skipped unless `allow_gated = true` and the license class is `open` or `attribution`. Prior art (LLM Torrents) chooses never to rehost gated weights; we default to the same.
5. **Restricted / non-commercial / no-redistribution licenses:** never published.
6. **Takedown process:** a published contact address in `CatalogInfo.publisher.contact` and a policy URL. On a valid request: write a `Tombstone` (one generation), delete webseed objects (R2/B2), stop seeding on publisher nodes. Seed daemons honor tombstones by default and stop seeding. Limits are stated publicly: past DataLayer history and existing swarm copies cannot be recalled.

---

## 12. Rust crates

| Need | Crate | Notes |
|---|---|---|
| Async runtime | `tokio` | multi-thread |
| HTTP + DataLayer RPC | `reqwest` with `rustls-tls` | client cert (mTLS) for port 8562 |
| HF Hub | `hf-hub` 1.x | Xet support via `hf-xet` |
| Serialization | `serde`, `serde_json` | canonical JSON helper (sorted keys) |
| Hashing / hex | `sha2`, `hex` | streaming SHA-256 |
| Signatures | `ed25519-dalek` 2.x, `blake2`, `base64` | minisign compatible format (6.9); BSD-3-Clause and MIT/Apache-2.0, pass `deny.toml` |
| CLI / config | `clap` (derive), `figment` + `toml` | |
| Errors / logs | `thiserror`, `anyhow`, `tracing`, `tracing-subscriber` | |
| State | `rusqlite` or `sqlx` (sqlite) | decision in M0 |
| S3-compatible upload (R2, B2) | `aws-sdk-s3` or `object_store` | both speak S3 API; decision in M0 |
| BitTorrent | `librqbit` | **Must verify in M0:** BEP 52 v2 download and seeding, v2/hybrid torrent creation, and BEP 19 webseed support. As of 2026-11-12, librqbit 9.1.0 has v2 metadata parsing, Merkle verification and `btmh` magnets, but the v2 tracking issue is still open (https://github.com/ikatson/rqbit/issues/546), and I found no confirmation of BEP 19 webseeds. |
| BitTorrent fallback | libtorrent-rasterbar 2.x (supports v2 and hybrid) via FFI bindings or a sidecar process; or write our own BEP 52 torrent builder (bencode + hashing) and seed with a v2-capable client | Choose in M0 based on spike results. |
| Bencode (if building torrents ourselves) | `serde_bencode` or `bendy` | |
| Chia (optional, direct chain work) | `chia-wallet-sdk` / `chia-sdk-driver` with `chip-0035` | For delegated writers or reading the store without a full Python node. M1 uses the official DataLayer RPC instead. |

There is no official Rust DataLayer daemon; M1 talks to the Python `chia data` service over RPC.

### 12.1 Workspace layout

| Crate | Role |
|---|---|
| `tensorseed-core` | Manifest and catalog JSON types, key builders (`ts/v1/...`), canonical JSON, streaming SHA-256, shared errors. No network code. |
| `tensorseed-hf` | Hugging Face API client (model info, tree, downloads by commit). |
| `tensorseed-catalog` | `CatalogBackend` trait and backends. `minisign` (signature format), `signed` (`SignedEntry`), `chia` (DataLayer RPC over mTLS with `reqwest` + rustls), `file` (signed static files and hash chain), `export` (bundles). |
| `tensorseed-torrent` | BitTorrent v2 torrent creation, seeding, and fetching with BEP 19 webseeds (librqbit pending M0). |
| `tensorseed-cli` | The `tensorseed` binary: clap commands, figment config, tracing setup. |

---

## 13. Cost notes (list prices as of 2026-11-12; recheck before committing)

| Item | Figure | Source |
|---|---|---|
| Cloudflare R2 Standard storage | $0.015 per GB-month, first 10 GB-month free | https://developers.cloudflare.com/r2/pricing/ |
| R2 egress | $0 | same |
| R2 Class A / Class B operations | $4.50 / $0.36 per million | same |
| Cloudflare Project Alexandria | credits for qualifying open source projects | https://blog.cloudflare.com/expanding-our-support-for-oss-projects-with-project-alexandria/ |
| Backblaze B2 | $6.95 per TB-month; egress free up to 3x stored, then $0.01/GB | https://www.backblaze.com/cloud-storage/pricing |
| Sia (renterd) | median about $1 per TB-month storage (market price, varies) | https://sia.tech/provide-storage |
| DataLayer fee | config default 0.001 XCH per on-chain update if no fee is set | https://docs.chia.net/datalayer-rpc/ |
| DataLayer mirror coin | `amount` minimum 0, refunded on delete | https://docs.chia.net/guides/datalayer-user-guide/ |
| Chia transaction block interval | about 52 s average | https://docs.chia.net/chia-blockchain/coin-set-model/costs/ |

Implications:
- One publish is one catalog generation, so chain cost is at most one fee per published revision (or per `sync` run if several repos are batched into one pending root; decision below).
- Storage cost is dominated by R2: for example, 1 TB (1,000 GB) is 990 billable GB x $0.015 = $14.85 per month with zero egress, matching Cloudflare's own worked example on the pricing page.
- Rejected design for comparison: putting 1 TiB inside DataLayer needs at least 4,195 generations under the 250 MiB per-generation subscriber default (1 TiB / 250 MiB), with each subscriber storing roughly 3 copies (inferred from `chia/data_layer/data_store.py` and `download_data.py`; not measured).

---

## 14. Prior art

- **The Hugging Bay**: community index of magnet links for openly licensed models with per-file SHA-256 manifests, webseeds, and a catalog distributed as a torrent (https://thehuggingbay.io/llms.txt).
- **LLM Torrents**: 119 models cataloged, SHA-256 checked against pinned HF revisions, signed manifests, HTTPS webseeds, no gated rehosting (https://llmtorrents.com/).
- **Academic Torrents LLM Weights collection** (https://academictorrents.com/collection/llm-weights).
- **DIG Network**: Chia-anchored Layer 2 that plans to pay mirrors for proven availability; whitepaper (May 2026) says "late prototype, pre-public-testnet" (https://dig.net/whitepapers/DIG-Network-Part1-Consensus.pdf). A possible future incentive layer, not usable today.

What this project adds: a Chia-anchored, versioned, publicly mirrorable catalog with Merkle proofs, so the index itself has no single point of failure and its history is auditable, plus a chain-free signed-file copy of the same signed entries. Interoperating with the two indexes above (for example, exporting our manifests in their format) is an open question.

---

## 15. Milestones

**M0: Spikes (scaffold only, no product logic)**
- Verify librqbit: v2 download, v2 seeding, v2 and hybrid torrent creation, BEP 19 webseeds against an R2 bucket with range requests. If any fail, prototype the fallback (section 12).
- DataLayer RPC from Rust over mTLS: `create_data_store` on testnet, `batch_update(submit_on_chain=false)`, `submit_pending_root`, confirmation polling, `get_value`, `subscribe` and `get_sync_status` from a second node, `add_mirror`.
- Confirm HF API field names (tree `lfs.oid`, `gated`, license tag) and hf-hub Xet downloads.
- Pick sqlite crate and S3 crate. Pick default `piece_length`.
- Check the `CatalogBackend` trait (section 6.0) against the real DataLayer call sequence and adjust it before M1 depends on it.
- Signed-file backend core (done): minisign compatible signing, `SignedEntry`, hash chain with rollback, fork, gap, and stale head checks, export bundles. Still in M0: encrypted key files and `catalog keygen`, trust anchor persistence.

**M1: Publish**
- `doctor`, `catalog create/info/add-mirror`, `publish`, `sync`, `jobs`. Resumable state, R2 upload, torrent creation, atomic catalog writes. Testnet first, then mainnet.
- File backend transport: HTTPS fetch with mirror fallback, upload of deltas, snapshots, and head to `catalog.tensorseed.dev`, head re-signing, snapshot torrent. `catalog export` / `catalog import` end to end between both backends.

**M2: Pull**
- `pull`, `verify`, `list`, `show`; tombstone handling; license and NOTICE output; `--manifest-file` mode.

**M3: Seeding daemon**
- `seed` with selection rules, disk budget, bandwidth caps, tombstone honoring, optional `--serve-catalog` and catalog torrent.

**M4: Catalog explorer**
- Static site at tensorseed.dev (generated from the catalog) listing repos, licenses, sizes, magnets, webseeds, attribution text, and catalog generation history. Hosted on R2 or any static host; it has no privileged role.

---

## 16. Open questions

1. Which models and datasets first, and how many TB in year one (for example, widely used models in GGUF Q4 / MLX 4-bit / NVFP4 variants)?
2. Do you already run a synced Chia full node and wallet? What XCH budget for fees and mirror coins?
3. Hosting hardware, uplink speed, and whether you can open port 8575 (DataLayer) and a torrent port.
4. R2 as primary webseed (zero egress, $0.015/GB-month) vs B2 ($6.95/TB-month, egress free up to 3x stored). Apply for Project Alexandria?
5. Single owner key, or CHIP-0035 delegated writers for co-maintainers?
6. License policy: open licenses only at launch, or also Llama-style attribution licenses?
7. Build standalone, or build on / interoperate with The Hugging Bay or LLM Torrents?
8. ~~Final project name.~~ Resolved: Tensorseed, primary domain tensorseed.dev.
9. ~~Second, chain-free catalog backend?~~ Resolved: yes, signed static files (6.8) next to Chia.
10. Publisher key rotation and revocation: a signed `ts/v1/keys` entry with successor keys, a threshold of maintainer keys, or both? Where does the offline key live?
11. Should both backends run in parallel from launch (Chia canonical, file as a live mirror), or should the file backend start as export-only?
12. Accept ssh-sig as a second signature type later (6.9), or stay minisign only?
13. Head expiry (`file.head_ttl_secs`, default 7 days): shorter is safer against freeze attacks but needs a reliable re-signing job.
