Tensorseed catalog host
=======================

This host is the public base URL for the signed static-file catalog backend
described in docs/SPEC.md section 6.8 (file backend) and 6.12 (mirror lists).

  base_url = https://catalog.tensorseed.dev/
  catalog  = tensorseed

The catalog is live at head revision 3. The first entry is
TunedChaos/ChaosNexus_Tuned_v1-GGUF at commit
b43d53d2b9755c4b3c554ba66f7c31d3cf35114d, license recorded as unknown
(upstream tag "other", allowed by an explicit publisher override).
Revision 3 adds the official HTTP webseed
https://mirror.tensorseed.dev/t/ (Cloudflare R2, bucket tensorseed-mirror).

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
Pull should fetch objects from https://mirror.tensorseed.dev/ first, then
fall back to the Hugging Face Hub if needed.

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

The first torrent is:

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
