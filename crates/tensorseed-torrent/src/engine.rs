// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Swarm engine: seed and download Tensorseed torrents with `librqbit`.
//!
//! librqbit 9.1 joins the v1 side of a hybrid torrent (SHA-1 pieces, BEP 47
//! pads never written to disk), which interoperates with v2 capable peers
//! such as libtorrent 2.x in the same hybrid swarm (verified in the M0
//! spike). It does not use BEP 19 webseeds, so webseed bytes come from
//! `tensorseed-fetch`, and it cannot open v2 only torrents, so
//! `Engine::add` refuses them. Downloads land in place; callers must check
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
    /// Where to persist the DHT routing table. `None` uses librqbit's
    /// default path. Ignored when `dht` is off.
    pub dht_persistence: Option<PathBuf>,
    /// Bind the DHT (and listen sockets) to IPv4 only. The default listen
    /// address is `0.0.0.0`; dual-stack `[::]` binds fail on some Windows
    /// hosts.
    pub ipv4_only: bool,
    /// Peer connections per torrent (`None`: librqbit's default of 128).
    /// librqbit 9.1 has no session wide connection cap.
    pub peers_per_torrent: Option<usize>,
    /// Outgoing peer connections through this SOCKS5 proxy
    /// (`socks5://[user:pass@]host:port`, or `socks5h://` to resolve names
    /// through the proxy). Listening, DHT, and uTP do not go through it.
    pub proxy_url: Option<String>,
    /// Bind sockets to this network interface (for example a VPN device).
    /// Linux and macOS only; librqbit rejects it on Windows.
    pub bind_device: Option<String>,
    /// Torrents checked (initialized) at the same time (`None`: 3).
    pub concurrent_init_limit: Option<usize>,
    /// Upload cap per torrent in bytes per second (`None`: unlimited).
    pub torrent_upload_bps: Option<u32>,
    /// Download cap per torrent in bytes per second (`None`: unlimited).
    pub torrent_download_bps: Option<u32>,
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
            dht_persistence: None,
            ipv4_only: true,
            peers_per_torrent: None,
            proxy_url: None,
            bind_device: None,
            concurrent_init_limit: None,
            torrent_upload_bps: None,
            torrent_download_bps: None,
        }
    }
}

/// A running librqbit session.
pub struct Engine {
    session: Arc<Session>,
    torrent_limits: LimitsConfig,
    peers_per_torrent: Option<usize>,
}

/// A cloneable handle to change an [`Engine`]'s session caps from another
/// task (speed schedules).
#[derive(Clone)]
pub struct RateHandle {
    session: Arc<Session>,
}

impl RateHandle {
    /// Set both session caps (bytes per second, `None` for unlimited).
    pub fn set(&self, upload_bps: Option<u32>, download_bps: Option<u32>) {
        self.session
            .ratelimits
            .set_upload_bps(upload_bps.and_then(NonZeroU32::new));
        self.session
            .ratelimits
            .set_download_bps(download_bps.and_then(NonZeroU32::new));
    }

    /// Current caps (upload, download).
    pub fn get(&self) -> (Option<u32>, Option<u32>) {
        (
            self.session
                .ratelimits
                .get_upload_bps()
                .map(NonZeroU32::get),
            self.session
                .ratelimits
                .get_download_bps()
                .map(NonZeroU32::get),
        )
    }
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

/// Why [`Engine::download`] returned before every file was complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadStop {
    /// Every selected piece is present and checked.
    Finished,
    /// The overall timeout passed.
    Timeout,
    /// No peer connected within the no-peer window.
    NoPeers,
    /// Verified bytes did not increase within the stall window.
    Stall,
}

