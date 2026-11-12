// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Consumer commands: `pull`, `verify`, `list`, `show`, `jobs`.
//!
//! A pulled directory carries `.tensorseed/pull.json` (`ts.pull/1`) with the
//! expected file list, plus `.tensorseed/manifest.json` (the signed entry or
//! plain manifest it came from) when the source was a manifest. `verify <dir>`
//! re-hashes against `pull.json`, re-checking the manifest signature first
//! when one is present.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use tensorseed_catalog::signed::SignedEntry;
use tensorseed_core::hash::ExpectedDigest;
use tensorseed_core::keys::{RepoRef, RepoType, check_commit, index_prefix};
use tensorseed_core::manifest::{
    FileEntry, FilePage, IndexEntry, LatestPointer, Manifest, SCHEMA_FILEPAGE, SCHEMA_INDEX,
    SCHEMA_LATEST, SCHEMA_TOMBSTONE, Tombstone,
};
use tensorseed_core::verify::{ExpectedFile, META_DIR, VerifyOptions, verify_dir};
use tensorseed_fetch::state::StateDb;
use tensorseed_fetch::{
    FetchOptions, Fetcher, FileJob, JobSpec, Outcome, Source, SourceKind, job_id,
};
use tensorseed_hf::HfClient;
use tensorseed_hf::policy::Policy as HfPolicy;

use crate::backend::{open_reader, pinned_key, read_verified};
use crate::cli::{PullArgs, VerifyArgs};
use crate::config::{Config, expand};

/// Exit code for a failed verification or download.
pub const EXIT_VERIFY_FAILED: u8 = 3;

/// Schema of `.tensorseed/pull.json`.
pub const PULL_SCHEMA: &str = "ts.pull/1";

/// Record written next to pulled files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRecord {
    /// Always [`PULL_SCHEMA`].
    pub schema: String,
    /// Repo type.
    #[serde(rename = "type")]
    pub repo_type: RepoType,
    /// Repo id.
    pub repo: String,
    /// Commit SHA.
    pub commit: String,
    /// `catalog`, `manifest-file`, `manifest-url`, or `huggingface`.
    pub source: String,
    /// True if the expected list came from a signature verified manifest.
    pub signed: bool,
    /// When the pull finished (RFC 3339 UTC).
    pub pulled_at: String,
    /// Every file of the revision, including skipped ones.
    pub files: Vec<PullFile>,
    /// Files deliberately not downloaded (pickle formats).
    pub skipped: Vec<String>,
}

/// One expected file in a [`PullRecord`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullFile {
    /// Relative path.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// `sha256` or `git-sha1`.
    pub algorithm: String,
    /// Lowercase hex digest.
    pub digest: String,
    /// Pickle based format.
    pub pickle_risk: bool,
}

impl PullFile {
    fn from_expected(e: &ExpectedFile) -> Self {
        Self {
            path: e.path.clone(),
            size: e.size,
            algorithm: e.digest.algorithm().into(),
            digest: e.digest.hex().into(),
            pickle_risk: e.pickle_risk,
        }
    }

    fn to_expected(&self) -> anyhow::Result<ExpectedFile> {
        let digest = match self.algorithm.as_str() {
            "sha256" => ExpectedDigest::Sha256(self.digest.clone()),
            "git-sha1" => ExpectedDigest::GitBlobSha1(self.digest.clone()),
            other => bail!("unknown digest algorithm {other:?} for {}", self.path),
        }
        .normalized()?;
        Ok(ExpectedFile {
            path: self.path.clone(),
            size: self.size,
            digest,
            pickle_risk: self.pickle_risk,
        })
    }
}

/// Split `org/name[@rev]`; an explicit `--revision` wins.
pub fn parse_repo(
    spec: &str,
    repo_type: RepoType,
    revision: Option<&str>,
) -> anyhow::Result<(RepoRef, Option<String>)> {
    let (id, rev) = match spec.split_once('@') {
        Some((id, rev)) if !rev.is_empty() => (id, Some(rev.to_string())),
        Some((_, _)) => bail!("empty revision in {spec:?}"),
        None => (spec, None),
    };
    let repo = RepoRef::new(repo_type, id)?;
    Ok((repo, revision.map(str::to_owned).or(rev)))
}

