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
use std::time::Duration;

use anyhow::{Context, bail};
use tensorseed_core::keys::{RepoRef, RepoType, index_prefix};
use tensorseed_core::manifest::{FileEntry, IndexEntry, SCHEMA_INDEX};
use tensorseed_core::verify::{ExpectedFile, META_DIR, VerifyOptions, verify_dir};
use tensorseed_fetch::{
    FetchOptions, Fetcher, FileJob, JobSpec, Outcome, Source, SourceKind, job_id,
};
use tensorseed_hf::policy::Policy as HfPolicy;

pub use tensorseed_app::pull::*;

use crate::backend::{open_reader, pinned_key, read_verified};
use crate::cli::{PullArgs, VerifyArgs};
use crate::config::{Config, FetchSource};

/// Exit code for a failed verification or download.
pub const EXIT_VERIFY_FAILED: u8 = 3;

/// Resolve a manifest through the configured catalog (see
/// [`tensorseed_app::pull::catalog_manifest`]), reporting on stderr any
/// catalog that took the revision down while another still lists it.
pub async fn catalog_manifest(
    cfg: &Config,
    repo: &RepoRef,
    revision: Option<&str>,
    no_wait: bool,
    catalog: Option<&str>,
) -> anyhow::Result<Resolved> {
    let r = tensorseed_app::pull::catalog_manifest(cfg, repo, revision, no_wait, catalog).await?;
    for c in &r.taken_down_in {
        eprintln!("taken down in catalog {c} (still listed by another catalog)");
    }
    Ok(r)
}

fn hf_policy(cfg: &Config, args: &PullArgs) -> HfPolicy {
    HfPolicy {
        license_allowlist: cfg.policy.license_allowlist.clone(),
        attribution_licenses: cfg.policy.attribution_licenses.clone(),
        allow_gated: cfg.policy.allow_gated || args.allow_gated,
        allow_pickle_formats: cfg.policy.allow_pickle_formats || args.allow_pickle,
        license_overrides: args.allow_license.clone(),
    }
}

/// Skip reason for a file, or `None` to download it.
///
/// `--exclude` wins over `--include`. Unselected paths are skipped before
/// the pickle policy, so a path that is both unselected and a pickle file
/// is recorded as unselected.
fn skip_reason(ef: &ExpectedFile, args: &PullArgs, allow_pickle: bool) -> Option<&'static str> {
    if !tensorseed_serve::select::path_selected(&ef.path, &args.include, &args.exclude) {
        return Some("unselected");
    }
    if ef.pickle_risk && !allow_pickle {
        return Some("pickle");
    }
    None
}

/// Fail when `--include` / `--exclude` matched no files in the revision.
fn reject_empty_selection(
    expected: &[ExpectedFile],
    args: &PullArgs,
    repo_id: &str,
) -> anyhow::Result<()> {
    if args.include.is_empty() && args.exclude.is_empty() {
        return Ok(());
    }
    if expected
        .iter()
        .all(|f| !tensorseed_serve::select::path_selected(&f.path, &args.include, &args.exclude))
    {
        bail!("no files matched --include/--exclude for {repo_id}");
    }
    Ok(())
}

