// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! `tensorseed doctor`: health checks for a client or seeder install.
//!
//! Every check returns a [`Check`] with a status ([`Status::Ok`],
//! [`Status::Warn`], or [`Status::Fail`]) and a one line message, collected
//! into a [`Report`]. Front ends only render the report: the CLI prints it
//! as text or JSON (`ts.doctor/1`), and Tensorseed Desktop can show the same
//! structure.
//!
//! What is checked:
//!
//! - the config file is found and parses;
//! - each followed catalog is reachable, its head signature verifies against
//!   the pinned key, and (with a Chia store) the head matches the chain
//!   anchor;
//! - the catalog's published mirror list verifies, and the mirrors answer;
//! - a Hugging Face token is present (and where it comes from; the token
//!   itself is never included), optionally checked with `whoami`;
//! - the configured torrent and `serve` ports are free, or held by a running
//!   `tensorseed serve`;
//! - free disk space on the download directory;
//! - the download directory, the DHT state file, and the state database
//!   location are writable;
//! - publishers only: each enabled upload target has its credentials in the
//!   environment (variable names are shown, values never are).
//!
//! Network checks run concurrently, each bounded by [`Options::timeout`],
//! and are skipped with [`Options::offline`]. Like `list`, the catalog check
//! syncs the local catalog copy; nothing else is written except a short
//! lived probe file in each directory checked for writability.

use std::net::{SocketAddr, TcpListener, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use tensorseed_catalog::anchor::CoinState;
use tensorseed_hf::HfError;

use crate::backend::{file_catalog, open_reader, pinned_key};
use crate::config::{ChainCheck, Config, Subscription, expand};

/// Schema of the JSON report.
pub const DOCTOR_SCHEMA: &str = "ts.doctor/1";

/// Default free space below which the disk check warns (10 GiB).
pub const DEFAULT_WARN_FREE_BYTES: u64 = 10 * GIB;

/// Free space below which the disk check fails (512 MiB).
pub const DEFAULT_FAIL_FREE_BYTES: u64 = 512 * 1024 * 1024;

const GIB: u64 = 1024 * 1024 * 1024;

/// Outcome of one check. Ordered from best to worst.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Working as expected.
    Ok,
    /// Works, but something deserves attention.
    Warn,
    /// Broken; the command that needs it will fail.
    Fail,
}

impl Status {
    /// `ok`, `warn`, or `fail`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

/// One check result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    /// Stable machine id, e.g. `catalog.tensorseed` or `port.torrent`.
    pub id: String,
    /// Short human title, e.g. `catalog tensorseed`.
    pub title: String,
    /// Result.
    pub status: Status,
    /// One line summary.
    pub message: String,
    /// Extra lines (per mirror results, hints). May be empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<String>,
}

impl Check {
    fn new(id: impl Into<String>, title: impl Into<String>, status: Status, msg: String) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            status,
            message: msg,
            details: Vec::new(),
        }
    }

    fn with_details(mut self, details: Vec<String>) -> Self {
        self.details = details;
        self
    }
}

/// Every check, in a stable order.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// [`DOCTOR_SCHEMA`].
    pub schema: &'static str,
    /// Tool version.
    pub version: String,
    /// Config file that was checked.
    pub config_path: PathBuf,
    /// True when network checks were skipped.
    pub offline: bool,
    /// Wall time of the whole run in milliseconds.
    pub elapsed_ms: u64,
    /// The checks.
    pub checks: Vec<Check>,
}

impl Report {
    /// The worst status of any check (`Ok` for an empty report).
    pub fn worst(&self) -> Status {
        self.checks
            .iter()
            .map(|c| c.status)
            .max()
            .unwrap_or(Status::Ok)
    }

    /// Number of checks with `status`.
    pub fn count(&self, status: Status) -> usize {
        self.checks.iter().filter(|c| c.status == status).count()
    }

    /// True when at least one check failed.
    pub fn has_failures(&self) -> bool {
        self.worst() == Status::Fail
    }
}

/// Knobs for [`run`].
#[derive(Debug, Clone)]
pub struct Options {
    /// Skip every network check.
    pub offline: bool,
    /// Upper bound for each network check.
    pub timeout: Duration,
    /// Warn below this many free bytes on the download directory.
    pub warn_free_bytes: u64,
    /// Fail below this many free bytes on the download directory.
    pub fail_free_bytes: u64,
    /// Ask the Hub who the token belongs to (one small request).
    pub whoami: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            offline: false,
            timeout: Duration::from_secs(15),
            warn_free_bytes: DEFAULT_WARN_FREE_BYTES,
            fail_free_bytes: DEFAULT_FAIL_FREE_BYTES,
            whoami: true,
        }
    }
}

