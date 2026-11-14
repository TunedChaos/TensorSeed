# Changelog

## Unreleased

- Tensorseed Desktop Flatpak (`desktop/flatpak/`): a manifest for
  `dev.tensorseed.desktop` on the GNOME 51 runtime that builds Desktop and
  the `tensorseed` CLI offline from vendored Cargo and pnpm sources
  (`update-sources.sh` regenerates them), with AppStream metadata, icons,
  a desktop file that handles `tensorseed://`, and the tray library from
  Flathub's shared modules (git submodule). Permissions are network,
  Wayland with X11 fallback, GPU, the tray watcher, and only Tensorseed's
  config, data, and downloads folders. Inside a Flatpak, Tensorseed reads
  the host's config directory (so Desktop and a host CLI share one
  config), pulls default to `tensorseed` under the download directory,
  Desktop skips its runtime scheme registration, and the tray icon is
  written where the host can read it. Not published anywhere.
- Inference engines are documented as a separate install: the README
  (new Install section), the website, `docs/SELL-INFERENCE.md` (section 3,
  "Install an inference engine"), `docs/RUN-A-MIRROR.md`, the Desktop
  README (serving is CLI only; how to serve next to the Flatpak), the
  example config, and `serve-inference --help` and `sign-responses --help`
  say that Tensorseed does not bundle llama.cpp or vLLM, what each is
  for, how to install it per OS, and how Tensorseed finds it (`PATH`,
  `inference.llama_server` or `inference.vllm`, or
  `TENSORSEED_INFERENCE__LLAMA_SERVER` and `TENSORSEED_INFERENCE__VLLM`
  for one run). The missing engine error in `serve-inference`,
  `reference-logprobs`, and `claim-serving` now says the engine is not
  installed, how to install it, and links the guide; `doctor` mentions
  the guide too.
- Tensorseed Desktop copy status: local copies and the entry page label
  each copy as complete, partial, unsigned, or at another revision, with
  a one line reason and the copy's own commit, file count, and size on
  disk, instead of reporting a partial or unsigned copy as a failed
  verification. Verify checks such a copy against its own pull record by
  default, with **Verify against catalog** and **Complete this copy**
  (pull the missing files from the signed catalog) next to it. The
  catalog entry's signature and anchor status are never affected by a
  local copy.
- Tensorseed Desktop NSIS uninstaller: removes the
  `HKCU\Software\Tuned Chaos\Tensorseed Desktop` key it leaves behind
  (only its own values, so an installed MSI keeps its), which stops a
  later MSI install from reusing the old NSIS folder, and deletes the
  WebView2 profile when "Delete the application data" is ticked. Updates
  keep both. Config, data, pulled copies, and mirrors are never touched.
- Workspace: `tensorseed-core` and `tensorseed-verify` declare
  `default-features = false` in `[workspace.dependencies]` and members
  that want the defaults ask for them, which Cargo 1.98 requires.

- Tensorseed Desktop (M3.2, `desktop/`): a tray icon (show or hide the
  window, live seeding and downloading counts and rates, pause all,
  resume all, quit); a settings view for every `[torrent]` option (caps,
  per torrent caps, peer and connection limits, uTP, SOCKS5, bind
  interface, ratio and time stop rules, scheduled limits) that validates
  with the CLI's loader and edits only `[torrent]` in the CLI's config
  file, keeping comments, other sections, and unknown keys; entry pages
  with the manifest, signature and chain anchor status, license and
  commercial mark, files and sizes, and pull, verify, and seed actions;
  a local copies view; and `tensorseed://` deep links (`catalogs`,
  `settings`, `entry/[model|dataset/]<org>/<name>?revision=&catalog=`)
  that only navigate, with single instance forwarding to the running
  window. Pull runs the `tensorseed` CLI for now; verify and seed run in
  process, one seeder per machine (Desktop will not start a second one
  next to a running `serve`). Linux packages get a desktop file with
  `%u` and `MimeType=x-scheme-handler/tensorseed`. The updater stays
  off. See `desktop/README.md`.
