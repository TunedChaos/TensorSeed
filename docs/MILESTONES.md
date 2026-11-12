# Tensorseed milestones

Derived from [SPEC.md](SPEC.md) section 16. Current milestone: **M0** (the
BitTorrent spike and a Chia testnet run are the open items); much of the HTTP
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

## M0: Spikes

### BitTorrent (librqbit)
- [ ] BEP 52 v2 download works
- [ ] v2 seeding works
- [ ] v2 torrent creation works
- [ ] Hybrid v1+v2 torrent creation works
- [ ] BEP 19 webseeds (`url-list`) work against an HTTP host with range requests
- [ ] If any check fails: prototype the fallback (libtorrent-rasterbar 2.x via FFI or sidecar, or own BEP 52 builder)

### Chia DataLayer on testnet
- [ ] Run the implemented calls against a real node and confirm response fields (`get_root`, `get_keys` paging, missing key errors)
- [ ] `subscribe` and `get_sync_status` from a second node
- [ ] Check the `CatalogBackend` trait against the real call sequence and adjust it

### Signed-file catalog
- [ ] Confirm value size overhead of the `SignedEntry` envelope against the 1 MiB target (spec 6.5)

### Decisions
- [ ] Pick default `piece_length`
- [ ] Hybrid torrents on or off by default

## M1: Publish
- [ ] `doctor`
- [ ] `publish` state machine with torrents (spec section 8.1)
- [ ] `sync`
- [ ] Torrent creation and `.torrent` upload to storage
- [ ] File backend: catalog snapshot torrent
- [ ] Atomic catalog writes through a single write queue
- [ ] `takedown` (tombstone plus webseed deletion on every storage target)
- [ ] `torrent` storage targets and torrent mirrors (needs the BitTorrent spike)
- [ ] Testnet end to end, then mainnet

## M2: Pull
- [ ] Torrent fetching alongside webseeds
- [ ] `--seed-after`

## M3: Seeding daemon
- [ ] `seed` with selection rules (including seeding harvested revisions from local disk)
- [ ] Disk budget and bandwidth caps
- [ ] Honor tombstones
- [ ] Optional `--serve-catalog`
- [ ] Catalog `.dat` torrent (Chia) and snapshot torrent seeding (file)

## M4: Catalog explorer
- [ ] Static site at tensorseed.dev generated from the catalog
- [ ] Lists repos, licenses, sizes, magnets, webseeds, attribution text
- [ ] Catalog generation history
- [ ] Hosted on any static host, with no privileged role
