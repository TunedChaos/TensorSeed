# Tensorseed: Project Specification (Revision 9)

> Name: **Tensorseed** (binary and crate prefix `tensorseed`).
> Website: https://tensorseed.dev (primary). Also registered: tensorseed.org, tensorseed.garden.
> Publisher: Tuned Chaos. License: AGPL-3.0-or-later.
> Status: pre-alpha, milestone M0 (spikes). Design approved in principle on 2026-11-12. Working today: `pull`, `verify`, `list`, `show`, `jobs`, `harvest`, `mirror`, and the `catalog` commands on the file backend; `seed` and `pull --swarm`/`--seed-after` over BitTorrent (hybrid v1+v2 torrents built in pure Rust, swarm through librqbit); the Chia backend is implemented against mocked RPC responses only.

Revision 9 changes: `tensorseed serve`, the long running seeder (8.3): catalog driven loop with selection rules, disk budget, takedowns, auto fetch through the pull pipeline, upload and download caps, uTP, opt in NAT-PMP and UPnP IGD port mapping, HTTP webseed serving with ranges, `--serve-catalog` (also on `seed`), a localhost JSON health endpoint, state file resume, graceful SIGTERM, and a hardened systemd unit. New crate `tensorseed-serve`; the `[serve]` config block replaces `[seed]` (9, 10, 12.1).

Revision 8 changes: the Hugging Face token can also come from `huggingface.token_file` (must not be readable by group or others); an inline `token =` in the config is rejected with a clear error. License overrides: `--allow-license <id>` on `pull`, `harvest`, and `publish`, and a per-repo `allow_license` in watchlist entries, let a publisher mirror their own `license: other` repos while the default allowlist stays strict; overridden repos keep their honest license class in the manifest. `publish <repo>` now runs the harvest pipeline for one repo (5, 9, 10, 11, 13).

Revision 7 changes: M0 BitTorrent spike done (results in [M0-TORRENT-SPIKE.md](M0-TORRENT-SPIKE.md)). librqbit 9.1 cannot create v2 torrents, open v2 only torrents, resolve `btmh` only magnets, or use BEP 19 webseeds, but it seeds and downloads the v1 side of hybrid torrents and interoperates with libtorrent 2.x in the same swarm. Decisions: Tensorseed builds BEP 52 v2 and hybrid torrents itself in pure Rust (byte identical to libtorrent), hybrid is the default, librqbit is the swarm engine, webseed bytes come from Tensorseed's own HTTP fetcher while torrents still list webseeds for other clients, and the default piece length is automatic (4 MiB up to 64 MiB, at most 16,384 pieces). `harvest` writes torrents and manifest `torrent` blocks; `seed`, `pull --swarm`, `pull --seed-after`, and `provider = "torrent"` storage targets work (7.1, 8.2, 8.3, 9, 10, 10.1, 12).

Revision 6 changes: storage targets and mirrors are named, repeatable provider blocks (`[[storage.target]]` with `provider = "r2" | "b2" | "vultr" | "wasabi" | "minio" | "aws" | "s3" | "http" | "torrent" | "local"` and derived endpoints; `[[mirror]]` in `mirrors.toml`), several per provider; publish writes to every enabled target and pull tries sources by priority (10.1); publishers can sign and serve a mirror list for the file backend (6.12).

Revision 5 changes: object storage is optional and provider neutral (local disk by default; any S3 compatible service, several at once; section 10.1); torrents are optional in manifests until the BitTorrent spike lands, so HTTP webseeds and the Hub are the byte sources for now; Hugging Face API facts confirmed against live responses (section 5); crate decisions made (`reqwest` instead of `hf-hub`, `rusqlite`, `object_store`; section 12); consumer side (`pull`, `verify`), publisher keys (`catalog keygen`), file backend transport, and the Chia RPC calls implemented; new automatic harvest mode (section 13).

Revision 4 changes: added a second, chain-free catalog backend (signed static files, section 6.8) next to Chia DataLayer, made every catalog entry a publisher-signed envelope in every backend (6.9), and defined `catalog export` / `catalog import` for moving a catalog between backends with signatures intact (6.10).

Revision 3 changes: renamed the project to Tensorseed, switched key and schema prefixes to `ts`, and made the catalog layer explicitly pluggable through a `CatalogBackend` trait with Chia DataLayer as the first implementation (section 6.0).

---

## 1. Summary

A Rust CLI and daemon that keeps open AI models and datasets from Hugging Face (HF) available even if HF goes away. It does this in two layers:

1. **Catalog layer (pluggable; Chia DataLayer first, signed static files second):** a small, signed, versioned index behind a `CatalogBackend` trait so other chains can be added later without changing the manifest format. Every entry is signed with one publisher key in every backend, so the catalog can move between backends. The `file` backend (6.8) needs no chain at all. In the first backend, one DataLayer store maps each HF repository to its mirrored revisions. Each revision has a manifest: file list, sizes, SHA-256 hashes, license, the HF commit it came from, and how to fetch the bytes. Only the Merkle root of this store goes on chain. Values are kilobytes, not gigabytes.
2. **Content layer (BitTorrent v2 plus HTTP webseeds):** the actual model bytes travel as BitTorrent v2 torrents (BEP 52) with BEP 19 webseeds. Publishers keep files on local disk by default and seed from there; an always-on HTTP webseed is optional and can be any static web server or one or more S3 compatible buckets from any provider (section 10.1). Sia is an optional cold backup. Volunteers seed whatever subset they choose.

A user who knows only the catalog store ID (for the Chia backend) can find any mirrored model, download it from the swarm or the webseed, and verify every byte against hashes that are anchored on Chia and traceable to the original HF commit.

