// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! `tensorseed harvest`: mirror repos from the Hugging Face Hub while it is
//! available (spec section 13).
//!
//! One pass: build the candidate list (explicit repos plus discovery by
//! author and tags), resolve each repo's current commit, skip revisions
//! already harvested, apply the takedown list, catalog tombstones, gated
//! check, license allowlist, size cap, and disk budget, then download with
//! hash verification and resume, write a signed manifest, commit catalog
//! entries, and optionally upload to S3 compatible storage. `--watch`
//! repeats passes. Seeding is not started here (pending the M0 spike).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use tensorseed_catalog::signed::SignedEntry;
use tensorseed_catalog::sink::{CachePolicy, ObjectSink};
use tensorseed_catalog::{CatalogId, CatalogOp};
use tensorseed_core::hash::{ExpectedDigest, sha256_file};
use tensorseed_core::keys::{RepoRef, RepoType};
use tensorseed_core::manifest::{
    FileEntry, FilePage, IndexEntry, LatestPointer, LicenseInfo, Manifest, RepoInfo as MRepoInfo,
    SCHEMA_FILEPAGE, SCHEMA_INDEX, SCHEMA_LATEST, SCHEMA_MANIFEST, TorrentInfo, Totals,
};
use tensorseed_core::time::now_rfc3339;
use tensorseed_core::verify::META_DIR;
use tensorseed_fetch::state::StateDb;
use tensorseed_fetch::{FetchOptions, Fetcher, FileJob, JobSpec, Source, SourceKind, job_id};
use tensorseed_hf::policy::Policy as HfPolicy;
use tensorseed_hf::{HfClient, HfError, TreeEntry};

use crate::backend::{Backend, chia, file_catalog, remote_sink};
use crate::cli::HarvestArgs;
use crate::config::{Config, expand};
use crate::keys::load_secret;

/// Manifests with more files than this are split into file pages.
pub const PAGE_THRESHOLD: usize = 2000;
/// Files per page.
pub const PAGE_SIZE: usize = 1000;

/// Harvest status values in the state DB.
const DONE: &str = "done";
const FAILED: &str = "failed";

/// A repo to consider.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    repo_type: RepoType,
    id: String,
    explicit: bool,
    /// Per-repo license overrides from the watchlist entry.
    allow_license: Vec<String>,
    /// Pinned revision (`publish --revision`); `None` means the Hub's main.
    revision: Option<String>,
}

/// Outcome counts of one pass.
#[derive(Debug, Default)]
struct PassReport {
    harvested: usize,
    up_to_date: usize,
    skipped: usize,
    failed: usize,
}

fn repo_type_order(t: RepoType) -> u8 {
    match t {
        RepoType::Model => 0,
        RepoType::Dataset => 1,
    }
}

fn load_takedown(cfg: &Config) -> anyhow::Result<BTreeSet<String>> {
    let mut set: BTreeSet<String> = cfg.harvest.takedown.iter().cloned().collect();
    if !cfg.harvest.takedown_file.is_empty() {
        let p = expand(&cfg.harvest.takedown_file);
        let text = std::fs::read_to_string(&p)
            .with_context(|| format!("reading takedown file {}", p.display()))?;
        for line in text.lines() {
            let l = line.split('#').next().unwrap_or("").trim();
            if !l.is_empty() {
                set.insert(l.to_string());
            }
        }
    }
    Ok(set)
}

/// The catalog the harvester publishes to.
struct Publisher {
    backend: Backend,
    id: CatalogId,
    /// Chia first writes for the file catalog (spec 6.14).
    chain: Option<crate::chain::ChainWriter>,
}

