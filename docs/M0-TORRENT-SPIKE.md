# M0 BitTorrent spike: results

Date: 2026-11-12. Question from [SPEC.md](SPEC.md) section 12 and
[MILESTONES.md](MILESTONES.md): can `librqbit` carry Tensorseed's content layer
(BitTorrent v2, hybrid torrents, BEP 19 webseeds), and if not, what fills the
gaps? Spike programs and rerun steps: [`spikes/`](../spikes/README.md).

## Setup

- **librqbit 9.1.0** (latest on crates.io, Apache-2.0), `rust-tls` feature,
  TCP only, no DHT, trackers, or LSD.
- **libtorrent-rasterbar 2.0.11** (Debian trixie `python3-libtorrent`,
  BSD-3-Clause) as the reference v2 implementation.
- Everything on one machine over loopback (`127.0.0.1`), two independent
  sessions per test, no public trackers or DHT.
- Test content: one directory `org__tiny@0123456789ab` with four random files,
  276,865,064 bytes (200 MiB and 64 MiB "shards", a 40,000 byte file in a
  subdirectory, a 1,000 byte file). 4 MiB pieces. The hybrid v1 view with
  BEP 47 pad files is 285,212,672 bytes.
- Webseed host: `spikes/webseed_server.py`, a static server with Range
  support serving the spec layout `t/<content name>/<path>` and logging every
  request.
- Edge case fixture for the builder: empty files, a file and a directory that
  sort next to each other (`a.txt`, `a/`), files of exactly 16 KiB and 64 KiB,
  odd sizes, a non ASCII file name, an upper case name.

Loopback timings only show that transfers complete; they say nothing about
real network throughput.

## Results

| # | Capability | librqbit 9.1.0 | libtorrent 2.0.11 | Tensorseed (pure Rust, this repo) |
|---|---|---|---|---|
| a1 | Create v2 torrent | **Fail.** `create_torrent` builds v1 only (`TorrentMetaV1Info`; no `file tree`, `piece layers`, `meta version`). | Pass | **Pass.** `tensorseed-torrent::build_torrent`; info hashes identical to libtorrent in all 14 cases checked (piece 16 KiB to 16 MiB, v2 and hybrid, edge case fixture and 265 MB set). |
| a2 | Download v2 only torrent | **Fail.** Rejected on load: `error decoding torrent: missing field 'pieces'`. librqbit-core declares v2 error types but nothing uses them. | Pass (peer download 2.5 s, 4/4 files identical) | n/a (engine is librqbit) |
| a3 | Seed v2 only torrent | **Fail** (same load error) | Pass (in place, recheck 1.1 s) | n/a |
| b1 | Create hybrid v1+v2 | **Fail** (v1 only) | Pass | **Pass.** Same layout as libtorrent: canonical file order, `.pad/<n>` after every file not ending on a piece boundary, last file included. |
| b2 | Seed and download hybrid | **Partial.** Works through the v1 side: seed in place (check 0.30 s), download 1.11 s, 4/4 identical, pad files never written to disk. Pieces are checked with SHA-1 only; v2 Merkle data is ignored. | Pass (9.1 s) | Pass with librqbit (engine test on our own torrents) |
| b3 | Hybrid interop between the two | librqbit downloads from a libtorrent seeder: **pass** (1.2 s, identical). libtorrent downloads from a librqbit seeder: **pass** (6.2 s, identical). | | |
| c | BEP 19 webseed with no peers | **Fail.** 0 of 285,212,672 bytes after 30 s and not one HTTP request reached the server. The source has no `url-list` handling at all. | Pass: v2 1.0 s, hybrid 1.0 s, and a Tensorseed built hybrid 1.1 s, all from webseed alone (ranged GETs in the server log). | Our own HTTP fetcher (`tensorseed-fetch`) already downloads the same layout with SHA-256 checks and resume. |
| d | Seed from an existing directory without copying | **Pass** (`output_folder` = content directory, `overwrite = true`; 265 MB checked in 0.3 to 0.4 s, nothing copied) | Pass (`save_path`, recheck about 1 s) | `tensorseed seed` uses this. |
| e | Multi-file torrent, large files | **Pass** (v1, 276,865,064 bytes in 1.2 s over loopback, files identical) | Pass | Builder hashes the 265 MB set in 0.6 s (release, one thread per file). |
| f1 | `btmh` only magnet | **Fail.** `magnet link didn't contain a BTv1 infohash` | Pass (65.7 s; slow metadata exchange, not investigated) | n/a |
| f2 | Hybrid magnet (`btih` + `btmh`) | **Pass** (uses `btih`; 1.2 s with an initial peer) | Pass | Manifests carry this form. |

