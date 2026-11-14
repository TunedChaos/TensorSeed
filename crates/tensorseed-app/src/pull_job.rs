// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! The pull pipeline shared by `tensorseed pull` and Desktop: resolve the
//! revision (catalog, manifest file or URL, or the Hub), try the swarm,
//! then download and verify every selected file over HTTP, and write the
//! `.tensorseed` records.
//!
//! Nothing here prints. Progress goes to a [`PullControl`] sink as
//! [`PullEvent`]s, and the result comes back as a [`PullDone`]; each front
//! end renders both its own way. A run can be stopped through the
//! control's token: partial files and the job record are kept, and the
//! same options resume later (from `.tensorseed/pull-pending.json`, which
//! lives until the pull completes).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use tensorseed_core::keys::{RepoRef, RepoType};
use tensorseed_core::manifest::{FileEntry, Manifest};
use tensorseed_core::paths::{join_slash, native_path};
use tensorseed_core::verify::{ExpectedFile, META_DIR};
use tensorseed_fetch::{
    FetchError, FetchEvent, FetchOptions, Fetcher, FileJob, JobSpec, Observer, Outcome, Source,
    SourceKind, job_id,
};
use tensorseed_hf::policy::Policy as HfPolicy;
use tokio_util::sync::CancellationToken;

use crate::config::{Config, FetchSource, expand};
use crate::pull::{
    PULL_SCHEMA, PullFile, PullRecord, check_manifest_identity, default_out, fetch_bytes,
    hf_client, parse_manifest_bytes, parse_repo, state_db, tombstone_covers, write_meta,
};

/// File in `.tensorseed/` that holds the options of an unfinished pull.
pub const PENDING_FILE: &str = "pull-pending.json";
/// Files the swarm stage of an unfinished pull may have written in place
/// (a JSON list of relative paths), so a cancel can remove them.
pub const SWARM_FILE: &str = "pull-swarm.json";
/// Schema of [`PENDING_FILE`].
pub const PENDING_SCHEMA: &str = "ts.pullpending/1";
/// Lock file in `.tensorseed/`, held while a pull writes to the folder.
pub const LOCK_FILE: &str = "pull.lock";

/// What to pull and how. The CLI fills it from `tensorseed pull` flags;
/// Desktop from the entry page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PullOptions {
    /// `org/name`, optionally `@revision`.
    pub repo: String,
    /// Model or dataset.
    pub repo_type: RepoType,
    /// Revision (commit, branch, or tag).
    pub revision: Option<String>,
    /// Output directory (default: under `general.download_dir`).
    pub out: Option<PathBuf>,
    /// Hashes from the Hub instead of a catalog.
    pub from_hf: bool,
    /// Plain or signed manifest file.
    pub manifest_file: Option<PathBuf>,
    /// Plain or signed manifest URL.
    pub manifest_url: Option<String>,
    /// Never use the Hub as an HTTP source.
    pub no_hf_fallback: bool,
    /// Ignore dead mirror marks for this run.
    pub retry_dead_mirrors: bool,
    /// Parallel downloads (default `general.max_concurrent_downloads`).
    pub jobs: Option<usize>,
    /// Include pickle based formats.
    pub allow_pickle: bool,
    /// Only these paths (globs).
    pub include: Vec<String>,
    /// Not these paths (globs); wins over `include`.
    pub exclude: Vec<String>,
    /// Allow gated repos (`from_hf`).
    pub allow_gated: bool,
    /// License overrides (`from_hf`).
    pub allow_license: Vec<String>,
    /// Do not wait for a catalog sync.
    pub no_wait: bool,
    /// Only this catalog.
    pub catalog: Option<String>,
    /// Download a taken down revision anyway.
    pub ignore_tombstone: bool,
    /// Force the swarm on.
    pub swarm: bool,
    /// Skip the swarm.
    pub no_swarm: bool,
    /// Swarm only, no HTTP sources.
    pub swarm_only: bool,
    /// Extra peers (`host:port`).
    pub peer: Vec<String>,
    /// Fetch the torrent even when the swarm is off (to seed afterwards).
    pub need_torrent: bool,
}

impl Default for PullOptions {
    fn default() -> Self {
        Self {
            repo: String::new(),
            repo_type: RepoType::Model,
            revision: None,
            out: None,
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
            no_wait: false,
            catalog: None,
            ignore_tombstone: false,
            swarm: false,
            no_swarm: false,
            swarm_only: false,
            peer: Vec::new(),
            need_torrent: false,
        }
    }
}

/// How one file ended, in the order the CLI prints them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileLine {
    /// Downloaded and verified over HTTP.
    Downloaded {
        /// Relative path.
        path: String,
        /// Source kind.
        kind: SourceKind,
        /// Bytes that were already there.
        resumed_from: u64,
    },
    /// Written by the swarm and verified.
    Swarm {
        /// Relative path.
        path: String,
    },
    /// Already on disk and verified.
    AlreadyPresent {
        /// Relative path.
        path: String,
    },
    /// Every source failed.
    Failed {
        /// Relative path.
        path: String,
        /// Errors from each source.
        errors: Vec<String>,
        /// Swarm only run (no HTTP sources were tried).
        swarm_only: bool,
    },
}

