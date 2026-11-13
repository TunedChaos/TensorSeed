// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! `tensorseed serve`: the long running seeder (spec section 8.3).
//!
//! Every `sync_interval_secs` the daemon syncs the signed catalog, picks the
//! revisions to seed with the `[serve]` selection rules, and reconciles:
//! revisions that are taken down or no longer selected stop at once (and
//! their downloaded copies are deleted with `prune = true`); selected
//! revisions found on disk (`serve.data_dir`, `storage.local_dir`, torrent
//! storage targets, `serve.dirs`) are checked against the signed manifest
//! and seeded in place; the rest are downloaded with the pull pipeline when
//! `fetch = true`. Optional extras: HTTP serving of seeded revisions in the
//! BEP 19 webseed layout, a mirrored copy of the catalog files
//! (`serve_catalog`), NAT port mapping (`port_mapping`), and a JSON health
//! endpoint on localhost. State (what was seeded, lifetime byte counters)
//! is kept in `<data_dir>/serve-state.json`, so a restart resumes seeding
//! before the first sync finishes. SIGTERM and SIGINT stop it cleanly.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use tensorseed_catalog::CatalogId;
use tensorseed_catalog::file::{HEAD_PATH, Head, delta_path};
use tensorseed_catalog::minisign::PublicKey;
use tensorseed_core::keys::{PREFIX, RepoRef, RepoType, check_commit, index_prefix};
use tensorseed_core::manifest::{
    IndexEntry, Manifest, SCHEMA_INDEX, SCHEMA_TOMBSTONE, Tombstone, TorrentInfo,
};
use tensorseed_core::verify::META_DIR;
use tensorseed_serve::health::{self, Note, SharedStatus, Status, TorrentStatus};
use tensorseed_serve::http::{self as web, CatalogDir, HttpConfig, HttpStats, Library, Published};
use tensorseed_serve::portmap::{PortMapOptions, PortMapper, Protocol};
use tensorseed_serve::ratelimit::RateLimiter;
use tensorseed_serve::select::{Candidate, Decision, Rules};
use tensorseed_torrent::Metainfo;
use tensorseed_torrent::engine::{Engine, Handle};
use tokio::sync::{mpsc, watch};

use crate::backend::{Backend, file_catalog, open_reader, pinned_key, read_verified};
use crate::cli::{PullArgs, RepoTypeArg, ServeArgs};
use crate::config::{Config, expand};
use crate::torrents::{PULLED_TORRENT, engine_options, fetch_for_manifest, parse_duration};

/// Schema of `<data_dir>/serve-state.json`.
pub const STATE_SCHEMA: &str = "ts.servestate/1";

/// Retry a failed download after this long.
const FETCH_RETRY_SECS: u64 = 3600;

/// `[serve]`: the long running seeder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServeConfig {
    /// Downloaded revisions (`t/`), the catalog mirror, and the state file
    /// (default: `~/.local/share/tensorseed/serve` on Unix,
    /// `%LOCALAPPDATA%\tensorseed\serve` on Windows).
    pub data_dir: String,
    /// Extra published trees (with `t/`) to seed from, next to
    /// `storage.local_dir` and torrent storage targets.
    pub dirs: Vec<String>,
    /// Seconds between catalog syncs (takedowns apply within one period).
    pub sync_interval_secs: u64,
    /// Repo id globs to seed (`*`, `org/*`, `org/name`); empty seeds nothing.
    pub select: Vec<String>,
    /// Repo id globs never seeded.
    pub exclude: Vec<String>,
    /// Also select the `[harvest]` watchlist (`repos` and `authors`).
    pub select_watchlist: bool,
    /// Repo types to seed (`model`, `dataset`); empty means both.
    pub types: Vec<String>,
    /// License classes to seed (`open`, `attribution`, `restricted`,
    /// `unknown`); empty means any class the publisher published.
    pub license_classes: Vec<String>,
    /// Seed only each repo's latest revision.
    pub latest_only: bool,
    /// Disk budget: total bytes of all seeded revisions (0 = unlimited).
    pub max_bytes: u64,
    /// Skip revisions larger than this (0 = no cap).
    pub max_revision_bytes: u64,
    /// Download selected revisions that are not on disk yet.
    pub fetch: bool,
    /// Downloads try the swarm first (then webseeds, mirrors, the Hub).
    pub fetch_swarm: bool,
    /// Delete downloaded copies (under `data_dir`) when a revision is taken
    /// down or no longer selected. Published trees are never deleted.
    pub prune: bool,
    /// Upload cap in KiB/s (0 = `torrent.upload_limit_kbps`).
    pub upload_limit_kbps: u64,
    /// Swarm download cap in KiB/s (0 = unlimited).
    pub download_limit_kbps: u64,
    /// Accept and dial uTP next to TCP (UDP on the torrent port).
    pub utp: bool,
    /// Ask the router to forward the torrent port (NAT-PMP, then UPnP IGD).
    /// Off by default: it opens a port on the router.
    pub port_mapping: bool,
    /// Port mapping lease, renewed at half time.
    pub port_mapping_lease_secs: u32,
    /// Serve seeded revisions over HTTP here (`0.0.0.0:8080`); empty = off.
    pub http_listen: String,
    /// HTTP upload cap in KiB/s over all connections (0 = unlimited).
    pub http_upload_limit_kbps: u64,
    /// Mirror the signed catalog files and serve them over HTTP.
    pub serve_catalog: bool,
    /// Health endpoint (JSON); empty = off. Keep it on localhost.
    pub health_listen: String,
    /// Longest time to check one revision's pieces before seeding it.
    pub check_timeout_secs: u64,
}

impl Default for ServeConfig {
    fn default() -> Self {
        Self {
            data_dir: crate::config::default_data_file("serve"),
            dirs: Vec::new(),
            sync_interval_secs: 300,
            select: vec!["*".into()],
            exclude: Vec::new(),
            select_watchlist: false,
            types: Vec::new(),
            license_classes: Vec::new(),
            latest_only: true,
            max_bytes: 0,
            max_revision_bytes: 0,
            fetch: false,
            fetch_swarm: true,
            prune: true,
            upload_limit_kbps: 0,
            download_limit_kbps: 0,
            utp: true,
            port_mapping: false,
            port_mapping_lease_secs: 3600,
            http_listen: String::new(),
            http_upload_limit_kbps: 0,
            serve_catalog: false,
            health_listen: "127.0.0.1:6880".into(),
            check_timeout_secs: 3600,
        }
    }
}