/// `tensorseed pull`.
pub async fn pull(cfg: &Config, args: &PullArgs) -> anyhow::Result<ExitCode> {
    let use_swarm = swarm_wanted(cfg, args);
    if args.seed_after.is_some() && args.from_hf {
        bail!("--seed-after needs a Tensorseed manifest with a torrent, not --from-hf");
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

    if args.from_hf {
        source = "huggingface";
        let info = hf.repo_info(&repo, revision.as_deref()).await?;
        if info.disabled {
            bail!("{} is disabled on the Hub", repo.id);
        }
        let tree = hf.files(&repo, &info.sha).await?;
        let policy = hf_policy(cfg, args);
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
            if let Some(reason) = skip_reason(&ef, args, policy.allow_pickle_formats) {
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
        reject_empty_selection(&expected, args, &repo.id)?;
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
            catalog_manifest(
                cfg,
                &repo,
                revision.as_deref(),
                args.no_wait,
                args.catalog.as_deref(),
            )
            .await?
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
        // priority. HTTP source order follows `[fetch] source_order`.
        let mut seeds = m.clone();
        seeds.webseeds = crate::mirrors::webseed_bases(cfg, &m.webseeds);
        if use_swarm || args.seed_after.is_some() {
            if m.torrent.is_some() {
                torrent = Some(crate::torrents::fetch_for_manifest(m, &seeds.webseeds).await?);
            } else if args.seed_after.is_some() {
                bail!("--seed-after needs a Tensorseed manifest with a torrent");
            }
            // No torrent on the revision: skip the swarm silently and use HTTP.
        }
        for f in &m.files {
            let ef = ExpectedFile::from_manifest(f);
            if let Some(reason) = skip_reason(&ef, args, args.allow_pickle) {
                skipped.push(ef.path.clone());
                if reason == "unselected" {
                    unselected.push(ef.path.clone());
                }
            } else {
                let sources =
                    http_sources(cfg, args, &seeds, &f.path, hf_source(&commit, &f.path))?;
                files.push(FileJob {
                    path: f.path.clone(),
                    size: f.size,
                    digest: ef.digest.clone(),
                    sources,
                });
            }
            expected.push(ef);
        }
        reject_empty_selection(&expected, args, &repo.id)?;
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
    for s in &unselected {
        println!("skip   {s} (unselected)");
    }
    for s in &skipped {
        if unselected.iter().any(|u| u == s) {
            continue;
        }
        println!("skip   {s} (pickle format; --allow-pickle to include)");
    }
    // Files complete on disk before the swarm ran; anything else the HTTP
    // pass finds already present was written by the swarm (and is verified
    // by that pass like everything else).
    let mut before_swarm: Option<std::collections::HashSet<String>> = None;
    if use_swarm && let Some(t) = &torrent {
        before_swarm = Some(
            files
                .iter()
                .filter(|f| std::fs::metadata(out.join(&f.path)).is_ok_and(|m| m.len() == f.size))
                .map(|f| f.path.clone())
                .collect(),
        );
        if let Err(e) = swarm_download(cfg, args, t, &out, &files).await {
            if args.swarm_only {
                return Err(e);
            }
            println!("swarm  unavailable ({e}); using HTTP sources");
        }
    }
    let fetcher = Fetcher::new(Some(state_db(cfg)?))?;
    let report = fetcher
        .run(
            &spec,
            files,
            FetchOptions {
                concurrency: jobs,
                retry_dead_mirrors: args.retry_dead_mirrors,
                dead_mirror_cooldown: Duration::from_secs(cfg.fetch.dead_mirror_cooldown_secs),
                ..FetchOptions::default()
            },
        )
        .await?;
    for n in &report.skipped_dead {
        println!("{}", n.line);
    }
    let (mut swarm_files, mut swarm_bytes, mut http_bytes) = (0usize, 0u64, 0u64);
    for f in &report.files {
        match &f.outcome {
            Outcome::Downloaded {
                kind, resumed_from, ..
            } => {
                http_bytes += f.size.saturating_sub(*resumed_from);
                let resumed = if *resumed_from > 0 {
                    format!(", resumed at {resumed_from}")
                } else {
                    String::new()
                };
                println!("ok     {} ({}{resumed})", f.path, kind.label());
            }
            Outcome::AlreadyPresent
                if before_swarm
                    .as_ref()
                    .is_some_and(|pre| !pre.contains(&f.path)) =>
            {
                swarm_files += 1;
                swarm_bytes += f.size;
                println!("ok     {} (swarm)", f.path);
            }
            Outcome::AlreadyPresent => println!("ok     {} (already present)", f.path),
            Outcome::Failed if args.swarm_only => {
                println!(
                    "FAILED {} (the swarm did not deliver it; --swarm-only)",
                    f.path
                );
            }
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
        unselected,
    };
    if let Some(t) = &torrent {
        write_meta(&out, crate::torrents::PULLED_TORRENT, &t.bytes)?;
    }
    write_meta(&out, "pull.json", &serde_json::to_vec_pretty(&record)?)?;
    let present = present - swarm_files;
    if before_swarm.is_some() {
        println!(
            "{swarm_files} from the swarm ({swarm_bytes} bytes), {down} downloaded over HTTP \
             ({http_bytes} bytes), {present} already present, {failed} failed"
        );
    } else {
        println!(
            "{down} downloaded ({http_bytes} bytes), {present} already present, {failed} failed"
        );
    }
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

/// Whether this pull should try the swarm. `--from-hf` and `--no-swarm`
/// skip it; `--swarm-only` and `--swarm` force it on; otherwise
/// `torrent.pull_swarm` (on by default) is used. A missing torrent is
/// skipped later, silently.
fn swarm_wanted(cfg: &Config, args: &PullArgs) -> bool {
    if args.from_hf || args.no_swarm {
        false
    } else if args.swarm_only || args.swarm {
        true
    } else {
        cfg.torrent.pull_swarm && cfg.fetch.wants(FetchSource::Swarm)
    }
}

/// HTTP sources for one file, in `[fetch] source_order`.
///
/// `--swarm-only` returns no HTTP sources. `--no-hf-fallback` omits the
/// Hub. A gated or missing file on the Hub is a 401/403/404 that the
/// fetcher treats as "try the next source" without marking the host dead.
fn http_sources(
    cfg: &Config,
    args: &PullArgs,
    seeds: &tensorseed_core::manifest::Manifest,
    path: &str,
    hub: Source,
) -> anyhow::Result<Vec<Source>> {
    if args.swarm_only {
        return Ok(Vec::new());
    }
    let mut sources = Vec::new();
    for kind in cfg.fetch.sources()? {
        match kind {
            FetchSource::Swarm => {}
            FetchSource::Hub => {
                if !args.no_hf_fallback {
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
    if sources.is_empty() && !swarm_wanted(cfg, args) {
        bail!("{path} has no HTTP sources (check fetch.source_order and --no-hf-fallback)");
    }
    Ok(sources)
}

/// Swarm first: let librqbit fetch what peers have, in place. The session
/// is outbound only (no listen port). Nothing it writes is trusted: the
/// HTTP fetcher that runs next hashes every file against the manifest,
/// keeps the ones that match, and downloads the rest. `--seed-after` is
/// the only path that starts a listening seeder after a verified pull.
async fn swarm_download(
    cfg: &Config,
    args: &PullArgs,
    t: &tensorseed_torrent::Metainfo,
    out: &Path,
    files: &[FileJob],
) -> anyhow::Result<()> {
    use tensorseed_torrent::engine::{DownloadOptions, DownloadStop, Engine};
    let mut peers = cfg.torrent.peer_addrs()?;
    for p in &args.peer {
        use std::net::ToSocketAddrs;
        peers.extend(
            p.to_socket_addrs()
                .with_context(|| format!("--peer {p:?}"))?,
        );
    }
    let timeout = Duration::from_secs(cfg.torrent.swarm_timeout_secs.max(1));
    // --swarm-only has no HTTP fallback, so wait the full timeout.
    let (no_peer, stall) = if args.swarm_only {
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
    let t0 = std::time::Instant::now();
    let r = engine
        .download(
            t,
            out,
            DownloadOptions {
                peers,
                only: Some(files.iter().map(|f| f.path.clone()).collect()),
                no_peer,
                stall,
            },
            timeout,
        )
        .await;
    match r {
        Ok((h, p, stop)) => {
            let line = match (stop, args.swarm_only) {
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
            println!("{line}");
            let _ = engine.remove(&h).await;
        }
        Err(e) if args.swarm_only => println!("swarm  unavailable ({e})"),
        Err(e) => println!("swarm  unavailable ({e}); using HTTP sources"),
    }
    engine.stop().await;
    Ok(())
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
        let res = catalog_manifest(
            cfg,
            r,
            rev.as_deref(),
            args.no_wait,
            args.catalog.as_deref(),
        )
        .await?;
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
            allow_missing: if args.strict {
                Vec::new()
            } else {
                allow_missing_from(&record)
            },
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
pub async fn list(
    cfg: &Config,
    filter: Option<&str>,
    no_wait: bool,
    catalog: Option<&str>,
) -> anyhow::Result<ExitCode> {
    if let Some(name) = catalog {
        let sub = cfg.subscription(Some(name))?;
        return list_one(&cfg.for_subscription(&sub), filter, no_wait, false).await;
    }
    let subs = cfg.subscriptions()?;
    if subs.len() <= 1 {
        let view = match subs.first() {
            Some(s) => cfg.for_subscription(s),
            None => cfg.clone(),
        };
        return list_one(&view, filter, no_wait, false).await;
    }
    list_across(cfg, &subs, filter, no_wait).await
}

async fn list_one(
    cfg: &Config,
    filter: Option<&str>,
    no_wait: bool,
    with_agreement: bool,
) -> anyhow::Result<ExitCode> {
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
        let agreement = if with_agreement {
            format!("  {}", crate::follow::agreement_line(1, 1, false))
        } else {
            String::new()
        };
        println!(
            "{rest}  {}  {} bytes  {}{}{agreement}",
            ix.latest_commit.get(..12).unwrap_or(&ix.latest_commit),
            ix.total_bytes,
            ix.license_spdx.as_deref().unwrap_or("license unknown"),
            if ix.taken_down { "  [taken down]" } else { "" }
        );
    }
    eprintln!("{n} entries");
    Ok(ExitCode::SUCCESS)
}

async fn list_across(
    cfg: &Config,
    subs: &[crate::config::Subscription],
    filter: Option<&str>,
    no_wait: bool,
) -> anyhow::Result<ExitCode> {
    // repo+commit -> (sub, IndexEntry, Option<fingerprint>)
    let mut rows: Vec<(
        String,
        String,
        crate::config::Subscription,
        IndexEntry,
        Option<String>,
    )> = Vec::new();
    let mut errors = 0usize;
    for sub in subs {
        let view = cfg.for_subscription(sub);
        let pk = match pinned_key(&view) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("catalog {}: {e:#}", sub.display());
                errors += 1;
                continue;
            }
        };
        let (backend, id) = match open_reader(&view, None, no_wait).await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("catalog {}: {e:#}", sub.display());
                errors += 1;
                continue;
            }
        };
        let b = backend.as_dyn();
        let prefix = index_prefix();
        let keys = match b.keys(&id, &prefix).await {
            Ok(k) => k,
            Err(e) => {
                eprintln!("catalog {}: {e:#}", sub.display());
                errors += 1;
                continue;
            }
        };
        for key in keys {
            let rest = &key[prefix.len()..];
            if filter.is_some_and(|f| !rest.contains(f)) {
                continue;
            }
            let Some(e) = read_verified(b, &id, &key, &pk, &view.catalog.name).await? else {
                continue;
            };
            let ix: IndexEntry =
                serde_json::from_str(&e.value).with_context(|| format!("parsing {key}"))?;
            if ix.schema != SCHEMA_INDEX {
                bail!("{key} has schema {:?}", ix.schema);
            }
            let typ_and_id = rest.to_string();
            let (ty, repo_id) = rest.split_once('/').unwrap_or(("model", rest));
            let repo_type = match ty {
                "dataset" => RepoType::Dataset,
                _ => RepoType::Model,
            };
            let fp = match RepoRef::new(repo_type, repo_id) {
                Ok(repo) => try_one(cfg, sub, &repo, Some(&ix.latest_commit), no_wait)
                    .await
                    .map(|r| crate::follow::revision_fingerprint(&r.manifest)),
                Err(_) => None,
            };
            rows.push((typ_and_id, ix.latest_commit.clone(), sub.clone(), ix, fp));
        }
    }
    // Group by repo + commit.
    type Hit = (crate::config::Subscription, IndexEntry, Option<String>);
    let mut groups: std::collections::BTreeMap<(String, String), Vec<Hit>> =
        std::collections::BTreeMap::new();
    for (id, commit, sub, ix, fp) in rows {
        groups.entry((id, commit)).or_default().push((sub, ix, fp));
    }
    let mut n = 0;
    for ((rest, commit), hits) in groups {
        n += 1;
        let listed = hits.len();
        let mut fps: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        for (_, _, fp) in &hits {
            if let Some(f) = fp {
                *fps.entry(f.clone()).or_default() += 1;
            }
        }
        let conflict = fps.len() > 1;
        let agreeing = fps.values().copied().max().unwrap_or(listed);
        let ix = &hits[0].1;
        let down = hits.iter().filter(|(_, i, _)| i.taken_down).count();
        let down_tag = if down == listed && listed > 0 {
            "  [taken down]"
        } else if down > 0 {
            "  [taken down in some catalogs]"
        } else {
            ""
        };
        println!(
            "{rest}  {}  {} bytes  {}{}  {}",
            commit.get(..12).unwrap_or(&commit),
            ix.total_bytes,
            ix.license_spdx.as_deref().unwrap_or("license unknown"),
            down_tag,
            crate::follow::agreement_line(agreeing, subs.len(), conflict)
        );
        if conflict {
            eprintln!(
                "CONFLICT: {rest}@{commit} has different hashes; pass --catalog <name> to inspect one"
            );
        }
    }
    eprintln!("{n} entries");
    if errors > 0 {
        eprintln!("{errors} catalog(s) could not be read (other catalogs were listed)");
    }
    if errors > 0 && n == 0 {
        anyhow::bail!("no catalog could be read");
    }
    Ok(ExitCode::SUCCESS)
}

/// `tensorseed show`.
pub async fn show(
    cfg: &Config,
    repo: &str,
    repo_type: RepoType,
    revision: Option<&str>,
    no_wait: bool,
    catalog: Option<&str>,
) -> anyhow::Result<ExitCode> {
    let (r, rev) = parse_repo(repo, repo_type, revision)?;
    let res = catalog_manifest(cfg, &r, rev.as_deref(), no_wait, catalog).await?;
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
    let dead = db.list_dead_mirrors()?;
    if !dead.is_empty() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let cool = cfg.fetch.dead_mirror_cooldown_secs;
        println!("dead mirrors (cooldown {cool}s; tensorseed pull --retry-dead-mirrors to retry):");
        for m in dead {
            let age = tensorseed_fetch::dead_ago_label(m.marked_unix, now);
            let why = m.reason.as_deref().unwrap_or("");
            if why.is_empty() {
                println!("  {}  marked {age}", m.origin);
            } else {
                println!("  {}  marked {age}  ({why})", m.origin);
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}