/// Progress of a pull.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullEvent {
    /// A catalog took the revision down while another still lists it.
    TakenDownIn(String),
    /// The revision is resolved and the download starts.
    Started {
        /// Job id in the shared job database (`tensorseed jobs`).
        job: String,
        /// `model org/name@commit`.
        label: String,
        /// Selected files.
        files: usize,
        /// Selected bytes.
        bytes: u64,
        /// Output directory.
        out: PathBuf,
        /// `catalog`, `manifest-file`, `manifest-url`, or `huggingface`.
        source: String,
    },
    /// A file is not downloaded: unselected, or a pickle format.
    Skipped {
        /// Relative path.
        path: String,
        /// True for a pickle format, false for unselected.
        pickle: bool,
    },
    /// The swarm step is starting.
    SwarmStarted,
    /// One line about the swarm step (`swarm  ...`).
    Swarm(String),
    /// Swarm step progress, about once a second.
    SwarmProgress {
        /// Bytes fetched from peers so far.
        downloaded: u64,
        /// Connected peers.
        peers: u32,
        /// Download speed in bytes per second.
        rate: u64,
    },
    /// Files the swarm step wrote in place; the HTTP pass verifies them
    /// next and reports them as already present.
    SwarmWrote(Vec<String>),
    /// A dead mirror was skipped.
    DeadMirror(String),
    /// Live download progress.
    Fetch(FetchEvent),
    /// A file result, after all downloads ended.
    File(FileLine),
}

impl PullEvent {
    /// The text `tensorseed pull` prints for this event, and whether it
    /// goes to standard error. `None` for live progress, which only
    /// graphical front ends show.
    pub fn line(&self) -> Option<(String, bool)> {
        let out = |s: String| Some((s, false));
        match self {
            Self::TakenDownIn(c) => Some((
                format!("taken down in catalog {c} (still listed by another catalog)"),
                true,
            )),
            Self::Started {
                label,
                files,
                job: _,
                bytes,
                out: dir,
                source,
            } => out(format!(
                "pulling {label} ({files} files, {bytes} bytes) into {} from {source}",
                dir.display()
            )),
            Self::Skipped {
                path,
                pickle: false,
            } => out(format!("skip   {path} (unselected)")),
            Self::Skipped { path, pickle: true } => out(format!(
                "skip   {path} (pickle format; --allow-pickle to include)"
            )),
            Self::Swarm(line) | Self::DeadMirror(line) => out(line.clone()),
            Self::File(FileLine::Downloaded {
                path,
                kind,
                resumed_from,
            }) => {
                let resumed = if *resumed_from > 0 {
                    format!(", resumed at {resumed_from}")
                } else {
                    String::new()
                };
                out(format!("ok     {path} ({}{resumed})", kind.label()))
            }
            Self::File(FileLine::Swarm { path }) => out(format!("ok     {path} (swarm)")),
            Self::File(FileLine::AlreadyPresent { path }) => {
                out(format!("ok     {path} (already present)"))
            }
            Self::File(FileLine::Failed {
                path,
                swarm_only: true,
                ..
            }) => out(format!(
                "FAILED {path} (the swarm did not deliver it; --swarm-only)"
            )),
            Self::File(FileLine::Failed { path, errors, .. }) => {
                let mut s = format!("FAILED {path}");
                for e in errors {
                    s.push_str(&format!("\n         {e}"));
                }
                out(s)
            }
            Self::SwarmStarted
            | Self::SwarmProgress { .. }
            | Self::SwarmWrote(_)
            | Self::Fetch(_) => None,
        }
    }
}

/// The line for one file as the fetcher finishes it, for front ends that
/// list files as they go (the CLI prints [`PullEvent::File`] lines at the
/// end instead). `swarm` holds the files the swarm step wrote
/// ([`PullEvent::SwarmWrote`]); they show as from the swarm.
pub fn live_file_line(r: &tensorseed_fetch::FileResult, swarm: &HashSet<String>) -> String {
    let line = match &r.outcome {
        Outcome::Downloaded {
            kind, resumed_from, ..
        } => FileLine::Downloaded {
            path: r.path.clone(),
            kind: *kind,
            resumed_from: *resumed_from,
        },
        Outcome::AlreadyPresent if swarm.contains(&r.path) => FileLine::Swarm {
            path: r.path.clone(),
        },
        Outcome::AlreadyPresent => FileLine::AlreadyPresent {
            path: r.path.clone(),
        },
        Outcome::Failed => FileLine::Failed {
            path: r.path.clone(),
            errors: r.errors.clone(),
            swarm_only: false,
        },
    };
    PullEvent::File(line)
        .line()
        .map(|l| l.0)
        .unwrap_or_default()
}

/// Result of a pull that ran to the end (files may still have failed).
#[derive(Debug, Clone)]
pub struct PullDone {
    /// Output directory.
    pub out: PathBuf,
    /// `model org/name@commit`.
    pub label: String,
    /// Repo.
    pub repo: RepoRef,
    /// Commit.
    pub commit: String,
    /// Where the hashes came from.
    pub source: String,
    /// Hashes came from a signed catalog entry.
    pub signed: bool,
    /// Every selected file is in place and verified.
    pub ok: bool,
    /// The swarm step ran.
    pub swarm_ran: bool,
    /// Files the swarm delivered.
    pub swarm_files: usize,
    /// Bytes the swarm delivered.
    pub swarm_bytes: u64,
    /// Files downloaded over HTTP.
    pub downloaded: usize,
    /// Bytes downloaded over HTTP.
    pub http_bytes: u64,
    /// Files already present before this run.
    pub present: usize,
    /// Files that failed.
    pub failed: usize,
    /// The revision's torrent, when it was fetched.
    pub torrent: Option<tensorseed_torrent::Metainfo>,
}

/// The error a stopped pull returns (find it with `downcast_ref`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stopped;

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("stopped; run the same pull again to resume")
    }
}

impl std::error::Error for Stopped {}

/// True when `e` is a stopped pull.
pub fn is_stopped(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Stopped>().is_some()
}

type Sink = Arc<dyn Fn(PullEvent) + Send + Sync>;

/// Where events go and how to stop a pull.
#[derive(Clone)]
pub struct PullControl {
    sink: Sink,
    stop: CancellationToken,
}