- Tensorseed Desktop on Windows: `pnpm --dir desktop tauri build
  --bundles msi,nsis` makes unsigned MSI (per machine) and NSIS (per
  user) installers, tested on Windows 11 (install, deep links, single
  instance, close to tray, pull without a console window, verify, seed,
  settings, `tensorseed gui`, uninstall). The MSI gets a numeric version
  (`bundle.windows.wix.version`) and a pinned upgrade code. An installed
  Desktop no longer writes its own per user `tensorseed://` registration
  over the installer's, which an MSI uninstall left behind pointing at the
  removed program. Settings edits keep CRLF line endings. Desktop looks
  for the CLI in `~/.cargo/bin` on Windows too, and only in `/usr/bin` and
  similar on Unix.
- `tensorseed-app`: fix a Windows only clippy lint in the executable
  check.
- `tensorseed gui [TARGET] [--type] [--print-command]` opens Tensorseed
  Desktop, optionally at an entry or a `tensorseed://` link. It looks in
  `TENSORSEED_DESKTOP`, next to the CLI, on `PATH`, and in the standard
  install places for Linux (including AppImage and Flatpak), Windows,
  and macOS, and passes the config path along. If Desktop is not
  installed it says where it looked and where to get it, and exits 1.
  The CLI links no GUI code.
- Config: `TENSORSEED_CLI` and `TENSORSEED_DESKTOP` (binary locations
  for Desktop and `tensorseed gui`) are no longer read as config keys,
  so setting them does not break config loading.
- Response signing (`docs/plans/verified-serving.md` sections 3, 5.1,
  and 5.8). `tensorseed response-key create|show` makes an Ed25519
  response key in an owner-only `response-key.json` next to the config
  and prints only the public key; the secret is never printed, logged,
  or taken as an argument. `tensorseed claim-serving <dir|org/name>`
  verifies the copy and writes the `ts.served-weights/1` claim (response
  public key, manifest hash, repo, revision, file, engine and version,
  endpoint, optional `--wallet` as plain data, `claim_sha256`) and prints
  the exact EIP-191 message; `--sig 0x...` checks that a signature made
  in the seller's own wallet recovers to that wallet and attaches it.
  Tensorseed holds no wallet keys. `tensorseed sign-responses` (and
  `serve-inference --sign`, which moves the runtime to a private loopback
  port) runs a signing proxy on `127.0.0.1:8090` that forwards requests
  and buyer headers unchanged, uses or mints `X-Tensorseed-Request-Id`,
  signs every JSON response with the `X-Tensorseed-*` headers over the
  canonical `ts.response-sig/1` digest, ends event streams with a signed
  SSE comment line, and serves the claim at
  `/.well-known/tensorseed/served-weights.json`.
  `tensorseed verify-response` checks a saved response (headers and body,
  or a stream) against a claim: signature, body hash, model, request id,
  claim hash, and timestamp skew. `check-endpoint` verifies signatures
  when the endpoint serves a claim or `--claim` is given (an unsigned or
  badly signed answer fails its challenge), takes
  `--require-signatures` and `--max-skew`, and reports per-challenge
  signature status and a summary. `serve-inference` without `--sign`
  still writes `ts.served-weights/0`.
- `tensorseed reference-logprobs <dir|org/name>` generates a
  `ts.reference-logprobs/1` file for logprob challenges (the first two
  commands of `docs/plans/verified-serving.md`). It verifies the copy
  against the signed manifest first, then runs the same llama.cpp or vLLM
  launch as `serve-inference` on a private loopback port (or uses
  `--endpoint` for a running server), asks each challenge with fixed
  decoding, and records the expected chosen token and top-k logprobs.
  The file is bound to the manifest hash, revision, entrypoint file and
  its SHA-256, runtime, and engine version, and sealed by `set_hash`
  (SHA-256 of the canonical JSON without `set_hash`, with a random salt).
  Prompts come from `--prompts` (chat or completions) or the published
  generator `ts.challenge-gen/1` with a random or given seed.
  `--commitment` writes the public `ts.reference-logprobs-commitment/1`
  without prompts; the secret file is written owner-only. llama.cpp
  reference runs send `cache_prompt: false` so values come from a full
  prefill.