/// Default output directory for a repo: `<download_dir>/<org>/<name>`
/// (datasets under `datasets/`).
pub fn default_out(cfg: &Config, repo: &RepoRef) -> PathBuf {
    let base = expand(&cfg.general.download_dir);
    match repo.repo_type {
        RepoType::Model => base.join(&repo.id),
        RepoType::Dataset => base.join("datasets").join(&repo.id),
    }
}

fn hf_client(cfg: &Config) -> anyhow::Result<HfClient> {
    Ok(HfClient::from_env(
        &cfg.huggingface.endpoint,
        &cfg.huggingface.token_env,
    )?)
}

/// A manifest with its full file list and provenance.
pub struct Resolved {
    /// The manifest (with `files` filled in from pages if it was paged).
    pub manifest: Manifest,
    /// Raw bytes to store as `.tensorseed/manifest.json`.
    pub raw: Vec<u8>,
    /// True if the signature was verified against the pinned key.
    pub signed: bool,
    /// Tombstone covering this revision, if any.
    pub tombstone: Option<Tombstone>,
}

fn tombstone_covers(t: &Tombstone, commit: &str) -> bool {
    t.revisions.is_empty() || t.revisions.iter().any(|r| r == commit)
}

/// Resolve a manifest through the configured catalog.
pub async fn catalog_manifest(
    cfg: &Config,
    repo: &RepoRef,
    revision: Option<&str>,
    no_wait: bool,
) -> anyhow::Result<Resolved> {
    let pk = pinned_key(cfg)?;
    let name = cfg.catalog.name.as_str();
    let (backend, id) = open_reader(cfg, None, no_wait).await?;
    let b = backend.as_dyn();

    let tombstone = match read_verified(b, &id, &repo.tombstone_key(), &pk, name).await? {
        Some(e) => {
            let t: Tombstone = serde_json::from_str(&e.value).context("parsing tombstone")?;
            if t.schema != SCHEMA_TOMBSTONE {
                bail!("tombstone has schema {:?}", t.schema);
            }
            Some(t)
        }
        None => None,
    };

    let commit = match revision {
        Some(r) => {
            check_commit(r).map_err(|_| {
                anyhow::anyhow!("catalog revisions are full 40 hex commit SHAs, got {r:?} (or use --from-hf for branches and tags)")
            })?;
            r.to_string()
        }
        None => {
            let Some(e) = read_verified(b, &id, &repo.latest_key(), &pk, name).await? else {
                bail!("{} {} is not in the catalog", repo.repo_type, repo.id);
            };
            let l: LatestPointer =
                serde_json::from_str(&e.value).context("parsing latest pointer")?;
            if l.schema != SCHEMA_LATEST {
                bail!("latest pointer has schema {:?}", l.schema);
            }
            check_commit(&l.commit)?;
            if l.manifest_key != repo.manifest_key(&l.commit)? {
                bail!(
                    "latest pointer names an unexpected key {:?}",
                    l.manifest_key
                );
            }
            l.commit
        }
    };
    let key = repo.manifest_key(&commit)?;
    let Some(entry) = read_verified(b, &id, &key, &pk, name).await? else {
        bail!(
            "{} {}@{commit} is not in the catalog",
            repo.repo_type,
            repo.id
        );
    };
    let mut manifest = Manifest::from_json(entry.value.as_bytes()).context("parsing manifest")?;
    check_manifest_identity(&manifest, repo, &commit)?;
    if manifest.file_pages > 0 {
        let mut files = Vec::new();
        for page in 0..manifest.file_pages {
            let k = repo.file_page_key(&commit, page)?;
            let Some(e) = read_verified(b, &id, &k, &pk, name).await? else {
                bail!("missing file page {k}");
            };
            let p: FilePage =
                serde_json::from_str(&e.value).with_context(|| format!("parsing {k}"))?;
            if p.schema != SCHEMA_FILEPAGE || p.page != page {
                bail!("file page {k} has the wrong schema or page number");
            }
            for f in &p.files {
                f.validate()?;
            }
            files.extend(p.files);
        }
        let bytes: u64 = files.iter().map(|f| f.size).sum();
        if manifest.totals.files != files.len() as u64 || manifest.totals.bytes != bytes {
            bail!("file pages do not match manifest totals");
        }
        manifest.files = files;
        manifest.file_pages = 0;
    }
    Ok(Resolved {
        manifest,
        raw: entry.to_bytes()?,
        signed: true,
        tombstone,
    })
}