impl PullControl {
    /// Send events to `sink`.
    pub fn new(sink: impl Fn(PullEvent) + Send + Sync + 'static) -> Self {
        Self {
            sink: Arc::new(sink),
            stop: CancellationToken::new(),
        }
    }

    /// Stop the pull when `token` is cancelled.
    #[must_use]
    pub fn stop_on(mut self, token: CancellationToken) -> Self {
        self.stop = token;
        self
    }

    fn emit(&self, ev: PullEvent) {
        (self.sink)(ev);
    }

    fn check(&self) -> anyhow::Result<()> {
        if self.stop.is_cancelled() {
            return Err(Stopped.into());
        }
        Ok(())
    }
}

/// The options file of an unfinished pull.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pending {
    schema: String,
    options: PullOptions,
}

/// Options of the unfinished pull in `dir`, if any. They name the
/// resolved commit and the output directory, so running them again
/// resumes the same job.
///
/// # Errors
///
/// Fails when the file is there but does not parse.
pub fn read_pending(dir: &Path) -> anyhow::Result<Option<PullOptions>> {
    let p = dir.join(META_DIR).join(PENDING_FILE);
    let bytes = match std::fs::read(&p) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", p.display())),
    };
    let v: Pending =
        serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", p.display()))?;
    if v.schema != PENDING_SCHEMA {
        bail!("{}: unknown schema {}", p.display(), v.schema);
    }
    Ok(Some(v.options))
}

/// Forget the unfinished pull in `dir` (after it completes or is
/// cancelled).
pub fn clear_pending(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(META_DIR).join(PENDING_FILE));
    let _ = std::fs::remove_file(dir.join(META_DIR).join(SWARM_FILE));
}

/// Delete the files the swarm stage wrote in place that no HTTP pass has
/// verified since (`done` holds the verified paths). The engine
/// preallocates whole files, so they can be any mix of written and empty
/// pieces.
fn remove_unverified_swarm_files(out: &Path, done: &HashSet<String>) -> anyhow::Result<()> {
    let p = out.join(META_DIR).join(SWARM_FILE);
    let paths: Vec<String> = match std::fs::read(&p) {
        Ok(b) => serde_json::from_slice(&b).with_context(|| format!("parsing {}", p.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", p.display())),
    };
    for rel in paths.iter().filter(|r| !done.contains(*r)) {
        let Ok(f) = tensorseed_core::verify::safe_join(out, rel) else {
            continue;
        };
        match std::fs::remove_file(&f) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing {}", f.display())),
        }
    }
    remove_empty_dirs(out);
    Ok(())
}

/// Remove `out` when nothing but the pull lock is left in it (a pull
/// cancelled before any file finished), and its parent folder when that
/// is then empty and inside `root` (the `org` folder of `org/name` in the
/// download folder).
fn remove_if_unused(out: &Path, root: &Path) {
    let meta = out.join(META_DIR);
    let only = |dir: &Path, name: &str| {
        std::fs::read_dir(dir).is_ok_and(|rd| {
            rd.flatten()
                .map(|e| e.file_name())
                .all(|n| n == std::ffi::OsStr::new(name))
        })
    };
    if !only(out, META_DIR) || !only(&meta, LOCK_FILE) {
        return;
    }
    let _ = std::fs::remove_file(meta.join(LOCK_FILE));
    let _ = std::fs::remove_dir(&meta);
    if std::fs::remove_dir(out).is_ok()
        && let Some(parent) = out.parent()
        && parent.starts_with(root)
        && parent != root
    {
        let _ = std::fs::remove_dir(parent);
    }
}

/// Remove empty folders under `dir` (not `dir` itself or the metadata).
fn remove_empty_dirs(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() && e.file_name() != META_DIR {
            remove_empty_dirs(&p);
            let _ = std::fs::remove_dir(&p);
        }
    }
}

/// Options that resume job `rec` of the job database: its pending file,
/// or for jobs started before pending files existed, the repo, commit, and
/// folder in its record (other options at their defaults).
pub fn resume_options(rec: &tensorseed_fetch::state::JobRecord) -> Option<PullOptions> {
    let out = PathBuf::from(&rec.out_dir);
    if let Ok(Some(mut o)) = read_pending(&out) {
        o.out = Some(out);
        return Some(o);
    }
    let (ty, rest) = rec.label.split_once(' ')?;
    let (id, commit) = rest.rsplit_once('@')?;
    let repo_type = match ty {
        "model" => RepoType::Model,
        "dataset" => RepoType::Dataset,
        _ => return None,
    };
    Some(PullOptions {
        repo: id.to_string(),
        repo_type,
        revision: Some(commit.to_string()),
        out: Some(out),
        ..PullOptions::default()
    })
}

/// Cancel an unfinished pull for good: delete its partial downloads and
/// pending file and drop job `job` from the job database. Completed files
/// stay. Refuses while a pull is writing to `out`.
///
/// # Errors
///
/// Fails when a pull is running in `out`, or the database or the partial
/// folder cannot be changed.
pub fn discard(cfg: &Config, out: &Path, job: &str) -> anyhow::Result<()> {
    let _lock = lock_out(out)?;
    let db = state_db(cfg)?;
    let done: HashSet<String> = db
        .job_files(job)?
        .into_iter()
        .filter(|f| f.status == tensorseed_fetch::state::status::DONE)
        .map(|f| f.path)
        .collect();
    remove_unverified_swarm_files(out, &done)?;
    let partial = out.join(META_DIR).join("partial");
    match std::fs::remove_dir_all(&partial) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("removing {}", partial.display())),
    }
    clear_pending(out);
    db.clear_job(job)?;
    drop(_lock);
    remove_if_unused(out, &expand(&cfg.general.download_dir));
    Ok(())
}

