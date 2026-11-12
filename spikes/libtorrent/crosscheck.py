#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright 2026 Tuned Chaos
"""M0 spike: compare Tensorseed's torrent builder with libtorrent 2.x.

Usage: crosscheck.py <mktorrent binary> <content dir> <piece length>...
Builds v2 and hybrid torrents with both and compares info hashes and whether
libtorrent loads the Tensorseed torrent.
"""
import os
import subprocess
import sys
import tempfile

import libtorrent as lt

exe, content = sys.argv[1], sys.argv[2]
parent, name = os.path.split(os.path.abspath(content))
ok = True
for pl in map(int, sys.argv[3:]):
    for kind, flags in (("v2", lt.create_torrent.v2_only), ("hybrid", 0)):
        fs = lt.file_storage()
        lt.add_files(fs, os.path.join(parent, name))
        ct = lt.create_torrent(fs, pl, flags=flags)
        lt.set_piece_hashes(ct, parent)
        ref = lt.torrent_info(lt.bencode(ct.generate()))
        out = tempfile.mktemp(suffix=".torrent")
        subprocess.run([exe, content, out, str(pl), kind], check=True, capture_output=True)
        ours = lt.torrent_info(out)
        r, o = ref.info_hashes(), ours.info_hashes()
        same = str(r.v2) == str(o.v2) and (kind == "v2" or str(r.v1) == str(o.v1))
        ok &= same
        print(f"{'MATCH' if same else 'DIFFER'}\tpiece={pl}\t{kind}\tlibtorrent v1={r.v1 if r.has_v1() else '-'} v2={r.v2}\ttensorseed v1={o.v1 if o.has_v1() else '-'} v2={o.v2}")
        os.unlink(out)
sys.exit(0 if ok else 1)
