Tensorseed catalog host
=======================

This host is the public base URL for the signed static-file catalog backend
described in docs/SPEC.md section 6.8 (file backend) and 6.12 (mirror lists).

  base_url = https://catalog.tensorseed.dev/
  catalog  = tensorseed

The catalog is live at head revision 4, anchored on Chia mainnet.
Revision 3 added the official HTTP webseed
https://mirror.tensorseed.dev/t/ (Cloudflare R2, bucket tensorseed-mirror).
Revision 4 adds eight official entries (seven models and one dataset),
all ungated, Apache-2.0 or MIT. Together with the existing first entry
they are:

  TunedChaos/ChaosNexus_Tuned_v1-GGUF
    commit b43d53d2b9755c4b3c554ba66f7c31d3cf35114d, 5.36 GB,
    license recorded as unknown (upstream tag "other", allowed by an
    explicit publisher override)
  openai/gsm8k (dataset)
    commit 740312add88f781978c0658806c59bc2815b9866, 5.90 MB, mit
  BAAI/bge-small-en-v1.5
    commit 5c38ec7c405e, 0.27 GB, mit
  sentence-transformers/all-MiniLM-L6-v2
    commit 1110a243fdf4, 0.77 GB, apache-2.0
  openai/whisper-small
    commit 973afd24965f, 2.91 GB, apache-2.0
  TinyLlama/TinyLlama-1.1B-Chat-v1.0
    commit fe8a4ea1ffed, 2.20 GB, apache-2.0
  google-bert/bert-base-uncased
    commit 86b5e0934494, 2.48 GB, apache-2.0
  openai-community/gpt2
    commit 607a30d783df, 5.08 GB, mit
  Qwen/Qwen2.5-1.5B-Instruct-GGUF
    commit 91cad51170dc, 13.32 GB, apache-2.0

Use the full commit from `tensorseed list` or `show` when pinning. The
byte counts above are the published file totals, which can be smaller
than Hugging Face usedStorage when a Hub repo also carries other
exports not included here.

Pin this publisher public key (minisign, key id CE1E0E3D543DFFB8):

  RWS4/z1UPQ4ezmjrcpKqDYpqyn5h1WUS2oDs1B73Dh91kVzzNIZd03xd

Chia mainnet DataLayer store that anchors every catalog revision
(SPEC 6.14; Chia is the source of truth, these files are a cache):

  3dcd43dced47a32f8e38e8ec77f583d8d1bafe1ab24e381538e50a36034b7a76

Clients: set catalog.backend = "file", catalog.name = "tensorseed",
catalog.publisher_pubkey to the key above, file.base_url to this host,
chia.catalog_store_id to the store above, catalog.chain_check = "strict",
and chia.coin_api_urls = ["https://api.coinset.org"], then run
`tensorseed catalog chain-status`, `catalog info`, `list`, `show`, `pull`,
and `verify`.
Every head, delta, snapshot, and mirrors.toml is signature checked against
that key. Model bytes are not hosted on Pages (the per-file limit is 25 MiB).
Pull uses the BitTorrent swarm first, then the HTTP mirror at
https://mirror.tensorseed.dev/, then the Hugging Face Hub.

Layout (SPEC 6.8, 6.12)
-----------------------

  <base_url>/<catalog>/head.json                  (+ head.json.minisig)
  <base_url>/<catalog>/deltas/<revision:010>.json (+ .minisig)
  <base_url>/<catalog>/snapshots/<revision:010>.json (+ .minisig)
  <base_url>/<catalog>/mirrors.toml               (+ mirrors.toml.minisig)
  <base_url>/<catalog>/chain-proof.json           (Chia anchor proof, unsigned:
                                                   every field is checked)

- head.json is rewritten each revision and is re-signed before its expiry.
  Cache it briefly.
- deltas and snapshots are append-only: a published file is never modified
  or deleted. They may be cached as immutable.
- The trusted comment on head, delta, and snapshot signatures is
  ts.file/1 kind=<head|delta|snapshot> catalog=<name> rev=<n> root=<hex>.
- mirrors.toml is the publisher-signed list (ts.mirrors/1). Its signature
  trusted comment is ts.mirrors/1 catalog=<name> sha256=<hex of the body>.
  The current list advertises https://mirror.tensorseed.dev/. The list can
  be updated and re-signed later without rotating the publisher key.

Webseeds and torrent files (SPEC 7.1, 7.2, 10.1)
------------------------------------------------

  <base_url>t/<content name>.torrent     BitTorrent v2 hybrid metainfo
  https://mirror.tensorseed.dev/t/<content name>/   object bytes (R2)

Content name form: {org}__{name}@{commit[0:12]}

Torrents for every official revision live under /t/ on this host and
on the object mirror, named {org}__{name}@{commit[0:12]}.torrent. The
first entry remains:

  https://catalog.tensorseed.dev/t/TunedChaos__ChaosNexus_Tuned_v1-GGUF@b43d53d2b975.torrent
  https://mirror.tensorseed.dev/t/TunedChaos__ChaosNexus_Tuned_v1-GGUF@b43d53d2b975.torrent

This Pages origin does not serve the content directory under
/t/<content name>/. Treat https://mirror.tensorseed.dev/ as the HTTP
webseed for model bytes.

Schemas
-------

  head       ts.filehead/1
  delta      ts.filedelta/1
  snapshot   ts.filesnapshot/1
  mirrors    ts.mirrors/1

Copyright Tuned Chaos. Licensed under AGPL-3.0-or-later.