This directly answers the question in the tweet that started this project: models "cryptographically signed, then hosted any arbitrary place if the sigs matched" (https://x.com/ErikVoorhees/status/2120707384071143510).

### 1.1 What changed from Revision 1 and why

| Revision 1 idea | Revision 2 | Reason |
|---|---|---|
| Store model bytes as 4 to 8 MB hex chunks in DataLayer | Store only manifests in DataLayer; bytes go over BitTorrent v2 and HTTP | Subscribers reject any per-generation delta file over `max_delta_file_size: 250` MiB by default, so 1 TiB would need at least 4,195 sequential on-chain generations. Every subscriber also keeps roughly 3 copies (key/value blobs, all delta `.dat` files, latest full `.dat`), and new subscribers replay the whole history. Sources in section 14. |
| "Mirrored across the chain and in multiple plots" | Plots are not involved at all | Chia plots contain no user data (https://www.chia.net/blog/why-chia-plots-dont-contain-real-user-data/). Only the root hash is on chain. |
| One DataLayer store per model plus a master catalog | One catalog store (optionally sharded later) | Manifests are KB-sized, so per-repo stores add on-chain transactions and mirror coins without benefit. |
| `https://huggingface.co<repo_id>` | `/api/models/{repo_id}`, `/api/datasets/{repo_id}`, tree endpoint | Original path was malformed. |
| Poll `get_root` | `get_root` for owned stores; `get_local_root` / `get_sync_status` for subscribed stores | `get_root` returns an invalid hash for subscribed stores (DataLayer RPC reference). |
| Each `batch_update` is visible immediately | Stage with `submit_on_chain=false`, publish with `submit_pending_root` | Gives one atomic generation per publish, so subscribers never see a half-written revision. |
| Built-in DataLayer HTTP server fronted by Nginx/Caddy for model bytes | HTTP webseeds (static host or object storage) for bytes; DataLayer mirror only for the small catalog `.dat` files | Chia subscribers fetch whole `.dat` files from one server at a time; they do not use byte ranges or swarm. |

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
- N2. Paying seeders or running an incentive token. (Possible future work, see DIG Network in section 15.)
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
                    | (2) file download (resolve URLs, streaming hash check)
                    v
 +-------------------------------------------------------------+
 | tensorseed publish (publisher node)                         |
 |  staging dir -> verify sha256 -> build BEP 52 torrent       |
 |   -> keep on local disk; optional upload to S3 targets      |
 |   -> start seeding                                          |
 |   -> write manifest + pointers to DataLayer (staged batch)  |
 |   -> submit_pending_root -> wait for confirmation           |
 +-------------------------------------------------------------+
        |                         |                     |
        | DataLayer RPC (mTLS,    | HTTPS (BEP 19)      | BitTorrent v2 swarm
        | localhost:8562)         |                     | (DHT + trackers)
        v                         v                     v
  Chia DataLayer             HTTP webseeds         Volunteer seeders
  catalog store              (local web server or  (tensorseed seed)
  (root hash on chain;        any S3 provider)
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
| Malicious or buggy webseed / peer serves wrong bytes | Corrupt or poisoned weights | Piece checks in transit (SHA-1 pieces in the built-in librqbit engine, which uses the v1 side of hybrid torrents; v2 clients such as libtorrent check 16 KiB blocks with SHA-256 Merkle proofs), plus a full-file SHA-256 check against the signed manifest before a file counts as present. HTTP downloads go through a temp file and an atomic rename. Failing sources are dropped. |
| Malicious DataLayer mirror serves forged catalog `.dat` | Fake manifests | Chia DataLayer validates downloaded data against the on-chain root before accepting it. Clients pin the catalog store ID. Every entry is also verified against the pinned publisher key (6.9). |
| Malicious file-catalog host or mirror forges, rolls back, or forks the catalog | Fake or outdated manifests | Head, deltas, and snapshots are minisign-signed with the pinned publisher key; every entry is verified again; the revision and `prev_root` hash chain plus the local trust anchor reject rollback, replay, gaps, and forks (6.8). |
| File-catalog host freezes the catalog (serves an old, validly signed head forever) | Users miss new revisions and tombstones | `head.json` carries `expires_unix`; clients refuse expired heads. Publisher re-signs the head before `file.head_ttl_secs` lapses. |
| Publisher shows different file-catalog chains to different users (split view) | Targeted users get different manifests | Not preventable by the file backend alone. Detectable by comparing heads across mirrors; prevented by the Chia backend's on-chain root (6.11). |
| Publisher signing key compromise (any backend) | Attacker signs valid looking entries | Keep the key offline; rotation and revocation design is an open question (section 17). Rollback and fork checks still stop rewriting history already seen by clients. |
| Catalog owner key compromise | Attacker publishes manifests pointing to malicious weights (pickle `.bin`/`.pt` can execute code on load) | Keep the owner key in a cold or Vault-style setup; publish from a CHIP-0035 delegated writer key that the owner can revoke (optional, see 6.6). Manifests record `hf_commit` and HF-reported hashes so anyone can audit against HF while it exists. `pull` warns loudly on pickle-based formats and prefers `safetensors`/`gguf`. `pull` shows the publishing generation and timestamp and supports `--expect-sha256` / pinned revisions. |
| Upstream HF repo itself is malicious or later altered | We mirror bad content, or a force-push rewrites history | We mirror by immutable commit SHA; new commits become new revisions and never overwrite old manifests. Record HF security-scan status if exposed by the API (unverified, M0). |
| Publisher host compromise | Same as key compromise; also object storage credentials | Least-privilege S3 keys (write to one bucket each, read from env only), DataLayer RPC never exposed beyond localhost (RPC reference warns port 8562 must not be public). |
| Takedown request (license or legal) | Must stop distributing | See section 11. Honest limit: the DataLayer delta history is permanent for anyone who already synced, and the swarm cannot be recalled. We can tombstone the catalog entry and delete webseed objects. |
| Catalog spam / bloat | Only relevant if write access is delegated | Single writer by default; delegated writers are explicit and revocable. |
| Seeder privacy | Seeder IPs visible in swarm | Documented; seeders may use a VPN or seed only via webseed-style HTTP hosting. |
| Hash algorithm confusion | v2 `pieces root` is not a plain SHA-256 of the file | Manifest stores both: `sha256` (plain, matches HF LFS oid) and `bt_v2_pieces_root` (BEP 52). Both are checked. |

---

## 5. Hugging Face integration

Confirmed against live responses on 2026-11-12 (code: `tensorseed-hf`):

- Model info: `GET {endpoint}/api/models/{repo_id}` (datasets: `/api/datasets/{repo_id}`), optionally `/revision/{rev}`. Fields used: `sha` (40-hex commit), `gated` (`false`, `"auto"`, or `"manual"`), `private`, `disabled`, `lastModified`, `cardData.license` (a string or a list), and `tags` (which include `license:<id>`). A missing or private repo returns 401 without a token.
- File tree: `GET {endpoint}/api/models/{repo_id}/tree/{commit}?recursive=true`. Every entry has `type`, `path`, `size`, and `oid` (the git blob SHA-1). LFS entries add `lfs.oid` (SHA-256) and `lfs.size`; Xet backed files also carry `xetHash`. Pages of 1000 entries, continued through a `Link: <...>; rel="next"` header, which is only followed on the endpoint's own origin.
- Listing (harvest discovery): `GET {endpoint}/api/models?author=..&filter=<tag>&sort=downloads&direction=-1&limit=N`, same pagination; entries carry `id`, `downloads`, `tags`, `gated`.
- Downloads: `GET {endpoint}/{repo_id}/resolve/{commit}/{path}` (datasets under `/datasets/`), usually redirected to a CDN; Xet stored files are served there too. Always by commit SHA, never by branch.
- Verification: LFS files against `lfs.oid` (SHA-256); small non-LFS files against their git blob id (SHA-1 of `"blob <size>\0" || content`), then hashed with SHA-256 for the manifest.
- Auth: optional token from the env var named by `huggingface.token_env` (default `HF_TOKEN`), else from `huggingface.token_file` (refused if readable by group or others; chmod 600). The token is never written in the config file: an inline `token =` is a config error. It is sent only to the endpoint origin (the HTTP client drops it on cross-origin redirects). Gated repos are refused unless explicitly allowed (`--allow-gated` for personal pulls, `harvest.allow_gated` for mirroring).
- Rate limits: HTTP 429 and 5xx are retried with exponential backoff, honoring `Retry-After` (capped at 5 minutes). Downloads use bounded concurrency (default 2 files) and HTTP range resume.

The `hf-hub` crate is not used: version 1.0 pulls in `hf-xet` (over 550 crates) and is built around the Hugging Face cache layout, while Tensorseed needs the raw tree hashes, its own output layout, streaming verification, and resumable jobs (section 12).

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
  "webseed_bases": ["https://models.example.org/t/"],
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
    "torrent_file_urls": ["https://models.example.org/t/org__name@<commit12>.torrent"]
  },
  "webseeds": ["https://models.example.org/t/"],
  "backups": [ { "kind": "sia", "locator": "sia://bucket/prefix/" } ],
  "published_at": "2026-11-12T00:00:00Z",
  "publisher_tool": "tensorseed 0.1.0"
}
```

Field rules:
- `sha256` is the plain SHA-256 of the whole file and is always present. `hf_lfs_oid` must equal it when present; a mismatch aborts publish.
- `bt_v2_pieces_root` is the BEP 52 per-file Merkle root (empty files have none).
- `piece_length` is a power of two, at least 16 KiB (BEP 52). The default is automatic (`torrent.piece_length = 0`, section 10): 4 MiB, doubled until the revision has at most 16,384 pieces, capped at 64 MiB.
- `pickle_risk` is true for `.bin`, `.pt`, `.pth`, `.pkl`, `.ckpt` and similar formats.
- `files` is empty and `file_pages > 0` when paging is used (harvest pages above 2000 files, 1000 per page).
- `torrent` is optional (`null` for revisions published before torrents existed or with `torrent.create = false`); when present, `torrent.name` must equal the content name `{org}__{name}@{commit[0:12]}`, `magnet` carries both `btih` and `btmh` for hybrids, and `torrent_file_urls` lists `<webseed base><content name>.torrent` for every webseed base.
- Each `webseeds` entry is an http(s) base ending in `/`; a file's URL is `<base><content name>/<path>` with each path segment percent encoded (BEP 19 multi-file rule).

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

**Layout** under `file.base_url` (for example `https://catalog.tensorseed.dev/`; any static host, or the public URL of a storage target), one directory per logical catalog name:

```
<base_url>/<catalog>/head.json                       (+ head.json.minisig)    rewritten each revision
<base_url>/<catalog>/deltas/<revision:010>.json      (+ .minisig)             one per revision, never rewritten
<base_url>/<catalog>/snapshots/<revision:010>.json   (+ .minisig)             full state, every file.snapshot_every revisions
```

Everything except `head.json` is append-only: a published delta or snapshot is never modified or deleted, so any static host, mirror, or archive can carry the full history. Each snapshot is also published as a small BitTorrent v2 torrent (magnet listed in `head.json` in M1), so the catalog survives if every HTTPS host vanishes. Extra read mirrors come from mirror lists (6.12) and `file.mirror_urls`.

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

Clients keep a trust anchor `(revision, root)` and the verified entries in `file.state_dir/<catalog>/state.json` (re-verified on load) and never go below it. Sync tries `base_url`, then each mirror; rollback, stale, or tampered sources are skipped, while a fork aborts. The publisher writes to the configured storage (section 10.1) in the order delta, snapshot (at revision 1 and every `snapshot_every`), head, and advances its local state only after every upload succeeded; when no source serves a head yet (for example local only storage), it continues from its verified local state. A new client bootstraps from the newest snapshot, then applies later deltas. The publisher re-signs `head.json` (same revision, new `expires_unix`) before `file.head_ttl_secs` lapses.

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
| Mirror discovery | permissionless, on-chain `add_mirror` coins | publisher signed `mirrors.toml` served with the catalog, plus local `[[mirror]]` blocks and `file.mirror_urls` (6.12) |
| Dependencies | Chia full node, wallet, XCH for fees | none beyond HTTPS or BitTorrent |
| Cost per update | one transaction fee (often 0) | a few object writes (local disk or any S3 provider) |
| Best for | the canonical, publicly auditable catalog | offline use, simple mirrors, and insurance against Chia becoming unavailable |

Sia stays an optional cold store for *bytes* (7.3); it has no catalog or signature features and is not a catalog backend. Other chains (for example an EVM layer 2) can be added later as further `CatalogBackend` implementations only if users ask for them; the entry format and export bundles already make that a migration, not a redesign.


### 6.12 Mirror lists (`mirrors.toml`)

Mirrors use the same named, repeatable block style as storage targets. A mirror list (`ts.mirrors/1`) is TOML with `[[mirror]]` blocks: `name` (unique), `provider` (`http`, `s3`, `r2`, `b2`, `vultr`, `wasabi`, `minio`, `aws`, informational for HTTPS hosts; or `torrent` with a `magnet`, reserved for catalog snapshot torrents, not used yet), `url` (public base of a published tree: catalog files under `<url><catalog>/`, webseed objects under `<url>t/`), `priority` (default 100, lower first), `enabled` (default true), optional `label`, and optional `publisher_pubkey` (use this mirror only for catalogs signed by that key). Example: `config/mirrors.example.toml`.

Two lists exist:

- **Local** (`mirrors.file`, default `~/.config/tensorseed/mirrors.toml`): the user's own list, edited by hand or with `tensorseed mirror add|remove`.
- **Published**: the publisher signs a list with `tensorseed mirror publish` and it is written through storage (6.8 layout) as `<catalog>/mirrors.toml` plus `mirrors.toml.minisig`. The signature's trusted comment is `ts.mirrors/1 catalog=<name> sha256=<hex of the body>`, so a list cannot be replayed for another catalog. With `mirrors.fetch_published = true` (default), sync and pull fetch it from `file.base_url` or any known mirror, verify it against `catalog.publisher_pubkey`, and cache it under `file.state_dir`; a missing, tampered, or unreachable list falls back to the last verified copy. This gives the file backend the mirror discovery that Chia gets from `add_mirror` coins.

The effective ranking merges the local list, the published list, `file.mirror_urls` (priority 100), and storage targets' public URLs, keeps enabled HTTP mirrors whose pin (if any) matches, deduplicates by URL keeping the lowest priority, and sorts by priority. Catalog reads try `file.base_url` first and then this ranking; pulls use it for webseeds (10.1). `tensorseed mirror list` prints it. Mirrors are untrusted by design: catalog files are signature and hash chain checked and every downloaded byte is hash checked, so a hostile mirror can only be slow, stale (detected by the head expiry and rollback checks), or absent.

---

## 7. Content layer

### 7.1 BitTorrent v2

Decided in the M0 spike ([M0-TORRENT-SPIKE.md](M0-TORRENT-SPIKE.md)).

- One torrent per (repo, commit). Multi-file torrent; `name` = `{org}__{name}@{commit[0:12]}`; files keep their HF relative paths.
- **v2 required** for per-file SHA-256 Merkle trees and `btmh` magnets (BEP 52, https://www.bittorrent.org/beps/bep_0052.html). Each manifest file records its `bt_v2_pieces_root`.
- **Hybrid v1+v2 by default** (`torrent.hybrid = true`): v1 only clients can join, and the built-in engine needs the v1 side. Layout matches libtorrent 2.x: files in canonical path order, each followed by a BEP 47 pad file (`.pad/<n>`, attribute `p`) up to the next piece boundary, including the last file. Pads are never written to disk. `torrent.hybrid = false` builds v2 only torrents for other clients; the built-in engine refuses to seed those.
- **Creation is ours, in pure Rust** (`tensorseed-torrent`: `bencode`, `merkle`, `metainfo`): one read per file yields the BEP 52 leaves, piece layer and root, the aligned v1 SHA-1 pieces, and the plain SHA-256 (compared with the verified download). Output is deterministic (no creation date), so anyone can rebuild the info hashes from the files. Tests pin info hashes computed by libtorrent 2.0.11.
- **Engine: librqbit** (Apache-2.0) for seeding and swarm downloads. It joins the v1 side of a hybrid swarm (SHA-1 pieces) and interoperates with v2 capable peers. It cannot open v2 only torrents, resolve `btmh` only magnets, or use webseeds; Tensorseed works around all three (hybrid torrents, hybrid magnets, its own webseed fetcher). libtorrent-rasterbar 2.x stays a possible later engine (sidecar first); switching does not change torrents or manifests.
- **Integrity does not depend on the engine.** Swarm bytes land in place, then every file is hashed with SHA-256 against the signed manifest before it counts as present; `Metainfo::verify_file` can also recheck the BEP 52 root.
- Trackers: a short configurable list (`torrent.trackers`, http, https, or udp) plus DHT. Private trackers are not used. Static peers (`torrent.peers`, `pull --peer`) help reach known seed boxes.
- The `.torrent` file sits next to the content in storage, at `t/<content name>.torrent` beside `t/<content name>/` (10.1), so every webseed base also serves it; its URLs are stored in the manifest. A later catalog revision of the same commit may rewrite that file (for example to add webseeds or trackers) while the info dict stays the same, so `t/<content name>.torrent` must not be uploaded or served with a long-lived immutable Cache-Control. The payload under `t/<content name>/` is immutable.

### 7.2 Webseeds (BEP 19)

- Webseeds are optional. A manifest's `webseeds` lists `<public_base_url>t/` for every storage location that has a public URL: `storage.local_public_base_url` when a web server exposes the local directory, and every enabled `[[storage.target]]` `public_base_url`, in priority order. Pull adds mirror list entries (6.12) by priority. Per BEP 19, when the URL ends in `/`, clients append the torrent name and file path, so objects live at `t/<content name>/<relative path>` (https://bittorrent.org/beps/bep_0019.html). The content name includes the commit, so every revision has its own immutable prefix.
- Any host works as long as it serves those paths with HTTP range support: a static web server over the local directory, or a public bucket from any S3 compatible provider (custom endpoint, path style or virtual hosted). Uploads stream large files as S3 multipart uploads. Several targets can be written at once for redundancy across providers.
- `pull` tries webseeds first and falls back to the Hub's resolve URL (unless `--no-hf-fallback`), dropping any source that serves wrong bytes. Tensorseed's own fetcher is the BEP 19 client (librqbit has none); torrents still carry every base in `url-list`, so other clients (libtorrent, qBittorrent, Transmission) use the webseeds directly. Verified with libtorrent: a Tensorseed hybrid torrent downloads from the webseed alone with no peers.

### 7.3 Cold backups (optional)

- Sia stays optional cold storage for bytes only; it is not a catalog backend (6.11).
- An S3 compatible target without a `public_base_url` is an upload only cold copy with the same object layout. Sia (via `renterd`) is recorded under `backups` in the manifest so a future operator can restore a webseed. Not used by `pull` unless `--allow-backup-sources` is set (not implemented).

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
 -> DOWNLOAD       resolve URLs by commit into <storage.local_dir>/t/<name>/
                   bounded concurrency; resumable; record per-file state in sqlite
 -> VERIFY         sha256 every file; compare to hf lfs oid where present
                   mismatch -> FAILED(integrity)
 -> BUILD_TORRENT  compute BEP 52 piece layers (+v1 if hybrid); write .torrent
 -> UPLOAD         to every enabled [[storage.target]] with upload = true (multipart for S3)
                   HEAD-check size; resumable via sqlite part tracking
 -> SEED_START     add torrent to local seeding session (data from staging or seed dir)
 -> STAGE_CATALOG  batch_update(submit_on_chain=false): manifest, pages, index, latest
 -> SUBMIT         submit_pending_root (through the write queue)
 -> CONFIRM        poll until confirmed root == expected; timeout -> RETRY or FAILED(chain)
 -> CLEANUP        if keep_local_seed=false: delete local files after every upload confirmed
                   else move to seed_dir
 -> DONE(published)
```

Resumability: every transition is recorded in sqlite (`jobs`, `files`, `upload_parts`, `chain_ops`). On restart, a job resumes at its last completed state. CONFIRM is idempotent: if the expected root is already on chain, it advances.

Failure after STAGE_CATALOG but before SUBMIT: use `clear_pending_roots` and restage, or submit if the staged root matches the expected one.

### 8.2 Pull

```
START (args: <repo_id>[@commit] [--revision] | --manifest-file | --manifest-url | --from-hf)
 -> ENSURE_SUB     subscribe(catalog, urls from config) if not subscribed
 -> WAIT_SYNC      get_sync_status / get_local_root until local generation == chain generation
                   (or --no-wait to use the current local copy)
 -> READ           get_value latest or rev/<commit>; read pages; check tombstone
                   tombstoned -> STOP with explanation unless --ignore-tombstone
 -> PLAN           compare with existing files in output dir (sha256); skip verified ones
 -> FETCH          optional swarm pass (--swarm), then HTTP webseeds, mirrors, Hub
 -> VERIFY         full-file sha256 vs manifest (torrent checked against the manifest first)
 -> FINALIZE       atomic rename into output dir; write license files + NOTICE
 -> optional SEED  keep seeding for --seed-after <duration>
 -> DONE
```

`pull` also works without a local Chia node via `--manifest-file` or `--manifest-url` (a signed entry is verified against the pinned key; a plain manifest is accepted with a warning), or with `--from-hf`, which verifies against the Hub's own hashes and says the source was unsigned.

Implemented today: the catalog is synced (`--no-wait` uses the local copy), revisions must be full commit SHAs (otherwise `latest` is followed), tombstones stop the pull unless `--ignore-tombstone`, pickle formats are skipped unless `--allow-pickle`, and files download through `tensorseed-fetch`: up to `--jobs` files at once, webseeds and mirrors in priority order (10.1, 6.12) then the Hub, streaming SHA-256 (or git blob SHA-1 for small Hub files), resume from `.tensorseed/partial/` with HTTP ranges, job records in the SQLite state DB, and an atomic rename into place. The output directory gets `.tensorseed/pull.json` (`ts.pull/1`, the full expected file list), `.tensorseed/manifest.json` (the signed entry, when there is one), and `.tensorseed/NOTICE` (license and attribution). With `--swarm` or `--seed-after`, pull first downloads the manifest's torrent (`torrent_file_urls`, then `<base><content name>.torrent` on every webseed and mirror base), parses it, and checks that its name, files, sizes, roots, and info hashes match the manifest. `--swarm` then lets librqbit download from peers (DHT, trackers, `torrent.peers`, `--peer`) for up to `torrent.swarm_timeout_secs`, into the output directory; the HTTP pass that follows hashes every file, keeps those that match, and downloads the rest (a partly fetched file is currently fetched again in full). The torrent is kept as `.tensorseed/torrent`. `--seed-after <duration>` (`30s`, `10m`, `2h`, `1d`, `forever`) seeds the verified result, leaving out skipped pickle files. `verify` re-hashes a directory against `pull.json` (re-checking the stored manifest signature), the catalog (`verify org/name` or `--repo`), a manifest file, or the Hub (`--hf`), prints a PASS or FAIL line per file, and exits with code 3 on any mismatch. Missing pickle files count as skipped unless `--strict`.

### 8.3 Seed daemon (`tensorseed serve`)

```
START   load <data_dir>/serve-state.json; seed last run's revisions (resume)
LOOP every serve.sync_interval_secs:
  sync catalog (file: head, deltas, snapshot; chia: sync status)
  read head.json expiry; with serve_catalog, mirror the signed catalog files
  desired = rules(select/exclude globs, watchlist, types, license classes,
                  latest_only, max_revision_bytes, max_bytes budget) minus tombstoned
  for seeded items not desired: stop seeding, stop HTTP serving; delete if owned and prune
  for desired items on disk: check torrent against the signed manifest, librqbit piece check, seed
  for desired items missing: if fetch, pull (one at a time, in the background) then seed
  update the health document; persist state
SIGTERM / SIGINT: delete port mappings, persist state, stop
```

Implemented (Revision 9). `tensorseed serve [--for DURATION] [--serve-catalog] [--http ADDR:PORT] [--health ADDR:PORT|off] [--fetch|--no-fetch] [--dir DIR]... [--sync-interval SECS]` runs the loop above; flags override `[serve]` keys.

- **Where revisions come from.** Selected revisions are looked up as `t/<content name>/` (with `t/<content name>.torrent`) under `serve.data_dir` (downloaded by the daemon, "owned"), `storage.local_dir`, every enabled `provider = "torrent"` target, and `serve.dirs`. The local torrent must match the signed manifest (`Metainfo::check_manifest`: name, files, sizes, BEP 52 roots, info hashes), then librqbit checks every piece before the revision is announced, so a revision is only seeded and served when it matches the catalog. If no torrent file is on disk, it is fetched from the manifest's `torrent_file_urls`. With `fetch = true`, missing revisions are downloaded into `<data_dir>/t/<content name>/` with the pull pipeline (8.2: swarm first when `fetch_swarm`, then webseeds, mirrors, the Hub; every file SHA-256 checked), one at a time in the background so takedowns stay prompt; a failed download is retried after an hour. An incomplete owned copy (crash mid download) is resumed rather than seeded.
- **Selection.** `select` globs (`*` matches across `/`) with `exclude`, plus the `[harvest]` watchlist with `select_watchlist`; `types`, `license_classes`, `latest_only`, `max_revision_bytes`, and the `max_bytes` disk budget (sum of all seeded revisions). Priority for the budget: first matching `select` pattern, then latest before older revisions, then newer `published_at`. The default `select = ["*"]` with `fetch = false` seeds everything of the catalog that is already on disk and downloads nothing.
- **Takedowns.** A tombstone covering a revision (or an index entry marked `taken_down`) stops seeding and HTTP serving at the next sync (default every 300 s). Owned copies are deleted with `prune = true` (default), also when a revision is deselected; published trees (`storage.local_dir`, `serve.dirs`) are never deleted by `serve`. An in-flight download of a revision that is taken down is cancelled.
- **Limits.** Upload cap from `serve.upload_limit_kbps` (falls back to `torrent.upload_limit_kbps`), swarm download cap `serve.download_limit_kbps`, HTTP upload cap `serve.http_upload_limit_kbps` shared by all connections.
- **NAT traversal.** uTP (BEP 29) is on by default in `serve` (`serve.utp`): librqbit 9.1 accepts and dials uTP on the UDP side of `torrent.listen_port` next to TCP (librqbit itself still defaults to TCP only, so `pull` and `seed` keep TCP only). Port mapping is off by default because it opens a port on the router; `serve.port_mapping = true` tries NAT-PMP (RFC 6886, which PCP routers also answer) on the default gateway, then UPnP IGD (`WANIPConnection` / `WANPPPConnection` `AddPortMapping`), for TCP and UDP, with one hour leases renewed at half time and deleted on shutdown. If the router grants a different external port, the daemon warns (peers learn the local port). uTP is not hole punching: two peers that are both behind NAT without a mapping still cannot connect.
- **HTTP serving** (`serve.http_listen`, off by default): `GET /t/<content name>.torrent` and `GET /t/<content name>/<path>` for every seeded revision, which is the BEP 19 layout, so `http://host:port/` works as a `mirrors.toml` entry or a webseed base (`.../t/`). Single `Range` requests get 206 with `Content-Range`, unsatisfiable ones 416, `HEAD` works, only files listed in the torrent are reachable (no `.tensorseed/` state, no traversal), files whose size changed on disk are refused, and a revision disappears from HTTP the moment it stops seeding.
- **`--serve-catalog`** (`serve.serve_catalog`, needs `http_listen`): after each sync the daemon copies `head.json`, every `deltas/*.json`, the head's snapshot, and `mirrors.toml`, each with its `.minisig`, from the sync source into `<data_dir>/catalog/<catalog>/`, checking every signature against the pinned key, and serves them at `/<catalog>/`, so the daemon is a valid `file.mirror_urls` entry. `seed --serve-catalog` serves the publisher's own `<storage.local_dir>/<catalog>/` on `serve.http_listen`.
- **Health** (`serve.health_listen`, default `127.0.0.1:6880`; keep it on localhost): `GET /health` returns `ts.serve.health/1` JSON: status, uptime, catalog (name, backend, revision, root, `head_expires_unix`, source, last sync times, `last_sync_error`, serving), swarm (listen address, uTP, DHT, bytes up and down for this run and in total, speeds, live and connecting peers, limits), every seeded torrent (name, repo, commit, dir, bytes, info hash, uploaded bytes, peers, state, owned), seeded bytes and budget, downloads in progress, skipped revisions with reasons, takedowns applied, HTTP counters, and port mapping state. It answers 503 when the last sync failed and the head is expired (or no sync ever worked); `GET /healthz` is the plain text version.
- **State and shutdown.** `<data_dir>/serve-state.json` (`ts.servestate/1`) records the seeded revisions and lifetime byte counters; torrents are kept in `<data_dir>/torrents/`. On restart the daemon seeds those revisions again right away (librqbit rechecks pieces), then the first sync applies any takedown. SIGTERM and SIGINT remove port mappings, save state, and exit 0. A hardened unit is in `contrib/systemd/tensorseed-serve.service` (DynamicUser, ProtectSystem=strict, empty capability set, syscall filter) with a minimal config next to it.
- **Field test (two real hosts, 2026-11-12).** A home seeder behind NAT and a hosted server: the home router answered neither NAT-PMP nor UPnP (a host firewall can also drop SSDP replies), and the server's firewall dropped the swarm and HTTP ports, so no direct connection was possible without opening a port. Over SSH port forwards instead, `pull --swarm-only` moved the 5.36 GB GGUF revision from the server's `serve` to the home host entirely over the swarm in 455 s (about 11.8 MB/s, limited by the SSH tunnel), and `verify` passed; the home seeder uploaded at about 3 MB/s (its upstream). HTTP range requests and the catalog mirror on the server returned the expected bytes. Seeders need `torrent.listen_port` open for TCP and UDP (and `serve.http_listen`'s port for HTTP).
- **Not yet:** a free disk space check before fetching (only the byte budget applies), BitTorrent seeding of catalog snapshots, announcing a different external port than the local one, and Chia head expiry (the Chia backend reports its generation only).

`tensorseed seed` stays the simple one shot seeder: it seeds every revision under `storage.local_dir`, every enabled `provider = "torrent"` storage target `dir`, or the directories passed with `--dir`, in place, honors `torrent.upload_limit_kbps`, `dht`, and `lsd`, and runs until `--for <duration>` or Ctrl-C. The `[seed]` config block is superseded by `[serve]` and ignored.

---

## 9. CLI

Single binary `tensorseed`.

| Command | Purpose | Status |
|---|---|---|
| `doctor` | Check config, pinned publisher key, catalog backend, storage credentials, torrent ports, disk space. | stub |
| `catalog keygen [--out-dir] [--name] [--no-password] [--force]` | Write a minisign compatible key pair; the secret key is scrypt encrypted with a password from `TENSORSEED_KEY_PASSWORD` or a prompt. | done |
| `catalog create` | File: write `ts/v1/catalog` as revision 1. Chia: `create_data_store` and print the store id. | done |
| `catalog info [--no-wait]` | Revision or generation, root, entries, sources, publisher. | done |
| `catalog add-mirror --url ... [--amount] [--fee]` | Advertise a DataLayer mirror (Chia only; the file backend uses `mirror publish`). | done (mocked RPC) |
| `mirror add [URL] [--name] [--provider] [--priority] [--magnet] [--label] [--pubkey] [--disabled]`, `mirror remove NAME\|URL` | Edit the local `mirrors.toml` (6.12). | done |
| `mirror list [--offline]` | Refresh the published list and print the merged mirror ranking. | done |
| `mirror publish [--file]` | Sign a mirror list and write `<catalog>/mirrors.toml` (+ `.minisig`) to every storage target. | done |
| `catalog export --out FILE [--backend chia\|file]` | Write every signed entry to a backend neutral `ts.export/1` bundle (6.10). | done |
| `catalog import FILE [--backend chia\|file] [--dry-run]` | Verify a bundle against the pinned key and publish its entries unchanged as one generation (6.10). | done |
| `catalog resign-head`, `catalog snapshot` | File backend: re-sign `head.json` with a fresh expiry; write a snapshot now. | done |
| `publish <repo_id> [--type] [--revision <commit>] [--allow-license ID]...` | Run the harvest pipeline for exactly one repo (policy, verified download, torrent, signed entry, uploads), on the watchlist or not. | done |
| `sync` | Publish every target in config. | stub |
| `harvest [--watch] [--dry-run] [--only ...] [--status] [--allow-license ID]...` | Watchlist driven mirroring from the Hub (section 13). | done |
| `pull <repo_id>[@commit] [--out DIR] [--from-hf\|--manifest-file\|--manifest-url] [--jobs N] [--allow-pickle] [--allow-gated] [--allow-license ID]... [--no-hf-fallback] [--no-wait] [--ignore-tombstone] [--swarm\|--swarm-only] [--peer HOST:PORT] [--seed-after DURATION]` | Download and verify (8.2). `--swarm-only` never uses HTTP or the Hub; files the swarm does not deliver fail (exit 3), which makes it a seeder check. | done |
| `verify <dir\|repo_id[@commit]> [--repo] [--dir] [--hf] [--manifest-file] [--strict] [--threads N]` | Re-hash a local copy; exit 3 on mismatch. | done |
| `list [--filter] [--no-wait]` | List catalog entries (from `index` keys). | done |
| `show <repo_id>[@commit] [--revision]` | Print a manifest. | done |
| `jobs [--show ID] [--clear ID]` | Inspect resumable download jobs. | done |
| `seed [--dir DIR]... [--for DURATION] [--serve-catalog]` | Seed published revisions or pulled directories in place; `--serve-catalog` also serves the local catalog files on `serve.http_listen` (8.3). | done |
| `serve [--for DURATION] [--serve-catalog] [--http ADDR] [--health ADDR\|off] [--fetch\|--no-fetch] [--dir DIR]... [--sync-interval SECS]` | Long running seeder: catalog loop, selection rules, takedowns, budget, auto fetch, HTTP webseed, catalog mirror, health endpoint, port mapping (8.3). | done |
| `takedown <repo_id> --reason ... [--revisions ...] [--delete-webseed]` | Write a tombstone, optionally delete webseed objects on every storage target. | stub |

Exit codes: 0 success, 1 error, 2 not implemented yet, 3 verification or download failure.

Additional scaffold command: `config show` prints the effective merged configuration (secrets are never stored in it).

Global flags: `--config`, `--log-format json|pretty`, `-v`.

---

## 10. Configuration (TOML, loaded with figment; env overrides `TENSORSEED_*`, nested keys split on `__`)

```toml
[general]
state_db = "~/.local/share/tensorseed/state.sqlite"
download_dir = "."               # pull default: <download_dir>/<org>/<name>
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
token_env = "HF_TOKEN"              # name of the env var, never the token
token_file = ""                     # or a chmod 600 file; inline `token =` is rejected

[torrent]                        # section 7.1
create = true                    # build a torrent for every harvested revision
hybrid = true                    # v1+v2; the built-in engine needs hybrids
piece_length = 0                 # 0 = auto: 4 MiB, doubled to at most 16384 pieces, max 64 MiB
trackers = []                    # http(s):// or udp:// announce URLs
listen_addr = "0.0.0.0"
listen_port = 6881
upload_limit_kbps = 0            # 0 = unlimited
dht = true
lsd = false                      # BEP 14 local discovery
peers = []                       # "host:port" peers to dial (your seed boxes)
swarm_timeout_secs = 600         # pull --swarm: then HTTP fills the rest

[storage]
local_dir = "~/.local/share/tensorseed/publish"   # always written; the seeder serves it
local_public_base_url = ""       # set if a web server exposes local_dir (makes it a webseed)
local_priority = 100
# [[storage.target]]             # optional, repeatable: name + provider + fields (10.1)

[mirrors]                        # 6.12
file = "~/.config/tensorseed/mirrors.toml"
fetch_published = true

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

[serve]                          # section 8.3; [seed] is superseded and ignored
data_dir = "~/.local/share/tensorseed/serve"
dirs = []
sync_interval_secs = 300
select = ["*"]
exclude = []
select_watchlist = false
types = []
license_classes = []
latest_only = true
max_bytes = 0                    # disk budget (0 = unlimited)
max_revision_bytes = 0
fetch = false
fetch_swarm = true
prune = true
upload_limit_kbps = 0            # 0 = torrent.upload_limit_kbps
download_limit_kbps = 0
utp = true
port_mapping = false             # NAT-PMP / UPnP IGD; opens a router port
port_mapping_lease_secs = 3600
http_listen = ""                 # e.g. "0.0.0.0:8080" (webseed layout)
http_upload_limit_kbps = 0
serve_catalog = false
health_listen = "127.0.0.1:6880"
check_timeout_secs = 3600

[harvest]                        # section 13
repos = []                       # ids or { id = "org/name", allow_license = ["other"] }
authors = []
tags = ["safetensors"]
min_downloads = 1000
max_repo_bytes = 0
disk_budget_bytes = 0
allow_gated = false
takedown = []
```

Secrets are only read from environment variables or a file outside the repo; never stored in the TOML. The publisher key password comes from `TENSORSEED_KEY_PASSWORD` or an interactive prompt. Default config path: `~/.config/tensorseed/tensorseed.toml` (override with `--config` or `TENSORSEED_CONFIG`). An example lives at `config/tensorseed.example.toml` in the repository.

---

### 10.1 Storage

Object storage is optional. Everything the publisher writes (signed catalog files under `<catalog name>/`, harvested model files under `t/<content name>/`, signed manifests under `manifests/`, the published mirror list) always goes to `storage.local_dir`, seeding works from those local files, and nothing needs an account anywhere. Exposing that directory with any static web server and setting `local_public_base_url` turns it into a webseed and a catalog source.

Remote copies are named, repeatable `[[storage.target]]` blocks. Each has a unique `name` and a `provider`, one of `r2`, `b2`, `vultr`, `wasabi`, `minio`, `aws`, `s3`, `http`, `torrent`, `local`. Several blocks may use the same provider, so one operator can run several buckets, regions, or accounts. Common fields, with defaults:

| Field | Default | Meaning |
|---|---|---|
| `enabled` | `true` | Disabled blocks are ignored entirely (kept for later). |
| `priority` | `100` | Webseed and mirror order; lower is tried first. Manifest webseeds count as 100, `local_public_base_url` uses `local_priority`. |
| `upload` | `true` | Publish writes to this target. `false`: only advertise `public_base_url` (a bucket filled by replication or another process). |
| `public_base_url` | empty | Public URL serving the target's root: `<url>t/` becomes a webseed, `<url>` a catalog mirror. Empty: upload only (cold copy). |
| `bucket`, `prefix` | empty | S3 family: bucket (required) and key prefix. |
| `path_style` | per provider | `true` for everything except `aws`. |
| `access_key_env`, `secret_key_env` | `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` | The names of the environment variables holding credentials, e.g. `access_key_env = "TS_R2_KEY"`. Secrets never go in the file; a value that does not look like a variable name is rejected at load time. |
| `endpoint` | derived | Overrides the derived endpoint for any S3 provider. |

Provider fields and derived endpoints (checked against each provider's documented S3 endpoint format). Prices change; the notes are qualitative on purpose, check current terms:

| `provider` | Fields | Derived endpoint and region | Notes |
|---|---|---|---|
| `r2` | `account_id` or `account_id_env` | `https://<account>.r2.cloudflarestorage.com`, region `auto` | No egress fees. |
| `b2` | `region` (required, e.g. `us-west-004`) | `https://s3.<region>.backblazeb2.com` | Low storage price; free egress up to 3x average stored data, unlimited through partner CDNs such as Cloudflare (Bandwidth Alliance). https://www.backblaze.com/cloud-storage/pricing |
| `vultr` | `region` (location id, default `ewr1`) or `hostname` | `https://<region>.vultrobjects.com` | Flat monthly plans that bundle storage and transfer. https://docs.vultr.com/products/storage/object-storage/faq.md |
| `wasabi` | `region` (default `us-east-1`) | `https://s3.<region>.wasabisys.com` | No egress fees while monthly egress stays at or below stored volume; 90 day minimum storage duration on pay as you go. https://wasabi.com/product-terms |
| `minio` | `endpoint` (default `http://localhost:9000`) | as given, region `us-east-1` | Your own hardware; plain HTTP allowed. |
| `aws` | `region` (required) | AWS default endpoint, virtual hosted addressing | Egress is billed. |
| `s3` | `endpoint` (required), `region` (default `us-east-1`) | as given, path style unless `path_style = false` | Any other S3 compatible service (Garage, Ceph, Storj gateway, ...). |
| `local` | `dir` (required) | none | A second directory: another disk, an NFS or SMB mount. |
| `http` | `public_base_url` (required) | none | A host something else fills (rsync, a CDN pull zone); advertised, never uploaded. |
| `torrent` | none | none | A seeding root: `tensorseed seed` serves every revision under `<dir>/t/` (default `storage.local_dir`). Takes only `dir`, `enabled`, and `priority`. Publishing does not write to it; point `dir` at a copy kept by another target (for example a `local` target on a NAS). |

**Publish** writes every object to `local_dir` and then to every target with `enabled = true` and `upload = true`; a write counts only when every target accepted it, so a failed target fails the publish and a rerun retries (writes are idempotent). **Pull** builds one source list per file: manifest webseeds, `local_public_base_url`, every enabled target's `public_base_url`, `file.mirror_urls`, and the mirror lists of 6.12, sorted by `priority` (stable, deduplicated by URL), with the Hub last unless `--no-hf-fallback`. A failing source moves on to the next one; every byte is still hash checked.

Multi-provider example (the full commented version, with every block disabled so the default stays local only, is in `config/tensorseed.example.toml`):

```toml
[storage]
local_dir = "/srv/tensorseed"
local_public_base_url = "https://seed.example.org/"
local_priority = 50

[[storage.target]]                 # primary: R2, zero egress
name = "r2-primary"
provider = "r2"
account_id_env = "TS_R2_ACCOUNT_ID"
bucket = "tensorseed"
priority = 10
public_base_url = "https://models.example.org/"
access_key_env = "TS_R2_KEY"
secret_key_env = "TS_R2_SECRET"

[[storage.target]]                 # second R2 account, cold copy (no public URL)
name = "r2-backup"
provider = "r2"
account_id = "0123456789abcdef0123456789abcdef"
bucket = "tensorseed-backup"
access_key_env = "TS_R2B_KEY"
secret_key_env = "TS_R2B_SECRET"

[[storage.target]]                 # B2 in the US
name = "b2-us"
provider = "b2"
region = "us-west-004"
bucket = "tensorseed"
priority = 20
public_base_url = "https://f004.backblazeb2.com/file/tensorseed/"
access_key_env = "TS_B2_KEY_ID"
secret_key_env = "TS_B2_APP_KEY"

[[storage.target]]                 # B2 in the EU, filled by bucket replication
name = "b2-eu"
provider = "b2"
region = "eu-central-003"
bucket = "tensorseed-eu"
upload = false
priority = 30
public_base_url = "https://f003.backblazeb2.com/file/tensorseed-eu/"

[[storage.target]]
name = "vultr-sjc"
provider = "vultr"
region = "sjc1"
bucket = "tensorseed"
public_base_url = "https://tensorseed.sjc1.vultrobjects.com/"
access_key_env = "TS_VULTR_KEY"
secret_key_env = "TS_VULTR_SECRET"
```

The S3 client is `object_store` (section 12): custom endpoints, path style or virtual hosted addressing, and multipart uploads (files of 32 MiB and up go in 16 MiB parts).

---

## 11. Licensing policy

1. **Record:** every manifest stores the HF license tag, a normalized SPDX id where one exists, the license class (`open`, `attribution`, `restricted`, `unknown`), whether the repo was gated upstream, and the repo's license files (which are always mirrored).
2. **Default allowlist:** only `open` licenses are published by default (see `policy.license_allowlist`). Unknown or missing license means skip.
3. **Attribution-style licenses (Llama-style):** the Llama 3.1 Community License allows redistribution if you include a copy of the agreement, keep the required "Notice" text file, and prominently display "Built with Llama" (https://github.com/meta-llama/llama-models/blob/main/models/llama3_1/LICENSE). These are allowed only when listed in `attribution_licenses`; the publisher then (a) mirrors the license and notice files, (b) sets `license.attribution` in the manifest to the required text, and (c) `pull` writes the license and NOTICE into the output directory and prints the attribution. The catalog explorer displays "Built with Llama" for those entries.
4. **Gated repos:** "gated" is an HF access setting, not a license term. Gated repos are skipped unless `allow_gated = true` and the license class is `open` or `attribution`. Prior art (LLM Torrents) chooses never to rehost gated weights; we default to the same.
5. **Restricted / non-commercial / no-redistribution licenses:** never published by default.
5a. **Explicit overrides:** a publisher who holds the rights (typically their own repo tagged `license: other`) can accept one license id for one run with `--allow-license <id>` on `harvest` or `publish`, or for one repo with a watchlist entry `{ id = "org/name", allow_license = ["other"] }`. Overrides lift only the license refusal: gated, private, disabled, takedown, and tombstone rules still apply, the default allowlist is unchanged, and the manifest keeps the honest class (an overridden `other` is recorded as `unknown`, never `open`). On `pull` the flag only silences the "would not be published" warning, since personal downloads are never refused for license.
6. **Takedown process:** a published contact address in `CatalogInfo.publisher.contact` and a policy URL. On a valid request: write a `Tombstone` (one generation), delete webseed objects on every storage target, stop seeding on publisher nodes. `harvest` never fetches repos on its takedown list (`harvest.takedown`, `harvest.takedown_file`) or tombstoned in the catalog. Seed daemons honor tombstones by default and stop seeding. Limits are stated publicly: past DataLayer history and existing swarm copies cannot be recalled.

---

## 12. Rust crates

| Need | Crate | Notes |
|---|---|---|
| Async runtime | `tokio` | multi-thread |
| HTTP + DataLayer RPC | `reqwest` with `rustls-tls` | client cert (mTLS) for port 8562 |
| HF Hub | plain `reqwest` (decided) | `hf-hub` 1.0 was rejected: it pulls `hf-xet` (over 550 crates) and is built around the HF cache layout; we need raw tree hashes, our own layout, streaming verification, and resumable jobs. Xet files still download through resolve URLs. |
| Serialization | `serde`, `serde_json` | canonical JSON helper (sorted keys) |
| Hashing / hex | `sha2`, `hex` | streaming SHA-256 |
| Signatures | `ed25519-dalek` 2.x, `blake2`, `base64` | minisign compatible format (6.9); BSD-3-Clause and MIT/Apache-2.0, pass `deny.toml` |
| CLI / config | `clap` (derive), `figment` + `toml` | |
| Errors / logs | `thiserror`, `anyhow`, `tracing`, `tracing-subscriber` | |
| State | `rusqlite` with bundled SQLite (decided) | synchronous, small, no async pool needed for a CLI; jobs, files, and harvest records in `general.state_db` |
| S3 compatible upload (any provider) | `object_store` 0.14, `aws` feature (decided) | Apache Arrow project, MIT/Apache-2.0; custom endpoints, path style, multipart; shares `reqwest` 0.13. `aws-sdk-s3` rejected as much heavier; `rust-s3` pulls an older `reqwest` (0.12). |
| Key encryption / prompts | `scrypt`, `getrandom`, `rpassword` | minisign compatible encrypted secret keys (libsodium scrypt parameters) |
| Test HTTP mocks | `wiremock` (dev only) | every network test runs against local mock servers |
| BitTorrent engine | `librqbit` 9.1 (decided in M0), feature `engine` of `tensorseed-torrent` | Apache-2.0, passes `deny.toml`, about 110 extra crates. Seeds and downloads the v1 side of hybrid torrents; no v2 only torrents, `btmh` only magnets, or webseeds (spike results). One advisory ignored with a reason (RUSTSEC-2026-0293, `ringbuf` via `librqbit-utp`, unreachable for `u8`). Retest when https://github.com/ikatson/rqbit/issues/546 closes. |
| Torrent creation and parsing | our own (`tensorseed-torrent`: `bencode`, `merkle`, `metainfo`) | BEP 52 v2 and hybrid, deterministic, byte identical to libtorrent 2.x; only `sha1`, `sha2`, `hex`. |
| BitTorrent fallback engine | libtorrent-rasterbar 2.x (BSD-3-Clause) via a sidecar or bindings (`libtorrent-sys`, `lt-rs`, MIT) | Not used now: a native C++ and Boost dependency breaks the single static binary. Revisit if v2 only swarms or `btmh` only magnets matter. |
| Chia (optional, direct chain work) | `chia-wallet-sdk` / `chia-sdk-driver` with `chip-0035` | For delegated writers or reading the store without a full Python node. M1 uses the official DataLayer RPC instead. |

There is no official Rust DataLayer daemon; M1 talks to the Python `chia data` service over RPC.

### 12.1 Workspace layout

| Crate | Role |
|---|---|
| `tensorseed-core` | Manifest and catalog JSON types, key builders (`ts/v1/...`), canonical JSON, streaming SHA-256, shared errors. No network code. |
| `tensorseed-hf` | Hugging Face API client (repo info, paginated tree, listings, resolve URLs, backoff) and the license and gated policy. |
| `tensorseed-fetch` | Verified, resumable, parallel HTTP downloads with source fallback and SQLite job state. |
| `tensorseed-catalog` | `CatalogBackend` trait and backends. `minisign` (signature format and key files), `signed` (`SignedEntry`), `chia` (DataLayer RPC over mTLS with `reqwest` + rustls), `file` (signed static files, hash chain, HTTPS sync), `sink` (local directory and S3 targets), `export` (bundles). |
| `tensorseed-torrent` | BEP 52 v2 and hybrid torrent creation and parsing in pure Rust (BEP 19 `url-list`, magnets, Merkle verification), and the librqbit swarm engine (seed in place, download with peers). |
| `tensorseed-serve` | Building blocks of `serve`: BEP 19 HTTP file serving with ranges and the catalog mirror route, the health document and endpoint, NAT-PMP and UPnP IGD port mapping, selection rules, a shared rate limiter. |
| `tensorseed-cli` | The `tensorseed` binary: clap commands, figment config, tracing setup, pull, verify, harvest, seed, and the `serve` loop. |

MSRV is Rust 1.87 (needed by a patched dependency for RUSTSEC-2026-0385).

---

## 13. Harvest mode

`tensorseed harvest` mirrors repos from the Hub automatically while it is available. One pass by default; `--watch` repeats every `harvest.poll_interval_secs` (daemon mode); `--dry-run` prints the plan without downloading; `--only <ids>` limits a run; `--status` lists finished and failed revisions.

**Watchlist** (`[harvest]`): explicit `repos` (models) and `datasets` (with `include_datasets`), plus discovery rules: `authors` (users or orgs, listed by downloads, at most `max_per_author` each), `tags` (all required), and `min_downloads`. Explicit repos skip the discovery rules, but every candidate passes the same gates.

**Per candidate, in order:**

1. Takedown list (`takedown`, `takedown_file`): never fetched.
2. Repo info at the default branch gives the current commit. Gated repos are skipped unless `allow_gated` (and a token with accepted terms); private or disabled repos are skipped.
3. Already harvested (state DB says `done` for this commit): nothing to do. This makes passes idempotent and restart safe.
4. Tombstoned in the catalog: skipped.
5. License: the policy (section 11) with `harvest.license_allowlist` (or `[policy] license_allowlist` when empty), plus per-repo `allow_license` and `--allow-license` overrides (11.5a); refusals skip the repo. Pickle formats are excluded unless `[policy] allow_pickle_formats`.
6. Size cap (`max_repo_bytes`) and disk budget (`disk_budget_bytes`, counting finished harvests).
7. Download through `tensorseed-fetch` into `<storage.local_dir>/t/<content name>/` with hash verification, HTTP resume, and job records; files stay there for seeding.
8. Build the manifest (SHA-256 for every file, `hf_lfs_oid` for LFS files, license, file pages above 2000 files, `torrent: null`, `webseeds` from storage), upload files to every enabled `[[storage.target]]` with `upload = true` first (when `harvest.upload = true`), then stage and commit `rev/<commit>` (plus pages), `latest`, and `index` as one catalog revision (file backend, or Chia), and write the signed manifest to `manifests/<type>/<org>/<name>/<commit>.json` in storage. With `publish = false`, an unsigned manifest is written there instead and the catalog is untouched.
9. Record the outcome (`done` with bytes, or `failed` with the error and attempt count) in `harvest_revisions` in the state DB. Failed revisions are retried on the next pass.

**Politeness:** Hub API calls are spaced by `request_delay_ms`; 429 and 5xx responses back off (`max_retries`, `retry_base_ms`, `Retry-After` honored); repos are processed one at a time with `general.max_concurrent_downloads` files in flight. `HF_TOKEN` is optional.

**Not yet:** seeding of harvested revisions waits for the torrent layer (M0 spike), and watching specific branches other than the default is not supported. All harvest tests use local mock servers.

---

## 14. Cost notes (list prices as of 2026-11-12; recheck before committing)

| Item | Figure | Source |
|---|---|---|
| Cloudflare R2 Standard storage | $0.015 per GB-month, first 10 GB-month free | https://developers.cloudflare.com/r2/pricing/ |
| R2 egress | $0 | same |
| R2 Class A / Class B operations | $4.50 / $0.36 per million | same |
| Cloudflare Project Alexandria | credits for qualifying open source projects | https://blog.cloudflare.com/expanding-our-support-for-oss-projects-with-project-alexandria/ |
| Backblaze B2 | $6.95 per TB-month; egress free up to 3x stored, then $0.01/GB; unlimited free egress through partner CDNs | https://www.backblaze.com/cloud-storage/pricing |
| Wasabi | no egress fees while monthly egress is at most stored volume; 90 day minimum storage duration (pay as you go) | https://wasabi.com/product-terms |
| Sia (renterd) | median about $1 per TB-month storage (market price, varies) | https://sia.tech/provide-storage |
| DataLayer fee | config default 0.001 XCH per on-chain update if no fee is set | https://docs.chia.net/datalayer-rpc/ |
| DataLayer mirror coin | `amount` minimum 0, refunded on delete | https://docs.chia.net/guides/datalayer-user-guide/ |
| Chia transaction block interval | about 52 s average | https://docs.chia.net/chia-blockchain/coin-set-model/costs/ |

Implications:
- One publish is one catalog generation, so chain cost is at most one fee per published revision (or per `sync` run if several repos are batched into one pending root; decision below).
- Object storage is optional (local disk costs nothing extra). If used, it dominates cost; for example on R2, 1 TB (1,000 GB) is 990 billable GB x $0.015 = $14.85 per month with zero egress, matching Cloudflare's own worked example on the pricing page. Other providers trade storage price against egress terms (10.1).
- Rejected design for comparison: putting 1 TiB inside DataLayer needs at least 4,195 generations under the 250 MiB per-generation subscriber default (1 TiB / 250 MiB), with each subscriber storing roughly 3 copies (inferred from `chia/data_layer/data_store.py` and `download_data.py`; not measured).

---

## 15. Prior art

- **The Hugging Bay**: community index of magnet links for openly licensed models with per-file SHA-256 manifests, webseeds, and a catalog distributed as a torrent (https://thehuggingbay.io/llms.txt).
- **LLM Torrents**: 119 models cataloged, SHA-256 checked against pinned HF revisions, signed manifests, HTTPS webseeds, no gated rehosting (https://llmtorrents.com/).
- **Academic Torrents LLM Weights collection** (https://academictorrents.com/collection/llm-weights).
- **DIG Network**: Chia-anchored Layer 2 that plans to pay mirrors for proven availability; whitepaper (May 2026) says "late prototype, pre-public-testnet" (https://dig.net/whitepapers/DIG-Network-Part1-Consensus.pdf). A possible future incentive layer, not usable today.

What this project adds: a Chia-anchored, versioned, publicly mirrorable catalog with Merkle proofs, so the index itself has no single point of failure and its history is auditable, plus a chain-free signed-file copy of the same signed entries. Interoperating with the two indexes above (for example, exporting our manifests in their format) is an open question.

---

## 16. Milestones

**M0: Spikes (scaffold only, no product logic)**
- Verify librqbit: v2 download, v2 seeding, v2 and hybrid torrent creation, BEP 19 webseeds against an HTTP host with range requests. If any fail, prototype the fallback (section 12). Done: see [M0-TORRENT-SPIKE.md](M0-TORRENT-SPIKE.md); own pure Rust builder plus librqbit on hybrid torrents.
- DataLayer RPC from Rust over mTLS: `create_data_store` on testnet, `batch_update(submit_on_chain=false)`, `submit_pending_root`, confirmation polling, `get_value`, `subscribe` and `get_sync_status` from a second node, `add_mirror`.
- Confirm HF API field names (tree `lfs.oid`, `gated`, license tag). Done (section 5); `hf-hub` dropped.
- Pick sqlite crate and S3 crate (done: `rusqlite`, `object_store`). Pick default `piece_length` (done: automatic, 7.1).
- Check the `CatalogBackend` trait (section 6.0) against the real DataLayer call sequence and adjust it before M1 depends on it.
- Signed-file backend core (done): minisign compatible signing, `SignedEntry`, hash chain with rollback, fork, gap, and stale head checks, export bundles. Encrypted key files, `catalog keygen`, and trust anchor persistence are done too. The DataLayer calls are implemented and tested against mocked responses; the testnet run is still open.

**M1: Publish**
- `doctor`, `publish`, `sync`. Storage upload of torrents is done in `harvest`; atomic catalog writes through one queue. Testnet first, then mainnet. (`catalog create/info/add-mirror`, `jobs`, resumable state, and S3 upload are done.)
- File backend: snapshot torrent (HTTPS sync, publishing to storage, head re-signing, and export/import are done).

**M2: Pull**
- Done over HTTP: `pull`, `verify`, `list`, `show`, tombstones, NOTICE, `--manifest-file`/`--manifest-url`/`--from-hf`. Torrent fetching (`--swarm`) and `--seed-after` are done too. Remaining: HTTP resume of files the swarm left partial.

**M3: Seeding daemon**
- `seed` with selection rules, disk budget, tombstone honoring, optional `--serve-catalog` and catalog torrent. (Seeding published trees and pulled directories in place, upload caps, and DHT, trackers, and LSD are done.)

**M4: Catalog explorer**
- Static site at tensorseed.dev (generated from the catalog) listing repos, licenses, sizes, magnets, webseeds, attribution text, and catalog generation history. Hosted on any static host; it has no privileged role.

---

## 17. Open questions

1. Which models and datasets first, and how many TB in year one (for example, widely used models in GGUF Q4 / MLX 4-bit / NVFP4 variants)?
2. Do you already run a synced Chia full node and wallet? What XCH budget for fees and mirror coins?
3. Hosting hardware, uplink speed, and whether you can open port 8575 (DataLayer) and a torrent port.
4. ~~R2 as primary webseed?~~ Resolved: storage is optional and provider neutral (10.1). Still open: which provider(s), if any, for the public webseed, and whether to apply for Cloudflare Project Alexandria credits.
5. Single owner key, or CHIP-0035 delegated writers for co-maintainers?
6. License policy: open licenses only at launch, or also Llama-style attribution licenses?
7. Build standalone, or build on / interoperate with The Hugging Bay or LLM Torrents?
8. ~~Final project name.~~ Resolved: Tensorseed, primary domain tensorseed.dev.
9. ~~Second, chain-free catalog backend?~~ Resolved: yes, signed static files (6.8) next to Chia.
10. Publisher key rotation and revocation: a signed `ts/v1/keys` entry with successor keys, a threshold of maintainer keys, or both? Where does the offline key live?
11. Should both backends run in parallel from launch (Chia canonical, file as a live mirror), or should the file backend start as export-only?
12. Accept ssh-sig as a second signature type later (6.9), or stay minisign only?
13. Head expiry (`file.head_ttl_secs`, default 7 days): shorter is safer against freeze attacks but needs a reliable re-signing job.
14. Harvest watchlist for launch: which authors, tags, download floor, and disk budget?