impl Publisher {
    async fn open(cfg: &Config) -> anyhow::Result<Self> {
        let sk = load_secret(cfg)?;
        match cfg.catalog.backend.as_str() {
            "file" => {
                let chain = crate::chain::ChainWriter::open(cfg, &sk)?;
                let cat = file_catalog(cfg, Some(sk))?;
                cat.sync_for_publish()
                    .await
                    .context("syncing the catalog before publishing")?;
                Ok(Self {
                    backend: Backend::File(cat),
                    id: CatalogId(cfg.catalog.name.clone()),
                    chain,
                })
            }
            "chia" => {
                if cfg.chia.catalog_store_id.is_empty() {
                    bail!("chia.catalog_store_id is not set");
                }
                let c = chia(cfg)?.with_signer(sk)?;
                Ok(Self {
                    backend: Backend::Chia(c),
                    id: CatalogId(cfg.chia.catalog_store_id.clone()),
                    chain: None,
                })
            }
            other => bail!("unknown backend {other:?}"),
        }
    }

    async fn tombstoned(&self, repo: &RepoRef, commit: &str) -> anyhow::Result<bool> {
        let Some(e) = self
            .backend
            .as_dyn()
            .get_signed(&self.id, &repo.tombstone_key())
            .await?
        else {
            return Ok(false);
        };
        let t: tensorseed_core::manifest::Tombstone = serde_json::from_str(&e.value)?;
        Ok(t.revisions.is_empty() || t.revisions.iter().any(|r| r == commit))
    }

    async fn signed(&self, key: &str) -> anyhow::Result<SignedEntry> {
        self.backend
            .as_dyn()
            .get_signed(&self.id, key)
            .await?
            .with_context(|| format!("{key} missing right after commit"))
    }

    async fn commit(&self, ops: Vec<CatalogOp>) -> anyhow::Result<u64> {
        let b = self.backend.as_dyn();
        b.stage(&self.id, &ops).await?;
        if let Backend::File(cat) = &self.backend {
            return Ok(
                crate::chain::commit_file(cat, &self.id, self.chain.as_ref())
                    .await?
                    .number,
            );
        }
        let p = b.commit(&self.id).await?;
        Ok(b.wait_confirmed(&p).await?.number)
    }
}

async fn polite<T, F, Fut>(delay: Duration, f: F) -> Result<T, HfError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<T, HfError>>,
{
    let r = f().await;
    tokio::time::sleep(delay).await;
    r
}

async fn candidates(
    cfg: &Config,
    hf: &HfClient,
    only: &[String],
    delay: Duration,
) -> anyhow::Result<Vec<Candidate>> {
    let h = &cfg.harvest;
    let mut out: Vec<Candidate> = Vec::new();
    let push = |out: &mut Vec<Candidate>, repo_type, id: &str, explicit, allow: &[String]| {
        if !out.iter().any(|c| c.repo_type == repo_type && c.id == id) {
            out.push(Candidate {
                repo_type,
                id: id.to_string(),
                explicit,
                allow_license: allow.to_vec(),
                revision: None,
            });
        }
    };
    for r in &h.repos {
        push(&mut out, RepoType::Model, r.id(), true, r.allow_license());
    }
    if h.include_datasets {
        for r in &h.datasets {
            push(&mut out, RepoType::Dataset, r.id(), true, r.allow_license());
        }
    } else if !h.datasets.is_empty() {
        tracing::warn!("harvest.datasets is set but include_datasets = false; skipping datasets");
    }
    let mut types = vec![RepoType::Model];
    if h.include_datasets {
        types.push(RepoType::Dataset);
    }
    for author in &h.authors {
        for t in &types {
            let list = polite(delay, || {
                hf.list_repos(*t, author, &h.tags, h.max_per_author.max(1))
            })
            .await
            .with_context(|| format!("listing {t} repos of {author}"))?;
            for r in list {
                if r.downloads < h.min_downloads || r.private {
                    continue;
                }
                if !h.tags.iter().all(|tag| r.tags.iter().any(|x| x == tag)) {
                    continue;
                }
                push(&mut out, *t, &r.id, false, &[]);
            }
        }
    }
    if !only.is_empty() {
        out.retain(|c| only.iter().any(|o| o == &c.id));
    }
    out.sort_by(|a, b| {
        repo_type_order(a.repo_type)
            .cmp(&repo_type_order(b.repo_type))
            .then(a.id.cmp(&b.id))
    });
    Ok(out)
}

