Tensorseed catalog host
=======================

This host is the public base URL for the signed static-file catalog backend
described in docs/SPEC.md section 6.8 (file backend) and 6.12 (mirror lists).

  base_url = https://catalog.tensorseed.dev/
  catalog  = tensorseed

The catalog is live. Head revision 2 is published. The first entry is
TunedChaos/ChaosNexus_Tuned_v1-GGUF at commit
b43d53d2b9755c4b3c554ba66f7c31d3cf35114d, license recorded as unknown
(upstream tag "other", allowed by an explicit publisher override).

Pin this publisher public key (minisign, key id CE1E0E3D543DFFB8):

  RWS4/z1UPQ4ezmjrcpKqDYpqyn5h1WUS2oDs1B73Dh91kVzzNIZd03xd

Clients: set catalog.backend = "file", catalog.name = "tensorseed",
catalog.publisher_pubkey to the key above, and file.base_url to this host,
then run `tensorseed catalog info`, `list`, `show`, `pull`, and `verify`.
Every head, delta, snapshot, and mirrors.toml is signature checked against
that key. Model bytes are not hosted on Pages (the per-file limit is 25 MiB).
Until an object mirror is online, pull uses the Hugging Face Hub as the HTTP
source after verifying the signed manifest.

Layout (SPEC 6.8, 6.12)
-----------------------

  <base_url>/<catalog>/head.json                  (+ head.json.minisig)
  <base_url>/<catalog>/deltas/<revision:010>.json (+ .minisig)
  <base_url>/<catalog>/snapshots/<revision:010>.json (+ .minisig)
  <base_url>/<catalog>/mirrors.toml               (+ mirrors.toml.minisig)

- head.json is rewritten each revision and is re-signed before its expiry.
  Cache it briefly.
- deltas and snapshots are append-only: a published file is never modified
  or deleted. They may be cached as immutable.
- The trusted comment on head, delta, and snapshot signatures is
  ts.file/1 kind=<head|delta|snapshot> catalog=<name> rev=<n> root=<hex>.
- mirrors.toml is the publisher-signed list (ts.mirrors/1). Its signature
  trusted comment is ts.mirrors/1 catalog=<name> sha256=<hex of the body>.
  The list can be updated and re-signed later (for example to add
  https://mirror.tensorseed.dev/ once the R2 bucket tensorseed-mirror is
  filled) without rotating the publisher key and without rewriting
  catalog entries.

Webseeds and torrent files (SPEC 7.1, 7.2, 10.1)
------------------------------------------------

  <base_url>t/<content name>.torrent     BitTorrent v2 hybrid metainfo

Content name form: {org}__{name}@{commit[0:12]}

The first torrent is:

  https://catalog.tensorseed.dev/t/TunedChaos__ChaosNexus_Tuned_v1-GGUF@b43d53d2b975.torrent

This host does not serve the content directory under /t/<content name>/.
Do not treat this origin as a webseed for model bytes. A future HTTP
mirror at https://mirror.tensorseed.dev/ will use the standard
"<url>t/" object layout once it is published in mirrors.toml.

Schemas
-------

  head       ts.filehead/1
  delta      ts.filedelta/1
  snapshot   ts.filesnapshot/1
  mirrors    ts.mirrors/1

Copyright Tuned Chaos. Licensed under AGPL-3.0-or-later.