Raw lines from the runs (abridged):

```
spike-rqbit (librqbit 9.1.0)
RESULT v1_create_seed_download OK   infohash_v1=0b16bf35... seed_check=303ms download=1.18s bytes=276865064/276865064 files_identical=4
RESULT v2_only_load            FAIL error decoding torrent: missing field `pieces`
RESULT hybrid_seed_in_place    OK   check=303ms
RESULT hybrid_download         OK   download=1.11s bytes=285212672/285212672 files_identical=4 pad_files_on_disk=false
RESULT webseed_only_hybrid     FAIL timed out after 30s: 0 / 285212672 bytes   (webseed server log: 0 requests)
RESULT magnet_btmh_only        FAIL magnet link didn't contain a BTv1 infohash
RESULT magnet_btih_plus_btmh   OK   download=1.24s files_identical=4
RESULT rqbit_get (from libtorrent hybrid seeder) OK download=1.20s
swarm.py (libtorrent 2.0.11)
RESULT v2_seed_in_place OK recheck=1.10s      RESULT hybrid_seed_in_place OK recheck=1.00s
RESULT v2_peer_download OK 2.51s identical=4  RESULT hybrid_peer_download OK 9.13s identical=4
RESULT v2_webseed_only  OK 1.01s identical=4  RESULT hybrid_webseed_only  OK 1.01s identical=4
RESULT magnet_btmh_only OK 65.73s identical=4
RESULT libtorrent_from_rqbit_hybrid OK 6.21s
RESULT libtorrent_webseed_only_tensorseed_hybrid OK 1.08s identical=4
crosscheck.py: 14 x MATCH (v1 and v2 info hashes, libtorrent vs Tensorseed)
```

## libtorrent-rasterbar as the engine instead

- **Capabilities:** everything Tensorseed wants (v2, hybrid, webseeds, `btmh`).
- **Rust bindings on crates.io:** `libtorrent-sys` 1.0.0 (cxx), `lt-rs` 0.1.4,
  `libtorrent` 0.1.1, all MIT and all small, young projects. Each needs a C++
  toolchain, Boost, and the libtorrent headers at build time, and links
  native code. Not built in this spike.
- **Sidecar:** run libtorrent through its Python bindings or a small C++
  helper as a subprocess and drive it over a pipe. No unsafe code in our
  crates, but a second runtime and packaging work on every OS.
- **Licenses:** libtorrent-rasterbar is BSD-3-Clause and the bindings are MIT,
  both compatible with AGPL-3.0-or-later and already allowed in `deny.toml`.
  Boost is BSL-1.0, also compatible; it would only need adding to `deny.toml`
  if a Rust crate declared it. cargo-deny cannot see native libraries, so
  their licenses would have to be tracked by hand.
- **Cost:** a native dependency breaks the single static Rust binary goal
  (spec G7) and complicates Windows and macOS builds.

## Pure Rust v2 creation

Done in this round, about 1,500 lines with tests (`bencode.rs`, `merkle.rs`,
`metainfo.rs` in `tensorseed-torrent`), no new dependencies beyond `sha1`,
`sha2`, and `hex`, which the workspace already had:

- BEP 52 Merkle trees per file (16 KiB leaves, zero padding, piece layers),
  plus aligned v1 SHA-1 pieces for hybrids and the plain SHA-256, all in one
  read of each file.
- Deterministic output (no creation date), so a manifest's info hashes can be
  rebuilt by anyone from the files.
- A strict parser that rejects v1 only torrents, bad piece layers, v1 and v2
  views that disagree, and unsafe paths, and checks a torrent against a
  manifest (`Metainfo::check_manifest`).
- Unit tests pin the info hashes libtorrent 2.0.11 computes for a
  deterministic fixture.

## Recommendation (adopted)

1. **Create torrents ourselves** with the pure Rust builder. v2 and hybrid,
   byte compatible with libtorrent.