/// `tensorseed harvest [--watch]`.
pub async fn run(cfg: &Config, args: &HarvestArgs) -> anyhow::Result<ExitCode> {
    let h = &cfg.harvest;
    if args.status {
        let p = expand(&cfg.general.state_db);
        let db = StateDb::open(&p)?;
        for r in db.harvest_list()? {
            println!(
                "{:<7} {} {}@{}  {} bytes  {} attempt(s){}",
                r.status,
                r.repo_type,
                r.repo,
                &r.commit[..r.commit.len().min(12)],
                r.bytes,
                r.attempts,
                r.detail.map(|d| format!("  ({d})")).unwrap_or_default()
            );
        }
        println!("{} bytes harvested in total", db.harvest_bytes_done()?);
        return Ok(ExitCode::SUCCESS);
    }
    let hf = cfg
        .huggingface
        .client()?
        .with_retry(h.max_retries, Duration::from_millis(h.retry_base_ms));
    let db = open_db(cfg)?;
    let mut cycle = 0u64;
    let mut any_failed = false;
    loop {
        cycle += 1;
        let report = pass(cfg, args, &hf, &db, None).await?;
        any_failed |= report.failed > 0;
        println!(
            "harvest pass {cycle}: {} {}, {} up to date, {} skipped, {} failed",
            report.harvested,
            if args.dry_run { "planned" } else { "harvested" },
            report.up_to_date,
            report.skipped,
            report.failed
        );
        if !args.watch || (args.max_cycles > 0 && cycle >= args.max_cycles) {
            break;
        }
        tokio::time::sleep(Duration::from_secs(h.poll_interval_secs.max(1))).await;
    }
    Ok(if any_failed && !args.watch {
        ExitCode::from(crate::consumer::EXIT_VERIFY_FAILED)
    } else {
        ExitCode::SUCCESS
    })
}

fn open_db(cfg: &Config) -> anyhow::Result<Arc<StateDb>> {
    let p = expand(&cfg.general.state_db);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    Ok(Arc::new(
        StateDb::open(&p).with_context(|| format!("opening {}", p.display()))?,
    ))
}

/// `tensorseed publish <repo>`: run the harvest pipeline (policy checks,
/// verified download, torrent, signed catalog entry, uploads) for exactly
/// one repo, whether or not it is on the watchlist. Always publishes.
pub async fn publish(
    cfg: &Config,
    repo_id: &str,
    repo_type: RepoType,
    revision: Option<&str>,
    allow_license: &[String],
) -> anyhow::Result<ExitCode> {
    let h = &cfg.harvest;
    let hf = cfg
        .huggingface
        .client()?
        .with_retry(h.max_retries, Duration::from_millis(h.retry_base_ms));
    let db = open_db(cfg)?;
    // A watchlist entry for the same repo contributes its overrides too.
    let listed = if repo_type == RepoType::Model {
        &h.repos
    } else {
        &h.datasets
    };
    let mut allow: Vec<String> = allow_license.to_vec();
    for w in listed.iter().filter(|w| w.id() == repo_id) {
        allow.extend(w.allow_license().iter().cloned());
    }
    let cand = Candidate {
        repo_type,
        id: repo_id.to_string(),
        explicit: true,
        allow_license: allow,
        revision: revision.map(str::to_owned),
    };
    let args = HarvestArgs {
        watch: false,
        only: Vec::new(),
        dry_run: false,
        status: false,
        max_cycles: 0,
        allow_license: Vec::new(),
    };
    let report = pass(cfg, &args, &hf, &db, Some(vec![cand])).await?;
    println!(
        "publish: {} published, {} already up to date, {} skipped, {} failed",
        report.harvested, report.up_to_date, report.skipped, report.failed
    );
    Ok(if report.failed > 0 || report.skipped > 0 {
        ExitCode::from(crate::consumer::EXIT_VERIFY_FAILED)
    } else {
        ExitCode::SUCCESS
    })
}

