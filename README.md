# Tensorseed

**Verifiable, chain anchored mirrors of open Hugging Face models, shared over BitTorrent v2.**

Website: [tensorseed.dev](https://tensorseed.dev)

> **Status: pre-alpha, milestone M0.** Downloading and verifying works today
> (`pull`, `verify`), as do automatic mirroring from the Hub (`harvest`), the
> signed-file catalog commands, and BitTorrent: harvested revisions get hybrid
> v1+v2 torrents, `seed` serves them, `serve` keeps seeding whatever the
> catalog says, and `pull --swarm` / `--seed-after` use the swarm. The
> official catalog is anchored on Chia mainnet, and clients check the file
> catalog against the Chia store by default (strict). `takedown` and `doctor`
> still print "not implemented yet". Expect breaking changes.

**Want to help keep models available?** Run a mirror or seeder: see
[docs/RUN-A-MIRROR.md](docs/RUN-A-MIRROR.md).

## What it is

Tensorseed keeps open models and datasets from Hugging Face available even if the
original host goes away, and lets anyone check that a copy is genuine.

- **Signed, versioned catalog.** A small catalog lists each mirrored repo revision:
  every file, its size, its SHA-256, the license, and the Hugging Face commit it came
  from. Only the catalog owner can update it, every version is kept, and anyone can
  subscribe and re-serve it. The catalog sits behind a pluggable `CatalogBackend`
  trait with two backends: **Chia DataLayer** (first; only the Merkle root goes on
  chain) and a chain-free **signed static-file catalog** served over HTTPS and as a
  torrent. Every entry is signed with the same publisher key in both, so the catalog
  can be exported from one backend and imported into another with signatures intact.
- **Bytes over BitTorrent v2.** Model files travel as hybrid v1+v2 (BEP 52)
  torrents that Tensorseed builds itself in pure Rust, plus optional BEP 19 HTTP
  webseeds so downloads work even with zero peers. The built-in swarm engine is
  librqbit; see [docs/M0-TORRENT-SPIKE.md](docs/M0-TORRENT-SPIKE.md) for why.
  Storage is local disk by default; add any number of named
  `[[storage.target]]` blocks (R2, B2, Vultr, Wasabi, MinIO, AWS, any S3, another
  directory, or a plain HTTP host), and mirrors in `mirrors.toml`. Pull tries
  them by priority and falls back to the Hub.
- **Trust the hashes, not the host.** Any peer, webseed, or mirror can serve bytes,
  because the client checks them against the catalog before accepting them.

Read the full design in [docs/SPEC.md](docs/SPEC.md), the plan in
[docs/MILESTONES.md](docs/MILESTONES.md), and the volunteer guide in
[docs/RUN-A-MIRROR.md](docs/RUN-A-MIRROR.md). API docs: `cargo doc
--workspace --no-deps --open`.

## Use it

```sh
# Download a repo straight from the Hub, verifying every file against the Hub's
# own SHA-256 (LFS) and git blob ids, then check it again later:
tensorseed pull --from-hf hf-internal-testing/tiny-random-gpt2
tensorseed verify hf-internal-testing/tiny-random-gpt2          # uses .tensorseed/pull.json
tensorseed verify --hf hf-internal-testing/tiny-random-gpt2     # re-checks against the Hub

# With a signed catalog configured (catalog.publisher_pubkey, file.base_url):
tensorseed list
tensorseed pull org/name            # webseeds first, the Hub as fallback
tensorseed verify org/name          # against the signed manifest; exit 3 on mismatch
tensorseed pull org/name --swarm --seed-after 2h   # peers first, HTTP fills gaps, then seed
tensorseed pull org/name --swarm-only --peer host:6881   # peers only: checks that a seeder works

# Publisher side:
tensorseed catalog keygen           # minisign compatible, password protected
tensorseed catalog create
tensorseed harvest --dry-run        # plan from the [harvest] watchlist
tensorseed harvest --watch          # keep mirroring new revisions
tensorseed publish me/my-model --allow-license other   # one repo, e.g. your own `license: other` repo
tensorseed mirror add https://mirror.example.org/ts/ --name eu-1 --priority 10
tensorseed mirror list              # merged mirror ranking
tensorseed seed                     # seed everything under storage.local_dir (Ctrl-C to stop)
```

### Run a seeder

`tensorseed serve` is the long running seeder anyone can run to help keep the
catalog available. It syncs the signed catalog every few minutes, picks
revisions with the `[serve]` rules (globs, watchlist, types, license classes,
latest only, a disk budget), seeds the ones it has on disk, downloads new ones
if `fetch = true`, and stops seeding anything that gets taken down at the next
sync.

```sh
tensorseed serve                                  # seed what is on disk and selected
tensorseed serve --fetch                          # also download newly selected revisions
tensorseed serve --http 0.0.0.0:8080 --serve-catalog   # also act as a webseed and catalog mirror
curl -s http://127.0.0.1:6880/health              # JSON: bytes, peers, torrents, catalog head, last error
```

- Swarm traffic uses TCP and uTP on `torrent.listen_port`. Forward that port
  (TCP and UDP) on your router, or set `[serve] port_mapping = true` to have
  the daemon ask the router through NAT-PMP or UPnP (off by default).
- `--http` serves verified files in the BEP 19 webseed layout
  (`/t/<name>/<path>`, with byte ranges), so others can add the address as a
  mirror. `--serve-catalog` adds the signed catalog under `/<catalog>/`.
- Upload caps: `[serve] upload_limit_kbps` and `http_upload_limit_kbps`.
- SIGTERM stops cleanly; a restart picks up where it left off.
- A hardened systemd unit and a minimal config are in
  [`contrib/systemd/`](contrib/systemd/).

The full volunteer guide (ports, selection rules, disk budget, webseeds,
S3 compatible storage, systemd, health checks, and how to get a mirror
listed) is [docs/RUN-A-MIRROR.md](docs/RUN-A-MIRROR.md).

Pickle based files (`.bin`, `.pt`, ...) are skipped unless `--allow-pickle`, and
gated repos are refused unless `--allow-gated`. Publishing uses a strict license
allowlist; `--allow-license <id>` or a watchlist entry
`{ id = "org/name", allow_license = ["other"] }` lets you mirror a repo you hold
the rights to without loosening it for anything else.

A Hugging Face token is optional. It is never written in `tensorseed.toml`
(an inline `token =` is rejected): set the variable named by
`[huggingface] token_env` (default `HF_TOKEN`), or point `token_file` at a file
only you can read (`chmod 600`). Downloads resume after
interruption; `tensorseed jobs` shows their state.

### Chia is the source of truth

The on-chain Chia DataLayer root is authoritative. The signed file catalog on
tensorseed.dev, R2, mirrors, and seeders is a cache that clients check against
the chain (spec section 6.14):

- Every Chia update carries a signed `ts/v1/file-head` anchor naming the file
  catalog revision and root. The publisher writes Chia first and uploads the
  file catalog, plus a `chain-proof.json` proof of inclusion, only after the
  update confirmed. Weekly head re-signs need no chain transaction.
- Clients need no Chia software: they verify the proof locally and look the
  store's on-chain coin up, read only, on a full node RPC compatible API (a
  public one by default; add more, or your own node). A file head the chain
  does not anchor is refused.

