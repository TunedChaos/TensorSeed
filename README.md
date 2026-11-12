# Tensorseed

**Verifiable, chain anchored mirrors of open Hugging Face models, shared over BitTorrent v2.**

Website: [tensorseed.dev](https://tensorseed.dev)

> **Status: pre-alpha, milestone M0 (spikes).** The workspace compiles and the core
> manifest types and hashing are tested, but `publish`, `pull`, `seed`, and the other
> commands print "not implemented yet". Do not use this for anything real yet.

## What it is

Tensorseed keeps open models and datasets from Hugging Face available even if the
original host goes away, and lets anyone check that a copy is genuine.

- **Signed, versioned catalog.** A small catalog lists each mirrored repo revision:
  every file, its size, its SHA-256, the license, and the Hugging Face commit it came
  from. Only the catalog owner can update it, every version is kept, and anyone can
  subscribe and re-serve it. The catalog sits behind a pluggable `CatalogBackend`
  trait; the first backend is **Chia DataLayer**, where only the Merkle root goes on
  chain. Other chains can be added later without changing the manifest format.
- **Bytes over BitTorrent v2.** Model files travel as BEP 52 torrents, which verify
  every block with SHA-256, plus BEP 19 HTTP webseeds (Cloudflare R2 first) so
  downloads work even with zero peers.
- **Trust the hashes, not the host.** Any peer, webseed, or mirror can serve bytes,
  because the client checks them against the catalog before accepting them.

Read the full design in [docs/SPEC.md](docs/SPEC.md) and the plan in
[docs/MILESTONES.md](docs/MILESTONES.md).

## Workspace layout

| Crate | Purpose |
|---|---|
| `crates/tensorseed-core` | Manifest and catalog JSON types, `ts/v1/` keys, canonical JSON, streaming SHA-256 |
| `crates/tensorseed-hf` | Hugging Face Hub API client (stub) |
| `crates/tensorseed-catalog` | `CatalogBackend` trait and the Chia DataLayer backend (stub) |
| `crates/tensorseed-torrent` | BitTorrent v2 + webseed content layer (stub, pending M0) |
| `crates/tensorseed-cli` | The `tensorseed` binary |

## Build

Requires a stable Rust toolchain (pinned by `rust-toolchain.toml`; install with
[rustup](https://rustup.rs)).

```sh
cargo build --release
cargo test
./target/release/tensorseed --help
```

Configuration: copy `config/tensorseed.example.toml` to
`~/.config/tensorseed/tensorseed.toml`. Secrets (HF token, R2 and B2 keys) are only
read from environment variables, never from the file. Check the merged result with
`tensorseed config show`.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Copyright 2026 Tuned Chaos.

Tensorseed is licensed under the GNU Affero General Public License v3.0 or later
(**AGPL-3.0-or-later**). See [LICENSE](LICENSE).
