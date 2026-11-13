# Changelog

## Unreleased

- `tensorseed doctor` is implemented. It checks the config, each followed
  catalog (reachable, head signature, Chia anchor), the published mirror
  list and whether the mirrors answer, where the Hugging Face token comes
  from (never the token itself), storage credentials by variable name, the
  torrent and `serve` ports, free disk space, and that the download, DHT,
  and state paths are writable. Each check reports ok, warn, or fail; exit
  code 1 when any check fails. `--json` prints a `ts.doctor/1` report,
  `--offline` skips network checks, and every network check has a timeout
  (`--timeout`, default 15 seconds). The checks live in `tensorseed-app` so
  Desktop can show them.
- Windows: paths are built per component and config paths are normalized
  to native separators, so `pull` and other commands no longer print mixed
  paths like `C:/Users/x/downloads\datasets\openai/gsm8k`. Linux and macOS
  paths and on-disk layout are unchanged. On Windows, an interrupted `pull`
  started by an older build into such a mixed path restarts its job record
  instead of resuming (files already downloaded are still reused).
- `tensorseed catalog update-info` rewrites the signed catalog info
  (`ts/v1/catalog`) from the config, so a publisher can set or change
  `publisher_contact`, `publisher_policy_url`, webseed bases, and trackers
  after `catalog create`. `--dry-run` shows the current and proposed values
  without signing.
- mirror.tensorseed.dev landing and not found pages (`site/mirror/`).
- New `tensorseed-app` library crate (groundwork for Tensorseed Desktop in
  0.3.0): config, catalog access and chain check, mirror ranking, torrent
  helpers, manifest resolution, and Hugging Face login state moved out of
  the CLI. No change to CLI commands, flags, or output.
- New `ui/` package (`@tensorseed/ui`): the explorer's entry list, status
  banner, command box, theme toggle, types, formatting helpers, and theme,
  shared with Tensorseed Desktop. The explorer looks and behaves the same.
- Tensorseed Desktop scaffold (`desktop/`, Tauri 2, not released yet): a
  window that lists the followed catalogs from the shared config and shows
  a catalog's entries, synced through the same chain check as
  `tensorseed list`, with each entry signature verified natively. Built
  outside the root Cargo workspace, so CLI builds are unaffected. No
  updater and no telemetry.

## 0.2.0

