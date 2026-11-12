// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Swarm engine: seed and download Tensorseed torrents with `librqbit`.
//!
//! librqbit 9.1 joins the v1 side of a hybrid torrent (SHA-1 pieces, BEP 47
//! pads never written to disk), which interoperates with v2 capable peers
//! such as libtorrent 2.x in the same hybrid swarm (verified in the M0
//! spike). It does not use BEP 19 webseeds, so webseed bytes come from
//! `tensorseed-fetch`, and it cannot open v2 only torrents, so
//! [`Engine::add`] refuses them. Downloads land in place; callers must check
//! each finished file against the signed manifest (SHA-256) and, if wanted,
//! [`Metainfo::verify_file`] before trusting it.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use librqbit::{
    AddTorrent, AddTorrentOptions, ConnectionOptions, ListenerMode, ListenerOptions,
    ManagedTorrent, Session, SessionOptions, TorrentStatsState, api::TorrentIdOrHash,
    limits::LimitsConfig,
};

use crate::bencode::{self, Value};
use crate::{Metainfo, TorrentError};

fn eng(e: impl std::fmt::Display) -> TorrentError {
    TorrentError::Engine(e.to_string())
}

/// Engine settings.
#[derive(Debug, Clone)]
pub struct EngineOptions {
    /// Address to accept peers on. `None`: outgoing connections only (no
    /// seeding to peers that dial in).
    pub listen: Option<SocketAddr>,
    /// Use the mainline DHT.
    pub dht: bool,
    /// Announce to the torrents' trackers.
    pub trackers: bool,
    /// Local service discovery (BEP 14).
    pub lsd: bool,
    /// Upload cap in bytes per second, `None` for unlimited.
    pub upload_bps: Option<u32>,
    /// Download cap in bytes per second, `None` for unlimited.
    pub download_bps: Option<u32>,
    /// Also accept and dial uTP (BEP 29, over UDP on the listen port) next
    /// to TCP. librqbit 9.1 implements uTP but still defaults to TCP only.
    pub utp: bool,
    /// Accept and dial TCP (default). Turning it off with `utp` on gives a
    /// uTP only session (tests).
    pub tcp: bool,
    /// Fallback output directory (every torrent here sets its own).
    pub default_dir: PathBuf,
}

impl EngineOptions {
    /// Loopback only: no DHT, trackers, or LSD (tests and local spikes).
    pub fn loopback(port: u16, dir: &Path) -> Self {
        Self {
            listen: Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)),
            dht: false,
            trackers: false,
            lsd: false,
            upload_bps: None,
            download_bps: None,
            utp: false,
            tcp: true,
            default_dir: dir.to_owned(),
        }
    }
}

/// A running librqbit session.
pub struct Engine {
    session: Arc<Session>,
}

/// One torrent inside an [`Engine`].
pub struct Handle {
    handle: Arc<ManagedTorrent>,
    v1: [u8; 20],
}

/// Snapshot of a torrent's progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// Verified bytes of the selected files (pads included for hybrids).
    pub have: u64,
    /// Bytes wanted.
    pub total: u64,
    /// Bytes uploaded to peers.
    pub uploaded: u64,
    /// All selected pieces present and checked.
    pub finished: bool,
    /// The torrent is live (checked and connected to the swarm).
    pub live: bool,
    /// Error, if the torrent stopped on one.
    pub error: Option<String>,
}

/// Session wide counters.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EngineStats {
    /// Payload bytes uploaded since the session started.
    pub uploaded: u64,
    /// Payload bytes fetched from peers since the session started.
    pub downloaded: u64,
    /// Connected peers over all torrents.
    pub peers_live: u32,
    /// Peers over uTP among them.
    pub peers_utp: u32,
    /// Peers being connected.
    pub peers_connecting: u32,
    /// Upload speed in MiB/s.
    pub upload_mibps: f64,
    /// Download speed in MiB/s.
    pub download_mibps: f64,
    /// Torrents in the session.
    pub torrents: usize,
}

