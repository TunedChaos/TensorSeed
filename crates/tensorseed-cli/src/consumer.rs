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

use anyhow::{Context, bail};
use tensorseed_core::commercial::{
    Assessment, CommercialServing, DISCLAIMER as COMMERCIAL_DISCLAIMER, assess, assess_license_info,
};
use tensorseed_core::keys::{RepoRef, RepoType, index_prefix};
use tensorseed_core::manifest::{FileEntry, IndexEntry, SCHEMA_INDEX};
use tensorseed_core::paths::native_path;
use tensorseed_core::verify::{ExpectedFile, META_DIR, VerifyOptions, verify_dir};

pub use tensorseed_app::pull::*;
use tensorseed_app::pull_job::{PullControl, PullEvent, PullOptions};

use crate::backend::{open_reader, pinned_key, read_verified};
use crate::cli::{PullArgs, VerifyArgs};
use crate::config::Config;

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

/// The shared pull options for `tensorseed pull` flags.
fn pull_options(args: &PullArgs) -> PullOptions {
    PullOptions {
        repo: args.repo.clone(),
        repo_type: args.repo_type.into(),
        revision: args.revision.clone(),
        out: args.out.clone(),
        from_hf: args.from_hf,
        manifest_file: args.manifest_file.clone(),
        manifest_url: args.manifest_url.clone(),
        no_hf_fallback: args.no_hf_fallback,
        retry_dead_mirrors: args.retry_dead_mirrors,
        jobs: args.jobs,
        allow_pickle: args.allow_pickle,
        include: args.include.clone(),
        exclude: args.exclude.clone(),
        allow_gated: args.allow_gated,
        allow_license: args.allow_license.clone(),
        no_wait: args.no_wait,
        catalog: args.catalog.clone(),
        ignore_tombstone: args.ignore_tombstone,
        swarm: args.swarm,
        no_swarm: args.no_swarm,
        swarm_only: args.swarm_only,
        peer: args.peer.clone(),
        need_torrent: args.seed_after.is_some(),
    }
}

/// Print one pull event the way `tensorseed pull` always has.
fn print_pull_event(ev: PullEvent) {
    match ev.line() {
        Some((line, true)) => eprintln!("{line}"),
        Some((line, false)) => println!("{line}"),
        None => {}
    }
}

/// `tensorseed pull`.
pub async fn pull(cfg: &Config, args: &PullArgs) -> anyhow::Result<ExitCode> {
    if args.seed_after.is_some() && args.from_hf {
        bail!("--seed-after needs a Tensorseed manifest with a torrent, not --from-hf");
    }
    let seed_after = args
        .seed_after
        .as_deref()
        .map(crate::torrents::parse_duration)
        .transpose()?;
    let ctl = PullControl::new(print_pull_event);
    let done = tensorseed_app::pull_job::run(cfg, &pull_options(args), &ctl).await?;
    if done.swarm_ran {
        println!(
            "{} from the swarm ({} bytes), {} downloaded over HTTP \
             ({} bytes), {} already present, {} failed",
            done.swarm_files,
            done.swarm_bytes,
            done.downloaded,
            done.http_bytes,
            done.present,
            done.failed
        );
    } else {
        println!(
            "{} downloaded ({} bytes), {} already present, {} failed",
            done.downloaded, done.http_bytes, done.present, done.failed
        );
    }
    if !done.ok {
        eprintln!("pull incomplete; run the same command again to resume");
        return Ok(ExitCode::from(EXIT_VERIFY_FAILED));
    }
    if !done.signed {
        println!(
            "note: hashes came from an unsigned source ({}), not the signed catalog",
            done.source
        );
    }
    if let (Some(dur), Some(t)) = (seed_after, done.torrent) {
        let item = crate::torrents::pulled_dir(&done.out)?;
        debug_assert_eq!(item.torrent.info_hash_v2, t.info_hash_v2);
        return crate::torrents::run_seeder(cfg, vec![item], dur).await;
    }
    Ok(ExitCode::SUCCESS)
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
                .as_deref()
                .map(native_path)
                .unwrap_or_else(|| native_path(Path::new(&args.target))),
            Some(r.clone()),
        ),
        (None, true) => (native_path(Path::new(&args.target)), None),
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
        (Some((r, _)), true) => args
            .dir
            .as_deref()
            .map(native_path)
            .unwrap_or_else(|| default_out(cfg, r)),
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