- `tensorseed check-endpoint <base_url> --reference <file>` sends a
  sampled subset of challenges (`--sample`, reproducible with
  `--sample-seed`) with bounded `--concurrency`, a per-request `--timeout`,
  and an optional API key read from `--api-key-env`. It parses both the
  chat (`logprobs.content[].top_logprobs`) and legacy completions logprob
  shapes, compares within `--max-mean-abs` (0.5 nats) and
  `--max-token-abs` (2 nats), repeats failing challenges (`--repeats`, 3),
  and reports pass, fail, inconclusive, or unsupported per challenge and
  overall. Exit 0 pass, 1 fail, 2 inconclusive or unsupported. `--json`
  prints `ts.check-endpoint/1`. No signing, attestation, or marketplace
  code is included.
- Commercial serving marks: every catalog entry gets a derived
  `commercial_serving` value (`allowed`, `restricted`, `forbidden`, or
  `unknown`) with a short note, computed on the client from the license id
  and class already in the catalog. Nothing in the signed catalog format
  changes. One mapping table (`tensorseed-core` `commercial`) feeds the
  CLI, the explorer (through the wasm bindings), and Desktop. `list` shows
  the mark on every row and takes `--commercial` (allowed or restricted),
  `--commercial-serving <mark>`, and `--json` (`ts.list/1`); `show` prints
  the mark on stderr; the explorer and the shared `EntryList` show a badge
  and a filter. Every surface says the marks are not legal advice.
- `tensorseed serve-inference <dir|org/name>` serves a verified local copy
  through an OpenAI compatible runtime: llama.cpp (`llama-server`) for
  GGUF, vLLM for safetensors. It re-hashes the copy against the signed
  manifest and refuses to serve on any mismatch, an unsigned manifest, or a
  takedown; refuses `forbidden` licenses unless `--i-have-rights`, and warns
  on `restricted` and `unknown`; waits for `/v1/models`; prints the base URL
  and model id; and writes an unsigned, versioned
  `.tensorseed/served-weights.json` (`ts.served-weights/0`), a placeholder
  for a signed weights statement whose format is not decided yet. New
  `[inference]` section (binary paths, host, port, extra arguments, an API
  key variable passed as `LLAMA_API_KEY` or `VLLM_API_KEY`, ready timeout);
  `--print-command` prints the exact command. No runtime is bundled. The
  logic is in `tensorseed-app` for Desktop. `doctor` checks `[inference]`
  when it is present. Guide: `docs/SELL-INFERENCE.md`.
- `pull --include` no longer leaves empty placeholder files for unselected
  paths (the torrent engine created them), and `verify` reports such
  placeholders left by older pulls as skipped instead of failing.
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
- Standard torrent options in `[torrent]` (documented in SPEC 7.1 and
  the example config). Defaults keep today's behaviour.
  `download_limit_kbps` (pull and seed; it was always unlimited before),
  `max_peers_per_torrent`, `torrent_upload_limit_kbps` and
  `torrent_download_limit_kbps` per torrent, `utp` for pull and seed,
  `proxy_url` (SOCKS5), `bind_interface` (Linux and macOS),
  `concurrent_init_limit`, seeding stop rules `seed_ratio_limit`,
  `seed_time_limit`, and `seed_limit_action` (`pause` or `remove`; files
  are never deleted), and `[torrent.alt_speed]` with day of week and
  local time windows, applied live. `pull` and `seed` take
  `--download-limit`, `--upload-limit`, and `--max-peers`; `seed` also
  takes `--ratio-limit` and `--seed-time` and exits once every torrent
  reached a limit. `serve` applies the stop rules per revision across
  restarts (`seed_stats` in `serve-state.json`, overridable in
  `[serve]`), follows the schedule unless `serve.alt_speed = false`, and
  applies `serve.download_limit_kbps` to background fetches too. There is
  no global connection cap and no protocol encryption setting, because
  librqbit 9.1 implements neither.
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