fn check_manifest_identity(m: &Manifest, repo: &RepoRef, commit: &str) -> anyhow::Result<()> {
    if m.repo.id != repo.id || m.repo.repo_type != repo.repo_type {
        bail!(
            "manifest is for {} {}, expected {} {}",
            m.repo.repo_type,
            m.repo.id,
            repo.repo_type,
            repo.id
        );
    }
    if m.hf_commit != commit {
        bail!("manifest is for commit {}, expected {commit}", m.hf_commit);
    }
    Ok(())
}

/// Parse manifest bytes: a signed entry (verified against the pinned key, if
/// one is configured) or a plain manifest (accepted with a warning).
pub fn parse_manifest_bytes(cfg: &Config, bytes: &[u8], what: &str) -> anyhow::Result<Resolved> {
    if let Ok(entry) = SignedEntry::from_bytes(bytes) {
        let pk = pinned_key(cfg)
            .context("a signed manifest needs catalog.publisher_pubkey to verify")?;
        entry
            .verify(&pk, &cfg.catalog.name)
            .with_context(|| format!("verifying the signature on {what}"))?;
        let manifest = Manifest::from_json(entry.value.as_bytes())?;
        let repo = RepoRef::new(manifest.repo.repo_type, manifest.repo.id.clone())?;
        if entry.key != repo.manifest_key(&manifest.hf_commit)? {
            bail!(
                "{what}: signed key {:?} does not match the manifest it carries",
                entry.key
            );
        }
        if manifest.file_pages > 0 {
            bail!("{what}: paged manifests need the catalog (file pages are separate entries)");
        }
        return Ok(Resolved {
            manifest,
            raw: bytes.to_vec(),
            signed: true,
            tombstone: None,
        });
    }
    let manifest = Manifest::from_json(bytes)
        .with_context(|| format!("{what} is neither a signed entry nor a manifest"))?;
    if manifest.file_pages > 0 {
        bail!("{what}: paged manifests need the catalog");
    }
    tracing::warn!(
        "{what} is an unsigned manifest; hashes are only as trustworthy as where it came from"
    );
    Ok(Resolved {
        manifest,
        raw: bytes.to_vec(),
        signed: false,
        tombstone: None,
    })
}

async fn fetch_bytes(url: &str) -> anyhow::Result<Vec<u8>> {
    let resp = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?
        .get(url)
        .send()
        .await?;
    if !resp.status().is_success() {
        bail!("GET {url}: HTTP {}", resp.status());
    }
    let b = resp.bytes().await?;
    if b.len() > 64 << 20 {
        bail!("manifest at {url} is larger than 64 MiB");
    }
    Ok(b.to_vec())
}

fn write_meta(out: &Path, name: &str, data: &[u8]) -> anyhow::Result<()> {
    let meta = out.join(META_DIR);
    std::fs::create_dir_all(&meta)?;
    let tmp = meta.join(format!(".{name}.tmp"));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, meta.join(name))?;
    Ok(())
}

fn hf_policy(cfg: &Config, allow_gated: bool, allow_pickle: bool) -> HfPolicy {
    HfPolicy {
        license_allowlist: cfg.policy.license_allowlist.clone(),
        attribution_licenses: cfg.policy.attribution_licenses.clone(),
        allow_gated: cfg.policy.allow_gated || allow_gated,
        allow_pickle_formats: cfg.policy.allow_pickle_formats || allow_pickle,
    }
}

fn state_db(cfg: &Config) -> anyhow::Result<Arc<StateDb>> {
    let p = expand(&cfg.general.state_db);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    Ok(Arc::new(
        StateDb::open(&p).with_context(|| format!("opening {}", p.display()))?,
    ))
}

