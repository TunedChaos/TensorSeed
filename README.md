# Tensorseed

**A decentralized, verifiable package manager for open AI models and datasets.**

Verifiable, chain anchored mirrors of open Hugging Face models and datasets, shared over BitTorrent v2.

Website: [tensorseed.dev](https://tensorseed.dev).
Catalog explorer: [explore.tensorseed.dev](https://explore.tensorseed.dev).

> **Status: 0.2.0, an early release.** Downloading and verifying works today
> (`pull`, `verify`), as do automatic mirroring from the Hub (`harvest`), the
> signed-file catalog commands, and BitTorrent: harvested revisions get hybrid
> v1+v2 torrents, `seed` serves them, `serve` keeps seeding what you already
> pulled, and `pull` uses the BitTorrent swarm first, then Hugging Face, then
> HTTP mirrors. `--seed-after` still seeds after a verified pull. One client
> can follow several catalogs at once (`[[catalogs]]`), and the browser
> explorer verifies the catalog and its Chia anchor without installing
> anything. The
> official catalog is anchored on Chia mainnet, and clients check the file
> catalog against the Chia store by default (strict). `tensorseed doctor`
> checks an install end to end; `takedown` still prints "not implemented
> yet". Expect breaking changes.

**Want to help keep models available?** Run a mirror or seeder: see
[docs/RUN-A-MIRROR.md](docs/RUN-A-MIRROR.md). **Want to publish your own
catalog?** See [docs/PUBLISH-A-CATALOG.md](docs/PUBLISH-A-CATALOG.md).

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
  webseeds so a download can fall back to HTTP when the swarm is quiet. The built-in swarm engine is
  librqbit; see [docs/M0-TORRENT-SPIKE.md](docs/M0-TORRENT-SPIKE.md) for why.
  Storage is local disk by default; add any number of named
  `[[storage.target]]` blocks (R2, B2, Vultr, Wasabi, MinIO, AWS, any S3, another
  directory, or a plain HTTP host), and mirrors in `mirrors.toml`. Pull uses
  the BitTorrent swarm first, then Hugging Face, then HTTP mirrors by priority.
- **Trust the hashes, not the host.** Any peer, webseed, or mirror can serve bytes,
  because the client checks them against the catalog before accepting them.

Read the full design in [docs/SPEC.md](docs/SPEC.md), the plan in
[docs/MILESTONES.md](docs/MILESTONES.md), the volunteer guide in
[docs/RUN-A-MIRROR.md](docs/RUN-A-MIRROR.md), and how to run an
independent catalog in
[docs/PUBLISH-A-CATALOG.md](docs/PUBLISH-A-CATALOG.md). API docs: `cargo doc
--workspace --no-deps --open` (or `just doc-open`).

## Use it

```sh
# Download a repo straight from the Hub, verifying every file against the Hub's
# own SHA-256 (LFS) and git blob ids, then check it again later:
tensorseed pull --from-hf hf-internal-testing/tiny-random-gpt2
tensorseed verify hf-internal-testing/tiny-random-gpt2          # uses .tensorseed/pull.json
tensorseed verify --hf hf-internal-testing/tiny-random-gpt2     # re-checks against the Hub

# With a signed catalog configured (catalog.publisher_pubkey, file.base_url):
tensorseed list
tensorseed list --catalog other     # only this subscribed catalog
tensorseed pull org/name            # swarm first, then the Hub, then HTTP mirrors
tensorseed pull org/name --catalog other  # required if two catalogs disagree on hashes
tensorseed catalog list             # subscriptions: name, label, head, chain status
tensorseed verify org/name          # against the signed manifest; exit 3 on mismatch
tensorseed pull org/name --no-swarm # skip the swarm; the Hub then HTTP mirrors
tensorseed pull org/name --retry-dead-mirrors  # try HTTP mirrors marked dead (clears on success)
tensorseed pull org/name --seed-after 2h   # swarm first, then seed the verified copy
tensorseed pull org/name --swarm-only --peer host:6881   # peers only: checks that a seeder works
tensorseed pull org/name --include '*.gguf' --exclude '*.md'  # only selected files
tensorseed list --commercial        # only licenses that permit commercial serving
tensorseed serve-inference org/name # verify, then serve it with llama.cpp or vLLM
tensorseed reference-logprobs org/name --out ref.json  # expected logprobs from a verified copy
tensorseed check-endpoint https://llm.example.com/v1 --reference ref.json  # logprob challenges

# Publisher side:
tensorseed catalog keygen           # minisign compatible, password protected
tensorseed catalog create
tensorseed harvest --dry-run        # plan from the [harvest] watchlist
tensorseed harvest --watch          # keep mirroring new revisions
tensorseed publish me/my-model --allow-license other   # one repo, e.g. your own `license: other` repo
tensorseed mirror add https://mirror.example.org/ts/ --name eu-1 --priority 10
tensorseed mirror list              # merged mirror ranking
tensorseed seed                     # seed everything under storage.local_dir (Ctrl-C to stop)
tensorseed completions bash         # print a bash completion script
```

The default download order is the BitTorrent swarm, then Hugging Face,
then HTTP mirrors. To follow more than one catalog, add `[[catalogs]]`
entries (see `config/tensorseed.example.toml`). `list` shows how many
subscribed catalogs agree on each revision; `pull` fails on a hash
conflict unless you pass `--catalog <name>`. A takedown in one catalog
does not hide a revision still listed by another.

Set `[fetch] source_order` to a list of `"swarm"`,
`"hub"`, and `"mirrors"` (no duplicates) to change it. `--no-swarm` and
`--swarm-only` still apply. A gated or missing file on the Hub falls
through to the mirrors without marking the Hub dead.

### Run a seeder

`tensorseed serve` is the long running seeder anyone can run to help keep
copies available. The usual setup is to `tensorseed pull` the repos you
care about, then run `serve`. With the default `fetch = false`, it only
seeds what is already on your disk (plus any folders in `serve.dirs`),
syncs the signed catalog every few minutes, and stops seeding anything
that gets taken down at the next sync. It does not download the rest of
the catalog. Turning on `fetch` with `select` globs is an optional
auto-follow mode; `select = ["*"]` with `fetch = true` would download the
whole catalog up to `max_bytes`. See [docs/RUN-A-MIRROR.md](docs/RUN-A-MIRROR.md).

```sh
tensorseed pull org/name                          # choose what to keep
tensorseed serve                                  # seed what you already pulled
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

The full volunteer guide (ports, what to seed, optional auto-follow,
webseeds, S3 compatible storage, systemd, health checks, and how to get a
mirror listed) is [docs/RUN-A-MIRROR.md](docs/RUN-A-MIRROR.md).

`--include` and `--exclude` take globs against each file's path in the
revision (`*` matches any run of characters, including `/`, the same
rules `serve` uses). `--exclude` wins when both match. `verify` on a
partial pull passes when every selected file matches; missing unselected
files fail only with `--strict`. `serve` and `--seed-after` seed the
selected subset with `only_files`, and do not announce missing files as a
complete torrent.

Pickle based files (`.bin`, `.pt`, ...) are skipped unless `--allow-pickle`, and
gated repos are refused unless `--allow-gated`. Publishing uses a strict license
allowlist; `--allow-license <id>` or a watchlist entry
`{ id = "org/name", allow_license = ["other"] }` lets you mirror a repo you hold
the rights to without loosening it for anything else.

Downloads resume after interruption; `tensorseed jobs` shows their state.

### Serve inference from a verified copy

`tensorseed serve-inference <dir|org/name>` re-hashes a pulled copy against
the signed manifest, refuses to serve it if anything differs, checks the
license, and starts an OpenAI compatible server you installed: llama.cpp
(`llama-server`) for GGUF files, vLLM for safetensors. It waits until
`/v1/models` answers, then prints the base URL and model id to list on any
OpenAI compatible marketplace, and records what it serves in an unsigned
`.tensorseed/served-weights.json`. No runtime is bundled; configure them in
`[inference]` or put them on `PATH`, and `--print-command` prints the exact
command instead. See [docs/SELL-INFERENCE.md](docs/SELL-INFERENCE.md) for
the full walk through, including a TLS reverse proxy with an API key.

`tensorseed reference-logprobs` computes expected next-token logprobs for
a set of challenge prompts from a verified copy of one file, bound to the
manifest hash and sealed by a `set_hash` you can publish before revealing
the prompts. `tensorseed check-endpoint` sends a sample of those
challenges to any OpenAI compatible endpoint and reports pass, fail,
inconclusive, or unsupported within stated tolerances. See the guide and
[docs/plans/verified-serving.md](docs/plans/verified-serving.md).

Every entry in `list`, `show`, the explorer, and Desktop carries a
commercial serving mark derived from its license id: `allowed`,
`restricted` (allowed with conditions, such as the Llama licenses'
700M monthly user cap and acceptable use policy), `forbidden`
(non-commercial), or `unknown`. `serve-inference` refuses `forbidden`
licenses unless you pass `--i-have-rights`. The marks are computed on the
client, not signed, and are **not legal advice**: read the license.

### Use your Hugging Face token

A token is optional, but it avoids anonymous rate limits and is required
for gated repos (with `--allow-gated`). It is never written in
`tensorseed.toml` (an inline `token =` is rejected) and is sent only to
the Hugging Face endpoint (dropped on any redirect to another site).

Tensorseed looks for a token in this order:

1. The environment variable named by `[huggingface] token_env` (default
   `HF_TOKEN`).
2. `huggingface.token_file`, if set (must be readable only by you:
   `chmod 600` on Unix).
3. The file written by `tensorseed hf login`
   (`~/.config/tensorseed/hf_token` on Unix, `%APPDATA%\tensorseed\hf_token`
   on Windows). The file is created with mode 0600 on Unix and a
   user-only ACL on Windows.
4. The token already saved by the Hugging Face CLI (`hf auth login`):
   `HF_TOKEN_PATH`, else `$HF_HOME/token`, else
   `~/.cache/huggingface/token` (or `%USERPROFILE%\.cache\huggingface\token`
   on Windows).

```sh
tensorseed hf login    # paste a token (hidden prompt, or pipe it on stdin)
tensorseed hf status   # username and token source; the token is never printed
tensorseed hf logout   # removes only the tensorseed login file
```

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
Running your own catalog (own key, own store, own host):
[docs/PUBLISH-A-CATALOG.md](docs/PUBLISH-A-CATALOG.md).

### Shell completions and man pages

`tensorseed completions bash` (or `zsh`, `fish`, `powershell`) prints a
completion script to stdout. Linux `.deb` and `.rpm` packages install the
bash, zsh, and fish scripts in the usual vendor directories, and install
man pages under `/usr/share/man/man1`. Release archives ship the same
files (plus the PowerShell script) under `completions/` and `man/`.

```sh
# from a release tarball, for the current shell:
#   source completions/tensorseed.bash
#   man man/tensorseed.1
tensorseed completions fish | source   # fish, until the package is installed
```

## Workspace layout

| Crate | Purpose |
|---|---|
| `crates/tensorseed-core` | Manifest and catalog JSON types, `ts/v1/` keys, canonical JSON, SHA-256 and git blob hashing, directory verification |
| `crates/tensorseed-hf` | Hugging Face Hub API client (repo info, tree, listings, backoff) and license policy |
| `crates/tensorseed-fetch` | Verified, resumable, parallel HTTP downloads with source fallback |
| `crates/tensorseed-verify` | Pure catalog verification: minisign, signed-file hash chain, and chain proofs (no network) |
| `crates/tensorseed-catalog` | `CatalogBackend` trait, HTTPS and Chia I/O on top of `tensorseed-verify` |
| `crates/tensorseed-wasm` | WebAssembly bindings for the explorer |
| `crates/tensorseed-torrent` | BEP 52 v2 and hybrid torrent builder and parser (pure Rust), Merkle checks, librqbit seeding and swarm downloads |
| `crates/tensorseed-serve` | Building blocks of `serve`: webseed HTTP server, health endpoint, NAT-PMP and UPnP port mapping, selection rules |
| `crates/tensorseed-app` | Application library shared by the CLI and Desktop: config, catalog access, mirrors, torrent helpers (no argument parsing, no printing) |
| `crates/tensorseed-cli` | The `tensorseed` binary, a front end over `tensorseed-app` |
| `spikes/` | M0 spike programs (not built by CI), see [spikes/README.md](spikes/README.md) |

## Build

Requires Rust 1.87 or newer (pinned by `rust-toolchain.toml`; install with
[rustup](https://rustup.rs)).

```sh
cargo build --release
cargo test
./target/release/tensorseed --help
```

With [just](https://just.systems): `just check-all` runs formatting, clippy,
tests, and docs; `just doc` builds the API docs. Release binaries for Linux
(musl static and glibc 2.17+, x86_64 and ARM64) and Windows (x86_64 and
ARM64) are cross compiled from one host with cargo-zigbuild: `just targets`
then `just dist`. Windows contributors can build natively with
`cargo build --release` (MSVC), or use the justfile from Git Bash or WSL.
Details in [CONTRIBUTING.md](CONTRIBUTING.md#cross-platform-builds).

### Release signatures

Release notes: [CHANGELOG.md](CHANGELOG.md).

Release archives come with `SHA256SUMS` and `SHA256SUMS.minisig`, signed by
the **release key**, which is a different key from the catalog publisher key
(key separation: the catalog key lives on an online publishing host, the
release key stays offline, so a leaked catalog key cannot sign binaries).

Release key id `F5D2D3C4C27D459D` (not the catalog publisher key
`CE1E0E3D543DFFB8`):

```text
RWSdRX3CxNPS9Vl1JNjDnZ04OmM/3xrfp9vqG5FO8TNpK8Sbg6ENAI4P
```

```sh
minisign -Vm SHA256SUMS -P RWSdRX3CxNPS9Vl1JNjDnZ04OmM/3xrfp9vqG5FO8TNpK8Sbg6ENAI4P
sha256sum -c --ignore-missing SHA256SUMS
```

Key roles, storage, and rotation: [RELEASE-KEY.md](RELEASE-KEY.md).
Reporting vulnerabilities: [SECURITY.md](SECURITY.md).

Configuration: copy `config/tensorseed.example.toml` to
`~/.config/tensorseed/tensorseed.toml` (Windows:
`%APPDATA%\tensorseed\tensorseed.toml`). Secrets (HF token, storage keys, the
publisher key password) are only read from environment variables, never from the
file. Check the merged result with `tensorseed config show`. Default data
directories are `~/.local/share/tensorseed/` on Unix and
`%LOCALAPPDATA%\tensorseed\` on Windows.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). To request that a Hugging Face repo
be added to the official catalog, open a "Request a catalog mirror" issue
(gated and restricted-license repos are refused).

## Support

Tensorseed is an open source project by Tuned Chaos, and support goes
through the studio. To help pay for storage and the official seeder, use
[GitHub Sponsors](https://github.com/sponsors/TunedChaos) or the
[Tuned Chaos supporters page](https://tunedchaos.com/supporters/), which
has a card form (pick Tensorseed in the project dropdown) and a Chia (XCH)
address. Support is not a tax-deductible donation.

## License

Copyright 2026 Tuned Chaos.

Tensorseed is licensed under the GNU Affero General Public License v3.0 or later
(**AGPL-3.0-or-later**). See [LICENSE](LICENSE).
