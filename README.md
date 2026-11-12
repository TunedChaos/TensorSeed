# Tensorseed

**Verifiable, chain anchored mirrors of open Hugging Face models, shared over BitTorrent v2.**

Website: [tensorseed.dev](https://tensorseed.dev)

> **Status: pre-alpha, milestone M0.** Downloading and verifying works today
> (`pull`, `verify`), as do automatic mirroring from the Hub (`harvest`), the
> signed-file catalog commands, and BitTorrent: harvested revisions get hybrid
> v1+v2 torrents, `seed` serves them, and `pull --swarm` / `--seed-after` use the
> swarm. The Chia backend has only been tested against mocked RPC responses, and
> `publish`, `takedown`, and `doctor` still print "not implemented yet". Expect
> breaking changes.

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

Read the full design in [docs/SPEC.md](docs/SPEC.md) and the plan in
[docs/MILESTONES.md](docs/MILESTONES.md).

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

# Publisher side:
tensorseed catalog keygen           # minisign compatible, password protected
tensorseed catalog create
tensorseed harvest --dry-run        # plan from the [harvest] watchlist
tensorseed harvest --watch          # keep mirroring new revisions
tensorseed mirror add https://mirror.example.org/ts/ --name eu-1 --priority 10
tensorseed mirror list              # merged mirror ranking
tensorseed seed                     # seed everything under storage.local_dir (Ctrl-C to stop)
```

Pickle based files (`.bin`, `.pt`, ...) are skipped unless `--allow-pickle`, and
gated repos are refused unless `--allow-gated`. Downloads resume after
interruption; `tensorseed jobs` shows their state.

## Workspace layout

| Crate | Purpose |
|---|---|
| `crates/tensorseed-core` | Manifest and catalog JSON types, `ts/v1/` keys, canonical JSON, SHA-256 and git blob hashing, directory verification |
| `crates/tensorseed-hf` | Hugging Face Hub API client (repo info, tree, listings, backoff) and license policy |
| `crates/tensorseed-fetch` | Verified, resumable, parallel HTTP downloads with source fallback |
| `crates/tensorseed-catalog` | `CatalogBackend` trait, signed entries and keys (minisign format), signed-file backend, Chia DataLayer backend, storage sinks |
| `crates/tensorseed-torrent` | BEP 52 v2 and hybrid torrent builder and parser (pure Rust), Merkle checks, librqbit seeding and swarm downloads |
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
