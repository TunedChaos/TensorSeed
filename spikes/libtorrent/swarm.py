#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright 2026 Tuned Chaos
"""M0 spike: libtorrent 2.x capabilities on loopback (no DHT, trackers, LSD).

Usage: swarm.py <data parent dir> <torrents dir> [external seeder host:port]
Prints RESULT lines. With an external seeder (for example the librqbit spike in
serve mode) it also checks libtorrent downloading a hybrid torrent from it.
"""
import filecmp
import os
import shutil
import sys
import tempfile
import time

import libtorrent as lt

parent, tdir = sys.argv[1], sys.argv[2]
external = sys.argv[3] if len(sys.argv) > 3 else None
SETTINGS = {
    "enable_dht": False,
    "enable_lsd": False,
    "enable_upnp": False,
    "enable_natpmp": False,
    "allow_multiple_connections_per_ip": True,
    "alert_mask": lt.alert.category_t.error_notification | lt.alert.category_t.status_notification,
}


def session(port):
    s = dict(SETTINGS)
    s["listen_interfaces"] = f"127.0.0.1:{port}"
    return lt.session(s)


def wait(h, pred, timeout):
    t0 = time.time()
    while time.time() - t0 < timeout:
        st = h.status()
        if st.errc.value() != 0:
            raise RuntimeError(f"torrent error: {st.errc.message()}")
        if pred(st):
            return time.time() - t0, st
        time.sleep(0.1)
    st = h.status()
    raise TimeoutError(f"timeout {timeout}s: {st.total_wanted_done}/{st.total_wanted} state={st.state}")


def same_tree(a, b):
    n = 0
    for root, _, files in os.walk(a):
        for f in files:
            p = os.path.join(root, f)
            q = os.path.join(b, os.path.relpath(p, a))
            if not filecmp.cmp(p, q, shallow=False):
                raise RuntimeError(f"differs: {q}")
            n += 1
    return n


def seed(ses, ti, save):
    # Seed in place: files already exist under save/<name>/. A recheck hashes
    # them; flags.seed_mode would skip even that (trust on first use).
    p = lt.add_torrent_params()
    p.ti = ti
    p.save_path = save
    h = ses.add_torrent(p)
    dt, _ = wait(h, lambda st: st.is_seeding, 120)
    return h, dt


def fetch(ses, save, timeout, ti=None, magnet=None, peer=None, url_seeds=True):
    p = lt.parse_magnet_uri(magnet) if magnet else lt.add_torrent_params()
    if ti is not None:
        p.ti = ti
    p.save_path = save
    h = ses.add_torrent(p)
    if not url_seeds:
        for u in h.url_seeds():
            h.remove_url_seed(u)
    if peer:
        host, port = peer.rsplit(":", 1)
        h.connect_peer((host, int(port)))
    return wait(h, lambda st: st.is_seeding, timeout)


def result(name, fn):
    try:
        print(f"RESULT\t{name}\tOK\t{fn()}", flush=True)
    except Exception as e:  # noqa: BLE001
        print(f"RESULT\t{name}\tFAIL\t{type(e).__name__}: {e}", flush=True)


work = tempfile.mkdtemp(prefix="lt-spike-")
ti = {k: lt.torrent_info(os.path.join(tdir, f"{k}.torrent")) for k in ("v1", "v2", "hybrid")}
name = ti["v2"].name()
content = os.path.join(parent, name)
print(f"libtorrent {lt.__version__}")

seeder = session(17101)
seed_handles = {}
for kind in ("v2", "hybrid"):
    def go(kind=kind):
        h, dt = seed(seeder, ti[kind], parent)
        seed_handles[kind] = h
        return f"recheck={dt:.2f}s"
    result(f"{kind}_seed_in_place", go)

for kind in ("v2", "hybrid"):
    def go(kind=kind):
        out = os.path.join(work, f"peer-{kind}")
        dt, st = fetch(session(0), out, 120, ti=lt.torrent_info(ti[kind]), peer="127.0.0.1:17101", url_seeds=False)
        return f"download={dt:.2f}s bytes={st.total_wanted_done} files_identical={same_tree(content, os.path.join(out, name))}"
    result(f"{kind}_peer_download", go)

for kind in ("v2", "hybrid"):
    def go(kind=kind):
        out = os.path.join(work, f"ws-{kind}")
        dt, st = fetch(session(0), out, 180, ti=lt.torrent_info(ti[kind]))
        return f"download={dt:.2f}s bytes={st.total_wanted_done} files_identical={same_tree(content, os.path.join(out, name))}"
    result(f"{kind}_webseed_only", go)


def go_magnet():
    out = os.path.join(work, "magnet-btmh")
    m = f"magnet:?xt=urn:btmh:1220{ti['v2'].info_hashes().v2}&dn={name}"
    dt, st = fetch(session(0), out, 120, magnet=m, peer="127.0.0.1:17101")
    return f"download={dt:.2f}s bytes={st.total_wanted_done} files_identical={same_tree(content, os.path.join(out, name))}"


result("magnet_btmh_only", go_magnet)

if external:
    def go_ext():
        out = os.path.join(work, "from-external")
        dt, st = fetch(session(0), out, 120, ti=lt.torrent_info(ti["hybrid"]), peer=external, url_seeds=False)
        return f"download={dt:.2f}s bytes={st.total_wanted_done} files_identical={same_tree(content, os.path.join(out, name))}"
    result("hybrid_from_external_seeder", go_ext)

shutil.rmtree(work, ignore_errors=True)
