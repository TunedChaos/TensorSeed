# Run a Tensorseed mirror

Tensorseed keeps open models and datasets from Hugging Face available by spreading copies
across many independent machines, the same way BitTorrent keeps popular files
alive. Every volunteer who seeds a few revisions adds redundancy, and nobody
has to trust anyone: each client checks the catalog against the Chia
blockchain and every byte against the signed manifest before using it. A
broken or malicious mirror can only be slow or missing, never wrong.

This guide covers running `tensorseed serve`, the long running seeder, and
optionally publishing your copy as an HTTP webseed or S3 compatible mirror.
The design behind it is in [SPEC.md](SPEC.md) (sections 6.14, 8.3, 10.1).
To run an independent catalog (own key, own Chia store, own host), see
[PUBLISH-A-CATALOG.md](PUBLISH-A-CATALOG.md). Nobody but the official
publisher can write to the official catalog at tensorseed.dev.

Contents:

1. [Install](#1-install)
2. [Configure the chain checked client](#2-configure-the-chain-checked-client)
3. [Choose what to seed](#3-choose-what-to-seed)
4. [Network: ports and port mapping](#4-network-ports-and-port-mapping)
5. [Run it](#5-run-it)
6. [Optional: HTTP webseed and catalog mirror](#6-optional-http-webseed-and-catalog-mirror)
7. [Optional: S3 compatible storage](#7-optional-s3-compatible-storage)
8. [Getting your mirror listed](#8-getting-your-mirror-listed)
9. [Run it as a service (systemd)](#9-run-it-as-a-service-systemd)
10. [Monitoring: the health endpoint](#10-monitoring-the-health-endpoint)
11. [Operating notes](#11-operating-notes)
12. [Publish your own catalog](#12-publish-your-own-catalog)

## 1. Install

### Prebuilt binaries

Release archives are built with `just dist` (see
[CONTRIBUTING.md](../CONTRIBUTING.md)) for:

| Target | Runs on |
|---|---|
| `x86_64-unknown-linux-musl` | Any x86_64 Linux (static, no glibc needed) |
| `aarch64-unknown-linux-musl` | Any 64 bit ARM Linux, including Raspberry Pi 4 and 5 (static) |
| `x86_64-unknown-linux-gnu` | x86_64 Linux with glibc 2.17 or newer |
| `x86_64-pc-windows-gnu` | 64 bit Windows 10 and 11 |
| `aarch64-pc-windows-gnullvm` | Windows on ARM |

Each archive is named `tensorseed-<version>-<target>.tar.gz` (`.zip` on
Windows). Download `SHA256SUMS` and its minisign signature
`SHA256SUMS.minisig` from the same GitHub release as the archive. They are signed with the **release key**, which is
deliberately not the catalog publisher key: the catalog key has to live on
an online publishing host, while the release key stays offline, so a
compromised catalog key can never sign a binary. Do not verify releases
with the catalog key.

Release key id `F5D2D3C4C27D459D` (not the catalog publisher key
`CE1E0E3D543DFFB8`):

```text
RWSdRX3CxNPS9Vl1JNjDnZ04OmM/3xrfp9vqG5FO8TNpK8Sbg6ENAI4P
```

Check both files before running anything:

```sh
minisign -Vm SHA256SUMS -P RWSdRX3CxNPS9Vl1JNjDnZ04OmM/3xrfp9vqG5FO8TNpK8Sbg6ENAI4P
sha256sum -c --ignore-missing SHA256SUMS
tar xzf tensorseed-0.2.0-x86_64-unknown-linux-musl.tar.gz
install -m 0755 tensorseed-0.2.0-x86_64-unknown-linux-musl/tensorseed ~/.local/bin/
tensorseed --version
```

Key roles and rotation are described in [RELEASE-KEY.md](../RELEASE-KEY.md).

### From source

Requires Rust (installed with [rustup](https://rustup.rs); the repo pins the
toolchain):

```sh
cargo install --locked --path crates/tensorseed-cli
```

## 2. Configure the chain checked client

Create `~/.config/tensorseed/tensorseed.toml` on Unix, or
`%APPDATA%\tensorseed\tensorseed.toml` on Windows (or
`/etc/tensorseed/tensorseed.toml` for the systemd unit). This is the
minimum to follow the official catalog with strict chain checking:

```toml
[catalog]
backend = "file"
name = "tensorseed"
publisher_pubkey = "RWS4/z1UPQ4ezmjrcpKqDYpqyn5h1WUS2oDs1B73Dh91kVzzNIZd03xd"
chain_check = "strict"           # refuse any catalog head the Chia store does not anchor

[file]
base_url = "https://catalog.tensorseed.dev/"

[chia]
catalog_store_id = "3dcd43dced47a32f8e38e8ec77f583d8d1bafe1ab24e381538e50a36034b7a76"
chain_source = "light"           # verify the published proof locally; no Chia software needed
coin_api_urls = ["https://api.coinset.org"]
```

What this does:

- **`publisher_pubkey`** pins the catalog signing key (minisign key id
  `CE1E0E3D543DFFB8`). Every catalog entry, delta, snapshot, head, and the
  mirror list must be signed by it.
- **`catalog_store_id`** is the official Chia DataLayer store on mainnet. The
  on-chain root is the source of truth; the files at `base_url` (and every
  mirror, including yours) are only a cache.
- **`chain_check = "strict"`** makes the client verify, on every sync, that
  the file catalog head is the one the chain anchors. `warn` logs a mismatch
  but continues; `off` skips the check. Volunteers should stay on `strict`.
- **`coin_api_urls`** are read only full node RPC compatible endpoints used to
  ask one question: does the store's current coin still exist? They cannot
  make a forged catalog pass, because the root is proven locally against
  the publisher's signature. The worst a bad endpoint can do is make you lag
  behind, and clients never accept an older revision than one they have
  already seen. List several endpoints, or your own Chia node, to spread
  trust.

Check the setup:

```sh
tensorseed config show                 # merged configuration
tensorseed catalog chain-status        # file catalog vs. chain, every value printed
tensorseed list                        # what the catalog contains
```

Secrets never go in this file. If you need a Hugging Face token (only for
gated repos, which the official catalog does not carry), set `HF_TOKEN` in
the environment or point `[huggingface] token_file` at a file with mode 0600.

## 3. Choose what to seed

The usual way to help is to pull the repos you care about, then run
`tensorseed serve`. By default `fetch` is off, so the daemon only seeds
what is already on your disk: the trees you pulled, plus any extra
folders listed in `dirs`. It will not download the rest of the catalog.

```sh
tensorseed pull org/name
tensorseed serve
```

If an HTTP mirror is down, later files and later runs skip it for an hour
(`[fetch] dead_mirror_cooldown_secs`). Pass `--retry-dead-mirrors` to try
them anyway, or wait for the cooldown to expire. `tensorseed jobs` lists
current marks. A 404 for one file does not mark the mirror dead.

A minimum `[serve]` section (these are also the built-in defaults):

```toml
[serve]
fetch = false                    # do not download anything you did not already pull
select = ["*"]                   # among what is on disk, seed any catalog revision
dirs = []                        # extra published trees (with t/) to seed from
latest_only = true
prune = true                     # delete your copy when a revision is taken down or deselected
sync_interval_secs = 300
# Unix default shown; on Windows the default is %LOCALAPPDATA%\tensorseed\serve
data_dir = "~/.local/share/tensorseed/serve"
```

`select = ["*"]` with `fetch = false` does not mean "mirror everything".
It means "seed any on-disk revision that is still in the catalog". An
empty `select` list seeds nothing.

Bandwidth caps (KiB per second, 0 = unlimited):

```toml
[torrent]
upload_limit_kbps = 0            # global swarm upload cap
[serve]
upload_limit_kbps = 0            # serve specific swarm cap (0 = use torrent.upload_limit_kbps)
download_limit_kbps = 0          # swarm download cap
http_upload_limit_kbps = 0       # cap for the optional HTTP webseed
```

Takedowns: when the publisher tombstones a revision, every seeder stops
serving it at its next sync and (with `prune = true`) deletes its copy.

### Auto-follow (advanced)

If you want `serve` to download new revisions on its own, turn on
`fetch` and name specific repos or an org. `select = ["*"]` with
`fetch = true` downloads the whole catalog, up to `max_bytes`.

```toml
[serve]
fetch = true
fetch_swarm = true               # download from peers first, then webseeds and mirrors
select = ["org/name"]            # or "org/*" for one org; [] seeds nothing
exclude = ["org/huge-model"]     # never seed these
types = []                       # "model", "dataset"; [] = both
license_classes = []             # "open", "attribution", "restricted", "unknown"; [] = any
latest_only = true
max_bytes = 50_000_000_000       # disk budget (here 50 GB; 0 = unlimited)
max_revision_bytes = 0           # skip single revisions larger than this (0 = no cap)
```

Or pass `--fetch` on the command line, which still honors your `select`
globs. Use a narrow `select` list if you turn this on.

How selection works: a revision must match a `select` glob and no
`exclude` glob. Revisions are ranked by the first `select` pattern they
match (earlier patterns first), then latest revisions and newer
publications first. In that order, `latest_only`, `types`,
`license_classes`, and `max_revision_bytes` filter them, and revisions
are added until `max_bytes` is reached. Anything that does not fit is
reported as skipped in the health document, with the reason.

## 4. Network: ports and port mapping

Seeding works best when other peers can connect to you.

| Port | Protocol | Purpose | Open to the internet? |
|---|---|---|---|
| `torrent.listen_port` (default 6881) | TCP | BitTorrent peer connections | Yes |
| `torrent.listen_port` (default 6881) | UDP | uTP peer connections and DHT | Yes |
| `serve.http_listen` (for example 8080) | TCP | Optional HTTP webseed and catalog mirror | Only if you run it (section 6) |
| `serve.health_listen` (default 6880) | TCP | Health endpoint | **No**, keep it on 127.0.0.1 |

Three ways to make the torrent port reachable:

1. **Forward it on your router**: TCP and UDP, external 6881 to your
   machine's 6881. Most reliable.
2. **Let the daemon ask the router**: set `[serve] port_mapping = true`.
   Tensorseed tries NAT-PMP first, then UPnP IGD, renews the lease
   (`port_mapping_lease_secs`, default one hour), and removes the mapping on
   a clean shutdown. It is off by default because it opens a port on your
   router; many routers ignore or disable these protocols, and the health
   document reports whether it worked.
3. **Do nothing**: you still upload to peers you connect to, but peers behind
   NAT cannot reach you. This still helps, just less.

On a host firewall, allow inbound TCP and UDP on the listen port, for
example `ufw allow 6881` (both protocols).

## 5. Run it

```sh
tensorseed serve                 # seed what you already pulled (fetch is off)
tensorseed serve --fetch         # also download revisions matching [serve] select
tensorseed serve --for 2h        # stop after two hours
```

`--fetch` honors your `select` globs. With the default `select = ["*"]` it
would download the whole catalog, up to `max_bytes`; see
[Auto-follow (advanced)](#auto-follow-advanced) before using it.

The first sync verifies the catalog against the chain, then every selected
revision already on disk is checked piece by piece before it is announced,
so you only ever seed bytes that match the signed manifest. Stop with Ctrl-C or SIGTERM;
state is saved in `<data_dir>/serve-state.json` and a restart resumes
seeding before the first sync.

## 6. Optional: HTTP webseed and catalog mirror

BitTorrent needs peers; HTTP webseeds work with zero peers and carry most
bytes for first downloads. If you have spare bandwidth and a public address:

```sh
tensorseed serve --http 0.0.0.0:8080 --serve-catalog
```

or in the config:

```toml
[serve]
http_listen = "0.0.0.0:8080"
serve_catalog = true
```

- `--http` serves your verified revisions in the BEP 19 webseed layout:
  `/t/<content name>/<path>` with byte ranges, and
  `/t/<content name>.torrent`. Only revisions that passed verification and
  have the size the manifest promises are served.
- `--serve-catalog` also mirrors the signed catalog files (head, deltas,
  snapshots, signatures, `chain-proof.json`, `mirrors.toml`) and serves them
  under `/<catalog>/`, for example `/tensorseed/head.json`. Clients still
  verify every file and check the head against the chain, so your copy is
  just another cache.

For a public mirror, put a TLS terminating reverse proxy (Caddy, nginx,
Cloudflare Tunnel) in front of the HTTP port and publish the HTTPS URL.
Keep long cache lifetimes only for `/t/<content name>/...` payloads (they
are immutable); `head.json`, `chain-proof.json`, `mirrors.toml`, and
`.torrent` files change and must not be cached for long.

## 7. Optional: S3 compatible storage

If you have object storage with free or cheap egress (Cloudflare R2,
Backblaze B2 behind a CDN, Wasabi, Vultr, MinIO, any S3 compatible service),
you can host a mirror without keeping a machine online. A mirror is a static
tree with the same layout as `serve`'s data directory:

```
<public base URL>/t/<content name>/<files>      payload, immutable
<public base URL>/t/<content name>.torrent      torrent, mutable
<public base URL>/tensorseed/...                signed catalog files (optional)
```

The simplest setup runs `tensorseed serve --serve-catalog` (with
`http_listen` set, even on 127.0.0.1) on the trees you already pulled, and
copies that data directory to the bucket on a timer, for example with
rclone. Add `--fetch` only if you also want auto-follow (section 3).

```sh
D=~/.local/share/tensorseed/serve   # Windows: %LOCALAPPDATA%\tensorseed\serve
rclone sync "$D/t" r2:my-bucket/t \
  --header-upload "Cache-Control: public, max-age=31536000, immutable" --exclude "*.torrent"
rclone copy "$D/t" r2:my-bucket/t --include "*.torrent" \
  --header-upload "Cache-Control: public, max-age=300"
rclone sync "$D/catalog/tensorseed" r2:my-bucket/tensorseed \
  --header-upload "Cache-Control: public, max-age=60"
```

Run the copy at least daily so the mirrored `head.json` stays fresh (heads
expire after 7 days unless the publisher re-signs them; clients simply skip
an expired or stale head and try the next source). Credentials stay in
rclone's own config or environment, never in `tensorseed.toml`.

If you also publish your own catalog, use `[[storage.target]]` blocks
instead: `tensorseed harvest` and `publish` upload to every enabled target.
See [PUBLISH-A-CATALOG.md](PUBLISH-A-CATALOG.md),
`config/tensorseed.example.toml`, and SPEC section 10.1.

## 8. Getting your mirror listed

There are two kinds of listing, and only HTTP mirrors need one:

- **BitTorrent seeders need no listing.** Peers find you through the DHT, any
  trackers listed in the torrents, and peer exchange. Just keep `serve`
  running with the port open.
- **HTTP and S3 mirrors** are listed in the publisher signed
  `tensorseed/mirrors.toml`, which every client fetches, verifies against
  the pinned key, and uses to rank download sources. To be added, open an
  issue or pull request on the project repository with:
  - the public base URL (HTTPS, ending in `/`), whose `t/` and, if you serve
    it, `tensorseed/` paths follow the layout above;
  - a short name and region (for example `eu-west-1`);
  - which repos you carry and roughly how much bandwidth you can offer;
  - a contact for outages.

  The catalog owner checks the mirror (for example by adding it to a local
  list and running `tensorseed pull <repo> --no-hf-fallback`), then adds it
  with
  `tensorseed mirror add <url> --name <name> --priority <n>` and signs a new
  list with `tensorseed mirror publish`. Clients pick it up at their next
  sync.

Anyone can also use an unlisted mirror right away by adding it to their own
local list (`tensorseed mirror add <url>`); it is verified exactly like a
listed one.

To ask for a Hugging Face repo to be published in the official catalog,
open a "Request a catalog mirror" issue on the project repository. Gated
and restricted-license repos are refused. That is a request to the official
publisher; it does not write to the catalog yourself.

## 9. Run it as a service (systemd)

[`contrib/systemd/`](../contrib/systemd/) has a hardened unit and a minimal
config that follows the official catalog with strict chain checking:

```sh
sudo install -m 0755 tensorseed /usr/local/bin/tensorseed
sudo install -d -m 0755 /etc/tensorseed
sudo install -m 0644 contrib/systemd/tensorseed.toml /etc/tensorseed/tensorseed.toml
sudo install -m 0644 contrib/systemd/tensorseed-serve.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now tensorseed-serve
journalctl -u tensorseed-serve -f
```

The unit runs as a dynamic user with a private `/var/lib/tensorseed` state
directory, a read only root filesystem, and a restricted system call set.
Edit `/etc/tensorseed/tensorseed.toml` for what you seed (pull first, or
turn on auto-follow with a narrow `select` and a disk budget); if you seed
a tree outside the state directory, add a
`ReadOnlyPaths=` (or `ReadWritePaths=`) line as the unit's comments explain.

## 10. Monitoring: the health endpoint

`serve` answers on `serve.health_listen` (default `127.0.0.1:6880`):

```sh
curl -s http://127.0.0.1:6880/health | jq .
curl -s http://127.0.0.1:6880/healthz      # "ok" or "degraded"
```

`/health` returns a `ts.serve.health/1` JSON document: uptime; catalog
revision, root, head expiry, and last sync result; swarm listen address,
uTP and DHT state, bytes up and down, peers, and limits; every seeded
torrent with its info hash, size, uploaded bytes, and peers; disk used
against the budget; downloads in progress; skipped revisions with reasons;
takedowns applied; HTTP counters; and port mapping state. Both endpoints
answer 503 when the last sync failed and the head has expired, which makes
them easy to wire into uptime checks. Keep the endpoint on localhost; it is
meant for you, not the internet.

## 11. Operating notes

- **Disk**: revisions are stored as plain files under
  `<data_dir>/t/<content name>/`, so you can inspect them, and
  `tensorseed verify <dir>` re-hashes any copy.
- **Updates**: replace the binary and restart; `serve` resumes from
  `serve-state.json`.
- **Pickle files** (`.bin`, `.pt`, ...) are skipped on download unless
  explicitly allowed, and gated repos are not mirrored.
- **Legal**: the catalog only carries repos whose licenses allow
  redistribution, and takedowns propagate automatically. You are still
  responsible for what you serve where you live; use `exclude`, `types`,
  and `license_classes` to narrow it.
- **Help**: `tensorseed doctor` (when implemented) and
  `tensorseed serve -v` for debug logs; issues on the project repository.

## 12. Publish your own catalog

Seeding what you pulled does not make you a catalog publisher, and nothing
in this guide writes to the official tensorseed.dev catalog. If you want
your own signed, chain-anchored catalog of Hugging Face repos, follow
[PUBLISH-A-CATALOG.md](PUBLISH-A-CATALOG.md). Clients follow it by
adding a `[[catalogs]]` entry with your key, catalog name, store id, and
base URL (or `tensorseed catalog add`). Since 0.2.0 one client can follow
several catalogs at once.