```toml
[catalog]
# chain_check = "strict"        # "strict" | "warn" | "off"; default strict once a store id is set

[chia]
# The official mainnet store; empty disables the chain check (with a notice).
catalog_store_id = "3dcd43dced47a32f8e38e8ec77f583d8d1bafe1ab24e381538e50a36034b7a76"
chain_source = "light"          # or "datalayer" to use your own Chia DataLayer node
coin_api_urls = ["https://api.coinset.org"]
```

```sh
tensorseed catalog chain-status     # file catalog vs. chain, with every value printed
tensorseed catalog anchor           # publisher: bootstrap or repair the chain anchor
```

The check trusts the coin endpoints only to say whether the store coin exists
and is spent; the root itself is proven locally. Details and limits are in the
spec. Running a Chia publisher: [docs/CHIA-RUNBOOK.md](docs/CHIA-RUNBOOK.md).

## Workspace layout

| Crate | Purpose |
|---|---|
| `crates/tensorseed-core` | Manifest and catalog JSON types, `ts/v1/` keys, canonical JSON, SHA-256 and git blob hashing, directory verification |
| `crates/tensorseed-hf` | Hugging Face Hub API client (repo info, tree, listings, backoff) and license policy |
| `crates/tensorseed-fetch` | Verified, resumable, parallel HTTP downloads with source fallback |
| `crates/tensorseed-catalog` | `CatalogBackend` trait, signed entries and keys (minisign format), signed-file backend, Chia DataLayer backend, storage sinks |
| `crates/tensorseed-torrent` | BEP 52 v2 and hybrid torrent builder and parser (pure Rust), Merkle checks, librqbit seeding and swarm downloads |
| `crates/tensorseed-serve` | Building blocks of `serve`: webseed HTTP server, health endpoint, NAT-PMP and UPnP port mapping, selection rules |
| `crates/tensorseed-cli` | The `tensorseed` binary |
| `spikes/` | M0 spike programs (not built by CI), see [spikes/README.md](spikes/README.md) |

## Build

Requires Rust 1.87 or newer (pinned by `rust-toolchain.toml`; install with
[rustup](https://rustup.rs)).

```sh
cargo build --release
cargo test
./target/release/tensorseed --help
```

Configuration: copy `config/tensorseed.example.toml` to
`~/.config/tensorseed/tensorseed.toml`. Secrets (HF token, storage keys, the
publisher key password) are only read from environment variables, never from the
file. Check the merged result with `tensorseed config show`.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Copyright 2026 Tuned Chaos.

Tensorseed is licensed under the GNU Affero General Public License v3.0 or later
(**AGPL-3.0-or-later**). See [LICENSE](LICENSE).