/// What `tensorseed list` prints and filters on.
#[derive(Debug, Clone, Default)]
pub struct ListOptions {
    /// Repo id substring.
    pub filter: Option<String>,
    /// Only entries whose license permits commercial use (`allowed` or `restricted`).
    pub commercial: bool,
    /// Only entries with one of these commercial serving marks.
    pub serving: Vec<CommercialServing>,
    /// Print one JSON document instead of text.
    pub json: bool,
}

impl ListOptions {
    fn keeps(&self, rest: &str) -> bool {
        self.filter.as_deref().is_none_or(|f| rest.contains(f))
    }

    fn keeps_mark(&self, a: &Assessment) -> bool {
        (!self.commercial || a.value.permits_commercial())
            && (self.serving.is_empty() || self.serving.contains(&a.value))
    }
}

/// Schema of `tensorseed list --json`.
pub const LIST_SCHEMA: &str = "ts.list/1";

/// One row of `tensorseed list`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ListRow {
    /// `model` or `dataset`.
    #[serde(rename = "type")]
    pub repo_type: String,
    /// `org/name`.
    pub repo: String,
    /// Latest commit.
    pub commit: String,
    /// Total bytes of the latest revision.
    pub total_bytes: u64,
    /// License id from the index entry.
    pub license: Option<String>,
    /// Derived commercial serving mark (client side, not signed).
    pub commercial_serving: CommercialServing,
    /// Reason or conditions for the mark.
    pub commercial_note: String,
    /// True when every listing catalog took the revision down.
    pub taken_down: bool,
    /// Catalogs listing this revision (multi catalog mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalogs: Option<usize>,
}

impl ListRow {
    fn new(rest: &str, commit: &str, ix: &IndexEntry, a: Assessment) -> Self {
        let (ty, repo) = rest.split_once('/').unwrap_or(("model", rest));
        Self {
            repo_type: ty.to_string(),
            repo: repo.to_string(),
            commit: commit.to_string(),
            total_bytes: ix.total_bytes,
            license: ix.license_spdx.clone(),
            commercial_serving: a.value,
            commercial_note: a.note,
            taken_down: ix.taken_down,
            catalogs: None,
        }
    }
}

fn print_list_json(rows: &[ListRow]) -> anyhow::Result<()> {
    let doc = serde_json::json!({
        "schema": LIST_SCHEMA,
        "disclaimer": COMMERCIAL_DISCLAIMER,
        "entries": rows,
    });
    println!("{}", serde_json::to_string_pretty(&doc)?);
    Ok(())
}

fn list_footer(n: usize, opts: &ListOptions) {
    eprintln!("{n} entries");
    if opts.commercial || !opts.serving.is_empty() || n > 0 {
        eprintln!("note: {COMMERCIAL_DISCLAIMER}");
    }
}

/// `tensorseed list`.
pub async fn list(
    cfg: &Config,
    opts: &ListOptions,
    no_wait: bool,
    catalog: Option<&str>,
) -> anyhow::Result<ExitCode> {
    if let Some(name) = catalog {
        let sub = cfg.subscription(Some(name))?;
        return list_one(&cfg.for_subscription(&sub), opts, no_wait, false).await;
    }
    let subs = cfg.subscriptions()?;
    if subs.len() <= 1 {
        let view = match subs.first() {
            Some(s) => cfg.for_subscription(s),
            None => cfg.clone(),
        };
        return list_one(&view, opts, no_wait, false).await;
    }
    list_across(cfg, &subs, opts, no_wait).await
}