/// Run every check against the config at `config_path`.
///
/// Never fails: problems are reported as checks. When the config cannot be
/// loaded, the report holds only that failure.
pub async fn run(config_path: &Path, opts: &Options) -> Report {
    let started = Instant::now();
    let (config_check, cfg) = check_config(config_path);
    let mut checks = vec![config_check];
    if let Some(cfg) = cfg {
        checks.extend(run_with_config(&cfg, opts).await);
    }
    Report {
        schema: DOCTOR_SCHEMA,
        version: env!("CARGO_PKG_VERSION").to_string(),
        config_path: config_path.to_path_buf(),
        offline: opts.offline,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        checks,
    }
}

/// The checks after the config check, for a config that is already loaded.
pub async fn run_with_config(cfg: &Config, opts: &Options) -> Vec<Check> {
    let mut out = Vec::new();
    // Network checks run together; local checks are instant.
    let catalogs = async {
        match cfg.subscriptions() {
            Ok(subs) if subs.is_empty() => vec![Check::new(
                "catalog",
                "catalog",
                Status::Fail,
                "no catalog is configured; set catalog.publisher_pubkey or add a [[catalogs]] entry"
                    .into(),
            )],
            Ok(subs) => {
                let runs = subs.iter().map(|s| check_catalog(cfg, s, opts));
                futures_util::future::join_all(runs)
                    .await
                    .into_iter()
                    .flatten()
                    .collect()
            }
            Err(e) => vec![Check::new(
                "catalog",
                "catalog",
                Status::Fail,
                format!("{e:#}"),
            )],
        }
    };
    let (cat_checks, hf, ports) =
        tokio::join!(catalogs, check_hf(cfg, opts), check_ports(cfg, opts));
    out.extend(cat_checks);
    out.push(hf);
    out.extend(ports);
    let download = expand(&cfg.general.download_dir);
    out.push(check_disk(&download, opts));
    out.push(check_writable_dir(
        "dir.download",
        "download dir",
        &download,
    ));
    out.push(check_dht(cfg));
    out.push(check_state_db(cfg));
    out.extend(check_storage(cfg));
    out
}

/// Config file found and parses.
pub fn check_config(path: &Path) -> (Check, Option<Config>) {
    let exists = path.is_file();
    match crate::config::load(path) {
        Ok(cfg) if exists => (
            Check::new(
                "config",
                "config",
                Status::Ok,
                format!("loaded {}", path.display()),
            ),
            Some(cfg),
        ),
        Ok(cfg) => (
            Check::new(
                "config",
                "config",
                Status::Warn,
                format!(
                    "no config file at {}; using built-in defaults and TENSORSEED_* variables",
                    path.display()
                ),
            )
            .with_details(vec![
                "copy config/tensorseed.example.toml there to follow the official catalog".into(),
            ]),
            Some(cfg),
        ),
        Err(e) => (
            Check::new(
                "config",
                "config",
                Status::Fail,
                format!("{}: {e:#}", path.display()),
            ),
            None,
        ),
    }
}

async fn timed<T>(t: Duration, f: impl Future<Output = T>) -> Result<T, String> {
    tokio::time::timeout(t, f)
        .await
        .map_err(|_| format!("timed out after {}s", t.as_secs()))
}

fn short(hex: &str) -> &str {
    hex.get(..12).unwrap_or(hex)
}

