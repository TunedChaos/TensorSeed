// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! M0 spike: what librqbit 9.1.0 can do for Tensorseed.
//!
//! Usage: spike-rqbit <data parent dir> <content name> <torrents dir> <webseed port>
//! Expects v1/v2/hybrid.torrent from spikes/libtorrent/make_torrents.py and an
//! HTTP server on 127.0.0.1:<webseed port> serving `t/<content name>/...`.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use librqbit::{
    AddTorrent, AddTorrentOptions, ConnectionOptions, CreateTorrentOptions, ListenerMode,
    ListenerOptions, Session, SessionOptions, create_torrent, spawn_utils::BlockingSpawner,
};

async fn seeder(dir: &Path, port: u16) -> Result<std::sync::Arc<Session>> {
    Session::new_with_opts(
        dir.to_owned(),
        SessionOptions {
            dht: None,
            listen: Some(ListenerOptions {
                mode: ListenerMode::TcpOnly,
                listen_addr: (Ipv4Addr::LOCALHOST, port).into(),
                ..Default::default()
            }),
            disable_local_service_discovery: true,
            disable_trackers: true,
            fastresume: false,
            ..Default::default()
        },
    )
    .await
}

async fn client(dir: &Path) -> Result<std::sync::Arc<Session>> {
    Session::new_with_opts(
        dir.to_owned(),
        SessionOptions {
            dht: None,
            listen: None,
            connect: Some(ConnectionOptions::default()),
            disable_local_service_discovery: true,
            disable_trackers: true,
            fastresume: false,
            ..Default::default()
        },
    )
    .await
}

/// Seed `add` from `dir` (already holding the files). Returns the listen addr.
async fn seed(
    s: &std::sync::Arc<Session>,
    add: AddTorrent<'_>,
    dir: &Path,
) -> Result<(SocketAddr, Duration)> {
    let t0 = Instant::now();
    let h = s
        .add_torrent(
            add,
            Some(AddTorrentOptions {
                overwrite: true,
                output_folder: Some(dir.to_str().unwrap().into()),
                ..Default::default()
            }),
        )
        .await?
        .into_handle()
        .context("no handle")?;
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            let st = h.stats();
            if let Some(e) = st.error {
                bail!("seeder error: {e}");
            }
            let live = matches!(st.state, librqbit::TorrentStatsState::Live) && st.finished;
            if live {
                return Ok::<_, anyhow::Error>(());
            }
        }
    })
    .await??;
    Ok((s.listen_addr().context("listen addr")?, t0.elapsed()))
}

async fn download(
    c: &std::sync::Arc<Session>,
    add: AddTorrent<'_>,
    out: &Path,
    peer: Option<SocketAddr>,
    wait: Duration,
) -> Result<(Duration, u64, u64)> {
    let t0 = Instant::now();
    let h = c
        .add_torrent(
            add,
            Some(AddTorrentOptions {
                initial_peers: peer.map(|p| vec![p]),
                output_folder: Some(out.to_str().unwrap().into()),
                overwrite: true,
                ..Default::default()
            }),
        )
        .await?
        .into_handle()
        .context("no handle")?;
    let r = tokio::time::timeout(wait, h.wait_until_completed()).await;
    let st = h.stats();
    match r {
        Ok(Ok(())) => Ok((t0.elapsed(), st.progress_bytes, st.total_bytes)),
        Ok(Err(e)) => bail!("download error: {e:#}"),
        Err(_) => bail!(
            "timed out after {:?}: {} / {} bytes",
            wait,
            st.progress_bytes,
            st.total_bytes
        ),
    }
}

fn same_tree(a: &Path, b: &Path) -> Result<usize> {
    let mut n = 0;
    for e in walk(a)? {
        let rel = e.strip_prefix(a)?;
        let x = std::fs::read(&e)?;
        let y = std::fs::read(b.join(rel)).with_context(|| format!("missing {rel:?}"))?;
        if x != y {
            bail!("content differs: {rel:?}");
        }
        n += 1;
    }
    Ok(n)
}

fn walk(d: &Path) -> Result<Vec<PathBuf>> {
    let mut out = vec![];
    for e in std::fs::read_dir(d)? {
        let p = e?.path();
        if p.is_dir() {
            out.extend(walk(&p)?);
        } else {
            out.push(p);
        }
    }
    Ok(out)
}

