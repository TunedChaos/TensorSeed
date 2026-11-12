Tensorseed catalog host
=======================

This host is the public base URL for the signed static-file catalog backend
described in docs/SPEC.md section 6.8 (file backend) and 6.12 (mirror lists).

  base_url = https://catalog.tensorseed.dev/

The signed catalog is not published yet. Until the first revision is written,
this directory only holds this README and a placeholder index. Model and
dataset bytes will not be stored on Cloudflare Pages (the per-file limit is
25 MiB). This host is for catalog metadata, signatures, and torrent files.

Layout (one directory per logical catalog name)
-----------------------------------------------

  <base_url>/<catalog>/head.json                  (+ head.json.minisig)
  <base_url>/<catalog>/deltas/<revision:010>.json (+ .minisig)
  <base_url>/<catalog>/snapshots/<revision:010>.json (+ .minisig)
  <base_url>/<catalog>/mirrors.toml               (+ mirrors.toml.minisig)

Where <catalog> is the logical catalog name (not this README's folder name)
and <revision:010> is the revision number padded to 10 digits.

- head.json is rewritten each revision and is also re-signed before its
  expiry (file.head_ttl_secs). Cache it briefly.
- deltas and snapshots are append-only: a published file is never modified
  or deleted. They may be cached as immutable.
- Every head, delta, and snapshot has a detached minisign signature
  (head.json.minisig, and so on). The trusted comment is
  ts.file/1 kind=<head|delta|snapshot> catalog=<name> rev=<n> root=<hex>.
- mirrors.toml is the publisher-signed mirror list (ts.mirrors/1) from
  SPEC 6.12. Its signature is mirrors.toml.minisig, with trusted comment
  ts.mirrors/1 catalog=<name> sha256=<hex of the body>.

Webseeds and torrent files (SPEC 7.1, 7.2, 10.1)
------------------------------------------------

  <base_url>t/<content name>/            content files (not hosted on Pages)
  <base_url>t/<content name>.torrent     BitTorrent v2 or hybrid metainfo

Content name form: {org}__{name}@{commit[0:12]}

Schemas
-------

  head       ts.filehead/1
  delta      ts.filedelta/1
  snapshot   ts.filesnapshot/1
  mirrors    ts.mirrors/1

Clients should pin catalog.publisher_pubkey, verify every signature, and
follow the hash chain in SPEC 6.8. A missing, tampered, or stale head must
not be trusted.

Copyright Tuned Chaos. Licensed under AGPL-3.0-or-later.