/// Published mirror list, catalog head and signature, chain anchor, and
/// mirror reachability for one followed catalog.
async fn check_catalog(cfg: &Config, sub: &Subscription, opts: &Options) -> Vec<Check> {
    let view = cfg.for_subscription(sub);
    let name = sub.display().to_string();
    let cat_id = format!("catalog.{}", sub.name);
    let cat_title = format!("catalog {name}");
    if let Err(e) = pinned_key(&view) {
        return vec![Check::new(
            cat_id,
            cat_title,
            Status::Fail,
            format!("{e:#}"),
        )];
    }
    if opts.offline {
        return vec![Check::new(
            cat_id,
            cat_title,
            Status::Ok,
            "skipped (offline)".into(),
        )];
    }
    if view.catalog.backend == "chia" {
        let msg = match timed(opts.timeout, open_reader(&view, None, false)).await {
            Ok(Ok((_, id))) => Check::new(
                cat_id,
                cat_title,
                Status::Ok,
                format!(
                    "DataLayer store {} is synced and passes the anchor check",
                    short(&id.0)
                ),
            ),
            Ok(Err(e)) => Check::new(cat_id, cat_title, Status::Fail, format!("{e:#}")),
            Err(e) => Check::new(cat_id, cat_title, Status::Fail, e),
        };
        return vec![msg];
    }
    let mut out = Vec::new();

    // Published mirror list first: the catalog reader uses it as extra bases.
    let published = if view.mirrors.fetch_published {
        let bases = mirror_bases(&view, None);
        let id = format!("mirrors.{}.published", sub.name);
        let title = format!("mirror list {name}");
        match timed(opts.timeout, crate::mirrors::fetch_published(&view, &bases)).await {
            Ok(Ok(Some(l))) => {
                out.push(Check::new(
                    id,
                    title,
                    Status::Ok,
                    format!(
                        "published list verified against the catalog key ({} mirror{})",
                        l.mirrors.len(),
                        if l.mirrors.len() == 1 { "" } else { "s" }
                    ),
                ));
                Some(l)
            }
            Ok(Ok(None)) => {
                out.push(
                    Check::new(
                        id,
                        title,
                        Status::Warn,
                        "no valid published mirror list (missing, or it failed verification)"
                            .into(),
                    )
                    .with_details(vec![
                        "run with -v to see why each source was skipped".into(),
                    ]),
                );
                None
            }
            Ok(Err(e)) => {
                out.push(Check::new(id, title, Status::Warn, format!("{e:#}")));
                None
            }
            Err(e) => {
                out.push(Check::new(id, title, Status::Warn, e));
                None
            }
        }
    } else {
        None
    };

    out.extend(check_file_catalog(&view, sub, &name, opts).await);

    let bases = mirror_bases(&view, published.as_ref());
    out.push(check_mirror_reachability(&view, sub, &name, &bases, opts).await);
    out
}

/// `file.base_url` followed by the merged mirror ranking, deduplicated.
fn mirror_bases(
    view: &Config,
    published: Option<&tensorseed_catalog::mirrors::MirrorList>,
) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    if !view.file.base_url.is_empty() {
        let mut b = view.file.base_url.clone();
        if !b.ends_with('/') {
            b.push('/');
        }
        v.push(b);
    }
    let ranked = match published {
        Some(l) => crate::mirrors::ranked(view, Some(l)),
        None => crate::mirrors::ranked(view, crate::mirrors::load_cached_published(view).as_ref()),
    };
    for r in ranked {
        if !v.contains(&r.base) {
            v.push(r.base);
        }
    }
    v
}