/// `tensorseed pull`.
pub async fn pull(cfg: &Config, args: &PullArgs) -> anyhow::Result<ExitCode> {
    let wants_torrent = args.swarm || args.seed_after.is_some();
    if wants_torrent && args.from_hf {
        bail!("--swarm and --seed-after need a Tensorseed manifest with a torrent, not --from-hf");
    }
    let seed_after = args
        .seed_after
        .as_deref()
        .map(crate::torrents::parse_duration)
        .transpose()?;
    let mut torrent: Option<tensorseed_torrent::Metainfo> = None;
    let (repo, revision) = parse_repo(&args.repo, args.repo_type.into(), args.revision.as_deref())?;
    let hf = hf_client(cfg)?;
    let jobs = args
        .jobs
        .unwrap_or(cfg.general.max_concurrent_downloads as usize)
        .max(1);

    let mut expected: Vec<ExpectedFile> = Vec::new();
    let mut files: Vec<FileJob> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
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

    if args.from_hf {
        source = "huggingface";
        let info = hf.repo_info(&repo, revision.as_deref()).await?;
        if info.disabled {
            bail!("{} is disabled on the Hub", repo.id);
        }
        let tree = hf.files(&repo, &info.sha).await?;
        let policy = hf_policy(cfg, args.allow_gated, args.allow_pickle);
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
            if ef.pickle_risk && !policy.allow_pickle_formats {
                skipped.push(ef.path.clone());
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
    } else {
        let resolved = if let Some(p) = &args.manifest_file {
            source = "manifest-file";
            parse_manifest_bytes(
                cfg,
                &std::fs::read(p).with_context(|| format!("reading {}", p.display()))?,
                &p.display().to_string(),
            )?
        } else if let Some(u) = &args.manifest_url {
            source = "manifest-url";
            parse_manifest_bytes(cfg, &fetch_bytes(u).await?, u)?
        } else {
            source = "catalog";
            catalog_manifest(cfg, &repo, revision.as_deref(), args.no_wait).await?
        };
        let m = &resolved.manifest;
        if args.manifest_file.is_some() || args.manifest_url.is_some() {
            check_manifest_identity(m, &repo, revision.as_deref().unwrap_or(&m.hf_commit))?;
        }
        if let Some(t) = &resolved.tombstone
            && tombstone_covers(t, &m.hf_commit)
        {
            if args.ignore_tombstone {
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
        // priority; the Hub stays the last resort.
        let mut seeds = m.clone();
        seeds.webseeds = crate::mirrors::webseed_bases(cfg, &m.webseeds);
        if wants_torrent {
            torrent = Some(crate::torrents::fetch_for_manifest(m, &seeds.webseeds).await?);
        }
        for f in &m.files {
            let ef = ExpectedFile::from_manifest(f);
            if ef.pickle_risk && !args.allow_pickle {
                skipped.push(ef.path.clone());
            } else {
                let mut sources: Vec<Source> = seeds
                    .webseed_file_urls(&f.path)
                    .into_iter()
                    .map(|url| Source {
                        url,
                        kind: SourceKind::Webseed,
                        bearer: None,
                    })
                    .collect();
                if !args.no_hf_fallback {
                    sources.push(hf_source(&commit, &f.path));
                }
                if sources.is_empty() {
                    bail!("{} has no webseeds and --no-hf-fallback was given", f.path);
                }
                files.push(FileJob {
                    path: f.path.clone(),
                    size: f.size,
                    digest: ef.digest.clone(),
                    sources,
                });
            }
            expected.push(ef);
        }
        manifest_raw = Some(resolved.raw);
    }

    let out = args.out.clone().unwrap_or_else(|| default_out(cfg, &repo));
    std::fs::create_dir_all(&out).with_context(|| format!("creating {}", out.display()))?;
    let label = format!("{} {}@{}", repo.repo_type, repo.id, commit);
    let spec = JobSpec {
        id: job_id(&label, &out),
        label: label.clone(),
        out_dir: out.clone(),
    };
    let total: u64 = files.iter().map(|f| f.size).sum();
    println!(
        "pulling {label} ({} files, {} bytes) into {} from {source}",
        files.len(),
        total,
        out.display()
    );
    for s in &skipped {
        println!("skip   {s} (pickle format; --allow-pickle to include)");
    }
    if args.swarm
        && let Some(t) = &torrent
    {
        swarm_download(cfg, args, t, &out, &files).await?;
    }
    let fetcher = Fetcher::new(Some(state_db(cfg)?))?;
    let report = fetcher
        .run(
            &spec,
            files,
            FetchOptions {
                concurrency: jobs,
                ..FetchOptions::default()
            },
        )
        .await?;
    for f in &report.files {
        match &f.outcome {
            Outcome::Downloaded {
                kind, resumed_from, ..
            } => {
                let resumed = if *resumed_from > 0 {
                    format!(", resumed at {resumed_from}")
                } else {
                    String::new()
                };
                println!("ok     {} ({}{resumed})", f.path, kind.label());
            }
            Outcome::AlreadyPresent => println!("ok     {} (already present)", f.path),
            Outcome::Failed => {
                println!("FAILED {}", f.path);
                for e in &f.errors {
                    println!("         {e}");
                }
            }
        }
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
    };
    if let Some(t) = &torrent {
        write_meta(&out, crate::torrents::PULLED_TORRENT, &t.bytes)?;
    }
    write_meta(&out, "pull.json", &serde_json::to_vec_pretty(&record)?)?;
    println!("{down} downloaded, {present} already present, {failed} failed");
    if !report.ok() {
        eprintln!("pull incomplete; run the same command again to resume");
        return Ok(ExitCode::from(EXIT_VERIFY_FAILED));
    }
    if !signed {
        println!("note: hashes came from an unsigned source ({source}), not the signed catalog");
    }
    if let (Some(dur), Some(t)) = (seed_after, torrent) {
        let item = crate::torrents::pulled_dir(&out)?;
        debug_assert_eq!(item.torrent.info_hash_v2, t.info_hash_v2);
        return crate::torrents::run_seeder(cfg, vec![item], dur).await;
    }
    Ok(ExitCode::SUCCESS)
}

/// Swarm first: let librqbit fetch what peers have, in place. Nothing it
/// writes is trusted: the HTTP fetcher that runs next hashes every file
/// against the manifest, keeps the ones that match, and downloads the rest.
async fn swarm_download(
    cfg: &Config,
    args: &PullArgs,
    t: &tensorseed_torrent::Metainfo,
    out: &Path,
    files: &[FileJob],
) -> anyhow::Result<()> {
    use tensorseed_torrent::engine::{DownloadOptions, Engine};
    let mut peers = cfg.torrent.peer_addrs()?;
    for p in &args.peer {
        use std::net::ToSocketAddrs;
        peers.extend(
            p.to_socket_addrs()
                .with_context(|| format!("--peer {p:?}"))?,
        );
    }
    let timeout = Duration::from_secs(cfg.torrent.swarm_timeout_secs.max(1));
    let engine = Engine::start(crate::torrents::engine_options(cfg, false, out)?)
        .await
        .context("starting the BitTorrent engine")?;
    let t0 = std::time::Instant::now();
    let r = engine
        .download(
            t,
            out,
            DownloadOptions {
                peers,
                only: Some(files.iter().map(|f| f.path.clone()).collect()),
            },
            timeout,
        )
        .await;
    match r {
        Ok((h, p)) => {
            println!(
                "swarm  {} of {} bytes in {:.1}s{}",
                p.have,
                p.total,
                t0.elapsed().as_secs_f64(),
                if p.finished {
                    ""
                } else {
                    " (HTTP sources fill the rest)"
                }
            );
            let _ = engine.remove(&h).await;
        }
        Err(e) => println!("swarm  unavailable ({e}); using HTTP sources"),
    }
    engine.stop().await;
    Ok(())
}

fn read_pull_record(dir: &Path) -> anyhow::Result<Option<PullRecord>> {
    let p = dir.join(META_DIR).join("pull.json");
    if !p.exists() {
        return Ok(None);
    }
    let r: PullRecord = serde_json::from_slice(&std::fs::read(&p)?)
        .with_context(|| format!("parsing {}", p.display()))?;
    if r.schema != PULL_SCHEMA {
        bail!("{} has schema {:?}", p.display(), r.schema);
    }
    Ok(Some(r))
}

async fn hf_expected(
    cfg: &Config,
    repo: &RepoRef,
    revision: Option<&str>,
) -> anyhow::Result<(String, Vec<ExpectedFile>)> {
    let hf = hf_client(cfg)?;
    let info = hf.repo_info(repo, revision).await?;
    let tree = hf.files(repo, &info.sha).await?;
    let mut out = Vec::with_capacity(tree.len());
    for e in &tree {
        out.push(ExpectedFile {
            path: e.path.clone(),
            size: e.content_size(),
            digest: e.expected_digest()?,
            pickle_risk: FileEntry::path_has_pickle_risk(&e.path),
        });
    }
    Ok((info.sha, out))
}

/// `tensorseed verify`.
pub async fn verify(cfg: &Config, args: &VerifyArgs) -> anyhow::Result<ExitCode> {
    let target_is_dir = Path::new(&args.target).is_dir();
    let (dir, repo_spec): (PathBuf, Option<String>) = match (&args.repo, target_is_dir) {
        (Some(r), _) => (
            args.dir
                .clone()
                .unwrap_or_else(|| PathBuf::from(&args.target)),
            Some(r.clone()),
        ),
        (None, true) => (PathBuf::from(&args.target), None),
        (None, false) => (PathBuf::new(), Some(args.target.clone())),
    };
    let repo = match &repo_spec {
        Some(s) => Some(parse_repo(
            s,
            args.repo_type.into(),
            args.revision.as_deref(),
        )?),
        None => None,
    };
    let dir = match (&repo, dir.as_os_str().is_empty()) {
        (Some((r, _)), true) => args.dir.clone().unwrap_or_else(|| default_out(cfg, r)),
        _ => dir,
    };
    if !dir.is_dir() {
        bail!("{} is not a directory", dir.display());
    }
    let record = read_pull_record(&dir)?;
    // The repo and revision fall back to what pull.json recorded.
    let recorded = || -> anyhow::Result<(RepoRef, Option<String>)> {
        let r = record.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "{} has no {META_DIR}/pull.json; name the repo",
                dir.display()
            )
        })?;
        Ok((
            RepoRef::new(r.repo_type, r.repo.clone())?,
            Some(r.commit.clone()),
        ))
    };

    let (label, expected, signed): (String, Vec<ExpectedFile>, bool) = if args.hf {
        let (r, rev) = match &repo {
            Some((r, rev)) => (r.clone(), rev.clone()),
            None => recorded()?,
        };
        let (commit, exp) = hf_expected(cfg, &r, rev.as_deref()).await?;
        (
            format!("{} {}@{commit} (Hugging Face hashes)", r.repo_type, r.id),
            exp,
            false,
        )
    } else if let Some(p) = &args.manifest_file {
        let res = parse_manifest_bytes(cfg, &std::fs::read(p)?, &p.display().to_string())?;
        let m = &res.manifest;
        (
            format!(
                "{} {}@{} ({})",
                m.repo.repo_type,
                m.repo.id,
                m.hf_commit,
                p.display()
            ),
            m.files.iter().map(ExpectedFile::from_manifest).collect(),
            res.signed,
        )
    } else if let Some((r, rev)) = &repo {
        let res = catalog_manifest(cfg, r, rev.as_deref(), args.no_wait).await?;
        if let Some(t) = &res.tombstone
            && tombstone_covers(t, &res.manifest.hf_commit)
        {
            tracing::warn!("{} is taken down ({:?}: {})", r.id, t.reason, t.detail);
        }
        let m = &res.manifest;
        (
            format!("{} {}@{} (signed catalog)", r.repo_type, r.id, m.hf_commit),
            m.files.iter().map(ExpectedFile::from_manifest).collect(),
            true,
        )
    } else {
        let rec = record
            .as_ref()
            .expect("dir mode has a record or failed above");
        let mut signed = false;
        let mp = dir.join(META_DIR).join("manifest.json");
        if mp.exists() {
            let res = parse_manifest_bytes(cfg, &std::fs::read(&mp)?, &mp.display().to_string())?;
            signed = res.signed;
            let from_manifest: Vec<ExpectedFile> = res
                .manifest
                .files
                .iter()
                .map(ExpectedFile::from_manifest)
                .collect();
            let from_record = rec
                .files
                .iter()
                .map(PullFile::to_expected)
                .collect::<anyhow::Result<Vec<_>>>()?;
            if !same_files(&from_manifest, &from_record) {
                bail!(
                    "{} does not match the file list in {}",
                    mp.display(),
                    "pull.json"
                );
            }
        }
        (
            format!(
                "{} {}@{} (recorded at pull, source {})",
                rec.repo_type, rec.repo, rec.commit, rec.source
            ),
            rec.files
                .iter()
                .map(PullFile::to_expected)
                .collect::<anyhow::Result<Vec<_>>>()?,
            signed,
        )
    };

    println!("verifying {} against {label}", dir.display());
    let report = verify_dir(
        &dir,
        &expected,
        VerifyOptions {
            allow_missing_pickle: !args.strict,
            threads: args.threads,
        },
    )?;
    for f in &report.files {
        use tensorseed_core::verify::FileStatus as S;
        let detail = match &f.status {
            S::SizeMismatch { expected, actual } => {
                format!(" (expected {expected} bytes, found {actual})")
            }
            S::HashMismatch { expected, actual } => format!(" (expected {expected}, got {actual})"),
            S::Error(e) => format!(" ({e})"),
            _ => String::new(),
        };
        let tag = if f.status.is_pass() { "PASS" } else { "FAIL" };
        println!("{tag}  {}  {}{detail}", f.status.label(), f.path);
    }
    for e in &report.extra {
        println!("note  extra file not in the manifest: {e}");
    }
    let failures = report.failures();
    println!(
        "{} files checked, {} passed, {failures} failed{}",
        report.files.len(),
        report.files.len() - failures,
        if signed {
            ""
        } else {
            " (expected hashes were not signed by the catalog publisher)"
        }
    );
    Ok(if report.passed() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(EXIT_VERIFY_FAILED)
    })
}