Tensorseed mirrors open Hugging Face models and datasets for preservation.
0.2.0 lets one client follow several signed catalogs at once and adds a
browser explorer at [explore.tensorseed.dev](https://explore.tensorseed.dev)
that verifies the catalog signature and its Chia anchor without installing
anything. Formats and commands may still change before 1.0.

### Highlights

- Multi-catalog: follow several signed, chain-anchored catalogs with
  `[[catalogs]]` or `tensorseed catalog add`. Revisions that agree across
  catalogs share one swarm, and `list` shows how many catalogs agree.
- Catalog explorer at explore.tensorseed.dev: browse and search the
  official catalog, open an entry, and check the minisign signature and
  the Chia DataLayer anchor in the browser (WebAssembly built from the
  same Rust verification code as the CLI). Add another catalog by URL,
  key, and store id to compare.
- Default download order is the BitTorrent swarm, then Hugging Face, then
  HTTP mirrors, configurable with `[fetch] source_order`.
- `tensorseed hf login`, `logout`, and `status`, and automatic reuse of an
  existing Hugging Face CLI token.
- `pull --include` and `--exclude` globs for partial pulls.
- `.deb` and `.rpm` packages, shell completions, and man pages.

### Download targets

The same five archives as 0.1.0 (Linux x86_64 and ARM64 static musl,
Linux x86_64 glibc 2.17, Windows x86_64, Windows ARM64), signed source
archives (`tensorseed-0.2.0-src.tar.gz` and `.zip`), and new `.deb` and
`.rpm` packages for x86_64 and aarch64. All are listed in `SHA256SUMS`,
signed with the release key (minisign, key id F5D2D3C4C27D459D).

### Added

- Read-only catalog explorer (`explorer/`, Svelte 5, TailwindCSS,
  DaisyUI, built with pnpm, no third party scripts) deployed at
  explore.tensorseed.dev. It verifies the catalog with a WebAssembly
  build of `tensorseed-verify` (`tensorseed-wasm`), shows chain status,
  per-entry files, hashes, licenses, and torrent details, and has a
  "use another catalog" form with agreement across catalogs.
  Catalog signature and chain checks moved into the `tensorseed-verify`
  crate so the CLI and the explorer share one implementation.
- `tensorseed pull --include GLOB` and `--exclude GLOB` (repeatable) select
  files by path. Globs use the same rules as `serve` (`*` matches any
  run of characters, including `/`). `--exclude` wins when both match.
  The swarm uses librqbit `only_files`; HTTP mirrors and the Hub use the
  same filter. `pull.json` records the full file list plus `skipped` and
  `unselected`. `verify` on a partial pull passes when selected files
  match; missing unselected files fail only with `--strict`. `serve` and
  `--seed-after` seed the selected subset with `only_files`, and do not
  announce missing files as a complete torrent.
- Default download order is the BitTorrent swarm, then Hugging Face,
  then HTTP mirrors. `[fetch] source_order` is a list of `"swarm"`,
  `"hub"`, and `"mirrors"` (validated, no duplicates) and applies to
  `pull` and `serve` auto-fetch. `--no-swarm`, `--swarm-only`, and
  dead-mirror memory are unchanged. A 401, 403, or 404 from the Hub
  falls through to the mirrors without marking the host dead.
- Hugging Face tokens are also loaded from `tensorseed hf login` and from
  the Hugging Face CLI (`HF_TOKEN_PATH`, `$HF_HOME/token`, or
  `~/.cache/huggingface/token`). Resolution order: `token_env`, then
  `token_file`, then the tensorseed login file, then the CLI token.
  `tensorseed hf login` reads a token from a hidden prompt or stdin and
  writes `~/.config/tensorseed/hf_token` (0600 on Unix, a user-only ACL
  on Windows). `hf logout` removes that file. `hf status` calls
  whoami-v2 and prints the username and token source, never the token.
- `just dist-packages` builds `.deb` and `.rpm` packages (x86_64 and
  aarch64) from the static musl binaries with nfpm. Each package
  installs `/usr/bin/tensorseed`, a disabled `tensorseed-serve` systemd
  unit running as the `tensorseed` system user, and
  `/etc/tensorseed/tensorseed.toml.example` (copied to `tensorseed.toml`
  on first install). `just dist` includes the packages in SHA256SUMS.
- `tensorseed completions <shell>` prints a bash, zsh, fish, or PowerShell
  completion script. `just generate-docs` also writes man pages
  (`tensorseed.1` and one page per subcommand). Linux packages install
  bash, zsh, and fish completions (fish to
  `/usr/share/fish/vendor_completions.d`) and the man pages to
  `/usr/share/man/man1`. Release tarballs and zip archives include
  `completions/` and `man/`.
- Follow several catalogs at once with `[[catalogs]]` (name, label,
  publisher key, store id, base URL, chain check, optional priority).
  A config with no `[[catalogs]]` still uses the top-level `[catalog]`,
  `[file]`, and `[chia]` block as the sole subscription. Revisions are
  keyed by Hugging Face repo and commit; agreeing hashes share one swarm
  and can use every agreeing catalog's HTTP sources. `list` prints
  "N of M catalogs agree". The same repo and commit with different
  hashes is a hard error unless `--catalog` is given. Tombstones are per
  catalog: a revision remains pullable and seedable if another
  subscription still lists it. `serve` seeds a revision that is on disk
  and still valid in at least one non-tombstoned subscription.
  `tensorseed catalog list`, `catalog add`, and `catalog remove` manage
  subscriptions; `catalog info` and `catalog chain-status` accept
  `--catalog`.

### Fixed

- The BitTorrent DHT binds IPv4-only when the configured listen address is
  IPv4, and stores its routing table under Tensorseed's data directory
  (`%LOCALAPPDATA%\tensorseed\dht.json` on Windows). A previous default
  used librqbit's dual-stack `[::]` bind and its own cache path, which
  failed to start the engine on Windows and aborted `pull` before HTTP
  sources could run. If the engine still fails to start, `pull` now
  falls back to HTTP unless `--swarm-only` is set.

## 0.1.0

Tensorseed mirrors open Hugging Face models and datasets for preservation.
A signed catalog lists each mirrored revision (files, sizes, SHA-256
hashes, license, and the Hugging Face commit). The catalog is anchored on
Chia DataLayer, and clients check the signed file catalog against the
chain. Downloads use the BitTorrent swarm first; HTTP mirrors and Hugging
Face are failover.

This is an early release. The software works today for pulling, verifying,
and seeding, and we are looking for volunteer seeders. Formats and
commands may still change before 1.0.

### Highlights

- Strict chain checking is the default once a store id is set:
  `tensorseed catalog chain-status` reports whether the file catalog
  matches the on-chain head.
- `tensorseed pull` uses the BitTorrent swarm first. HTTP mirrors and
  Hugging Face fill only what the swarm cannot deliver. If no peer
  connects within about 30 seconds, or the download stalls for a minute,
  the client switches to HTTP. Pulls are outbound only and do not keep
  seeding unless you pass `--seed-after`.
- Dead HTTP mirrors are remembered. A connect failure or repeated
  transient failures marks an origin dead for later files and later runs,
  with a one hour cooldown by default (`[fetch] dead_mirror_cooldown_secs`).
  After the cooldown it is retried automatically. `--retry-dead-mirrors`
  retries immediately. A 404 for one file does not mark the host dead.
  If every HTTP source for a file is marked dead, the client tries them
  anyway.
- Seeding is opt in per item. Pull the repos you care about, then run
  `tensorseed serve` to share them. The daemon does not download the rest
  of the catalog unless you turn on auto-follow.
- Windows uses standard directories (`%APPDATA%\tensorseed` for config,
  `%LOCALAPPDATA%\tensorseed` for data).
- Anyone can run an independent catalog (own publisher key, own Chia
  store, own host). See [docs/PUBLISH-A-CATALOG.md](docs/PUBLISH-A-CATALOG.md).
  Nobody but the official publisher can write to the official
  tensorseed.dev catalog. Following several catalogs in one process is
  planned for 0.2.0. To request a repo in the official catalog, open a
  "Request a catalog mirror" issue (gated and restricted-license repos
  are refused).

### Download targets

Prebuilt archives:

- Linux x86_64, static (`x86_64-unknown-linux-musl`)
- Linux ARM64, static (`aarch64-unknown-linux-musl`)
- Linux x86_64, glibc 2.17 (`x86_64-unknown-linux-gnu`)
- Windows x86_64 (`x86_64-pc-windows-gnu`)
- Windows ARM64 (`aarch64-pc-windows-gnullvm`); the file format is
  confirmed, but this build has not been run on ARM Windows hardware

There is no macOS build. macOS users can build from a checkout with
`cargo build --release` or `cargo install --locked --path crates/tensorseed-cli`
(Rust 1.87 or newer), or from GitHub with
`cargo install --locked --git https://github.com/TunedChaos/TensorSeed`.

Signed source archives (`tensorseed-0.1.0-src.tar.gz` and
`tensorseed-0.1.0-src.zip`) were added to this release after the initial
publication. They contain the tracked files from tag `v0.1.0`, are listed
in `SHA256SUMS`, and are signed with the same release key.

### Verify a release

Release archives ship with `SHA256SUMS` and `SHA256SUMS.minisig`, signed
by the release key (minisign, key id F5D2D3C4C27D459D). This is not the
catalog publisher key. Do not verify binaries with the catalog key.

```text
RWSdRX3CxNPS9Vl1JNjDnZ04OmM/3xrfp9vqG5FO8TNpK8Sbg6ENAI4P
```

```sh
minisign -Vm SHA256SUMS -P RWSdRX3CxNPS9Vl1JNjDnZ04OmM/3xrfp9vqG5FO8TNpK8Sbg6ENAI4P
sha256sum -c --ignore-missing SHA256SUMS
```

The public key file is also published as `tensorseed-release.pub` (and in
`keys/tensorseed-release.pub` in the repository).

### Known limitations

- `pull` downloads whole revisions. A file the swarm left partial is
  currently fetched again in full over HTTP.
- A client follows one catalog per config file. To follow a third party
  catalog, use a separate config and `--config`.
- Catalog and on-disk formats may change before 1.0.
- `takedown` and `doctor` still print "not implemented yet".