async fn check_file_catalog(
    view: &Config,
    sub: &Subscription,
    name: &str,
    opts: &Options,
) -> Vec<Check> {
    let cat_id = format!("catalog.{}", sub.name);
    let cat_title = format!("catalog {name}");
    let chain_id = format!("chain.{}", sub.name);
    let chain_title = format!("chain anchor {name}");
    let cat = match file_catalog(view, None) {
        Ok(c) => c,
        Err(e) => {
            return vec![Check::new(
                cat_id,
                cat_title,
                Status::Fail,
                format!("{e:#}"),
            )];
        }
    };
    let mode = view.chain_check();
    let strict = mode == ChainCheck::Strict;
    let has_store = !view.chia.catalog_store_id.trim().is_empty();

    let mut chain_check: Option<Check> = None;
    let anchor = if mode == ChainCheck::Off {
        chain_check = Some(if has_store {
            Check::new(
                chain_id.clone(),
                chain_title.clone(),
                Status::Warn,
                "chain check is off (catalog.chain_check = \"off\"); signatures only".into(),
            )
        } else {
            Check::new(
                chain_id.clone(),
                chain_title.clone(),
                Status::Ok,
                "no Chia store configured; the catalog is trusted on its signatures alone".into(),
            )
        });
        None
    } else {
        let fail = if strict { Status::Fail } else { Status::Warn };
        match timed(opts.timeout, crate::chain::chain_anchor(view, &cat)).await {
            Ok(Ok(r)) => Some(r),
            Ok(Err(e)) => {
                chain_check = Some(Check::new(
                    chain_id.clone(),
                    chain_title.clone(),
                    fail,
                    format!("{e:#}"),
                ));
                None
            }
            Err(e) => {
                chain_check = Some(Check::new(
                    chain_id.clone(),
                    chain_title.clone(),
                    fail,
                    format!("chain check {e}"),
                ));
                None
            }
        }
    };

    if let Some(r) = &anchor {
        cat.set_expected_head(Some((r.proof.anchor.revision, r.proof.anchor.root.clone())));
    }
    let synced = timed(opts.timeout, cat.sync()).await;
    cat.set_expected_head(None);
    let st = cat.state();
    let cat_check = match &synced {
        Ok(Ok(rep)) => {
            let mut c = Check::new(
                cat_id.clone(),
                cat_title.clone(),
                Status::Ok,
                format!(
                    "reachable at {}, revision {}, head signature valid ({} entries)",
                    rep.source,
                    st.revision,
                    st.entries.len()
                ),
            );
            if !rep.skipped.is_empty() {
                c.status = Status::Warn;
                c.details = rep
                    .skipped
                    .iter()
                    .map(|s| format!("skipped: {s}"))
                    .collect();
            }
            c
        }
        Ok(Err(e)) => Check::new(
            cat_id.clone(),
            cat_title.clone(),
            Status::Fail,
            if anchor.is_some() {
                format!("no catalog source serves the chain anchored head: {e}")
            } else {
                e.to_string()
            },
        ),
        Err(e) => Check::new(
            cat_id.clone(),
            cat_title.clone(),
            Status::Fail,
            format!("sync {e}"),
        ),
    };

    if let Some(r) = anchor {
        let a = &r.proof.anchor;
        let via = if r.endpoints.is_empty() {
            "local DataLayer".to_string()
        } else {
            r.endpoints.join(", ")
        };
        let synced_ok = matches!(synced, Ok(Ok(_)));
        chain_check = Some(if synced_ok && a.matches(st.revision, &st.root) {
            let (status, coin) = match r.state {
                CoinState::Current => (Status::Ok, "coin current".to_string()),
                CoinState::RecentlySpent { blocks_ago } => (
                    Status::Warn,
                    format!("chain moved on {blocks_ago} blocks ago; caches catching up"),
                ),
                CoinState::Stale { blocks_ago } => {
                    (Status::Warn, format!("coin spent {blocks_ago} blocks ago"))
                }
            };
            Check::new(
                chain_id,
                chain_title,
                status,
                format!(
                    "head matches Chia store {} at revision {} ({coin}, checked via {via})",
                    short(&r.proof.store_id),
                    a.revision
                ),
            )
        } else if synced_ok {
            Check::new(
                chain_id,
                chain_title,
                if strict { Status::Fail } else { Status::Warn },
                format!(
                    "catalog is at revision {} but the chain anchors revision {} root {}",
                    st.revision,
                    a.revision,
                    short(&a.root)
                ),
            )
        } else {
            Check::new(
                chain_id,
                chain_title,
                if strict { Status::Fail } else { Status::Warn },
                format!(
                    "Chia store {} anchors revision {} (via {via}), but no source served that head",
                    short(&r.proof.store_id),
                    a.revision
                ),
            )
        });
    }
    let mut out = vec![cat_check];
    out.extend(chain_check);
    out
}

