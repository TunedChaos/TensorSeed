# Tensorseed milestones

Derived from [SPEC.md](SPEC.md) section 16. Current milestone: **M0** (the Chia
testnet run, the BitTorrent spike, and the Chia mainnet anchor are done); much of
the HTTP side of M1 and M2 is already done.

## Done

- [x] Project named Tensorseed; primary site tensorseed.dev. tensorseed.org and tensorseed.garden are reserved as redirects to tensorseed.dev
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
- [x] Chia DataLayer calls: `create_data_store`, `get_value` with envelope verification, paginated `get_keys`, `batch_update` with `submit_on_chain=false`, `submit_pending_root`, confirmation polling, `subscribe`, `get_sync_status`, `add_mirror`, `get_root_history`
- [x] Chia testnet11 run (spec 6.13, [CHIA-RUNBOOK.md](CHIA-RUNBOOK.md)): live catalog imported with signatures intact, read back and verified from a second subscriber-only node, `publish` through the Chia backend; `catalog create` waits for the store, `catalog subscribe`, no-op re-imports, resume after an interrupted submit, rollback and fork anchor for Chia reads
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
- [x] Run the implemented calls against a real node and confirm response fields (`get_root`, `get_keys` paging, missing key errors)
- [x] `subscribe` and `get_sync_status` from a second node
- [x] Check the `CatalogBackend` trait against the real call sequence and adjust it
These were completed in the testnet11 run listed under Done.

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
- [x] Testnet end to end, then mainnet (official store anchored and checked strictly by default)

## M2: Pull
- [x] Torrent fetching alongside webseeds (`--swarm`, then HTTP fills gaps)
- [x] `--seed-after`
- [ ] Resume HTTP at the first missing piece of a file the swarm left partial (today it is fetched again in full)

## M3: Seeding daemon
- [x] `seed` for harvested revisions on local disk, torrent targets, and pulled directories
- [x] `serve`: catalog driven loop with selection rules (globs, excludes, watchlist, types, license classes, latest only)
- [x] Upload and download caps (`torrent.upload_limit_kbps`, `serve.upload_limit_kbps`, `serve.download_limit_kbps`, `serve.http_upload_limit_kbps`)
- [x] Disk budget (`serve.max_bytes`, `serve.max_revision_bytes`)
- [x] Honor tombstones (stop at the next sync, prune downloaded copies)
- [x] Auto fetch of newly selected revisions through the pull pipeline (`serve.fetch`)
- [x] uTP (librqbit 9.1, on by default in `serve`) and opt in NAT-PMP / UPnP IGD port mapping (`serve.port_mapping`)
- [x] HTTP webseed serving with ranges, `--serve-catalog`, health endpoint, state file resume, systemd unit
- [x] Throughput between two real hosts over SSH port forwards (about 11.8 MB/s server to home, 3 MB/s home upstream; SPEC 8.3)
- [ ] Direct connection between two real hosts with an open or mapped port (both test hosts were firewalled)
- [ ] Free disk space check before fetching (today only the byte budget applies)
- [ ] Catalog `.dat` torrent (Chia) and snapshot torrent seeding (file)

## M4: Catalog explorer
- [ ] Static site at tensorseed.dev generated from the catalog
- [ ] Lists repos, licenses, sizes, magnets, webseeds, attribution text
- [ ] Catalog generation history
- [ ] Hosted on any static host, with no privileged role

## 0.2.0

The design is in [SPEC.md](SPEC.md) section 18. Independent catalogs are
already possible in 0.1.0 (see [PUBLISH-A-CATALOG.md](PUBLISH-A-CATALOG.md));
0.2.0 is following several at once, plus the items below. Hugging Face
models and datasets only. The official seeder and the first additional
official catalog entries are already live (catalog revision 4, nine
entries). 0.2.0 ships with all of the items below; the explorer is live at
explore.tensorseed.dev.

- [x] Multi-catalog subscription: `[[catalogs]]` (name, publisher key, store
      id, base URL, chain check, optional label and priority); a single
      `[catalog]` block still works; each catalog is verified and stored
      independently
- [x] `list` / `pull` / `serve` dedupe by HF repo, commit, and manifest
      hashes (agreeing catalogs share one torrent swarm); show
      "N of M catalogs agree"; same commit with different hashes is a
      hard error unless `--catalog` is given
- [x] Per-catalog takedowns; `tensorseed catalog add` / `remove` / `list`
- [x] Read-only catalog explorer at explore.tensorseed.dev (SPEC 8.4 and
      18.2): Svelte 5 + TailwindCSS + DaisyUI (build-time, self-hosted,
      pnpm); wasm verification core from the existing Rust crates; no
      third party scripts; "use another catalog" form; agreement UI
- [x] `pull --include` / `--exclude` globs, using librqbit `only_files`;
      `verify` accepts a recorded partial pull; `serve` and `--seed-after`
      seed the selected subset with `only_files`
- [x] Default download order is torrent, then Hugging Face, then HTTP
      mirrors, with a `[fetch] source_order` setting so operators can
      choose a different order
- [x] Reuse an existing `hf auth login` token automatically, and
      `tensorseed hf login`, `logout`, and `status` (status confirms
      the token works without printing it)
- [x] .deb and .rpm for x86_64 and aarch64, built with nfpm from the
      static musl binaries; systemd unit disabled by default; example
      config under `/etc/tensorseed/`; packages listed in SHA256SUMS
      when `just dist` runs
- [x] Shell completions (bash, zsh, fish, PowerShell) via
      `tensorseed completions`, plus man pages; packages install fish
      completions under `/usr/share/fish/vendor_completions.d` and man
      pages under `/usr/share/man/man1`; release archives include both
- [x] Always-on official seeder and the first additional official catalog
      entries (catalog revision 4; details in SPEC 18.5)
- [ ] Optional follow-ups, not required to ship 0.2.0: an AUR PKGBUILD;
      later, project apt/dnf repositories. Flatpak and AppImage stay
      out of scope for the CLI.

## 0.3.0: Tensorseed Desktop

A separate product from the CLI (see [SPEC.md](SPEC.md) section 19).
The CLI stays a static, GUI-free binary. Work starts after the 0.2.0
explorer UI exists so Desktop can reuse it.

- [ ] Tauri desktop app with a system tray, reusing the explorer's
      Svelte 5 + TailwindCSS + DaisyUI UI and calling the Rust crates
      natively (seeding status, disk use, pause/resume, catalog views)
- [ ] Desktop bundles or depends on the CLI, and shares config, data
      directories, and `serve` daemon state; no second layout for the GUI
- [ ] `tensorseed gui` launcher in the CLI: opens Desktop if it is
      installed, otherwise prints a download link (does not link a GUI)
- [ ] Desktop packaging: Windows installer (MSI or NSIS); Linux .deb,
      .rpm, AppImage, and Flatpak. CLI packages remain GUI-free.