const LICENSE_CLASSES: [&str; 4] = ["open", "attribution", "restricted", "unknown"];

impl ServeConfig {
    /// Check values that would otherwise fail later.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.sync_interval_secs == 0 {
            bail!("serve.sync_interval_secs must be at least 1");
        }
        for (k, v) in [
            ("serve.http_listen", &self.http_listen),
            ("serve.health_listen", &self.health_listen),
        ] {
            if !v.is_empty() && v.parse::<SocketAddr>().is_err() {
                bail!("{k} must be ADDR:PORT (for example 127.0.0.1:6880), got {v:?}");
            }
        }
        if let Some(t) = self
            .types
            .iter()
            .find(|t| !["model", "dataset"].contains(&t.as_str()))
        {
            bail!("serve.types: unknown repo type {t:?} (use model or dataset)");
        }
        if let Some(c) = self
            .license_classes
            .iter()
            .find(|c| !LICENSE_CLASSES.contains(&c.as_str()))
        {
            bail!("serve.license_classes: unknown class {c:?} (use {LICENSE_CLASSES:?})");
        }
        if self.port_mapping && self.port_mapping_lease_secs < 120 {
            bail!("serve.port_mapping_lease_secs must be at least 120");
        }
        Ok(())
    }

    fn rules(&self, cfg: &Config) -> Rules {
        let mut select = self.select.clone();
        if self.select_watchlist {
            for r in cfg.harvest.repos.iter().chain(cfg.harvest.datasets.iter()) {
                select.push(r.id().to_owned());
            }
            for a in &cfg.harvest.authors {
                select.push(format!("{a}/*"));
            }
        }
        Rules {
            select,
            exclude: self.exclude.clone(),
            types: self.types.clone(),
            license_classes: self.license_classes.clone(),
            latest_only: self.latest_only,
            max_revision_bytes: self.max_revision_bytes,
            max_bytes: self.max_bytes,
        }
    }
}

fn kbps(k: u64) -> Option<u32> {
    (k > 0).then(|| u32::try_from(k.saturating_mul(1024)).unwrap_or(u32::MAX))
}

fn unix_now() -> u64 {
    tensorseed_catalog::file::unix_now()
}

fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}