async fn check_mirror_reachability(
    view: &Config,
    sub: &Subscription,
    name: &str,
    bases: &[String],
    opts: &Options,
) -> Check {
    let id = format!("mirrors.{}.reachable", sub.name);
    let title = format!("mirrors {name}");
    if bases.is_empty() {
        return Check::new(id, title, Status::Warn, "no catalog URLs configured".into());
    }
    let http = match reqwest::Client::builder()
        .user_agent(concat!("tensorseed/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(opts.timeout.min(Duration::from_secs(5)))
        .timeout(opts.timeout)
        .build()
    {
        Ok(h) => h,
        Err(e) => return Check::new(id, title, Status::Fail, format!("{e:#}")),
    };
    let catalog = &view.catalog.name;
    let probes = bases.iter().map(|b| {
        let http = http.clone();
        let url = format!("{b}{catalog}/head.json");
        async move {
            let t = Instant::now();
            let r = http.get(&url).send().await;
            let ms = t.elapsed().as_millis();
            match r {
                Ok(r) if r.status().is_success() => Ok(ms),
                Ok(r) => Err(format!("HTTP {}", r.status().as_u16())),
                Err(e) if e.is_timeout() => Err("timed out".into()),
                Err(e) if e.is_connect() => Err("connection failed".into()),
                Err(e) => Err(e.to_string()),
            }
        }
    });
    let results = futures_util::future::join_all(probes).await;
    let mut details = Vec::new();
    let mut up = 0usize;
    for (b, r) in bases.iter().zip(&results) {
        match r {
            Ok(ms) => {
                up += 1;
                details.push(format!("{b}  ok  {ms} ms"));
            }
            Err(e) => details.push(format!("{b}  unreachable  {e}")),
        }
    }
    let n = bases.len();
    let status = if up == n {
        Status::Ok
    } else if up == 0 {
        Status::Fail
    } else {
        Status::Warn
    };
    Check::new(
        id,
        title,
        status,
        format!("{up} of {n} catalog URLs answer"),
    )
    .with_details(details)
}

/// Hugging Face token presence and source; never the token itself.
async fn check_hf(cfg: &Config, opts: &Options) -> Check {
    let (id, title) = ("hf.token", "hugging face");
    let source = match cfg.huggingface.resolve_token_source() {
        Ok(Some((_token, source))) => source,
        Ok(None) => {
            return Check::new(
                id,
                title,
                Status::Ok,
                "no token; anonymous Hub access (a token is only needed for gated or private repos and higher rate limits)"
                    .into(),
            );
        }
        Err(e) => return Check::new(id, title, Status::Fail, format!("token unreadable: {e:#}")),
    };
    let from = format!("token from {}", source.label());
    if opts.offline || !opts.whoami {
        return Check::new(id, title, Status::Ok, from);
    }
    let client = match cfg.huggingface.client() {
        Ok(c) => c,
        Err(e) => return Check::new(id, title, Status::Fail, format!("{from}; {e:#}")),
    };
    match timed(opts.timeout, client.whoami()).await {
        Ok(Ok(user)) => Check::new(
            id,
            title,
            Status::Ok,
            format!("{from}, signed in as {user}"),
        ),
        Ok(Err(HfError::NotAccessible { status, .. })) => Check::new(
            id,
            title,
            Status::Fail,
            format!("{from}, but the Hub rejected it (HTTP {status}); run tensorseed hf login"),
        ),
        Ok(Err(e)) => Check::new(
            id,
            title,
            Status::Warn,
            format!("{from}; could not check it with the Hub: {e}"),
        ),
        Err(e) => Check::new(id, title, Status::Warn, format!("{from}; Hub check {e}")),
    }
}

/// Resolve `ADDR:PORT` (or a host name) to the first socket address.
fn resolve_addr(s: &str) -> Option<SocketAddr> {
    s.parse()
        .ok()
        .or_else(|| s.to_socket_addrs().ok().and_then(|mut a| a.next()))
}

/// What holds a port that is in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holder {
    Tensorseed,
    Unknown,
}

/// Probe `serve.health_listen` for a running `tensorseed serve`.
async fn serve_running(cfg: &Config, opts: &Options) -> bool {
    let Some(addr) = resolve_addr(&cfg.serve.health_listen) else {
        return false;
    };
    let host = if addr.ip().is_unspecified() {
        match addr {
            SocketAddr::V4(_) => SocketAddr::from(([127, 0, 0, 1], addr.port())),
            SocketAddr::V6(_) => SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, addr.port())),
        }
    } else {
        addr
    };
    let Ok(http) = reqwest::Client::builder()
        .timeout(opts.timeout.min(Duration::from_secs(2)))
        .build()
    else {
        return false;
    };
    let Ok(Ok(r)) = timed(
        Duration::from_secs(3),
        http.get(format!("http://{host}/health")).send(),
    )
    .await
    else {
        return false;
    };
    match r.json::<serde_json::Value>().await {
        Ok(v) => {
            v.get("schema").and_then(|s| s.as_str())
                == Some(tensorseed_serve::health::HEALTH_SCHEMA)
        }
        Err(_) => false,
    }
}