/// Download options.
#[derive(Debug, Clone, Default)]
pub struct DownloadOptions {
    /// Peers to dial right away (seed boxes, `--peer`).
    pub peers: Vec<SocketAddr>,
    /// Only these torrent paths (default: every file).
    pub only: Option<Vec<String>>,
    /// Give up if no peer is connected this long after the torrent is live.
    /// `None` disables the rule (wait for the overall timeout).
    pub no_peer: Option<Duration>,
    /// Give up if verified bytes do not increase for this long after the
    /// torrent is live. `None` disables the rule.
    pub stall: Option<Duration>,
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
    ///
    /// # Errors
    ///
    /// [`TorrentError::Engine`] when librqbit cannot start (for example the
    /// listen port is taken).
    pub async fn start(o: EngineOptions) -> Result<Self, TorrentError> {
        let session = Session::new_with_opts(
            o.default_dir.clone(),
            SessionOptions {
                dht: if o.dht {
                    Some(librqbit::DhtSessionConfig {
                        bootstrap_addrs: None,
                        port: None,
                        persistence: Some(librqbit::dht::DhtPersistenceConfig {
                            dump_interval: None,
                            config_filename: o.dht_persistence.clone(),
                        }),
                    })
                } else {
                    None
                },
                ipv4_only: o.ipv4_only,
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
                    proxy_url: o.proxy_url.clone(),
                    ..Default::default()
                }),
                bind_device_name: o.bind_device.clone(),
                concurrent_init_limit: o.concurrent_init_limit,
                peer_limit: o.peers_per_torrent,
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
        Ok(Self {
            session,
            torrent_limits: LimitsConfig {
                upload_bps: o.torrent_upload_bps.and_then(NonZeroU32::new),
                download_bps: o.torrent_download_bps.and_then(NonZeroU32::new),
            },
            peers_per_torrent: o.peers_per_torrent,
        })
    }

    /// A handle for changing the session caps from another task.
    pub fn rate_handle(&self) -> RateHandle {
        RateHandle {
            session: self.session.clone(),
        }
    }

    /// Address peers can dial, if listening.
    pub fn listen_addr(&self) -> Option<SocketAddr> {
        self.session.listen_addr()
    }

    /// Add a hybrid torrent to librqbit by its v1 side, with files already in
    /// `dir`, optional extra peers, and an optional subset of files.
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
                    ratelimits: self.torrent_limits,
                    peer_limit: self.peers_per_torrent,
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
    ///
    /// # Errors
    ///
    /// [`TorrentError::Engine`] if librqbit rejects the torrent, and
    /// [`TorrentError::Invalid`] when the initial check finds missing or wrong
    /// pieces or does not finish within `check_timeout`.
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