2. **Hybrid by default** (`torrent.hybrid = true`). The built-in engine needs
   the v1 side, and v1 only clients can join. v2 only torrents can still be
   built for other clients.
3. **librqbit as the swarm engine for now** (`tensorseed seed`,
   `pull --swarm`, `pull --seed-after`). It interoperates with v2 capable
   peers in the same hybrid swarm, keeps the binary pure Rust, and passes
   `cargo deny`.
4. **Webseeds over our own HTTP fetcher.** Tensorseed clients get webseed
   bytes from `tensorseed-fetch` (already verified, resumable, mirror
   ranked). The torrents still list the webseeds in `url-list`, so libtorrent,
   qBittorrent, and other BEP 19 clients use them directly (verified with
   libtorrent).
5. **Integrity does not depend on the engine.** librqbit checks SHA-1 pieces
   in transit. After that, every file is hashed with SHA-256 against the signed
   manifest before it counts as present (SHA-1 collisions cannot get past
   this), and manifests record each file's BEP 52 root, which
   `Metainfo::verify_file` can recheck.
6. **Keep libtorrent as a later option** (sidecar first) if v2 only swarms,
   `btmh` only magnets, or v2 block verification in transit turn out to
   matter. Watch librqbit's v2 tracking issue
   (https://github.com/ikatson/rqbit/issues/546); switching engines does not
   change torrents or manifests.

Decisions recorded for M0:

- **Default piece length: auto** (`torrent.piece_length = 0`): 4 MiB, doubled
  until a revision has at most 16,384 pieces, capped at 64 MiB. 4 MiB up to
  64 GiB per revision, 8 MiB at 100 GiB, 64 MiB at 1 TiB. This keeps the
  `.torrent` (32 bytes per v2 piece plus 20 per v1 piece) around 1 MiB or less.
- **Hybrid on** by default, for the reasons above.

## What was implemented

- `tensorseed-torrent`: builder, parser, Merkle verification, magnet links,
  and the librqbit `engine` (feature, on by default): `Engine::seed` (in
  place, refuses missing or corrupt files, optional file subset) and
  `Engine::download` (in place, peers, file subset, timeout).
- `harvest` builds the torrent after a verified download, records each
  file's `bt_v2_pieces_root` and the manifest `torrent` block (info hashes,
  hybrid magnet, `t/<name>.torrent` URL under every webseed base), writes
  `t/<content name>.torrent` next to the content, and uploads it to every
  storage target.
- `tensorseed seed [--dir DIR]... [--for 2h]`: seeds every revision under
  `storage.local_dir` and every enabled `provider = "torrent"` storage target
  `dir`, or the given pulled directories or trees.
- `pull --swarm [--peer host:port]`: fetches and checks the manifest's
  torrent, downloads from the swarm for up to `torrent.swarm_timeout_secs`,
  then the HTTP pass hashes every file and fetches whatever is missing or
  wrong. `--seed-after <duration>` seeds the verified result.
- Loopback tests: engine seed and download with per file Merkle checks;
  refusal of v2 only and corrupt seeds; an end to end CLI test where
  `harvest` publishes, `tensorseed seed` serves, and a subscriber pulls with
  `--swarm` (every file then "already present" for the HTTP pass) and seeds
  after.
- `cargo deny`: librqbit's tree passes the license allow list. One advisory is
  ignored with a reason: RUSTSEC-2026-0293 in `ringbuf` 0.4 (pinned by
  `librqbit-utp`) needs an element whose `Drop` panics; librqbit only stores
  `u8`. librqbit adds about 110 crates to the CLI build.

## Limits and next steps

- A file the swarm only partly fetched is downloaded again in full over HTTP
  (the fetcher sees a wrong hash). Next: resume HTTP at the first missing
  piece instead.
- `seed` does not yet check tombstones, selection rules, or disk budgets
  (M3), and `--serve-catalog` is still a stub.
- No uTP, UPnP, or NAT-PMP yet: seeders need a reachable TCP port
  (`torrent.listen_port`).
- Catalog snapshot torrents (`[[mirror]] provider = "torrent"`) stay
  reserved.
- Retest librqbit when a release closes issue 546; if it gains v2 and
  webseeds, `pull --swarm` can drop the HTTP gap fill for webseeds.
- Measure real network throughput between two hosts, and the `.torrent` size
  for a large (100 GB or more) revision.