/// Whether `addr` can be bound right now.
fn port_status(addr: SocketAddr, holder: Holder) -> (Status, String) {
    if addr.port() == 0 {
        return (
            Status::Ok,
            format!("{addr}: a free port is picked at startup"),
        );
    }
    match TcpListener::bind(addr) {
        Ok(l) => {
            drop(l);
            (Status::Ok, format!("{addr} is free"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => match holder {
            Holder::Tensorseed => (
                Status::Ok,
                format!("{addr} is in use by a running tensorseed serve"),
            ),
            Holder::Unknown => (
                Status::Warn,
                format!("{addr} is in use by another program; seed and serve cannot listen there"),
            ),
        },
        Err(e) => (Status::Fail, format!("cannot listen on {addr}: {e}")),
    }
}

/// Torrent listen port and the `serve` HTTP and health ports.
async fn check_ports(cfg: &Config, opts: &Options) -> Vec<Check> {
    let holder = if serve_running(cfg, opts).await {
        Holder::Tensorseed
    } else {
        Holder::Unknown
    };
    let mut out = Vec::new();
    let torrent = format!("{}:{}", cfg.torrent.listen_addr, cfg.torrent.listen_port);
    let torrent = if cfg.torrent.listen_addr.contains(':') {
        format!("[{}]:{}", cfg.torrent.listen_addr, cfg.torrent.listen_port)
    } else {
        torrent
    };
    let ports = [
        (
            "port.torrent",
            "torrent port",
            torrent,
            "torrent.listen_addr / listen_port",
        ),
        (
            "port.serve_http",
            "serve http port",
            cfg.serve.http_listen.clone(),
            "serve.http_listen",
        ),
        (
            "port.serve_health",
            "serve health port",
            cfg.serve.health_listen.clone(),
            "serve.health_listen",
        ),
    ];
    for (id, title, raw, key) in ports {
        if raw.trim().is_empty() {
            out.push(Check::new(id, title, Status::Ok, format!("{key} not set")));
            continue;
        }
        let Some(addr) = resolve_addr(&raw) else {
            out.push(Check::new(
                id,
                title,
                Status::Fail,
                format!("{key} {raw:?} is not an address"),
            ));
            continue;
        };
        let (status, msg) = port_status(addr, holder);
        out.push(Check::new(id, title, status, msg));
    }
    out
}

/// Nearest existing ancestor of `p` (itself when it exists).
fn existing_ancestor(p: &Path) -> Option<PathBuf> {
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(p)
    };
    // Drop `.` components so `download_dir = "."` reads as the directory.
    let abs: PathBuf = joined
        .components()
        .filter(|c| !matches!(c, std::path::Component::CurDir))
        .collect();
    abs.ancestors().find(|a| a.exists()).map(Path::to_path_buf)
}

/// Bytes as `12.3 GiB`.
pub fn human_bytes(b: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// Status for `free` bytes against the thresholds.
fn disk_status(free: u64, opts: &Options) -> Status {
    if free < opts.fail_free_bytes {
        Status::Fail
    } else if free < opts.warn_free_bytes {
        Status::Warn
    } else {
        Status::Ok
    }
}

/// Free space on the file system holding `dir`.
fn check_disk(dir: &Path, opts: &Options) -> Check {
    let (id, title) = ("disk.download_dir", "disk space");
    let Some(at) = existing_ancestor(dir) else {
        return Check::new(
            id,
            title,
            Status::Fail,
            format!("no existing parent of {}", dir.display()),
        );
    };
    match fs4::available_space(&at) {
        Ok(free) => {
            let status = disk_status(free, opts);
            let mut msg = format!("{} free on {}", human_bytes(free), at.display());
            if status != Status::Ok {
                msg.push_str(&format!(
                    " (warns below {})",
                    human_bytes(opts.warn_free_bytes)
                ));
            }
            Check::new(id, title, status, msg)
        }
        Err(e) => Check::new(
            id,
            title,
            Status::Warn,
            format!("could not read free space on {}: {e}", at.display()),
        ),
    }
}

/// Create and remove a probe file in `dir`.
fn probe_write(dir: &Path) -> std::io::Result<()> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let p = dir.join(format!(
        ".tensorseed-doctor-{}-{nanos}.tmp",
        std::process::id()
    ));
    let res = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&p)
        .and_then(|mut f| std::io::Write::write_all(&mut f, b"ok"));
    let _ = std::fs::remove_file(&p);
    res
}

/// `dir` is writable, or would be creatable under a writable parent.
fn check_writable_dir(id: &str, title: &str, dir: &Path) -> Check {
    if dir.exists() {
        if !dir.is_dir() {
            return Check::new(
                id,
                title,
                Status::Fail,
                format!("{} exists but is not a directory", dir.display()),
            );
        }
        return match probe_write(dir) {
            Ok(()) => Check::new(
                id,
                title,
                Status::Ok,
                format!("{} is writable", dir.display()),
            ),
            Err(e) => Check::new(
                id,
                title,
                Status::Fail,
                format!("{} is not writable: {e}", dir.display()),
            ),
        };
    }
    match existing_ancestor(dir) {
        Some(a) if a.is_dir() => match probe_write(&a) {
            Ok(()) => Check::new(
                id,
                title,
                Status::Ok,
                format!(
                    "{} does not exist yet; it will be created under {} (writable)",
                    dir.display(),
                    a.display()
                ),
            ),
            Err(e) => Check::new(
                id,
                title,
                Status::Fail,
                format!(
                    "{} does not exist and {} is not writable: {e}",
                    dir.display(),
                    a.display()
                ),
            ),
        },
        _ => Check::new(
            id,
            title,
            Status::Fail,
            format!("{} cannot be created (no writable parent)", dir.display()),
        ),
    }
}

/// A file `path` can be written: its directory is writable, and an existing
/// file opens for writing.
fn check_writable_file(id: &str, title: &str, path: &Path) -> Check {
    let Some(parent) = path.parent() else {
        return Check::new(
            id,
            title,
            Status::Fail,
            format!("{} has no parent", path.display()),
        );
    };
    let mut c = check_writable_dir(id, title, parent);
    if c.status == Status::Ok && path.is_file() {
        if let Err(e) = std::fs::OpenOptions::new().append(true).open(path) {
            return Check::new(
                id,
                title,
                Status::Fail,
                format!("{} is not writable: {e}", path.display()),
            );
        }
        c.message = format!("{} is writable", path.display());
    } else if c.status == Status::Ok {
        c.message = format!("{} (will be created; directory writable)", path.display());
    }
    c
}

/// DHT routing table persistence file.
fn check_dht(cfg: &Config) -> Check {
    let (id, title) = ("dir.dht", "dht state");
    if !cfg.torrent.dht {
        return Check::new(
            id,
            title,
            Status::Ok,
            "DHT is off (torrent.dht = false)".into(),
        );
    }
    check_writable_file(id, title, &crate::torrents::dht_persistence_path())
}

/// sqlite state database (jobs, dead mirrors).
fn check_state_db(cfg: &Config) -> Check {
    check_writable_file("dir.state_db", "state db", &expand(&cfg.general.state_db))
}

/// Upload targets (publishers): local directories writable, S3
/// credentials present in the named environment variables. Nothing is
/// uploaded and no value is read into the report.
fn check_storage(cfg: &Config) -> Vec<Check> {
    let mut out = Vec::new();
    for t in cfg.storage.upload_targets() {
        let id = format!("storage.{}", t.name);
        let title = format!("storage {}", t.name);
        if t.provider == "local" {
            out.push(check_writable_dir(&id, &title, &expand(&t.dir)));
            continue;
        }
        let r = match t.resolve_s3() {
            Ok(r) => r,
            Err(e) => {
                out.push(Check::new(id, title, Status::Fail, format!("{e:#}")));
                continue;
            }
        };
        let settings = tensorseed_catalog::sink::S3Settings {
            endpoint: r.endpoint,
            region: r.region,
            bucket: t.bucket.clone(),
            prefix: t.prefix.clone(),
            path_style: r.path_style,
            access_key_env: t.access_key_env.clone(),
            secret_key_env: t.secret_key_env.clone(),
        };
        out.push(
            match tensorseed_catalog::sink::S3Config::from_env(&settings) {
                Ok(_) => Check::new(
                    id,
                    title,
                    Status::Ok,
                    format!(
                        "{} bucket {:?}; credentials present in {} and {}",
                        t.provider, t.bucket, t.access_key_env, t.secret_key_env
                    ),
                ),
                Err(e) => Check::new(
                    id,
                    title,
                    Status::Fail,
                    format!("{} ({}): {e}", t.name, t.provider),
                ),
            },
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(statuses: &[Status]) -> Report {
        Report {
            schema: DOCTOR_SCHEMA,
            version: "0".into(),
            config_path: PathBuf::from("x"),
            offline: true,
            elapsed_ms: 0,
            checks: statuses
                .iter()
                .enumerate()
                .map(|(i, s)| Check::new(format!("c{i}"), "c", *s, String::new()))
                .collect(),
        }
    }

    #[test]
    fn worst_status_and_counts() {
        assert_eq!(report(&[]).worst(), Status::Ok);
        let r = report(&[Status::Ok, Status::Warn, Status::Ok]);
        assert_eq!(r.worst(), Status::Warn);
        assert!(!r.has_failures());
        assert_eq!(r.count(Status::Ok), 2);
        let r = report(&[Status::Warn, Status::Fail]);
        assert!(r.has_failures());
    }

    #[test]
    fn json_shape_is_stable() {
        let r = report(&[Status::Warn]);
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["schema"], "ts.doctor/1");
        assert_eq!(v["checks"][0]["status"], "warn");
        assert!(v["checks"][0].get("details").is_none());
    }

    #[test]
    fn disk_thresholds() {
        let o = Options::default();
        assert_eq!(disk_status(100 * GIB, &o), Status::Ok);
        assert_eq!(disk_status(5 * GIB, &o), Status::Warn);
        assert_eq!(disk_status(100 * 1024 * 1024, &o), Status::Fail);
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(3 * GIB / 2), "1.5 GiB");
    }

    #[test]
    fn writable_checks() {
        let d = tempfile::tempdir().unwrap();
        let c = check_writable_dir("d", "d", d.path());
        assert_eq!(c.status, Status::Ok, "{}", c.message);
        let nested = d.path().join("a").join("b");
        let c = check_writable_dir("d", "d", &nested);
        assert_eq!(c.status, Status::Ok, "{}", c.message);
        assert!(c.message.contains("will be created"));
        assert!(!nested.exists(), "the check must not create directories");
        let f = d.path().join("file");
        std::fs::write(&f, b"x").unwrap();
        assert_eq!(check_writable_dir("d", "d", &f).status, Status::Fail);
        assert_eq!(check_writable_file("f", "f", &f).status, Status::Ok);
        // No probe files are left behind.
        let left: Vec<_> = std::fs::read_dir(d.path()).unwrap().collect();
        assert_eq!(left.len(), 1);
    }

    #[test]
    fn port_checks() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let taken = l.local_addr().unwrap();
        assert_eq!(port_status(taken, Holder::Unknown).0, Status::Warn);
        assert_eq!(port_status(taken, Holder::Tensorseed).0, Status::Ok);
        drop(l);
        let any = SocketAddr::from(([127, 0, 0, 1], 0));
        assert_eq!(port_status(any, Holder::Unknown).0, Status::Ok);
    }

    #[tokio::test]
    async fn offline_run_does_no_network_and_reports_every_area() {
        let d = tempfile::tempdir().unwrap();
        let cfg_path = d.path().join("tensorseed.toml");
        let dl = d.path().join("dl");
        std::fs::write(
            &cfg_path,
            format!(
                r#"
                [general]
                download_dir = {dl:?}
                state_db = {db:?}
                [catalog]
                backend = "file"
                publisher_pubkey = "RWS4/z1UPQ4ezmjrcpKqDYpqyn5h1WUS2oDs1B73Dh91kVzzNIZd03xd"
                [file]
                base_url = "https://catalog.example.invalid/"
                state_dir = {st:?}
                [torrent]
                listen_port = 0
                [serve]
                health_listen = ""
                [huggingface]
                token_env = "TENSORSEED_DOCTOR_TEST_NO_SUCH_VAR"
                token_file = ""
                "#,
                dl = dl.to_string_lossy(),
                db = d.path().join("state.sqlite").to_string_lossy(),
                st = d.path().join("state").to_string_lossy(),
            ),
        )
        .unwrap();
        let opts = Options {
            offline: true,
            ..Options::default()
        };
        let r = run(&cfg_path, &opts).await;
        let ids: Vec<&str> = r.checks.iter().map(|c| c.id.as_str()).collect();
        for want in [
            "config",
            "catalog.tensorseed",
            "hf.token",
            "port.torrent",
            "disk.download_dir",
            "dir.download",
            "dir.dht",
            "dir.state_db",
        ] {
            assert!(ids.contains(&want), "missing {want} in {ids:?}");
        }
        let get = |id: &str| r.checks.iter().find(|c| c.id == id).unwrap();
        assert_eq!(get("config").status, Status::Ok);
        assert_eq!(get("catalog.tensorseed").message, "skipped (offline)");
        assert_eq!(get("dir.download").status, Status::Ok);
    }

    #[test]
    fn storage_credentials_are_named_not_read() {
        let mut cfg = Config::default();
        let t: crate::config::StorageTarget = serde_json::from_value(serde_json::json!({
            "name": "aws",
            "provider": "aws",
            "region": "us-east-1",
            "bucket": "tensorseed",
            "access_key_env": "TS_DOCTOR_TEST_NO_KEY",
            "secret_key_env": "TS_DOCTOR_TEST_NO_SECRET",
        }))
        .unwrap();
        cfg.storage.target = vec![t];
        let c = check_storage(&cfg);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].status, Status::Fail);
        assert!(
            c[0].message.contains("TS_DOCTOR_TEST_NO_KEY"),
            "{}",
            c[0].message
        );
    }

    #[test]
    fn broken_config_is_one_failure() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("bad.toml");
        std::fs::write(&p, "[general]\nno_such_key = 1\n").unwrap();
        let (c, cfg) = check_config(&p);
        assert_eq!(c.status, Status::Fail);
        assert!(cfg.is_none());
    }
}