async fn list_one(
    cfg: &Config,
    opts: &ListOptions,
    no_wait: bool,
    with_agreement: bool,
) -> anyhow::Result<ExitCode> {
    let pk = pinned_key(cfg)?;
    let (backend, id) = open_reader(cfg, None, no_wait).await?;
    let b = backend.as_dyn();
    let prefix = index_prefix();
    let mut rows = Vec::new();
    let mut table: Vec<Vec<String>> = Vec::new();
    for key in b.keys(&id, &prefix).await? {
        let rest = &key[prefix.len()..];
        if !opts.keeps(rest) {
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
        let a = assess(ix.license_spdx.as_deref(), None);
        if !opts.keeps_mark(&a) {
            continue;
        }
        if !opts.json {
            let agreement = if with_agreement {
                format!("  {}", crate::follow::agreement_line(1, 1, false))
            } else {
                String::new()
            };
            let tail = format!(
                "{}{agreement}",
                if ix.taken_down { "[taken down]" } else { "" }
            );
            table.push(vec![
                rest.to_string(),
                ix.latest_commit
                    .get(..12)
                    .unwrap_or(&ix.latest_commit)
                    .to_string(),
                format!("{} bytes", ix.total_bytes),
                ix.license_spdx
                    .as_deref()
                    .unwrap_or("license unknown")
                    .to_string(),
                format!("serving:{}", a.value),
                tail.trim_start().to_string(),
            ]);
        }
        rows.push(ListRow::new(rest, &ix.latest_commit, &ix, a));
    }
    print_aligned(&table);
    if opts.json {
        print_list_json(&rows)?;
    }
    list_footer(rows.len(), opts);
    Ok(ExitCode::SUCCESS)
}

async fn list_across(
    cfg: &Config,
    subs: &[crate::config::Subscription],
    opts: &ListOptions,
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
            if !opts.keeps(rest) {
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
            if !opts.keeps_mark(&assess(ix.license_spdx.as_deref(), None)) {
                continue;
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
    let mut out = Vec::new();
    let mut table: Vec<Vec<String>> = Vec::new();
    for ((rest, commit), hits) in groups {
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
        let a = assess(ix.license_spdx.as_deref(), None);
        let down = hits.iter().filter(|(_, i, _)| i.taken_down).count();
        let down_tag = if down == listed && listed > 0 {
            "  [taken down]"
        } else if down > 0 {
            "  [taken down in some catalogs]"
        } else {
            ""
        };
        if !opts.json {
            let tail = format!(
                "{}  {}",
                down_tag.trim_start(),
                crate::follow::agreement_line(agreeing, subs.len(), conflict)
            );
            table.push(vec![
                rest.clone(),
                commit.get(..12).unwrap_or(&commit).to_string(),
                format!("{} bytes", ix.total_bytes),
                ix.license_spdx
                    .as_deref()
                    .unwrap_or("license unknown")
                    .to_string(),
                format!("serving:{}", a.value),
                tail.trim_start().to_string(),
            ]);
        }
        if conflict {
            eprintln!(
                "CONFLICT: {rest}@{commit} has different hashes; pass --catalog <name> to inspect one"
            );
        }
        let mut row = ListRow::new(&rest, &commit, ix, a);
        row.taken_down = down == listed && listed > 0;
        row.catalogs = Some(listed);
        out.push(row);
    }
    print_aligned(&table);
    if opts.json {
        print_list_json(&out)?;
    }
    list_footer(out.len(), opts);
    if errors > 0 {
        eprintln!("{errors} catalog(s) could not be read (other catalogs were listed)");
    }
    if errors > 0 && out.is_empty() {
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
    let a = assess_license_info(&res.manifest.license);
    eprintln!("commercial serving: {} ({})", a.value, a.note);
    eprintln!("note: {COMMERCIAL_DISCLAIMER}");
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

/// Prints `list` rows as aligned columns. The size column (index 2) is
/// right aligned; the last column is free text and is not padded.
fn print_aligned(rows: &[Vec<String>]) {
    for line in align_rows(rows) {
        println!("{line}");
    }
}

fn align_rows(rows: &[Vec<String>]) -> Vec<String> {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let mut widths = vec![0usize; cols];
    for r in rows {
        for (i, c) in r.iter().enumerate() {
            widths[i] = widths[i].max(c.chars().count());
        }
    }
    rows.iter()
        .map(|r| {
            let mut line = String::new();
            for (i, c) in r.iter().enumerate() {
                let last = i + 1 == r.len();
                if i > 0 {
                    line.push_str("  ");
                }
                let pad = widths[i].saturating_sub(c.chars().count());
                if last {
                    line.push_str(c);
                } else if i == 2 {
                    line.push_str(&" ".repeat(pad));
                    line.push_str(c);
                } else {
                    line.push_str(c);
                    line.push_str(&" ".repeat(pad));
                }
            }
            line.trim_end().to_string()
        })
        .collect()
}

#[cfg(test)]
mod align_tests {
    use super::align_rows;

    #[test]
    fn columns_line_up_and_size_is_right_aligned() {
        let rows = vec![
            vec![
                "model/a".into(),
                "abc".into(),
                "5 bytes".into(),
                "mit".into(),
                "serving:allowed".into(),
                String::new(),
            ],
            vec![
                "dataset/long/name".into(),
                "def".into(),
                "12345 bytes".into(),
                "apache-2.0".into(),
                "serving:allowed".into(),
                "[taken down]".into(),
            ],
        ];
        let out = align_rows(&rows);
        assert_eq!(out[0].find("serving:"), out[1].find("serving:"));
        assert_eq!(out[0].find(" bytes"), out[1].find(" bytes"));
        assert!(!out[0].ends_with(' '));
        assert!(out[1].ends_with("[taken down]"));
    }
}