fn report(name: &str, r: Result<String>) {
    match r {
        Ok(s) => println!("RESULT\t{name}\tOK\t{s}"),
        Err(e) => println!("RESULT\t{name}\tFAIL\t{e:#}"),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let a: Vec<String> = std::env::args().collect();
    // Interop helpers: `serve <torrent> <content dir> <port>` seeds forever;
    // `get <torrent> <out dir> <peer>` downloads from one peer.
    if a[1] == "serve" {
        let s = seeder(Path::new(&a[3]), a[4].parse()?).await?;
        let (addr, dt) = seed(
            &s,
            AddTorrent::TorrentFileBytes(std::fs::read(&a[2])?.into()),
            Path::new(&a[3]),
        )
        .await?;
        println!("SERVING\t{addr}\tcheck={dt:?}");
        tokio::time::sleep(Duration::from_secs(3600)).await;
        return Ok(());
    }
    if a[1] == "get" {
        let out = Path::new(&a[3]);
        let c = client(out).await?;
        let r = download(
            &c,
            AddTorrent::TorrentFileBytes(std::fs::read(&a[2])?.into()),
            out,
            Some(a[4].parse()?),
            Duration::from_secs(120),
        )
        .await;
        report(
            "rqbit_get",
            r.map(|(dt, got, total)| format!("download={dt:?} bytes={got}/{total}")),
        );
        return Ok(());
    }
    let parent = PathBuf::from(&a[1]);
    let name = &a[2];
    let tdir = PathBuf::from(&a[3]);
    let ws_port: u16 = a[4].parse()?;
    let content = parent.join(name);
    let work = tempfile_dir("rqbit-spike")?;
    let bytes = |k: &str| -> Result<bytes::Bytes> { Ok(std::fs::read(tdir.join(k))?.into()) };

    // (d)+(e) librqbit-created v1 torrent, seeded in place, downloaded over loopback.
    report("v1_create_seed_download", async {
        let spawner = BlockingSpawner::new(4);
        let t = create_torrent(
            &content,
            CreateTorrentOptions { name: Some(name), piece_length: Some(4 << 20), ..Default::default() },
            &spawner,
        )
        .await?;
        let tb = t.as_bytes()?;
        let s = seeder(&parent, 17001).await?;
        let (addr, check) = seed(&s, AddTorrent::TorrentFileBytes(tb.clone()), &content).await?;
        let out = work.join("v1");
        let c = client(&out).await?;
        let (dt, got, total) =
            download(&c, AddTorrent::TorrentFileBytes(tb), &out, Some(addr), Duration::from_secs(120)).await?;
        let n = same_tree(&content, &out)?;
        Ok(format!("infohash_v1={} seed_check={check:?} download={dt:?} bytes={got}/{total} files_identical={n}", t.info_hash().as_string()))
    }.await);

    // (a) v2 only torrent made by libtorrent.
    report(
        "v2_only_load",
        async {
            let c = client(&work.join("v2")).await?;
            let r = c
                .add_torrent(
                    AddTorrent::TorrentFileBytes(bytes("v2.torrent")?),
                    Some(AddTorrentOptions {
                        list_only: true,
                        ..Default::default()
                    }),
                )
                .await?;
            Ok(format!(
                "accepted (list_only): {}",
                matches!(r, librqbit::AddTorrentResponse::ListOnly(_))
            ))
        }
        .await,
    );

    // (b) hybrid torrent made by libtorrent: seed in place and download.
    let hybrid_seed = seeder(&parent, 17002).await?;
    let hybrid_addr = seed(
        &hybrid_seed,
        AddTorrent::TorrentFileBytes(bytes("hybrid.torrent")?),
        &content,
    )
    .await;
    report(
        "hybrid_seed_in_place",
        hybrid_addr
            .as_ref()
            .map(|(a, d)| format!("addr={a} check={d:?}"))
            .map_err(|e| anyhow::anyhow!("{e:#}")),
    );
    let hybrid_addr = hybrid_addr.ok().map(|x| x.0);
    report(
        "hybrid_download",
        async {
            let out = work.join("hybrid");
            let c = client(&out).await?;
            let (dt, got, total) = download(
                &c,
                AddTorrent::TorrentFileBytes(bytes("hybrid.torrent")?),
                &out,
                hybrid_addr,
                Duration::from_secs(120),
            )
            .await?;
            let n = same_tree(&content, &out)?;
            let pads = out.join(".pad").exists();
            Ok(format!(
                "download={dt:?} bytes={got}/{total} files_identical={n} pad_files_on_disk={pads}"
            ))
        }
        .await,
    );

    // (c) webseed only: hybrid torrent with url-list, no peers at all.
    report(
        "webseed_only_hybrid",
        async {
            let out = work.join("ws");
            let c = client(&out).await?;
            let (dt, got, total) = download(
                &c,
                AddTorrent::TorrentFileBytes(bytes("hybrid.torrent")?),
                &out,
                None,
                Duration::from_secs(30),
            )
            .await?;
            Ok(format!(
                "download={dt:?} bytes={got}/{total} (webseed port {ws_port})"
            ))
        }
        .await,
    );

    // (f) magnets against the hybrid seeder.
    let hashes = std::fs::read_to_string(tdir.join("hashes.txt"))?;
    let line = hashes
        .lines()
        .find(|l| l.starts_with("hybrid"))
        .context("hybrid line")?;
    let v2 = line
        .split('\t')
        .find_map(|f| f.strip_prefix("v2="))
        .unwrap()
        .to_string();
    let v1 = line
        .split('\t')
        .find_map(|f| f.strip_prefix("v1="))
        .unwrap()
        .to_string();
    report(
        "magnet_btmh_only",
        async {
            let out = work.join("m2");
            let c = client(&out).await?;
            let m = format!("magnet:?xt=urn:btmh:1220{v2}&dn={name}");
            let (dt, got, total) = download(
                &c,
                AddTorrent::Url(m.into()),
                &out,
                hybrid_addr,
                Duration::from_secs(60),
            )
            .await?;
            Ok(format!("download={dt:?} bytes={got}/{total}"))
        }
        .await,
    );
    report(
        "magnet_btih_plus_btmh",
        async {
            let out = work.join("m12");
            let c = client(&out).await?;
            let m = format!("magnet:?xt=urn:btih:{v1}&xt=urn:btmh:1220{v2}&dn={name}");
            let (dt, got, total) = download(
                &c,
                AddTorrent::Url(m.into()),
                &out,
                hybrid_addr,
                Duration::from_secs(120),
            )
            .await?;
            let n = same_tree(&content, &out)?;
            Ok(format!(
                "download={dt:?} bytes={got}/{total} files_identical={n}"
            ))
        }
        .await,
    );
    let _ = std::fs::remove_dir_all(&work);
    Ok(())
}

fn tempfile_dir(p: &str) -> Result<PathBuf> {
    let d = std::env::temp_dir().join(format!("{p}-{}", std::process::id()));
    std::fs::create_dir_all(&d)?;
    Ok(d)
}