fn same_files(a: &[ExpectedFile], b: &[ExpectedFile]) -> bool {
    let mut a: Vec<_> = a
        .iter()
        .map(|e| (&e.path, e.size, e.digest.to_string()))
        .collect();
    let mut b: Vec<_> = b
        .iter()
        .map(|e| (&e.path, e.size, e.digest.to_string()))
        .collect();
    a.sort();
    b.sort();
    a == b
}

/// `tensorseed list`.
pub async fn list(cfg: &Config, filter: Option<&str>, no_wait: bool) -> anyhow::Result<ExitCode> {
    let pk = pinned_key(cfg)?;
    let (backend, id) = open_reader(cfg, None, no_wait).await?;
    let b = backend.as_dyn();
    let prefix = index_prefix();
    let mut n = 0;
    for key in b.keys(&id, &prefix).await? {
        let rest = &key[prefix.len()..];
        if filter.is_some_and(|f| !rest.contains(f)) {
            continue;
        }
        let Some(e) = read_verified(b, &id, &key, &pk, &cfg.catalog.name).await? else {
            continue;
        };
        let ix: IndexEntry =
            serde_json::from_str(&e.value).with_context(|| format!("parsing {key}"))?;
        if ix.schema != SCHEMA_INDEX {
            bail!("{key} has schema {:?}", ix.schema);
        }
        n += 1;
        println!(
            "{rest}  {}  {} bytes  {}{}",
            ix.latest_commit.get(..12).unwrap_or(&ix.latest_commit),
            ix.total_bytes,
            ix.license_spdx.as_deref().unwrap_or("license unknown"),
            if ix.taken_down { "  [taken down]" } else { "" }
        );
    }
    eprintln!("{n} entries");
    Ok(ExitCode::SUCCESS)
}