/// Download options.
#[derive(Debug, Clone, Default)]
pub struct DownloadOptions {
    /// Peers to dial right away (seed boxes, `--peer`).
    pub peers: Vec<SocketAddr>,
    /// Only these torrent paths (default: every file).
    pub only: Option<Vec<String>>,
}

/// v1 file indices (pads count) of the given non pad paths, as librqbit
/// numbers them.
fn v1_indices(t: &Metainfo, paths: &[String]) -> Result<Vec<usize>, TorrentError> {
    let top = bencode::decode(&t.bytes).map_err(eng)?;
    let list = top
        .get("info")
        .and_then(|i| i.get("files"))
        .and_then(Value::as_list)
        .ok_or_else(|| TorrentError::Invalid("not a hybrid torrent".into()))?;
    let mut out = Vec::new();
    for p in paths {
        let idx = list.iter().position(|e| {
            let pad = e
                .get("attr")
                .and_then(Value::as_str)
                .is_some_and(|a| a.contains('p'));
            let path: Option<Vec<&str>> = e
                .get("path")
                .and_then(Value::as_list)
                .map(|l| l.iter().filter_map(Value::as_str).collect());
            !pad && path.is_some_and(|c| c.join("/") == *p)
        });
        out.push(idx.ok_or_else(|| TorrentError::Invalid(format!("{p} is not in the torrent")))?);
    }
    Ok(out)
}

impl Engine {
    /// Start a session.
    pub async fn start(o: EngineOptions) -> Result<Self, TorrentError> {
        let session = Session::new_with_opts(
            o.default_dir.clone(),
            SessionOptions {
                dht: if o.dht {
                    Some(Default::default())
                } else {
                    None
                },
                listen: o.listen.map(|addr| ListenerOptions {
                    mode: match (o.tcp, o.utp) {
                        (true, true) => ListenerMode::TcpAndUtp,
                        (false, true) => ListenerMode::UtpOnly,
                        _ => ListenerMode::TcpOnly,
                    },
                    listen_addr: addr,
                    ipv4_only: addr.is_ipv4(),
                    ..Default::default()
                }),
                connect: Some(ConnectionOptions {
                    enable_tcp: o.tcp || !o.utp,
                    ..Default::default()
                }),
                disable_trackers: !o.trackers,
                disable_local_service_discovery: !o.lsd,
                fastresume: false,
                persistence: None,
                ratelimits: LimitsConfig {
                    upload_bps: o.upload_bps.and_then(NonZeroU32::new),
                    download_bps: o.download_bps.and_then(NonZeroU32::new),
                },
                client_name_and_version: Some(
                    concat!("tensorseed/", env!("CARGO_PKG_VERSION")).into(),
                ),
                ..Default::default()
            },
        )
        .await
        .map_err(eng)?;
        Ok(Self { session })
    }

    /// Address peers can dial, if listening.
    pub fn listen_addr(&self) -> Option<SocketAddr> {
        self.session.listen_addr()
    }

    async fn add(
        &self,
        t: &Metainfo,
        dir: &Path,
        peers: Vec<SocketAddr>,
        only: Option<&[String]>,
    ) -> Result<Handle, TorrentError> {
        let v1 = t.info_hash_v1.ok_or_else(|| {
            TorrentError::Invalid(
                "librqbit needs a hybrid torrent (v2 only torrents are not supported)".into(),
            )
        })?;
        let only_files = only.map(|p| v1_indices(t, p)).transpose()?;
        std::fs::create_dir_all(dir).map_err(|e| TorrentError::Io(e.to_string()))?;
        let dir_s = dir
            .to_str()
            .ok_or_else(|| TorrentError::Invalid("non UTF-8 directory".into()))?
            .to_owned();
        let resp = self
            .session
            .add_torrent(
                AddTorrent::TorrentFileBytes(Bytes::from(t.bytes.clone())),
                Some(AddTorrentOptions {
                    overwrite: true,
                    output_folder: Some(dir_s),
                    initial_peers: (!peers.is_empty()).then_some(peers),
                    only_files,
                    ..Default::default()
                }),
            )
            .await
            .map_err(eng)?;
        let handle = resp
            .into_handle()
            .ok_or_else(|| TorrentError::Engine("no torrent handle".into()))?;
        Ok(Handle { handle, v1 })
    }