async fn pass(
    cfg: &Config,
    args: &HarvestArgs,
    hf: &HfClient,
    db: &Arc<StateDb>,
    fixed: Option<Vec<Candidate>>,
) -> anyhow::Result<PassReport> {
    let h = &cfg.harvest;
    let delay = Duration::from_millis(h.request_delay_ms);
    let takedown = load_takedown(cfg)?;
    let force_publish = fixed.is_some();
    let publisher = if (h.publish || force_publish) && !args.dry_run {
        Some(Publisher::open(cfg).await?)
    } else {
        None
    };
    let upload_sink: Option<Arc<dyn ObjectSink>> = if h.upload && !args.dry_run {
        remote_sink(cfg)?
    } else {
        None
    };
    let mut policy = HfPolicy {
        license_allowlist: cfg.policy.license_allowlist.clone(),
        attribution_licenses: cfg.policy.attribution_licenses.clone(),
        allow_gated: h.allow_gated,
        allow_pickle_formats: cfg.policy.allow_pickle_formats,
        license_overrides: Vec::new(),
    };
    if !h.license_allowlist.is_empty() {
        policy.license_allowlist = h.license_allowlist.clone();
    }
    let store_root = expand(&cfg.storage.local_dir);
    let mut report = PassReport::default();

    let cands = match fixed {
        Some(v) => v,
        None => candidates(cfg, hf, &args.only, delay).await?,
    };
    for c in cands {
        let label = format!("{} {}", c.repo_type, c.id);
        policy.license_overrides = args
            .allow_license
            .iter()
            .chain(c.allow_license.iter())
            .cloned()
            .collect();
        if takedown.contains(&c.id) {
            println!("skip     {label}: on the takedown list");
            report.skipped += 1;
            continue;
        }
        let repo = match RepoRef::new(c.repo_type, c.id.clone()) {
            Ok(r) => r,
            Err(e) => {
                println!("skip     {label}: {e}");
                report.skipped += 1;
                continue;
            }
        };
        let info = match polite(delay, || hf.repo_info(&repo, c.revision.as_deref())).await {
            Ok(i) => i,
            Err(e) => {
                println!("FAILED   {label}: {e}");
                report.failed += 1;
                continue;
            }
        };
        let commit = info.sha.clone();
        let rlabel = format!("{label}@{}", &commit[..12]);
        if info.gated.is_gated() && !h.allow_gated {
            println!("skip     {rlabel}: gated (set harvest.allow_gated to mirror gated repos)");
            report.skipped += 1;
            continue;
        }
        if info.disabled || info.private {
            println!("skip     {rlabel}: disabled or private");
            report.skipped += 1;
            continue;
        }
        if let Some(r) = db.harvest_get(c.repo_type.as_str(), &c.id, &commit)?
            && r.status == DONE
        {
            report.up_to_date += 1;
            tracing::debug!("{rlabel} already harvested");
            continue;
        }
        if let Some(p) = &publisher
            && p.tombstoned(&repo, &commit).await?
        {
            println!("skip     {rlabel}: tombstoned in the catalog");
            report.skipped += 1;
            continue;
        }
        let tree = match polite(delay, || hf.files(&repo, &commit)).await {
            Ok(t) => t,
            Err(e) => {
                println!("FAILED   {rlabel}: {e}");
                report.failed += 1;
                continue;
            }
        };
        let pr = policy.evaluate(&info, &tree);
        let refusals: Vec<&String> = pr.refusals.iter().collect();
        if !refusals.is_empty() {
            println!(
                "skip     {rlabel}: {}",
                refusals
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            );
            report.skipped += 1;
            continue;
        }
        if pr.license_overridden {
            println!(
                "note     {rlabel}: license {} allowed by explicit override (recorded as {:?})",
                pr.license.as_deref().unwrap_or("(none)"),
                pr.class
            );
        }
        let kept: Vec<&TreeEntry> = tree
            .iter()
            .filter(|e| !pr.excluded_pickle.contains(&e.path))
            .collect();
        let bytes: u64 = kept.iter().map(|e| e.content_size()).sum();
        if h.max_repo_bytes > 0 && bytes > h.max_repo_bytes {
            println!(
                "skip     {rlabel}: {bytes} bytes is over max_repo_bytes ({})",
                h.max_repo_bytes
            );
            report.skipped += 1;
            continue;
        }
        let used = db.harvest_bytes_done()?;
        if h.disk_budget_bytes > 0 && used + bytes > h.disk_budget_bytes {
            println!(
                "skip     {rlabel}: {bytes} bytes would exceed the disk budget ({used} of {} used)",
                h.disk_budget_bytes
            );
            report.skipped += 1;
            continue;
        }
        if args.dry_run {
            println!(
                "plan     {rlabel}: {} files, {bytes} bytes{}",
                kept.len(),
                if pr.excluded_pickle.is_empty() {
                    String::new()
                } else {
                    format!(" ({} pickle files excluded)", pr.excluded_pickle.len())
                }
            );
            report.harvested += 1;
            continue;
        }
        match harvest_one(
            cfg,
            hf,
            db,
            &repo,
            &info,
            &pr,
            &kept,
            &store_root,
            publisher.as_ref(),
            upload_sink.as_deref(),
        )
        .await
        {
            Ok(rev) => {
                db.harvest_set(c.repo_type.as_str(), &c.id, &commit, DONE, bytes, None)?;
                println!(
                    "harvested {rlabel}: {} files, {bytes} bytes{}",
                    kept.len(),
                    rev.map(|r| format!(", catalog revision {r}"))
                        .unwrap_or_default()
                );
                report.harvested += 1;
            }
            Err(e) => {
                db.harvest_set(
                    c.repo_type.as_str(),
                    &c.id,
                    &commit,
                    FAILED,
                    0,
                    Some(&format!("{e:#}")),
                )?;
                println!("FAILED   {rlabel}: {e:#}");
                report.failed += 1;
            }
        }
    }
    Ok(report)
}