/// Take the pull lock of `out`, so the CLI and Desktop never write the same
/// folder at once. Released when the returned file closes.
fn lock_out(out: &Path) -> anyhow::Result<std::fs::File> {
    let meta = out.join(META_DIR);
    std::fs::create_dir_all(&meta).with_context(|| format!("creating {}", meta.display()))?;
    let p = meta.join(LOCK_FILE);
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&p)
        .with_context(|| format!("opening {}", p.display()))?;
    match fs4::FileExt::try_lock(&f) {
        Ok(()) => Ok(f),
        Err(fs4::TryLockError::WouldBlock) => bail!(
            "another pull is writing to {} (tensorseed pull or Desktop); wait for it to finish or stop it",
            out.display()
        ),
        Err(fs4::TryLockError::Error(e)) => {
            Err(e).with_context(|| format!("locking {}", p.display()))
        }
    }
}

/// True when a pull (in any process) is writing to `out` right now.
pub fn pull_running_in(out: &Path) -> bool {
    let p = out.join(META_DIR).join(LOCK_FILE);
    let Ok(f) = std::fs::OpenOptions::new().write(true).open(&p) else {
        return false;
    };
    matches!(
        fs4::FileExt::try_lock(&f),
        Err(fs4::TryLockError::WouldBlock)
    )
}

fn hf_policy(cfg: &Config, o: &PullOptions) -> HfPolicy {
    HfPolicy {
        license_allowlist: cfg.policy.license_allowlist.clone(),
        attribution_licenses: cfg.policy.attribution_licenses.clone(),
        allow_gated: cfg.policy.allow_gated || o.allow_gated,
        allow_pickle_formats: cfg.policy.allow_pickle_formats || o.allow_pickle,
        license_overrides: o.allow_license.clone(),
    }
}

/// Skip reason for a file, or `None` to download it.
///
/// `exclude` wins over `include`. Unselected paths are skipped before the
/// pickle policy, so a path that is both unselected and a pickle file is
/// recorded as unselected.
fn skip_reason(ef: &ExpectedFile, o: &PullOptions, allow_pickle: bool) -> Option<&'static str> {
    if !tensorseed_serve::select::path_selected(&ef.path, &o.include, &o.exclude) {
        return Some("unselected");
    }
    if ef.pickle_risk && !allow_pickle {
        return Some("pickle");
    }
    None
}

/// Fail when `include` / `exclude` matched no files in the revision.
fn reject_empty_selection(
    expected: &[ExpectedFile],
    o: &PullOptions,
    repo_id: &str,
) -> anyhow::Result<()> {
    if o.include.is_empty() && o.exclude.is_empty() {
        return Ok(());
    }
    if expected
        .iter()
        .all(|f| !tensorseed_serve::select::path_selected(&f.path, &o.include, &o.exclude))
    {
        bail!("no files matched --include/--exclude for {repo_id}");
    }
    Ok(())
}

/// Whether this pull should try the swarm. `from_hf` and `no_swarm` skip
/// it; `swarm_only` and `swarm` force it on; otherwise `torrent.pull_swarm`
/// (on by default) is used. A missing torrent is skipped later, silently.
pub fn swarm_wanted(cfg: &Config, o: &PullOptions) -> bool {
    if o.from_hf || o.no_swarm {
        false
    } else if o.swarm_only || o.swarm {
        true
    } else {
        cfg.torrent.pull_swarm && cfg.fetch.wants(FetchSource::Swarm)
    }
}

/// HTTP sources for one file, in `[fetch] source_order`.
///
/// `swarm_only` returns no HTTP sources. `no_hf_fallback` omits the Hub.
/// A gated or missing file on the Hub is a 401/403/404 that the fetcher
/// treats as "try the next source" without marking the host dead.
fn http_sources(
    cfg: &Config,
    o: &PullOptions,
    seeds: &Manifest,
    path: &str,
    hub: Source,
) -> anyhow::Result<Vec<Source>> {
    if o.swarm_only {
        return Ok(Vec::new());
    }
    let mut sources = Vec::new();
    for kind in cfg.fetch.sources()? {
        match kind {
            FetchSource::Swarm => {}
            FetchSource::Hub => {
                if !o.no_hf_fallback {
                    sources.push(hub.clone());
                }
            }
            FetchSource::Mirrors => {
                sources.extend(seeds.webseed_file_urls(path).into_iter().map(|url| Source {
                    url,
                    kind: SourceKind::Webseed,
                    bearer: None,
                }));
            }
        }
    }
    if sources.is_empty() && !swarm_wanted(cfg, o) {
        bail!("{path} has no HTTP sources (check fetch.source_order and --no-hf-fallback)");
    }
    Ok(sources)
}

