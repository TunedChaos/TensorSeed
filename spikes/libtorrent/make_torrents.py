#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright 2026 Tuned Chaos
"""M0 spike: build reference v1, v2 and hybrid torrents with libtorrent 2.x.

Usage: make_torrents.py <parent dir> <content name> <out dir> <webseed base> [piece length]
"""
import os
import sys

import libtorrent as lt

parent, name, out, webseed = sys.argv[1:5]
piece = int(sys.argv[5]) if len(sys.argv) > 5 else 4 * 1024 * 1024
os.makedirs(out, exist_ok=True)
for kind, flags in (("v1", lt.create_torrent.v1_only), ("v2", lt.create_torrent.v2_only), ("hybrid", 0)):
    fs = lt.file_storage()
    lt.add_files(fs, os.path.join(parent, name))
    ct = lt.create_torrent(fs, piece, flags=flags)
    ct.set_creator("tensorseed-m0-spike")
    if webseed:
        ct.add_url_seed(webseed)
    lt.set_piece_hashes(ct, parent)
    data = lt.bencode(ct.generate())
    path = os.path.join(out, f"{kind}.torrent")
    with open(path, "wb") as f:
        f.write(data)
    ti = lt.torrent_info(path)
    ih = ti.info_hashes()
    v1 = str(ih.v1) if ih.has_v1() else "-"
    v2 = str(ih.v2) if ih.has_v2() else "-"
    print(f"{kind}\tv1={v1}\tv2={v2}\tmagnet={lt.make_magnet_uri(ti)}")