#[allow(clippy::too_many_arguments)]
async fn harvest_one(
    cfg: &Config,
    hf: &HfClient,
    db: &Arc<StateDb>,
    repo: &RepoRef,
    info: &tensorseed_hf::RepoInfo,
    pr: &tensorseed_hf::policy::PolicyReport,
    kept: &[&TreeEntry],
    store_root: &Path,
    publisher: Option<&Publisher>,
    upload: Option<&dyn ObjectSink>,
) -> anyhow::Result<Option<u64>> {
    let commit = &info.sha;
    let name = TorrentInfo::name_for(&repo.id, commit);
    let content_dir: PathBuf = store_root.join("t").join(&name);
    std::fs::create_dir_all(&content_dir)?;

    // Download with verification and resume.
    let mut jobs = Vec::with_capacity(kept.len());
    for e in kept {
        jobs.push(FileJob {
            path: e.path.clone(),
            size: e.content_size(),
            digest: e.expected_digest()?,
            sources: vec![Source {
                url: hf.resolve_url(repo, commit, &e.path),
                kind: SourceKind::HuggingFace,
                bearer: hf.token().map(str::to_owned),
            }],
        });
    }
    let label = format!("harvest {} {}@{commit}", repo.repo_type, repo.id);
    let spec = JobSpec {
        id: job_id(&label, &content_dir),
        label,
        out_dir: content_dir.clone(),
    };
    let fetch = Fetcher::new(Some(db.clone()))?
        .run(
            &spec,
            jobs,
            FetchOptions {
                concurrency: (cfg.general.max_concurrent_downloads as usize).max(1),
                ..FetchOptions::default()
            },
        )
        .await?;
    if !fetch.ok() {
        let bad: Vec<String> = fetch
            .files
            .iter()
            .filter(|f| f.outcome == tensorseed_fetch::Outcome::Failed)
            .map(|f| format!("{} ({})", f.path, f.errors.join("; ")))
            .collect();
        bail!("download failed: {}", bad.join(", "));
    }
    // The content directory doubles as the webseed tree; drop our scratch dir.
    let meta = content_dir.join(META_DIR);
    if meta.exists() {
        std::fs::remove_dir_all(&meta)?;
    }

    // Manifest: SHA-256 for every file (LFS from the Hub, small files hashed
    // locally after their git blob id verified).
    let mut files = Vec::with_capacity(kept.len());
    for e in kept {
        let lfs_sha = match e.expected_digest()? {
            ExpectedDigest::Sha256(h) => Some(h),
            ExpectedDigest::GitBlobSha1(_) => None,
        };
        let sha256 = match &lfs_sha {
            Some(h) => h.clone(),
            None => sha256_file(content_dir.join(&e.path))?.sha256,
        };
        files.push(FileEntry {
            path: e.path.clone(),
            size: e.content_size(),
            sha256,
            hf_lfs_oid: lfs_sha,
            bt_v2_pieces_root: None,
            format: e.path.rsplit_once('.').map(|(_, x)| x.to_ascii_lowercase()),
            pickle_risk: FileEntry::path_has_pickle_risk(&e.path),
        });
    }
    // Torrent (spec 7.1): one hashing pass gives the BEP 52 roots, written
    // into the manifest, and `t/<name>.torrent` next to the content.
    let webseeds = cfg.storage.webseed_bases();
    let mut torrent_bytes: Option<Vec<u8>> = None;
    let torrent = if cfg.torrent.create {
        let t =
            crate::torrents::build_for_revision(cfg, &content_dir, &name, &mut files, &webseeds)
                .await
                .context("building the torrent")?;
        let tpath = store_root.join("t").join(format!("{name}.torrent"));
        let tmp = store_root.join("t").join(format!(".{name}.torrent.tmp"));
        std::fs::write(&tmp, &t.bytes)?;
        std::fs::rename(&tmp, &tpath)?;
        torrent_bytes = Some(t.bytes.clone());
        Some(t.torrent_info(crate::torrents::torrent_file_urls(&webseeds, &name)))
    } else {
        None
    };
    let totals = Totals {
        files: files.len() as u64,
        bytes: files.iter().map(|f| f.size).sum(),
    };
    let license_files: Vec<String> = files
        .iter()
        .filter(|f| {
            let base = f
                .path
                .rsplit('/')
                .next()
                .unwrap_or(&f.path)
                .to_ascii_uppercase();
            base.starts_with("LICENSE") || base.starts_with("LICENCE") || base.starts_with("NOTICE")
        })
        .map(|f| f.path.clone())
        .collect();
    let pages: Vec<Vec<FileEntry>> = if files.len() > PAGE_THRESHOLD {
        files.chunks(PAGE_SIZE).map(<[FileEntry]>::to_vec).collect()
    } else {
        Vec::new()
    };
    let hf_base = cfg.huggingface.endpoint.trim_end_matches('/');
    let manifest = Manifest {
        schema: SCHEMA_MANIFEST.into(),
        repo: MRepoInfo {
            repo_type: repo.repo_type,
            id: repo.id.clone(),
            hf_url: match repo.repo_type {
                RepoType::Model => format!("{hf_base}/{}", repo.id),
                RepoType::Dataset => format!("{hf_base}/datasets/{}", repo.id),
            },
        },
        hf_commit: commit.clone(),
        hf_last_modified: info.last_modified.clone(),
        license: LicenseInfo {
            spdx: pr.license.clone(),
            hf_tag: pr.license.clone(),
            class: pr.class,
            license_files,
            attribution: None,
            gated_upstream: pr.gated,
        },
        totals,
        files: if pages.is_empty() {
            files.clone()
        } else {
            Vec::new()
        },
        file_pages: pages.len() as u32,
        torrent,
        webseeds,
        backups: Vec::new(),
        published_at: now_rfc3339(),
        publisher_tool: concat!("tensorseed/", env!("CARGO_PKG_VERSION")).into(),
    };
    manifest.validate()?;
    let manifest_json = manifest.to_canonical_json()?;
    let manifest_key = repo.manifest_key(commit)?;
    let manifest_rel = format!("manifests/{}/{}/{commit}.json", repo.repo_type, repo.id);

    // Upload model files first, so a published manifest never points at a
    // webseed object that is missing.
    if let Some(s) = upload {
        for f in &files {
            let key = format!("t/{}/{}", name, f.path);
            s.put_file(
                &key,
                &content_dir.join(&f.path),
                "application/octet-stream",
                CachePolicy::Immutable,
            )
            .await
            .with_context(|| format!("uploading {key} to {}", s.describe()))?;
        }
        if let Some(b) = &torrent_bytes {
            // url-list and trackers can change in a later catalog revision of
            // the same commit, so this path must not be cached as immutable.
            s.put(
                &format!("t/{name}.torrent"),
                b.clone(),
                "application/x-bittorrent",
                CachePolicy::Mutable,
            )
            .await
            .with_context(|| format!("uploading {name}.torrent to {}", s.describe()))?;
        }
        tracing::info!(repo = %repo.id, "uploaded {} files to {}", files.len(), s.describe());
    }

    let Some(p) = publisher else {
        // Not publishing: keep an unsigned manifest next to the data.
        write_atomic(&store_root.join(&manifest_rel), &manifest_json)?;
        return Ok(None);
    };
    let now = now_rfc3339();
    let canon = |v: &dyn ErasedSer| v.canon();
    let manifest_key_copy = manifest_key.clone();
    let mut ops = vec![CatalogOp::Upsert {
        key: manifest_key.clone(),
        value: manifest_json,
    }];
    for (i, page) in pages.iter().enumerate() {
        let fp = FilePage {
            schema: SCHEMA_FILEPAGE.into(),
            page: i as u32,
            files: page.clone(),
        };
        ops.push(CatalogOp::Upsert {
            key: repo.file_page_key(commit, i as u32)?,
            value: canon(&fp)?,
        });
    }
    ops.push(CatalogOp::Upsert {
        key: repo.latest_key(),
        value: canon(&LatestPointer {
            schema: SCHEMA_LATEST.into(),
            commit: commit.clone(),
            manifest_key,
            updated_at: now.clone(),
        })?,
    });
    ops.push(CatalogOp::Upsert {
        key: repo.index_key(),
        value: canon(&IndexEntry {
            schema: SCHEMA_INDEX.into(),
            latest_commit: commit.clone(),
            license_spdx: pr.license.clone(),
            total_bytes: manifest.totals.bytes,
            updated_at: now,
            taken_down: false,
        })?,
    });
    let rev = p.commit(ops).await?;
    // Keep the signed manifest next to the data (and on every target) so a
    // copy of the storage tree verifies without the catalog.
    let signed_bytes = p.signed(&manifest_key_copy).await?.to_bytes()?;
    write_atomic(&store_root.join(&manifest_rel), &signed_bytes)?;
    if let Some(s) = upload {
        s.put(
            &manifest_rel,
            signed_bytes,
            "application/json",
            CachePolicy::Immutable,
        )
        .await?;
    }
    Ok(Some(rev))
}

trait ErasedSer {
    fn canon(&self) -> anyhow::Result<Vec<u8>>;
}
impl<T: serde::Serialize> ErasedSer for T {
    fn canon(&self) -> anyhow::Result<Vec<u8>> {
        Ok(tensorseed_core::canonical::to_canonical_vec(self)?)
    }
}

fn write_atomic(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    let dir = path.parent().context("bad path")?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(".manifest.tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