/// Delete the empty files the torrent engine created for paths that were
/// not selected and did not exist before the swarm ran, so a partial pull
/// leaves only what was asked for (and `verify` reports them as skipped).
fn remove_swarm_placeholders(
    out: &Path,
    expected: &[ExpectedFile],
    selected: &[FileJob],
    existed: &HashSet<String>,
) {
    let wanted: HashSet<&str> = selected.iter().map(|f| f.path.as_str()).collect();
    for e in expected {
        if wanted.contains(e.path.as_str()) || existed.contains(&e.path) || e.size == 0 {
            continue;
        }
        let p = join_slash(out, &e.path);
        if std::fs::metadata(&p).is_ok_and(|m| m.is_file() && m.len() == 0) {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// Swarm first: let librqbit fetch what peers have, in place. The session
/// is outbound only (no listen port). Nothing it writes is trusted: the
/// HTTP fetcher that runs next hashes every file against the manifest,
/// keeps the ones that match, and downloads the rest.
async fn swarm_download(
    cfg: &Config,
    o: &PullOptions,
    ctl: &PullControl,
    t: &tensorseed_torrent::Metainfo,
    out: &Path,
    files: &[FileJob],
) -> anyhow::Result<()> {
    use tensorseed_torrent::engine::{DownloadOptions, DownloadStop, Engine};
    let mut peers = cfg.torrent.peer_addrs()?;
    for p in &o.peer {
        use std::net::ToSocketAddrs;
        peers.extend(
            p.to_socket_addrs()
                .with_context(|| format!("--peer {p:?}"))?,
        );
    }
    let timeout = Duration::from_secs(cfg.torrent.swarm_timeout_secs.max(1));
    // swarm_only has no HTTP fallback, so wait the full timeout.
    let (no_peer, stall) = if o.swarm_only {
        (None, None)
    } else {
        (
            (cfg.torrent.swarm_no_peer_secs > 0)
                .then_some(Duration::from_secs(cfg.torrent.swarm_no_peer_secs)),
            (cfg.torrent.swarm_stall_secs > 0)
                .then_some(Duration::from_secs(cfg.torrent.swarm_stall_secs)),
        )
    };
    // listen = false: outbound connections only; pull does not open a port.
    let engine = Engine::start(crate::torrents::engine_options(cfg, false, out)?)
        .await
        .context("starting the BitTorrent engine")?;
    let _speed = crate::swarm::apply_plan(engine.rate_handle(), cfg.torrent.speed_plan()?);
    ctl.emit(PullEvent::SwarmStarted);
    let t0 = std::time::Instant::now();
    let download = engine.download(
        t,
        out,
        DownloadOptions {
            peers,
            only: Some(files.iter().map(|f| f.path.clone()).collect()),
            no_peer,
            stall,
        },
        timeout,
    );
    tokio::pin!(download);
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let r = loop {
        tokio::select! {
            r = &mut download => break r,
            () = ctl.stop.cancelled() => {
                engine.stop().await;
                return Err(Stopped.into());
            }
            _ = tick.tick() => {
                let s = engine.stats();
                ctl.emit(PullEvent::SwarmProgress {
                    downloaded: s.downloaded,
                    peers: s.peers_live,
                    rate: (s.download_mibps * 1024.0 * 1024.0) as u64,
                });
            }
        }
    };
    match r {
        Ok((h, p, stop)) => {
            let line = match (stop, o.swarm_only) {
                (DownloadStop::Finished, _) => format!(
                    "swarm  {} of {} bytes in {:.1}s",
                    p.have,
                    p.total,
                    t0.elapsed().as_secs_f64()
                ),
                (DownloadStop::NoPeers, _) => format!(
                    "swarm  no peers after {}s; using HTTP sources",
                    cfg.torrent.swarm_no_peer_secs
                ),
                (DownloadStop::Stall, _) => format!(
                    "swarm  no progress for {}s; using HTTP sources",
                    cfg.torrent.swarm_stall_secs
                ),
                (DownloadStop::Timeout, true) => format!(
                    "swarm  {} of {} bytes in {:.1}s (--swarm-only: the rest fails)",
                    p.have,
                    p.total,
                    t0.elapsed().as_secs_f64()
                ),
                (DownloadStop::Timeout, false) => format!(
                    "swarm  {} of {} bytes in {:.1}s (HTTP sources fill the rest)",
                    p.have,
                    p.total,
                    t0.elapsed().as_secs_f64()
                ),
            };
            ctl.emit(PullEvent::Swarm(line));
            let _ = engine.remove(&h).await;
        }
        Err(e) if o.swarm_only => ctl.emit(PullEvent::Swarm(format!("swarm  unavailable ({e})"))),
        Err(e) => ctl.emit(PullEvent::Swarm(format!(
            "swarm  unavailable ({e}); using HTTP sources"
        ))),
    }
    engine.stop().await;
    Ok(())
}

/// Run a pull to the end, or until `ctl` stops it (the error is then
/// [`Stopped`]).
///
/// # Errors
///
/// Fails when the revision cannot be resolved, is taken down (unless
/// `ignore_tombstone`), the selection matches nothing, the output
/// directory cannot be written, or the job database fails. Files that do
/// not download are reported in [`PullDone`], not as an error.
pub async fn run(cfg: &Config, o: &PullOptions, ctl: &PullControl) -> anyhow::Result<PullDone> {
    let use_swarm = swarm_wanted(cfg, o);
    let mut torrent: Option<tensorseed_torrent::Metainfo> = None;
    let (repo, revision) = parse_repo(&o.repo, o.repo_type, o.revision.as_deref())?;
    let hf = hf_client(cfg)?;
    let jobs = o
        .jobs
        .unwrap_or(cfg.general.max_concurrent_downloads as usize)
        .max(1);

    let mut expected: Vec<ExpectedFile> = Vec::new();
    let mut files: Vec<FileJob> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut unselected: Vec<String> = Vec::new();
    let commit: String;
    let source: &str;
    let mut signed = false;
    let mut manifest_raw: Option<Vec<u8>> = None;
    let mut notice: Option<String> = None;
    let hf_source = |commit: &str, path: &str| Source {
        url: hf.resolve_url(&repo, commit, path),
        kind: SourceKind::HuggingFace,
        bearer: hf.token().map(str::to_owned),
    };

    if o.from_hf {
        source = "huggingface";
        let info = hf.repo_info(&repo, revision.as_deref()).await?;
        if info.disabled {
            bail!("{} is disabled on the Hub", repo.id);
        }
        let tree = hf.files(&repo, &info.sha).await?;
        let policy = hf_policy(cfg, o);
        let report = policy.evaluate(&info, &tree);
        if report.gated && !policy.allow_gated {
            bail!(
                "{} is gated on the Hub; pass --allow-gated (with HF_TOKEN set and the terms accepted) to download it",
                repo.id
            );
        }
        for r in &report.refusals {
            if !r.contains("gated") {
                tracing::warn!("{r} (allowed for a personal download, would not be published)");
            }
        }
        if let Some(l) = &report.license {
            notice = Some(format!(
                "{} {} at {}\nLicense: {l} ({:?})\nSource: https://huggingface.co/{}{}\n",
                repo.repo_type,
                repo.id,
                info.sha,
                report.class,
                if repo.repo_type == RepoType::Dataset {
                    "datasets/"
                } else {
                    ""
                },
                repo.id
            ));
        }
        commit = info.sha.clone();
        for e in &tree {
            let ef = ExpectedFile {
                path: e.path.clone(),
                size: e.content_size(),
                digest: e.expected_digest()?,
                pickle_risk: FileEntry::path_has_pickle_risk(&e.path),
            };
            if let Some(reason) = skip_reason(&ef, o, policy.allow_pickle_formats) {
                skipped.push(ef.path.clone());
                if reason == "unselected" {
                    unselected.push(ef.path.clone());
                }
            } else {
                files.push(FileJob {
                    path: ef.path.clone(),
                    size: ef.size,
                    digest: ef.digest.clone(),
                    sources: vec![hf_source(&commit, &ef.path)],
                });
            }
            expected.push(ef);
        }
        reject_empty_selection(&expected, o, &repo.id)?;
    } else {
        let resolved = if let Some(p) = &o.manifest_file {
            source = "manifest-file";
            parse_manifest_bytes(
                cfg,
                &std::fs::read(p).with_context(|| format!("reading {}", p.display()))?,
                &p.display().to_string(),
            )?
        } else if let Some(u) = &o.manifest_url {
            source = "manifest-url";
            parse_manifest_bytes(cfg, &fetch_bytes(u).await?, u)?
        } else {
            source = "catalog";
            let r = crate::pull::catalog_manifest(
                cfg,
                &repo,
                revision.as_deref(),
                o.no_wait,
                o.catalog.as_deref(),
            )
            .await?;
            for c in &r.taken_down_in {
                ctl.emit(PullEvent::TakenDownIn(c.clone()));
            }
            r
        };
        let m = &resolved.manifest;
        if o.manifest_file.is_some() || o.manifest_url.is_some() {
            check_manifest_identity(m, &repo, revision.as_deref().unwrap_or(&m.hf_commit))?;
        }
        if let Some(t) = &resolved.tombstone
            && tombstone_covers(t, &m.hf_commit)
        {
            if o.ignore_tombstone {
                tracing::warn!(
                    "{} is taken down ({:?}: {}); downloading anyway",
                    repo.id,
                    t.reason,
                    t.detail
                );
            } else {
                bail!(
                    "{} is taken down ({:?}: {}). Use --ignore-tombstone to download anyway.",
                    repo.id,
                    t.reason,
                    t.detail
                );
            }
        }
        commit = m.hf_commit.clone();
        signed = resolved.signed;
        if let Some(a) = &m.license.attribution {
            notice = Some(format!("{} {} at {commit}\n{a}\n", repo.repo_type, repo.id));
        }
        // Publisher webseeds plus configured and published mirrors, by
        // priority. HTTP source order follows `[fetch] source_order`.
        let mut seeds = m.clone();
        seeds.webseeds = crate::mirrors::webseed_bases(cfg, &m.webseeds);
        if use_swarm || o.need_torrent {
            if m.torrent.is_some() {
                torrent = Some(crate::torrents::fetch_for_manifest(m, &seeds.webseeds).await?);
            } else if o.need_torrent {
                bail!("--seed-after needs a Tensorseed manifest with a torrent");
            }
            // No torrent on the revision: skip the swarm silently and use HTTP.
        }
        for f in &m.files {
            let ef = ExpectedFile::from_manifest(f);
            if let Some(reason) = skip_reason(&ef, o, o.allow_pickle) {
                skipped.push(ef.path.clone());
                if reason == "unselected" {
                    unselected.push(ef.path.clone());
                }
            } else {
                let sources = http_sources(cfg, o, &seeds, &f.path, hf_source(&commit, &f.path))?;
                files.push(FileJob {
                    path: f.path.clone(),
                    size: f.size,
                    digest: ef.digest.clone(),
                    sources,
                });
            }
            expected.push(ef);
        }
        reject_empty_selection(&expected, o, &repo.id)?;
        manifest_raw = Some(resolved.raw);
    }
    ctl.check()?;

    let out = o
        .out
        .as_deref()
        .map(native_path)
        .unwrap_or_else(|| default_out(cfg, &repo));
    std::fs::create_dir_all(&out).with_context(|| format!("creating {}", out.display()))?;
    let _lock = lock_out(&out)?;
    let label = format!("{} {}@{}", repo.repo_type, repo.id, commit);
    let spec = JobSpec {
        id: job_id(&label, &out),
        label: label.clone(),
        out_dir: out.clone(),
    };
    // The same options, pinned to this commit and folder, resume the job.
    let pending = Pending {
        schema: PENDING_SCHEMA.into(),
        options: PullOptions {
            revision: Some(commit.clone()),
            out: Some(out.clone()),
            ..o.clone()
        },
    };
    write_meta(&out, PENDING_FILE, &serde_json::to_vec_pretty(&pending)?)?;
    let total: u64 = files.iter().map(|f| f.size).sum();
    ctl.emit(PullEvent::Started {
        job: spec.id.clone(),
        label: label.clone(),
        files: files.len(),
        bytes: total,
        out: out.clone(),
        source: source.into(),
    });
    for s in &unselected {
        ctl.emit(PullEvent::Skipped {
            path: s.clone(),
            pickle: false,
        });
    }
    for s in &skipped {
        if unselected.iter().any(|u| u == s) {
            continue;
        }
        ctl.emit(PullEvent::Skipped {
            path: s.clone(),
            pickle: true,
        });
    }
    // Files complete on disk before the swarm ran; anything else the HTTP
    // pass finds already present was written by the swarm (and is verified
    // by that pass like everything else).
    let mut before_swarm: Option<HashSet<String>> = None;
    if use_swarm && let Some(t) = &torrent {
        before_swarm = Some(
            files
                .iter()
                .filter(|f| {
                    std::fs::metadata(join_slash(&out, &f.path)).is_ok_and(|m| m.len() == f.size)
                })
                .map(|f| f.path.clone())
                .collect(),
        );
        // The engine creates every file of the torrent, even unselected
        // ones; remember which existed so its empty placeholders can go.
        let existed: HashSet<String> = expected
            .iter()
            .filter(|e| join_slash(&out, &e.path).exists())
            .map(|e| e.path.clone())
            .collect();
        // Record the job before the swarm runs, so a pull paused or
        // interrupted in the swarm stage shows in `tensorseed jobs` and
        // Desktop and can be resumed (the HTTP pass updates it after).
        // Keep the list of an earlier, stopped run: its preallocated files
        // already have full size, so they look complete here.
        let mut in_place: std::collections::BTreeSet<String> =
            std::fs::read(out.join(META_DIR).join(SWARM_FILE))
                .ok()
                .and_then(|b| serde_json::from_slice::<Vec<String>>(&b).ok())
                .unwrap_or_default()
                .into_iter()
                .collect();
        in_place.extend(
            files
                .iter()
                .filter(|f| !before_swarm.as_ref().is_some_and(|b| b.contains(&f.path)))
                .map(|f| f.path.clone()),
        );
        write_meta(&out, SWARM_FILE, &serde_json::to_vec(&in_place)?)?;
        let db = state_db(cfg)?;
        db.upsert_job(
            &spec.id,
            &spec.label,
            &spec.out_dir.display().to_string(),
            files.len() as u64,
        )?;
        let r = swarm_download(cfg, o, ctl, t, &out, &files).await;
        remove_swarm_placeholders(&out, &expected, &files, &existed);
        if let Some(pre) = &before_swarm {
            let wrote: Vec<String> = files
                .iter()
                .filter(|f| !pre.contains(&f.path) && join_slash(&out, &f.path).exists())
                .map(|f| f.path.clone())
                .collect();
            if !wrote.is_empty() {
                ctl.emit(PullEvent::SwarmWrote(wrote));
            }
        }
        if let Err(e) = r {
            if is_stopped(&e) {
                db.set_job_status(&spec.id, tensorseed_fetch::state::status::PAUSED, None)?;
                return Err(e);
            }
            if o.swarm_only {
                db.set_job_status(
                    &spec.id,
                    tensorseed_fetch::state::status::FAILED,
                    Some(&format!("{e:#}")),
                )?;
                return Err(e);
            }
            ctl.emit(PullEvent::Swarm(format!(
                "swarm  unavailable ({e}); using HTTP sources"
            )));
        }
    }
    ctl.check()?;
    let sink = ctl.sink.clone();
    let fetcher = Fetcher::new(Some(state_db(cfg)?))?
        .observe(Observer::new(move |e| sink(PullEvent::Fetch(e.clone()))))
        .stop_on(ctl.stop.clone());
    let report = fetcher
        .run(
            &spec,
            files,
            FetchOptions {
                concurrency: jobs,
                retry_dead_mirrors: o.retry_dead_mirrors,
                dead_mirror_cooldown: Duration::from_secs(cfg.fetch.dead_mirror_cooldown_secs),
                ..FetchOptions::default()
            },
        )
        .await
        .map_err(|e| match e {
            FetchError::Stopped => anyhow::Error::new(Stopped),
            e => e.into(),
        })?;
    for n in &report.skipped_dead {
        ctl.emit(PullEvent::DeadMirror(n.line.clone()));
    }
    let (mut swarm_files, mut swarm_bytes, mut http_bytes) = (0usize, 0u64, 0u64);
    for f in &report.files {
        let line = match &f.outcome {
            Outcome::Downloaded {
                kind, resumed_from, ..
            } => {
                http_bytes += f.size.saturating_sub(*resumed_from);
                FileLine::Downloaded {
                    path: f.path.clone(),
                    kind: *kind,
                    resumed_from: *resumed_from,
                }
            }
            Outcome::AlreadyPresent
                if before_swarm
                    .as_ref()
                    .is_some_and(|pre| !pre.contains(&f.path)) =>
            {
                swarm_files += 1;
                swarm_bytes += f.size;
                FileLine::Swarm {
                    path: f.path.clone(),
                }
            }
            Outcome::AlreadyPresent => FileLine::AlreadyPresent {
                path: f.path.clone(),
            },
            Outcome::Failed => FileLine::Failed {
                path: f.path.clone(),
                errors: f.errors.clone(),
                swarm_only: o.swarm_only,
            },
        };
        ctl.emit(PullEvent::File(line));
    }
    let (down, present, failed) = report.counts();
    if let Some(raw) = &manifest_raw {
        write_meta(&out, "manifest.json", raw)?;
    }
    if let Some(n) = &notice {
        write_meta(&out, "NOTICE", n.as_bytes())?;
    }
    let record = PullRecord {
        schema: PULL_SCHEMA.into(),
        repo_type: repo.repo_type,
        repo: repo.id.clone(),
        commit: commit.clone(),
        source: source.into(),
        signed,
        pulled_at: tensorseed_core::time::now_rfc3339(),
        files: expected.iter().map(PullFile::from_expected).collect(),
        skipped,
        unselected,
    };
    if let Some(t) = &torrent {
        write_meta(&out, crate::torrents::PULLED_TORRENT, &t.bytes)?;
    }
    write_meta(&out, "pull.json", &serde_json::to_vec_pretty(&record)?)?;
    if report.ok() {
        clear_pending(&out);
    }
    Ok(PullDone {
        out,
        label,
        repo,
        commit,
        source: source.into(),
        signed,
        ok: report.ok(),
        swarm_ran: before_swarm.is_some(),
        swarm_files,
        swarm_bytes,
        downloaded: down,
        http_bytes,
        present: present - swarm_files,
        failed,
        torrent,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_pending(dir.path()).unwrap().is_none());
        let o = PullOptions {
            repo: "org/name".into(),
            repo_type: RepoType::Dataset,
            revision: Some("a".repeat(40)),
            include: vec!["*.json".into()],
            ..PullOptions::default()
        };
        let p = Pending {
            schema: PENDING_SCHEMA.into(),
            options: o.clone(),
        };
        write_meta(dir.path(), PENDING_FILE, &serde_json::to_vec(&p).unwrap()).unwrap();
        assert_eq!(read_pending(dir.path()).unwrap(), Some(o));
        clear_pending(dir.path());
        assert!(read_pending(dir.path()).unwrap().is_none());
    }

    #[test]
    fn partial_option_files_fill_defaults() {
        let o: PullOptions =
            serde_json::from_str(r#"{"repo":"org/x","repo_type":"model"}"#).unwrap();
        assert_eq!(o.repo, "org/x");
        assert!(o.include.is_empty() && !o.swarm_only);
    }

    fn job(label: &str, out: &Path) -> tensorseed_fetch::state::JobRecord {
        tensorseed_fetch::state::JobRecord {
            id: "j".into(),
            label: label.into(),
            out_dir: out.display().to_string(),
            status: "paused".into(),
            files_total: 1,
            files_done: 0,
            error: None,
            updated_unix: 0,
        }
    }

    #[test]
    fn resume_prefers_the_pending_file() {
        let dir = tempfile::tempdir().unwrap();
        let c = "c".repeat(40);
        let old = resume_options(&job(&format!("dataset org/d@{c}"), dir.path())).unwrap();
        assert_eq!(old.repo, "org/d");
        assert_eq!(old.repo_type, RepoType::Dataset);
        assert_eq!(old.revision.as_deref(), Some(c.as_str()));
        assert_eq!(old.out.as_deref(), Some(dir.path()));
        assert!(resume_options(&job("nonsense", dir.path())).is_none());

        let pending = Pending {
            schema: PENDING_SCHEMA.into(),
            options: PullOptions {
                repo: "org/m".into(),
                revision: Some(c.clone()),
                include: vec!["a.safetensors".into()],
                ..PullOptions::default()
            },
        };
        write_meta(
            dir.path(),
            PENDING_FILE,
            &serde_json::to_vec(&pending).unwrap(),
        )
        .unwrap();
        let o = resume_options(&job("nonsense", dir.path())).unwrap();
        assert_eq!(o.repo, "org/m");
        assert_eq!(o.include, ["a.safetensors"]);
        assert_eq!(o.out.as_deref(), Some(dir.path()));
    }

    #[test]
    fn one_pull_per_folder() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!pull_running_in(dir.path()));
        let held = lock_out(dir.path()).unwrap();
        assert!(pull_running_in(dir.path()));
        let e = lock_out(dir.path()).unwrap_err();
        assert!(e.to_string().contains("another pull is writing"), "{e}");
        drop(held);
        assert!(!pull_running_in(dir.path()));
        lock_out(dir.path()).unwrap();
    }

    #[test]
    fn discard_removes_unverified_swarm_files() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out");
        let mut cfg = Config::default();
        cfg.general.state_db = dir.path().join("state.db").display().to_string();
        cfg.general.download_dir = dir.path().display().to_string();
        std::fs::create_dir_all(out.join("sub")).unwrap();
        for f in ["kept.bin", "sub/half.bin", "verified.bin"] {
            std::fs::write(join_slash(&out, f), b"x").unwrap();
        }
        // kept.bin was complete before the pull; the swarm wrote the others
        // in place and the HTTP pass verified only verified.bin.
        write_meta(
            &out,
            SWARM_FILE,
            &serde_json::to_vec(&["sub/half.bin", "verified.bin", "../escape"]).unwrap(),
        )
        .unwrap();
        let db = state_db(&cfg).unwrap();
        db.upsert_job("j", "model o/n@c", &out.display().to_string(), 2)
            .unwrap();
        db.upsert_file("j", "verified.bin", 1, "sha256:00").unwrap();
        db.file_done("j", "verified.bin", "https://x/", None)
            .unwrap();
        discard(&cfg, &out, "j").unwrap();
        assert!(out.join("kept.bin").exists());
        assert!(out.join("verified.bin").exists());
        assert!(
            !out.join("sub").exists(),
            "unverified file and its empty folder go"
        );
        assert!(!out.join(META_DIR).join(SWARM_FILE).exists());
        assert!(db.jobs().unwrap().is_empty());
        assert!(out.exists(), "finished files keep the folder");

        // Nothing finished: the folder and its empty parent go too.
        let gone = dir.path().join("org").join("name");
        std::fs::create_dir_all(&gone).unwrap();
        write_meta(&gone, SWARM_FILE, b"[\"a.bin\"]").unwrap();
        std::fs::write(gone.join("a.bin"), b"x").unwrap();
        discard(&cfg, &gone, "k").unwrap();
        assert!(!dir.path().join("org").exists());
    }

    #[test]
    fn stopped_errors_are_recognized() {
        let e = anyhow::Error::new(Stopped).context("pulling");
        assert!(is_stopped(&e));
        assert!(!is_stopped(&anyhow::anyhow!("other")));
    }
}