    /// Download `t` into `out_dir` from the swarm. Returns when done, when
    /// `timeout` passes, or when a no-peer or stall rule in `o` fires (then
    /// `finished` is false and the partial pieces stay on disk for the HTTP
    /// fallback to replace).
    ///
    /// # Errors
    ///
    /// [`TorrentError::Engine`] if librqbit rejects the torrent or the download
    /// fails. A timeout or early failover is not an error.
    pub async fn download(
        &self,
        t: &Metainfo,
        out_dir: &Path,
        o: DownloadOptions,
        timeout: Duration,
    ) -> Result<(Handle, Progress, DownloadStop), TorrentError> {
        let h = self.add(t, out_dir, o.peers, o.only.as_deref()).await?;
        let t0 = Instant::now();
        let mut live_at: Option<Instant> = None;
        let mut last_have = 0u64;
        let mut last_progress_at = t0;
        let mut saw_peer = false;
        loop {
            let p = h.progress();
            if let Some(e) = p.error {
                return Err(eng(e));
            }
            if p.finished {
                return Ok((h, p, DownloadStop::Finished));
            }
            if p.live && live_at.is_none() {
                live_at = Some(Instant::now());
                last_have = p.have;
                last_progress_at = Instant::now();
            }
            if h.peers() > 0 {
                saw_peer = true;
            }
            if p.have > last_have {
                last_have = p.have;
                last_progress_at = Instant::now();
            }
            if t0.elapsed() >= timeout {
                return Ok((h, p, DownloadStop::Timeout));
            }
            if let Some(start) = live_at {
                if let Some(np) = o.no_peer
                    && !saw_peer
                    && start.elapsed() >= np
                {
                    return Ok((h, p, DownloadStop::NoPeers));
                }
                if let Some(st) = o.stall
                    && last_progress_at.elapsed() >= st
                {
                    return Ok((h, p, DownloadStop::Stall));
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Stop and forget a torrent (files stay on disk).
    ///
    /// # Errors
    ///
    /// [`TorrentError::Engine`] if librqbit fails to remove the torrent.
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

    /// Change the download cap (bytes per second, `None` for unlimited).
    pub fn set_download_limit(&self, bps: Option<u32>) {
        self.session
            .ratelimits
            .set_download_bps(bps.and_then(NonZeroU32::new));
    }

    /// Pause a torrent: peers are dropped, nothing is uploaded or fetched,
    /// and it stays in the session.
    ///
    /// # Errors
    ///
    /// [`TorrentError::Engine`] if librqbit cannot pause it (for example it
    /// is still being checked).
    pub async fn pause(&self, h: &Handle) -> Result<(), TorrentError> {
        if h.handle.is_paused() {
            return Ok(());
        }
        self.session.pause(&h.handle).await.map_err(eng)
    }

    /// Resume a paused torrent.
    ///
    /// # Errors
    ///
    /// [`TorrentError::Engine`] if librqbit cannot restart it.
    pub async fn resume(&self, h: &Handle) -> Result<(), TorrentError> {
        if !h.handle.is_paused() {
            return Ok(());
        }
        self.session.unpause(&h.handle).await.map_err(eng)
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

    /// True while paused by [`Engine::pause`].
    pub fn is_paused(&self) -> bool {
        self.handle.is_paused()
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
        let (_h, p, stop) = client
            .download(
                &t,
                &out,
                DownloadOptions {
                    peers: vec![addr],
                    ..Default::default()
                },
                Duration::from_secs(60),
            )
            .await
            .unwrap();
        assert!(
            p.finished && stop == DownloadStop::Finished,
            "{p:?} {stop:?}"
        );
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
        let (h, p, stop) = client
            .download(
                &t,
                &out,
                DownloadOptions {
                    peers: vec![addr],
                    ..Default::default()
                },
                Duration::from_secs(60),
            )
            .await
            .unwrap();
        assert!(
            p.finished && stop == DownloadStop::Finished,
            "{p:?} {stop:?}"
        );
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
    async fn pause_resume_and_live_caps() {
        let (d, t) = setup(true).await;
        let content = d.path().join(NAME);
        let mut o = EngineOptions::loopback(0, d.path());
        o.peers_per_torrent = Some(8);
        o.torrent_upload_bps = Some(1 << 20);
        o.concurrent_init_limit = Some(1);
        let e = Engine::start(o).await.unwrap();
        let h = e
            .seed(&t, &content, None, Duration::from_secs(30))
            .await
            .unwrap();
        assert!(!h.is_paused());
        e.pause(&h).await.unwrap();
        assert!(h.is_paused());
        e.pause(&h).await.unwrap(); // idempotent
        e.resume(&h).await.unwrap();
        assert!(!h.is_paused());
        let r = e.rate_handle();
        r.set(Some(4096), None);
        assert_eq!(r.get(), (Some(4096), None));
        e.set_download_limit(Some(8192));
        assert_eq!(r.get(), (Some(4096), Some(8192)));
        r.set(None, None);
        assert_eq!(r.get(), (None, None));
        e.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bad_proxy_url_is_rejected_at_start() {
        let d = tempfile::tempdir().unwrap();
        let mut o = EngineOptions::loopback(0, d.path());
        o.proxy_url = Some("http://proxy.invalid:8080".into());
        assert!(Engine::start(o).await.is_err());
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

    #[tokio::test(flavor = "multi_thread")]
    async fn download_gives_up_quickly_when_no_peers() {
        let (d, t) = setup(true).await;
        let out = d.path().join("out");
        let mut o = EngineOptions::loopback(0, &out);
        o.listen = None;
        o.dht = false;
        o.trackers = false;
        let client = Engine::start(o).await.unwrap();
        let t0 = Instant::now();
        let (_h, p, stop) = client
            .download(
                &t,
                &out,
                DownloadOptions {
                    no_peer: Some(Duration::from_secs(2)),
                    stall: Some(Duration::from_secs(30)),
                    ..Default::default()
                },
                Duration::from_secs(60),
            )
            .await
            .unwrap();
        client.stop().await;
        assert_eq!(stop, DownloadStop::NoPeers, "{p:?} {stop:?}");
        assert!(!p.finished);
        assert!(
            t0.elapsed() < Duration::from_secs(15),
            "no-peer failover took too long: {:?}",
            t0.elapsed()
        );
    }
}
