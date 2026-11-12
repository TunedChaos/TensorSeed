# Tensorseed milestones

Derived from [SPEC.md](SPEC.md) section 15. Current milestone: **M0**.

## Done

- [x] Project named Tensorseed; domains tensorseed.dev (primary), tensorseed.org, tensorseed.garden
- [x] Rust workspace scaffold: core types, `CatalogBackend` trait, CLI skeleton, config loading, CI
- [x] Decision: keep Chia DataLayer as the first backend and add a chain-free signed-file backend (spec 6.8)
- [x] Minisign compatible signing and verification (checked both ways against the `minisign` tool)
- [x] `SignedEntry` envelope: every entry signed by the publisher key, bound to catalog name and key
- [x] Signed-file backend: hash chain, rollback, fork, gap, and stale head detection, with unit tests
- [x] Export bundle (`ts.export/1`) build and verify; CLI `catalog export` / `catalog import` stubs (`import --dry-run` works)

## M0: Spikes (scaffold only, no product logic)

### BitTorrent (librqbit)
- [ ] BEP 52 v2 download works
- [ ] v2 seeding works
- [ ] v2 torrent creation works
- [ ] Hybrid v1+v2 torrent creation works
- [ ] BEP 19 webseeds (`url-list`) work against an R2 bucket with HTTP range requests
- [ ] If any check fails: prototype the fallback (libtorrent-rasterbar 2.x via FFI or sidecar, or own BEP 52 builder)

### Chia DataLayer RPC from Rust over mTLS (testnet)
- [ ] `create_data_store`
- [ ] `batch_update` with `submit_on_chain=false`
- [ ] `submit_pending_root`
- [ ] Confirmation polling (confirm exact `get_root` response fields)
- [ ] `get_value` and `get_keys` (confirm behavior for missing keys)
- [ ] `subscribe` and `get_sync_status` from a second node
- [ ] `add_mirror`
- [ ] Check the `CatalogBackend` trait against the real call sequence and adjust it

### Signed-file catalog (spec 6.8 to 6.10)
- [ ] Load minisign encrypted secret key files (scrypt) and add `catalog keygen`
- [ ] Persist the trust anchor `(revision, root)` in `file.state_dir`
- [ ] Confirm value size overhead of the `SignedEntry` envelope against the 1 MiB target (spec 6.5)

### Hugging Face
- [ ] Confirm API field names: tree `lfs.oid`, `gated`, `cardData.license`
- [ ] Confirm `hf-hub` Xet downloads by commit SHA

### Decisions
- [ ] Pick sqlite crate (`rusqlite` or `sqlx`)
- [ ] Pick S3 crate (`aws-sdk-s3` or `object_store`)
- [ ] Pick default `piece_length`
- [ ] Hybrid torrents on or off by default

## M1: Publish
- [ ] `doctor`
- [ ] `catalog create`, `catalog info`, `catalog add-mirror`
- [ ] Chia backend stores `SignedEntry` envelopes and verifies them on read
- [ ] File backend: HTTPS fetch of head, deltas, snapshots with mirror fallback
- [ ] File backend: upload to R2 (`catalog.tensorseed.dev`), snapshot every N revisions, head re-signing before expiry
- [ ] File backend: catalog snapshot torrent
- [ ] `catalog export` and `catalog import` end to end (Chia to file and file to Chia)
- [ ] `publish` state machine (spec section 8.1)
- [ ] `sync`
- [ ] `jobs`
- [ ] Resumable state in sqlite
- [ ] R2 multipart upload
- [ ] Torrent creation
- [ ] Atomic catalog writes through a single write queue
- [ ] Testnet end to end, then mainnet

## M2: Pull
- [ ] `pull` state machine (spec section 8.2)
- [ ] `verify`, `list`, `show`
- [ ] Tombstone handling
- [ ] License and NOTICE output
- [ ] `--manifest-file` and `--manifest-url` modes

## M3: Seeding daemon
- [ ] `seed` with selection rules
- [ ] Disk budget and bandwidth caps
- [ ] Honor tombstones
- [ ] Optional `--serve-catalog`
- [ ] Catalog `.dat` torrent (Chia) and snapshot torrent seeding (file)

## M4: Catalog explorer
- [ ] Static site at tensorseed.dev generated from the catalog
- [ ] Lists repos, licenses, sizes, magnets, webseeds, attribution text
- [ ] Catalog generation history
- [ ] Hosted on R2 or any static host, with no privileged role