/// `tensorseed show`.
pub async fn show(
    cfg: &Config,
    repo: &str,
    repo_type: RepoType,
    revision: Option<&str>,
    no_wait: bool,
) -> anyhow::Result<ExitCode> {
    let (r, rev) = parse_repo(repo, repo_type, revision)?;
    let res = catalog_manifest(cfg, &r, rev.as_deref(), no_wait).await?;
    println!("{}", serde_json::to_string_pretty(&res.manifest)?);
    if let Some(t) = &res.tombstone
        && tombstone_covers(t, &res.manifest.hf_commit)
    {
        eprintln!("taken down: {:?}: {}", t.reason, t.detail);
    }
    Ok(ExitCode::SUCCESS)
}

/// `tensorseed jobs`.
pub fn jobs(cfg: &Config, show: Option<&str>, clear: Option<&str>) -> anyhow::Result<ExitCode> {
    let db = state_db(cfg)?;
    if let Some(id) = clear {
        if db.clear_job(id)? {
            println!("cleared job {id} (downloaded files are kept)");
            return Ok(ExitCode::SUCCESS);
        }
        bail!("no job {id}");
    }
    if let Some(id) = show {
        for f in db.job_files(id)? {
            println!(
                "{:<8} {}/{} bytes  {}{}",
                f.status,
                f.bytes_done,
                f.size,
                f.path,
                f.error.map(|e| format!("  ({e})")).unwrap_or_default()
            );
        }
        return Ok(ExitCode::SUCCESS);
    }
    for j in db.jobs()? {
        println!(
            "{}  {:<8} {}/{} files  {}  {}{}",
            j.id,
            j.status,
            j.files_done,
            j.files_total,
            j.label,
            j.out_dir,
            j.error.map(|e| format!("  ({e})")).unwrap_or_default()
        );
    }
    Ok(ExitCode::SUCCESS)
}