    /// Seed `t` from `content_dir`, where the files already are (nothing is
    /// copied). Waits until librqbit has checked every selected piece; fails
    /// if any is missing or wrong.
    pub async fn seed(
        &self,
        t: &Metainfo,
        content_dir: &Path,
        only: Option<&[String]>,
        check_timeout: Duration,
    ) -> Result<Handle, TorrentError> {
        let h = self.add(t, content_dir, Vec::new(), only).await?;
        let t0 = Instant::now();
        loop {
            let p = h.progress();
            if let Some(e) = p.error {
                return Err(TorrentError::Engine(e));
            }
            if p.live {
                if p.finished {
                    return Ok(h);
                }
                let _ = self.remove(&h).await;
                return Err(TorrentError::Invalid(format!(
                    "{}: only {} of {} bytes present and valid; refusing to seed",
                    t.name, p.have, p.total
                )));
            }
            if t0.elapsed() > check_timeout {
                return Err(TorrentError::Engine(
                    "timed out checking local files".into(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Download `t` into `out_dir` from the swarm. Returns when done or when
    /// `timeout` passes (then `finished` is false and the partial pieces stay
    /// on disk for the HTTP fallback to replace).
    pub async fn download(
        &self,
        t: &Metainfo,
        out_dir: &Path,
        o: DownloadOptions,
        timeout: Duration,
    ) -> Result<(Handle, Progress), TorrentError> {
        let h = self.add(t, out_dir, o.peers, o.only.as_deref()).await?;
        let r = tokio::time::timeout(timeout, h.handle.wait_until_completed()).await;
        if let Ok(Err(e)) = r {
            return Err(eng(e));
        }
        let p = h.progress();
        Ok((h, p))
    }

    /// Stop and forget a torrent (files stay on disk).
    pub async fn remove(&self, h: &Handle) -> Result<(), TorrentError> {
        self.session
            .delete(TorrentIdOrHash::Id(h.handle.id()), false)
            .await
            .map_err(eng)
    }

    /// Session wide counters.
    pub fn stats(&self) -> EngineStats {
        let s = self.session.stats_snapshot();
        EngineStats {
            uploaded: s.counters.uploaded_bytes,
            downloaded: s.counters.fetched_bytes,
            peers_live: s.peers.live,
            peers_utp: s.peers.live_utp,
            peers_connecting: s.peers.connecting,
            upload_mibps: s.upload_speed.mbps,
            download_mibps: s.download_speed.mbps,
            torrents: self.session.with_torrents(|it| it.count()),
        }
    }

    /// Change the upload cap (bytes per second, `None` for unlimited).
    pub fn set_upload_limit(&self, bps: Option<u32>) {
        self.session
            .ratelimits
            .set_upload_bps(bps.and_then(NonZeroU32::new));
    }

    /// Stop the session.
    pub async fn stop(&self) {
        self.session.stop().await;
    }
}

impl Handle {
    /// Current progress.
    pub fn progress(&self) -> Progress {
        let s = self.handle.stats();
        Progress {
            have: s.progress_bytes,
            total: s.total_bytes,
            uploaded: s.uploaded_bytes,
            finished: s.finished,
            live: matches!(s.state, TorrentStatsState::Live),
            error: s.error,
        }
    }

    /// Connected peers of this torrent.
    pub fn peers(&self) -> u32 {
        self.handle
            .stats()
            .live
            .map(|l| l.snapshot.peer_stats.live)
            .unwrap_or(0)
    }

    /// v1 info hash the swarm uses.
    pub fn info_hash_v1(&self) -> [u8; 20] {
        self.v1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BuildParams;
    use crate::metainfo::tests::{NAME, write_fixture};

    async fn setup(hybrid: bool) -> (tempfile::TempDir, Metainfo) {
        let d = tempfile::tempdir().unwrap();
        let content = d.path().join(NAME);
        let files = write_fixture(&content);
        let t = crate::build_torrent(
            &content,
            &files,
            &BuildParams {
                name: NAME.into(),
                piece_length: 16384,
                hybrid,
                trackers: vec![],
                webseeds: vec![],
            },
            0,
        )
        .unwrap();
        (d, t)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn loopback_seed_and_download() {
        let (d, t) = setup(true).await;
        let content = d.path().join(NAME);
        let seeder = Engine::start(EngineOptions::loopback(0, d.path()))
            .await
            .unwrap();
        let _s = seeder
            .seed(&t, &content, None, Duration::from_secs(30))
            .await
            .unwrap();
        let addr = seeder.listen_addr().unwrap();

        let out = d.path().join("out");
        let mut o = EngineOptions::loopback(0, &out);
        o.listen = None;
        let client = Engine::start(o).await.unwrap();
        let (_h, p) = client
            .download(
                &t,
                &out,
                DownloadOptions {
                    peers: vec![addr],
                    only: None,
                },
                Duration::from_secs(60),
            )
            .await
            .unwrap();
        assert!(p.finished, "{p:?}");
        for f in &t.files {
            let got = out.join(&f.path);
            assert!(t.verify_file(&f.path, &got).unwrap(), "{}", f.path);
            assert_eq!(
                std::fs::read(&got).unwrap(),
                std::fs::read(content.join(&f.path)).unwrap()
            );
        }
        assert!(!out.join(".pad").exists(), "pad files must not be written");
        client.stop().await;
        seeder.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn loopback_over_utp_only() {
        let (d, t) = setup(true).await;
        let content = d.path().join(NAME);
        let utp = |dir: &Path| {
            let mut o = EngineOptions::loopback(0, dir);
            o.tcp = false;
            o.utp = true;
            o
        };
        let seeder = Engine::start(utp(d.path())).await.unwrap();
        let _s = seeder
            .seed(&t, &content, None, Duration::from_secs(30))
            .await
            .unwrap();
        let addr = seeder.listen_addr().unwrap();
        let out = d.path().join("out");
        let client = Engine::start(utp(&out)).await.unwrap();
        let (h, p) = client
            .download(
                &t,
                &out,
                DownloadOptions {
                    peers: vec![addr],
                    only: None,
                },
                Duration::from_secs(60),
            )
            .await
            .unwrap();
        assert!(p.finished, "{p:?}");
        for f in &t.files {
            assert!(t.verify_file(&f.path, &out.join(&f.path)).unwrap());
        }
        let st = seeder.stats();
        assert!(st.uploaded > 0, "{st:?}");
        assert_eq!(st.torrents, 1);
        let _ = h.peers();
        client.stop().await;
        seeder.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refuses_v2_only_and_incomplete_seeds() {
        let (d, t) = setup(false).await;
        let e = Engine::start(EngineOptions::loopback(0, d.path()))
            .await
            .unwrap();
        let err = e
            .seed(&t, &d.path().join(NAME), None, Duration::from_secs(5))
            .await
            .err()
            .unwrap();
        assert!(err.to_string().contains("hybrid"), "{err}");

        let (d, t) = setup(true).await;
        let content = d.path().join(NAME);
        let mut b = std::fs::read(content.join("z/big.bin")).unwrap();
        b[123] ^= 0xff;
        std::fs::write(content.join("z/big.bin"), b).unwrap();
        let e = Engine::start(EngineOptions::loopback(0, d.path()))
            .await
            .unwrap();
        let err = e
            .seed(&t, &content, None, Duration::from_secs(30))
            .await
            .err()
            .unwrap();
        assert!(err.to_string().contains("refusing to seed"), "{err}");
        // Seeding just the intact files works.
        let only: Vec<String> = vec!["a.txt".into(), "z/odd.bin".into()];
        e.seed(&t, &content, Some(&only), Duration::from_secs(30))
            .await
            .unwrap();
        e.stop().await;
    }
}