fn write_atomic(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    let dir = path.parent().context("path has no parent")?;
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = dir.join(format!(".{name}.tmp"));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn tombstone_covers(t: &Tombstone, commit: &str) -> bool {
    t.revisions.is_empty() || t.revisions.iter().any(|r| r == commit)
}

/// Persisted daemon state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct PersistedServe {
    schema: String,
    uploaded_bytes_total: u64,
    downloaded_bytes_total: u64,
    catalog_revision: u64,
    head_expires_unix: Option<u64>,
    revisions: Vec<PersistedRev>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedRev {
    name: String,
    repo_type: RepoType,
    repo_id: String,
    commit: String,
    dir: PathBuf,
    owned: bool,
    bytes: u64,
    only: Option<Vec<String>>,
}

struct Want {
    repo: RepoRef,
    commit: String,
    manifest: Manifest,
}

struct Seeding {
    repo: RepoRef,
    commit: String,
    dir: PathBuf,
    torrent: Metainfo,
    handle: Handle,
    owned: bool,
    bytes: u64,
    only: Option<Vec<String>>,
}

type FetchResult = (String, Result<(), String>);

struct Daemon {
    cfg: Arc<Config>,
    data_dir: PathBuf,
    pk: PublicKey,
    engine: Engine,
    seeding: BTreeMap<String, Seeding>,
    library: Option<Arc<Library>>,
    http_stats: Arc<HttpStats>,
    status: SharedStatus,
    shutdown_tx: watch::Sender<bool>,
    servers: Vec<tokio::task::JoinHandle<()>>,
    portmap: Option<PortMapper>,
    base: PersistedServe,
    manifests: HashMap<String, Manifest>,
    desired: BTreeMap<String, Want>,
    taken_down_now: HashSet<String>,
    fetching: HashMap<String, tokio::task::JoinHandle<()>>,
    fetch_queue: VecDeque<String>,
    fetch_failed: HashMap<String, (u64, String)>,
    fetch_tx: mpsc::UnboundedSender<FetchResult>,
    fetch_rx: mpsc::UnboundedReceiver<FetchResult>,
    skipped: Vec<Note>,
    taken_down: Vec<Note>,
    catalog_dir: Option<PathBuf>,
}

/// `tensorseed serve`.
pub async fn run(mut cfg: Config, args: &ServeArgs) -> anyhow::Result<ExitCode> {
    let sc = &mut cfg.serve;
    if args.serve_catalog {
        sc.serve_catalog = true;
    }
    if let Some(h) = &args.http {
        sc.http_listen = h.clone();
    }
    if let Some(h) = &args.health {
        sc.health_listen = if h == "off" { String::new() } else { h.clone() };
    }
    if args.fetch {
        sc.fetch = true;
    }
    if args.no_fetch {
        sc.fetch = false;
    }
    for d in &args.dir {
        sc.dirs.push(d.display().to_string());
    }
    if let Some(s) = args.sync_interval {
        sc.sync_interval_secs = s;
    }
    sc.validate()?;
    if sc.serve_catalog && sc.http_listen.is_empty() {
        bail!("--serve-catalog needs an HTTP address (--http ADDR:PORT or serve.http_listen)");
    }
    let duration = args
        .duration
        .as_deref()
        .map(parse_duration)
        .transpose()?
        .flatten();
    let cfg = Arc::new(cfg);
    let mut d = Daemon::start(cfg).await?;
    let r = d.run_loop(duration).await;
    d.shutdown().await;
    r
}

impl Daemon {
    async fn start(cfg: Arc<Config>) -> anyhow::Result<Self> {
        let sc = &cfg.serve;
        let pk = pinned_key(&cfg)?;
        let data_dir = expand(&sc.data_dir);
        std::fs::create_dir_all(data_dir.join("t"))
            .with_context(|| format!("creating {}", data_dir.display()))?;
        let base: PersistedServe = std::fs::read(data_dir.join("serve-state.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();

        let mut eo = engine_options(&cfg, true, &data_dir)?;
        eo.utp = sc.utp;
        if sc.upload_limit_kbps > 0 {
            eo.upload_bps = kbps(sc.upload_limit_kbps);
        }
        eo.download_bps = kbps(sc.download_limit_kbps);
        let (up_bps, down_bps) = (eo.upload_bps, eo.download_bps);
        let engine = Engine::start(eo)
            .await
            .context("starting the BitTorrent engine")?;
        let listen = engine.listen_addr();
        if let Some(a) = listen {
            println!(
                "listening on {a} (tcp{})",
                if sc.utp { " and utp" } else { "" }
            );
        }

        let mut st = Status::new(env!("CARGO_PKG_VERSION"), unix_now());
        st.sync_interval_secs = sc.sync_interval_secs;
        st.budget_bytes = sc.max_bytes;
        st.catalog.name = cfg.catalog.name.clone();
        st.catalog.backend = cfg.catalog.backend.clone();
        st.catalog.revision = base.catalog_revision;
        st.catalog.head_expires_unix = base.head_expires_unix;
        st.swarm.listen = listen.map(|a| a.to_string());
        st.swarm.utp = sc.utp;
        st.swarm.dht = cfg.torrent.dht;
        st.swarm.upload_limit_bps = up_bps.map(u64::from).unwrap_or(0);
        st.swarm.download_limit_bps = down_bps.map(u64::from).unwrap_or(0);
        st.port_mapping.enabled = sc.port_mapping;
        let status: SharedStatus = Arc::new(RwLock::new(st));

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let stop_signal = |mut rx: watch::Receiver<bool>| async move {
            let _ = rx.wait_for(|v| *v).await;
        };
        let mut servers = Vec::new();
        if !sc.health_listen.is_empty() {
            let l = tokio::net::TcpListener::bind(&sc.health_listen)
                .await
                .with_context(|| format!("binding serve.health_listen {}", sc.health_listen))?;
            println!("health   http://{}/health", l.local_addr()?);
            let (s, rx) = (status.clone(), shutdown_rx.clone());
            servers.push(tokio::spawn(async move {
                if let Err(e) = health::serve(l, s, stop_signal(rx)).await {
                    tracing::error!("health endpoint stopped: {e}");
                }
            }));
        }
        let http_stats = Arc::new(HttpStats::default());
        let mut library = None;
        let mut catalog_dir = None;
        if !sc.http_listen.is_empty() {
            let lib = Arc::new(Library::new());
            library = Some(lib.clone());
            let catalog = sc.serve_catalog.then(|| {
                let dir = data_dir.join("catalog").join(&cfg.catalog.name);
                catalog_dir = Some(dir.clone());
                CatalogDir {
                    name: cfg.catalog.name.clone(),
                    dir,
                }
            });
            let limit = sc.http_upload_limit_kbps.saturating_mul(1024);
            let hc = HttpConfig {
                library: Some(lib),
                catalog,
                limiter: (limit > 0).then(|| Arc::new(RateLimiter::new(limit))),
                stats: http_stats.clone(),
            };
            let l = tokio::net::TcpListener::bind(&sc.http_listen)
                .await
                .with_context(|| format!("binding serve.http_listen {}", sc.http_listen))?;
            let a = l.local_addr()?;
            // Wildcard binds: show the shape, the public name is not known here.
            let shown = if a.ip().is_unspecified() {
                format!("<this host>:{}", a.port())
            } else {
                a.to_string()
            };
            println!("http     http://{a}/ (webseed base http://{shown}/t/)");
            if sc.serve_catalog {
                println!("catalog  http://{shown}/{}/", cfg.catalog.name);
            }
            {
                let mut w = status.write().unwrap_or_else(|e| e.into_inner());
                w.http.listen = Some(a.to_string());
                w.http.webseed = true;
                w.http.upload_limit_bps = limit;
                w.catalog.serving = sc.serve_catalog;
            }
            let rx = shutdown_rx.clone();
            servers.push(tokio::spawn(async move {
                if let Err(e) = web::serve(l, web::router(hc), stop_signal(rx)).await {
                    tracing::error!("HTTP server stopped: {e}");
                }
            }));
        }
        let portmap = match (sc.port_mapping, listen) {
            (true, Some(a)) => {
                if a.ip().is_loopback() {
                    tracing::warn!("port_mapping is on but torrent.listen_addr is loopback");
                }
                let mut o = PortMapOptions::new(a.port());
                o.lease_secs = sc.port_mapping_lease_secs;
                if !sc.utp {
                    o.protocols = vec![Protocol::Tcp];
                }
                Some(PortMapper::spawn(o))
            }
            _ => None,
        };
        let (fetch_tx, fetch_rx) = mpsc::unbounded_channel();
        let mut d = Self {
            cfg,
            data_dir,
            pk,
            engine,
            seeding: BTreeMap::new(),
            library,
            http_stats,
            status,
            shutdown_tx,
            servers,
            portmap,
            base,
            manifests: HashMap::new(),
            desired: BTreeMap::new(),
            taken_down_now: HashSet::new(),
            fetching: HashMap::new(),
            fetch_queue: VecDeque::new(),
            fetch_failed: HashMap::new(),
            fetch_tx,
            fetch_rx,
            skipped: Vec::new(),
            taken_down: Vec::new(),
            catalog_dir,
        };
        d.resume().await;
        d.refresh_status();
        Ok(d)
    }

    fn torrent_copy(&self, name: &str) -> PathBuf {
        self.data_dir
            .join("torrents")
            .join(format!("{name}.torrent"))
    }

    /// Seed what the last run seeded, before the first sync.
    async fn resume(&mut self) {
        for r in self.base.revisions.clone() {
            let Ok(bytes) = std::fs::read(self.torrent_copy(&r.name)) else {
                continue;
            };
            let Ok(t) = Metainfo::parse(&bytes) else {
                continue;
            };
            let Ok(repo) = RepoRef::new(r.repo_type, r.repo_id.clone()) else {
                continue;
            };
            if t.name != r.name || !r.dir.is_dir() {
                continue;
            }
            match self
                .start_seed(
                    &r.name,
                    repo,
                    &r.commit,
                    r.dir.clone(),
                    t,
                    r.owned,
                    r.bytes,
                    r.only,
                )
                .await
            {
                Ok(()) => {}
                Err(e) => println!("FAILED   {} (resume): {e:#}", r.name),
            }
        }
        if !self.seeding.is_empty() {
            println!(
                "resumed {} revision(s) from the last run; checking the catalog",
                self.seeding.len()
            );
        }
    }

    async fn run_loop(&mut self, duration: Option<Duration>) -> anyhow::Result<ExitCode> {
        #[cfg(unix)]
        let (mut term, mut int) = {
            use tokio::signal::unix::{SignalKind, signal};
            (
                signal(SignalKind::terminate())?,
                signal(SignalKind::interrupt())?,
            )
        };
        let deadline = duration.map(|d| tokio::time::Instant::now() + d);
        let mut sync_tick =
            tokio::time::interval(Duration::from_secs(self.cfg.serve.sync_interval_secs));
        sync_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut stats_tick = tokio::time::interval(Duration::from_secs(5));
        let mut persist_tick = tokio::time::interval(Duration::from_secs(60));
        let mut report_tick = tokio::time::interval(Duration::from_secs(300));
        report_tick.tick().await;
        loop {
            #[cfg(unix)]
            let signalled = async {
                tokio::select! {
                    _ = term.recv() => "SIGTERM",
                    _ = int.recv() => "SIGINT",
                }
            };
            #[cfg(not(unix))]
            let signalled = async {
                let _ = tokio::signal::ctrl_c().await;
                "Ctrl-C"
            };
            tokio::select! {
                s = signalled => {
                    println!("{s}: shutting down");
                    break;
                }
                _ = async {
                    match deadline {
                        Some(d) => tokio::time::sleep_until(d).await,
                        None => std::future::pending().await,
                    }
                } => {
                    println!("time is up: shutting down");
                    break;
                }
                _ = sync_tick.tick() => self.cycle().await,
                Some((name, r)) = self.fetch_rx.recv() => self.fetch_done(name, r).await,
                _ = stats_tick.tick() => self.refresh_status(),
                _ = persist_tick.tick() => self.persist(),
                _ = report_tick.tick() => self.report(),
            }
        }
        Ok(ExitCode::SUCCESS)
    }

    fn report(&self) {
        let es = self.engine.stats();
        println!(
            "status   {} seeding, {} peers, uploaded {} bytes ({} total)",
            self.seeding.len(),
            es.peers_live,
            es.uploaded,
            self.base.uploaded_bytes_total + es.uploaded
        );
    }

    async fn cycle(&mut self) {
        let now = unix_now();
        self.status_mut().catalog.last_sync_unix = Some(now);
        let opened = match self.sync().await {
            Ok(o) => o,
            Err(e) => {
                let msg = format!("{e:#}");
                tracing::warn!("catalog sync failed: {msg}");
                println!("sync     failed: {msg}");
                self.status_mut().catalog.last_sync_error = Some(msg);
                self.refresh_status();
                return;
            }
        };
        match self.plan(&opened.0, &opened.1).await {
            Ok(()) => {
                let mut w = self.status_mut();
                w.catalog.last_sync_error = None;
                w.catalog.last_sync_ok_unix = Some(now);
            }
            Err(e) => {
                let msg = format!("reading the catalog: {e:#}");
                println!("sync     failed: {msg}");
                self.status_mut().catalog.last_sync_error = Some(msg);
                self.refresh_status();
                return;
            }
        }
        self.reconcile().await;
        self.persist();
        self.refresh_status();
    }

    fn status_mut(&self) -> std::sync::RwLockWriteGuard<'_, Status> {
        self.status.write().unwrap_or_else(|e| e.into_inner())
    }

    async fn sync(&mut self) -> anyhow::Result<(Backend, CatalogId)> {
        let cfg = self.cfg.clone();
        if cfg.catalog.backend != "file" {
            let (b, id) = open_reader(&cfg, None, false).await?;
            if let Backend::Chia(c) = &b {
                use tensorseed_catalog::CatalogBackend;
                if let Ok(s) = c.sync_status(&id).await {
                    self.status_mut().catalog.revision = s.local_generation;
                    self.base.catalog_revision = s.local_generation;
                }
            }
            return Ok((b, id));
        }
        crate::mirrors::refresh(&cfg, false).await;
        let cat = file_catalog(&cfg, None)?;
        let r = crate::chain::sync_checked(&cfg, &cat)
            .await
            .context("syncing the file catalog")?;
        let st = cat.state();
        if r.to != r.from {
            println!("catalog  revision {} -> {} from {}", r.from, r.to, r.source);
        }
        {
            let mut w = self.status_mut();
            w.catalog.revision = st.revision;
            w.catalog.root = Some(st.root.clone());
            w.catalog.source = Some(r.source.clone());
        }
        self.base.catalog_revision = st.revision;
        match self.mirror_catalog(&r.source).await {
            Ok(exp) => {
                self.status_mut().catalog.head_expires_unix = Some(exp);
                self.base.head_expires_unix = Some(exp);
            }
            Err(e) => tracing::warn!("reading head.json for expiry / catalog mirror: {e:#}"),
        }
        Ok((Backend::File(cat), CatalogId(cfg.catalog.name.clone())))
    }

    /// Fetch and verify `head.json` from `source` (for its expiry) and, with
    /// `serve_catalog`, copy every signed catalog file into the mirror
    /// directory. Returns the head's expiry.
    async fn mirror_catalog(&self, source: &str) -> anyhow::Result<u64> {
        let name = &self.cfg.catalog.name;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()?;
        let get = |rel: String| {
            let http = http.clone();
            let url = format!("{source}{name}/{rel}");
            async move {
                let r = http.get(&url).send().await?;
                if r.status() == reqwest::StatusCode::NOT_FOUND {
                    return Ok::<_, anyhow::Error>(None);
                }
                if !r.status().is_success() {
                    bail!("GET {url}: HTTP {}", r.status());
                }
                Ok(Some(r.bytes().await?.to_vec()))
            }
        };
        let signed = |rel: String| {
            let get = &get;
            let pk = &self.pk;
            async move {
                let Some(body) = get(rel.clone()).await? else {
                    return Ok::<_, anyhow::Error>(None);
                };
                let Some(sig) = get(format!("{rel}.minisig")).await? else {
                    bail!("{rel}: missing .minisig");
                };
                let sig = String::from_utf8(sig).context("signature is not UTF-8")?;
                pk.verify(&body, &sig)
                    .with_context(|| format!("{rel}: bad signature"))?;
                Ok(Some((body, sig)))
            }
        };
        let (hbody, hsig) = signed(HEAD_PATH.into())
            .await?
            .context("no head.json at the sync source")?;
        let head: Head = serde_json::from_slice(&hbody).context("parsing head.json")?;
        if &head.catalog != name {
            bail!("head.json is for catalog {:?}", head.catalog);
        }
        let Some(dir) = &self.catalog_dir else {
            return Ok(head.expires_unix);
        };
        let mut rels: Vec<String> = head.snapshot.iter().map(|s| s.path.clone()).collect();
        rels.extend((1..=head.revision).map(delta_path));
        let mut copied = 0;
        for rel in rels {
            let (p, ps) = (dir.join(&rel), dir.join(format!("{rel}.minisig")));
            if p.exists() && ps.exists() {
                continue;
            }
            match signed(rel.clone()).await {
                Ok(Some((b, s))) => {
                    write_atomic(&p, &b)?;
                    write_atomic(&ps, s.as_bytes())?;
                    copied += 1;
                }
                Ok(None) => tracing::debug!("{rel} not published; skipped"),
                Err(e) => tracing::warn!("catalog mirror: {e:#}"),
            }
        }
        match signed("mirrors.toml".into()).await {
            Ok(Some((b, s))) => {
                write_atomic(&dir.join("mirrors.toml"), &b)?;
                write_atomic(&dir.join("mirrors.toml.minisig"), s.as_bytes())?;
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("catalog mirror: mirrors.toml: {e:#}"),
        }
        // The chain proof (spec 6.14) for this head, so light clients can
        // check this mirror against the chain too. Written before the head.
        use tensorseed_catalog::anchor::{CHAIN_PROOF_PATH, ChainProof, FileAnchor};
        match get(CHAIN_PROOF_PATH.into()).await {
            Ok(Some(b)) => match ChainProof::from_bytes(&b)
                .and_then(|p| FileAnchor::from_entry(&p.entry, &self.pk, name))
            {
                Ok(a) if a.matches(head.revision, &head.root) => {
                    write_atomic(&dir.join(CHAIN_PROOF_PATH), &b)?;
                }
                Ok(_) => tracing::debug!("chain-proof.json is for another revision; not mirrored"),
                Err(e) => tracing::warn!("catalog mirror: chain-proof.json: {e}"),
            },
            Ok(None) => {}
            Err(e) => tracing::warn!("catalog mirror: chain-proof.json: {e:#}"),
        }
        write_atomic(&dir.join("head.json.minisig"), hsig.as_bytes())?;
        write_atomic(&dir.join(HEAD_PATH), &hbody)?;
        if copied > 0 {
            println!(
                "catalog  mirrored {copied} file(s); serving revision {}",
                head.revision
            );
        }
        Ok(head.expires_unix)
    }

    /// Read the catalog and decide what to seed.
    async fn plan(&mut self, backend: &Backend, id: &CatalogId) -> anyhow::Result<()> {
        let cfg = self.cfg.clone();
        let b = backend.as_dyn();
        let name = cfg.catalog.name.as_str();
        let rules = cfg.serve.rules(&cfg);
        let prefix = index_prefix();
        let mut candidates = Vec::new();
        let mut wants: HashMap<String, Want> = HashMap::new();
        let mut notes = Vec::new();
        for key in b.keys(id, &prefix).await? {
            let rest = &key[prefix.len()..];
            let Some((t, repo_id)) = rest.split_once('/') else {
                continue;
            };
            let repo_type = match t {
                "model" => RepoType::Model,
                "dataset" => RepoType::Dataset,
                _ => continue,
            };
            if rules.rank(repo_id).is_none() {
                continue;
            }
            let Some(e) = read_verified(b, id, &key, &self.pk, name).await? else {
                continue;
            };
            let ix: IndexEntry =
                serde_json::from_str(&e.value).with_context(|| format!("parsing {key}"))?;
            if ix.schema != SCHEMA_INDEX {
                bail!("{key} has schema {:?}", ix.schema);
            }
            let repo = RepoRef::new(repo_type, repo_id)?;
            let tombstone =
                match read_verified(b, id, &repo.tombstone_key(), &self.pk, name).await? {
                    Some(e) => {
                        let t: Tombstone =
                            serde_json::from_str(&e.value).context("parsing tombstone")?;
                        if t.schema != SCHEMA_TOMBSTONE {
                            bail!("tombstone has schema {:?}", t.schema);
                        }
                        Some(t)
                    }
                    None => None,
                };
            let commits = if rules.latest_only {
                vec![ix.latest_commit.clone()]
            } else {
                let rp = format!("{PREFIX}/repo/{t}/{repo_id}/rev/");
                let mut v: Vec<String> = b
                    .keys(id, &rp)
                    .await?
                    .into_iter()
                    .filter_map(|k| k.strip_prefix(&rp).map(str::to_owned))
                    .filter(|c| check_commit(c).is_ok())
                    .collect();
                if !v.contains(&ix.latest_commit) {
                    v.push(ix.latest_commit.clone());
                }
                v
            };
            for commit in commits {
                let cname = TorrentInfo::name_for(&repo.id, &commit);
                let taken_down = match &tombstone {
                    Some(t) => tombstone_covers(t, &commit),
                    None => ix.taken_down && commit == ix.latest_commit,
                };
                let manifest = match self.manifests.get(&cname) {
                    Some(m) => m.clone(),
                    None if taken_down => {
                        // No need to read it; nothing will be seeded.
                        candidates.push(Candidate {
                            repo_type: repo_type.to_string(),
                            repo_id: repo.id.clone(),
                            commit: commit.clone(),
                            bytes: 0,
                            license_class: "unknown".into(),
                            latest: commit == ix.latest_commit,
                            published_at: String::new(),
                            taken_down,
                        });
                        continue;
                    }
                    None => {
                        match crate::consumer::catalog_manifest(&cfg, &repo, Some(&commit), true)
                            .await
                        {
                            Ok(r) => {
                                self.manifests.insert(cname.clone(), r.manifest.clone());
                                r.manifest
                            }
                            Err(e) => {
                                notes.push(Note {
                                    item: format!("{}@{}", repo.id, short(&commit)),
                                    detail: format!("unreadable manifest: {e:#}"),
                                });
                                continue;
                            }
                        }
                    }
                };
                let class = serde_json::to_value(manifest.license.class)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "unknown".into());
                candidates.push(Candidate {
                    repo_type: repo_type.to_string(),
                    repo_id: repo.id.clone(),
                    commit: commit.clone(),
                    bytes: manifest.totals.bytes,
                    license_class: class,
                    latest: commit == ix.latest_commit,
                    published_at: manifest.published_at.clone(),
                    taken_down,
                });
                wants.insert(
                    cname,
                    Want {
                        repo: repo.clone(),
                        commit,
                        manifest,
                    },
                );
            }
        }
        self.desired.clear();
        self.taken_down_now.clear();
        for (c, d) in rules.plan(candidates) {
            let cname = TorrentInfo::name_for(&c.repo_id, &c.commit);
            match d {
                Decision::Selected => {
                    if let Some(w) = wants.remove(&cname) {
                        self.desired.insert(cname, w);
                    }
                }
                Decision::Skipped(reason) => {
                    if c.taken_down {
                        self.taken_down_now.insert(cname);
                    }
                    notes.push(Note {
                        item: format!("{}@{}", c.repo_id, short(&c.commit)),
                        detail: reason,
                    });
                }
            }
        }
        self.skipped = notes;
        Ok(())
    }

    /// Stop what is no longer wanted, start what is.
    async fn reconcile(&mut self) {
        let current: Vec<String> = self.seeding.keys().cloned().collect();
        for n in current {
            if !self.desired.contains_key(&n) {
                let down = self.taken_down_now.contains(&n);
                self.stop(
                    &n,
                    if down {
                        "taken down"
                    } else {
                        "no longer selected"
                    },
                    down,
                )
                .await;
            }
        }
        let fetching: Vec<String> = self.fetching.keys().cloned().collect();
        for n in fetching {
            if !self.desired.contains_key(&n)
                && let Some(h) = self.fetching.remove(&n)
            {
                h.abort();
                println!("cancel   {n}: no longer wanted");
            }
        }
        self.fetch_queue.retain(|n| self.desired.contains_key(n));
        // Downloaded copies of taken down revisions go even when not seeded.
        if self.cfg.serve.prune {
            for n in self.taken_down_now.clone() {
                let dir = self.data_dir.join("t").join(&n);
                if dir.exists() {
                    self.delete_owned(&n);
                }
            }
        }
        let wanted: Vec<String> = self.desired.keys().cloned().collect();
        for n in wanted {
            if self.seeding.contains_key(&n) || self.fetching.contains_key(&n) {
                continue;
            }
            match self.seed_local(&n).await {
                Ok(true) => {}
                Ok(false) if self.cfg.serve.fetch => {
                    if !self.fetch_queue.contains(&n) {
                        self.fetch_queue.push_back(n);
                    }
                }
                Ok(false) => self.skipped.push(Note {
                    item: n,
                    detail: "selected but not on disk (fetch is off)".into(),
                }),
                Err(e) => {
                    println!("FAILED   {n}: {e:#}");
                    self.skipped.push(Note {
                        item: n,
                        detail: format!("{e:#}"),
                    });
                }
            }
        }
        self.pump_fetches();
    }

    fn delete_owned(&self, n: &str) {
        let dir = self.data_dir.join("t").join(n);
        let tor = self.data_dir.join("t").join(format!("{n}.torrent"));
        let _ = std::fs::remove_file(&tor);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => println!("deleted  {}", dir.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => println!("FAILED   deleting {}: {e}", dir.display()),
        }
    }

    async fn stop(&mut self, n: &str, reason: &str, takedown: bool) {
        let Some(s) = self.seeding.remove(n) else {
            return;
        };
        if let Some(l) = &self.library {
            l.remove(n);
        }
        if let Err(e) = self.engine.remove(&s.handle).await {
            tracing::warn!("stopping {n}: {e}");
        }
        let _ = std::fs::remove_file(self.torrent_copy(n));
        println!("stopped  {n}: {reason}");
        if takedown {
            self.taken_down.push(Note {
                item: n.to_owned(),
                detail: format!("{}@{} taken down", s.repo.id, short(&s.commit)),
            });
        }
        if s.owned && self.cfg.serve.prune {
            self.delete_owned(n);
        }
    }

    /// Roots to look for `t/<name>/` in: `(root, owned)`.
    fn roots(&self) -> Vec<(PathBuf, bool)> {
        let mut v = vec![(self.data_dir.clone(), true)];
        for r in self.cfg.storage.seed_roots() {
            v.push((expand(&r), false));
        }
        for r in &self.cfg.serve.dirs {
            v.push((expand(r), false));
        }
        let mut seen = HashSet::new();
        v.retain(|(p, _)| seen.insert(p.clone()));
        v
    }

    /// Seed `n` from a local copy. `Ok(false)`: no usable copy.
    async fn seed_local(&mut self, n: &str) -> anyhow::Result<bool> {
        let (repo, commit, m) = {
            let w = &self.desired[n];
            (w.repo.clone(), w.commit.clone(), w.manifest.clone())
        };
        if m.torrent.is_none() {
            bail!("the manifest has no torrent (published before torrents existed)");
        }
        for (root, owned) in self.roots() {
            let dir = root.join("t").join(n);
            if !dir.is_dir() {
                continue;
            }
            let tpath = root.join("t").join(format!("{n}.torrent"));
            let local = std::fs::read(&tpath)
                .ok()
                .and_then(|b| Metainfo::parse(&b).ok());
            let torrent = match local {
                Some(t) => t,
                None => {
                    let t = fetch_for_manifest(&m, &[]).await?;
                    if owned {
                        write_atomic(&tpath, &t.bytes)?;
                    }
                    t
                }
            };
            torrent
                .check_manifest(&m, &m.files)
                .context("the local torrent does not match the signed manifest")?;
            let only = if owned {
                skipped_only(&dir, &torrent)
            } else {
                None
            };
            match self
                .start_seed(
                    n,
                    repo.clone(),
                    &commit,
                    dir.clone(),
                    torrent,
                    owned,
                    m.totals.bytes,
                    only,
                )
                .await
            {
                Ok(()) => return Ok(true),
                Err(e) if owned => {
                    // Incomplete download: let the pull pipeline resume it.
                    println!("partial  {n} in {}: {e:#}", dir.display());
                    return Ok(false);
                }
                Err(e) => println!("FAILED   {n} in {}: {e:#}", dir.display()),
            }
        }
        Ok(false)
    }

    #[allow(clippy::too_many_arguments)]
    async fn start_seed(
        &mut self,
        n: &str,
        repo: RepoRef,
        commit: &str,
        dir: PathBuf,
        torrent: Metainfo,
        owned: bool,
        bytes: u64,
        only: Option<Vec<String>>,
    ) -> anyhow::Result<()> {
        let check = Duration::from_secs(self.cfg.serve.check_timeout_secs);
        let handle = self
            .engine
            .seed(&torrent, &dir, only.as_deref(), check)
            .await?;
        if let Some(l) = &self.library {
            let files = torrent
                .files
                .iter()
                .filter(|f| only.as_ref().is_none_or(|o| o.contains(&f.path)))
                .map(|f| (f.path.clone(), f.length))
                .collect();
            l.insert(
                n,
                Published {
                    dir: dir.clone(),
                    torrent: bytes::Bytes::from(torrent.bytes.clone()),
                    files,
                },
            );
        }
        if let Err(e) = write_atomic(&self.torrent_copy(n), &torrent.bytes) {
            tracing::warn!("saving the torrent of {n}: {e:#}");
        }
        println!(
            "seeding  {n} from {} (btih {}, btmh 1220{})",
            dir.display(),
            torrent.info_hash_v1_hex().unwrap_or_default(),
            torrent.info_hash_v2_hex()
        );
        self.seeding.insert(
            n.to_owned(),
            Seeding {
                repo,
                commit: commit.to_owned(),
                dir,
                torrent,
                handle,
                owned,
                bytes,
                only,
            },
        );
        Ok(())
    }

    fn pump_fetches(&mut self) {
        if !self.fetching.is_empty() {
            return;
        }
        let now = unix_now();
        while let Some(n) = self.fetch_queue.pop_front() {
            if self.seeding.contains_key(&n) {
                continue;
            }
            let Some(w) = self.desired.get(&n) else {
                continue;
            };
            if let Some((at, e)) = self.fetch_failed.get(&n)
                && now < at + FETCH_RETRY_SECS
            {
                self.skipped.push(Note {
                    item: n.clone(),
                    detail: format!("download failed, retrying later: {e}"),
                });
                continue;
            }
            let cfg = self.cfg.clone();
            let (repo, commit, m) = (w.repo.clone(), w.commit.clone(), w.manifest.clone());
            let root = self.data_dir.clone();
            let tx = self.fetch_tx.clone();
            let name = n.clone();
            println!("fetch    {n}: {} bytes", m.totals.bytes);
            let h = tokio::spawn(async move {
                let r = fetch_revision(&cfg, &repo, &commit, &m, &root)
                    .await
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send((name, r));
            });
            self.fetching.insert(n, h);
            break;
        }
    }

    async fn fetch_done(&mut self, n: String, r: Result<(), String>) {
        if self.fetching.remove(&n).is_none() {
            return; // cancelled
        }
        match r {
            Ok(()) if self.desired.contains_key(&n) => {
                self.fetch_failed.remove(&n);
                match self.seed_local(&n).await {
                    Ok(true) => {}
                    Ok(false) => {
                        self.fetch_failed.insert(
                            n.clone(),
                            (unix_now(), "downloaded copy did not seed".into()),
                        );
                    }
                    Err(e) => {
                        println!("FAILED   {n}: {e:#}");
                        self.fetch_failed.insert(n, (unix_now(), format!("{e:#}")));
                    }
                }
            }
            Ok(()) => {
                if self.cfg.serve.prune {
                    self.delete_owned(&n);
                }
            }
            Err(e) => {
                println!("FAILED   fetch {n}: {e}");
                self.fetch_failed.insert(n, (unix_now(), e));
            }
        }
        self.persist();
        self.refresh_status();
        self.pump_fetches();
    }

    fn refresh_status(&self) {
        let es = self.engine.stats();
        let pm = self.portmap.as_ref().map(PortMapper::status);
        let mut w = self.status_mut();
        if w.status == "starting" {
            w.status = "ok".into();
        }
        w.swarm.uploaded_bytes = es.uploaded;
        w.swarm.downloaded_bytes = es.downloaded;
        w.swarm.uploaded_bytes_total = self.base.uploaded_bytes_total + es.uploaded;
        w.swarm.downloaded_bytes_total = self.base.downloaded_bytes_total + es.downloaded;
        w.swarm.upload_mibps = es.upload_mibps;
        w.swarm.download_mibps = es.download_mibps;
        w.swarm.peers_live = es.peers_live;
        w.swarm.peers_connecting = es.peers_connecting;
        w.torrents = self
            .seeding
            .iter()
            .map(|(n, s)| {
                let p = s.handle.progress();
                TorrentStatus {
                    name: n.clone(),
                    repo_type: s.repo.repo_type.to_string(),
                    repo_id: s.repo.id.clone(),
                    commit: s.commit.clone(),
                    dir: s.dir.display().to_string(),
                    bytes: s.bytes,
                    info_hash_v1: s.torrent.info_hash_v1_hex().unwrap_or_default(),
                    uploaded_bytes: p.uploaded,
                    peers: s.handle.peers(),
                    state: match p.error {
                        Some(e) => format!("error: {e}"),
                        None if p.live => "live".into(),
                        None => "checking".into(),
                    },
                    owned: s.owned,
                }
            })
            .collect();
        w.seeded_bytes = self.seeding.values().map(|s| s.bytes).sum();
        let mut fetching: Vec<Note> = self
            .fetching
            .keys()
            .map(|n| Note {
                item: n.clone(),
                detail: "downloading".into(),
            })
            .collect();
        fetching.extend(self.fetch_queue.iter().map(|n| Note {
            item: n.clone(),
            detail: "queued".into(),
        }));
        w.fetching = fetching;
        w.skipped = self.skipped.clone();
        w.taken_down = self.taken_down.clone();
        w.http.stats = self.http_stats.snapshot();
        if let Some(p) = pm {
            w.port_mapping = p;
        }
    }

    fn persist(&self) {
        let es = self.engine.stats();
        let doc = PersistedServe {
            schema: STATE_SCHEMA.into(),
            uploaded_bytes_total: self.base.uploaded_bytes_total + es.uploaded,
            downloaded_bytes_total: self.base.downloaded_bytes_total + es.downloaded,
            catalog_revision: self.base.catalog_revision,
            head_expires_unix: self.base.head_expires_unix,
            revisions: self
                .seeding
                .iter()
                .map(|(n, s)| PersistedRev {
                    name: n.clone(),
                    repo_type: s.repo.repo_type,
                    repo_id: s.repo.id.clone(),
                    commit: s.commit.clone(),
                    dir: s.dir.clone(),
                    owned: s.owned,
                    bytes: s.bytes,
                    only: s.only.clone(),
                })
                .collect(),
        };
        let r = serde_json::to_vec_pretty(&doc)
            .map_err(anyhow::Error::from)
            .and_then(|b| write_atomic(&self.data_dir.join("serve-state.json"), &b));
        if let Err(e) = r {
            tracing::warn!("writing serve-state.json: {e:#}");
        }
    }

    async fn shutdown(mut self) {
        for (_, h) in self.fetching.drain() {
            h.abort();
        }
        if let Some(p) = self.portmap.take() {
            p.shutdown().await;
        }
        self.refresh_status();
        self.persist();
        let _ = self.shutdown_tx.send(true);
        for s in self.servers.drain(..) {
            let _ = tokio::time::timeout(Duration::from_secs(5), s).await;
        }
        let es = self.engine.stats();
        self.engine.stop().await;
        println!(
            "stopped; uploaded {} bytes in this run ({} total), {} revision(s) were seeding",
            es.uploaded,
            self.base.uploaded_bytes_total + es.uploaded,
            self.seeding.len()
        );
    }
}

/// Files of a downloaded revision to seed: everything but what `pull`
/// skipped (pickle formats and unselected `--include` / `--exclude`
/// paths), or `None` for all. The seeder passes this list as
/// `only_files`, so a partial pull is announced as that subset, not as
/// a complete torrent with missing files.
fn skipped_only(dir: &Path, t: &Metainfo) -> Option<Vec<String>> {
    let skipped: Vec<String> = std::fs::read(dir.join(META_DIR).join("pull.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| {
            v.get("skipped").and_then(|s| s.as_array()).map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
        })
        .unwrap_or_default();
    (!skipped.is_empty()).then(|| {
        t.files
            .iter()
            .map(|f| f.path.clone())
            .filter(|p| !skipped.contains(p))
            .collect()
    })
}

/// Download one revision into `<root>/t/<name>/` with the pull pipeline and
/// put its torrent at `<root>/t/<name>.torrent`.
async fn fetch_revision(
    cfg: &Config,
    repo: &RepoRef,
    commit: &str,
    m: &Manifest,
    root: &Path,
) -> anyhow::Result<()> {
    let name = TorrentInfo::name_for(&repo.id, commit);
    let out = root.join("t").join(&name);
    let args = PullArgs {
        repo: format!("{}@{commit}", repo.id),
        repo_type: match repo.repo_type {
            RepoType::Model => RepoTypeArg::Model,
            RepoType::Dataset => RepoTypeArg::Dataset,
        },
        revision: None,
        out: Some(out.clone()),
        from_hf: false,
        manifest_file: None,
        manifest_url: None,
        no_hf_fallback: false,
        retry_dead_mirrors: false,
        jobs: None,
        allow_pickle: false,
        include: Vec::new(),
        exclude: Vec::new(),
        allow_gated: false,
        allow_license: Vec::new(),
        no_wait: true,
        ignore_tombstone: false,
        swarm: cfg.serve.fetch_swarm,
        no_swarm: !cfg.serve.fetch_swarm,
        swarm_only: false,
        peer: Vec::new(),
        seed_after: None,
    };
    let code = crate::consumer::pull(cfg, &args).await?;
    if code != ExitCode::SUCCESS {
        bail!("pull of {}@{} did not verify", repo.id, short(commit));
    }
    let tpath = root.join("t").join(format!("{name}.torrent"));
    let kept = out.join(META_DIR).join(PULLED_TORRENT);
    let bytes = match std::fs::read(&kept) {
        Ok(b) => b,
        Err(_) => fetch_for_manifest(m, &[]).await?.bytes,
    };
    write_atomic(&tpath, &bytes)
}

/// `seed --serve-catalog`: serve the publisher's own catalog files
/// (`<storage.local_dir>/<catalog>/`) on `serve.http_listen`.
pub async fn serve_local_catalog(cfg: &Config) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let name = cfg.catalog.name.clone();
    let dir = expand(&cfg.storage.local_dir).join(&name);
    if !dir.join(HEAD_PATH).exists() {
        bail!(
            "no local catalog at {} (publish first, or run `tensorseed serve --serve-catalog` to mirror a remote catalog)",
            dir.display()
        );
    }
    if cfg.serve.http_listen.is_empty() {
        bail!("--serve-catalog needs serve.http_listen (for example 0.0.0.0:8080)");
    }
    let l = tokio::net::TcpListener::bind(&cfg.serve.http_listen)
        .await
        .with_context(|| format!("binding serve.http_listen {}", cfg.serve.http_listen))?;
    println!(
        "catalog  http://{}/{name}/ from {}",
        l.local_addr()?,
        dir.display()
    );
    let router = web::router(HttpConfig {
        library: None,
        catalog: Some(CatalogDir { name, dir }),
        limiter: None,
        stats: Arc::new(HttpStats::default()),
    });
    Ok(tokio::spawn(async move {
        if let Err(e) = web::serve(l, router, std::future::pending()).await {
            tracing::error!("catalog server stopped: {e}");
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::result_large_err)] // figment::Error is large; fine in tests
    fn systemd_example_config_parses() {
        const TOML: &str = include_str!("../../../contrib/systemd/tensorseed.toml");
        figment::Jail::expect_with(|jail| {
            jail.create_file("tensorseed.toml", TOML)?;
            let cfg = crate::config::load(std::path::Path::new("tensorseed.toml"))
                .map_err(|e| e.to_string())?;
            assert_eq!(cfg.catalog.backend, "file");
            assert_eq!(cfg.serve.select, vec!["*".to_string()]);
            assert!(!cfg.serve.fetch && !cfg.serve.port_mapping);
            assert_eq!(cfg.serve.health_listen, "127.0.0.1:6880");
            Ok(())
        });
    }

    #[test]
    fn config_defaults_and_validation() {
        let s = ServeConfig::default();
        s.validate().unwrap();
        assert!(!s.port_mapping, "port mapping must be off by default");
        assert!(!s.fetch && s.http_listen.is_empty() && !s.serve_catalog);
        assert!(s.health_listen.starts_with("127.0.0.1:"));
        let bad = |f: fn(&mut ServeConfig)| {
            let mut s = ServeConfig::default();
            f(&mut s);
            s.validate().is_err()
        };
        assert!(bad(|s| s.sync_interval_secs = 0));
        assert!(bad(|s| s.http_listen = "nope".into()));
        assert!(bad(|s| s.types = vec!["space".into()]));
        assert!(bad(|s| s.license_classes = vec!["free".into()]));
        assert!(bad(|s| {
            s.port_mapping = true;
            s.port_mapping_lease_secs = 10;
        }));
        assert_eq!(kbps(0), None);
        assert_eq!(kbps(2), Some(2048));
    }

    #[test]
    fn watchlist_selection() {
        let mut cfg = Config::default();
        cfg.harvest.repos = vec!["a/b".into()];
        cfg.harvest.authors = vec!["TunedChaos".into()];
        cfg.serve.select = vec![];
        cfg.serve.select_watchlist = true;
        let r = cfg.serve.rules(&cfg);
        assert_eq!(r.select, vec!["a/b", "TunedChaos/*"]);
        assert!(r.rank("TunedChaos/ChaosNexus_Tuned_v1-GGUF").is_some());
        assert!(r.rank("other/x").is_none());
    }
}
