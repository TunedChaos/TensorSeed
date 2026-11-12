# Tensorseed milestones

Derived from [SPEC.md](SPEC.md) section 16. Current milestone: **M0** (a Chia
testnet run is the open item; the BitTorrent spike is done); much of the HTTP
side of M1 and M2 is already done.

## Done

- [x] Project named Tensorseed; domains tensorseed.dev (primary), tensorseed.org, tensorseed.garden
- [x] Rust workspace scaffold: core types, `CatalogBackend` trait, CLI skeleton, config loading, CI
- [x] Decision: keep Chia DataLayer as the first backend and add a chain-free signed-file backend (spec 6.8)
- [x] Minisign compatible signing and verification (checked both ways against the `minisign` tool)
- [x] `SignedEntry` envelope: every entry signed by the publisher key, bound to catalog name and key
- [x] Signed-file backend: hash chain, rollback, fork, gap, and stale head detection, with unit tests
- [x] Export bundle (`ts.export/1`) build and verify
- [x] Hugging Face API fields confirmed against live responses (spec 5); `hf-hub` dropped for plain `reqwest`
- [x] Decisions: `rusqlite` for state, `object_store` for S3 compatible storage
- [x] Minisign encrypted secret keys (scrypt) and `catalog keygen`; keys made by Tensorseed sign with the `minisign` tool and vice versa
- [x] File backend transport: HTTPS sync with mirror fallback, persisted trust anchor, publishing (delta, snapshot, head), head re-signing, on demand snapshots
- [x] Storage: local disk by default; optional named, repeatable `[[storage.target]]` blocks (`r2`, `b2`, `vultr`, `wasabi`, `minio`, `aws`, `s3`, `http`, `local`) with derived endpoints, per target `enabled`, `priority`, `upload`, `public_base_url`, credentials from named env vars only; publish writes to every enabled target, multipart uploads (spec 10.1)
- [x] Mirror lists: local `mirrors.toml` and publisher signed `<catalog>/mirrors.toml` (`[[mirror]]` blocks), merged by priority for catalog reads and pull webseeds; `mirror add|remove|list|publish` (spec 6.12)
- [x] Chia DataLayer calls: `create_data_store`, `get_value` with envelope verification, paginated `get_keys`, `batch_update` with `submit_on_chain=false`, `submit_pending_root`, confirmation polling, `subscribe`, `get_sync_status`, `add_mirror` (mocked RPC tests only)
- [x] `catalog create`, `info`, `add-mirror`, `export`, `import` (full), `resign-head`, `snapshot`
- [x] `tensorseed-fetch`: verified, resumable, parallel downloads with webseed then Hub fallback, SQLite job state, atomic rename
- [x] `pull` (catalog, `--manifest-file`, `--manifest-url`, `--from-hf`), `verify` (directory, catalog, manifest file, `--hf`), `list`, `show`, `jobs`
- [x] Tombstone handling, pickle formats skipped by default, gated repos refused by default, NOTICE output
- [x] `harvest` one shot and `--watch` daemon mode with watchlist rules, takedown list, size cap, disk budget, Hub backoff, idempotent SQLite state (spec 13)
- [x] End to end CLI tests against local mock servers; real smoke test pulling `hf-internal-testing/tiny-random-gpt2`
- [x] M0 BitTorrent spike: librqbit 9.1.0 and libtorrent 2.0.11 compared on loopback ([M0-TORRENT-SPIKE.md](M0-TORRENT-SPIKE.md))
- [x] Pure Rust BEP 52 v2 and hybrid torrent builder and parser, info hashes identical to libtorrent; Merkle file verification; hybrid magnets
- [x] librqbit swarm engine: seed in place (refuses missing or corrupt files), download with peers and file subsets
- [x] `harvest` writes `t/<name>.torrent`, per file `bt_v2_pieces_root`, the manifest `torrent` block, and uploads the torrent to every storage target
- [x] `seed` (published trees, `provider = "torrent"` targets, pulled directories), `pull --swarm`, `--peer`, `--seed-after`

## M0: Spikes

### BitTorrent (done, see [M0-TORRENT-SPIKE.md](M0-TORRENT-SPIKE.md))
- [x] BEP 52 v2 download works: librqbit no (v2 only torrents fail to load), libtorrent yes
- [x] v2 seeding works: librqbit no, libtorrent yes; librqbit seeds the v1 side of hybrids and interoperates with libtorrent
- [x] v2 torrent creation works: librqbit no; Tensorseed's own pure Rust builder yes (matches libtorrent)
- [x] Hybrid v1+v2 torrent creation works: own builder yes
- [x] BEP 19 webseeds (`url-list`): librqbit no (never requests them); libtorrent yes; Tensorseed clients use `tensorseed-fetch`
- [x] Fallback chosen: own BEP 52 builder plus librqbit on hybrid torrents; libtorrent-rasterbar kept as a later sidecar option

### Chia DataLayer on testnet
- [ ] Run the implemented calls against a real node and confirm response fields (`get_root`, `get_keys` paging, missing key errors)
- [ ] `subscribe` and `get_sync_status` from a second node
- [ ] Check the `CatalogBackend` trait against the real call sequence and adjust it

### Signed-file catalog
- [ ] Confirm value size overhead of the `SignedEntry` envelope against the 1 MiB target (spec 6.5)

### Decisions
- [x] Pick default `piece_length`: auto, 4 MiB doubled up to 64 MiB to keep at most 16,384 pieces
- [x] Hybrid torrents on or off by default: on (the built-in engine needs them)

## M1: Publish
- [ ] `doctor`
- [ ] `publish` state machine with torrents (spec section 8.1)
- [ ] `sync`
- [x] Torrent creation and `.torrent` upload to storage (in `harvest`; `publish` still to come)
- [ ] File backend: catalog snapshot torrent
- [ ] Atomic catalog writes through a single write queue
- [ ] `takedown` (tombstone plus webseed deletion on every storage target)
- [x] `torrent` storage targets (seeding roots)
- [ ] Torrent mirrors in `mirrors.toml` (catalog snapshot torrents)
- [ ] Testnet end to end, then mainnet

## M2: Pull
- [x] Torrent fetching alongside webseeds (`--swarm`, then HTTP fills gaps)
- [x] `--seed-after`
- [ ] Resume HTTP at the first missing piece of a file the swarm left partial (today it is fetched again in full)

## M3: Seeding daemon
- [x] `seed` for harvested revisions on local disk, torrent targets, and pulled directories
- [ ] Selection rules and the catalog driven loop
- [x] Upload cap (`torrent.upload_limit_kbps`)
- [ ] Disk budget
- [ ] Measure throughput between two real hosts; uTP and UPnP
- [ ] Honor tombstones
- [ ] Optional `--serve-catalog`
- [ ] Catalog `.dat` torrent (Chia) and snapshot torrent seeding (file)

## M4: Catalog explorer
- [ ] Static site at tensorseed.dev generated from the catalog
- [ ] Lists repos, licenses, sizes, magnets, webseeds, attribution text
- [ ] Catalog generation history
- [ ] Hosted on any static host, with no privileged role
