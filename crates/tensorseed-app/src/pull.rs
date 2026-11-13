// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Pull support: the `.tensorseed/pull.json` record (`ts.pull/1`), repo
//! spec parsing, and resolving a manifest through one or several catalogs
//! or from manifest bytes. The download pipeline and its output stay in the
//! front ends for now.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use tensorseed_catalog::signed::SignedEntry;
use tensorseed_core::hash::ExpectedDigest;
use tensorseed_core::keys::{RepoRef, RepoType, check_commit};
use tensorseed_core::manifest::{
    FilePage, LatestPointer, Manifest, SCHEMA_FILEPAGE, SCHEMA_LATEST, SCHEMA_TOMBSTONE, Tombstone,
};
use tensorseed_core::verify::{ExpectedFile, META_DIR};
use tensorseed_fetch::state::StateDb;
use tensorseed_hf::HfClient;

use crate::backend::{open_reader, pinned_key, read_verified};
use crate::config::{Config, expand};

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
    /// Files deliberately not downloaded (pickle formats and unselected paths).
    pub skipped: Vec<String>,
    /// Files not downloaded because they did not match `--include` / `--exclude`.
    /// Also listed in [`Self::skipped`] so `serve` can seed the selected
    /// subset with `only_files`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unselected: Vec<String>,
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
    /// Record form of an expected file.
    #[must_use]
    pub fn from_expected(e: &ExpectedFile) -> Self {
        Self {
            path: e.path.clone(),
            size: e.size,
            algorithm: e.digest.algorithm().into(),
            digest: e.digest.hex().into(),
            pickle_risk: e.pickle_risk,
        }
    }

    /// Expected file form of this record.
    ///
    /// # Errors
    ///
    /// Fails on an unknown algorithm or a malformed digest.
    pub fn to_expected(&self) -> anyhow::Result<ExpectedFile> {
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
///
/// # Errors
///
/// Fails on an empty revision or an invalid repo id.
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

/// Hub client with the configured token.
///
/// # Errors
///
/// Fails when the token cannot be resolved or the client cannot be built.
pub fn hf_client(cfg: &Config) -> anyhow::Result<HfClient> {
    cfg.huggingface.client()
}

/// A manifest with its full file list and provenance.
#[derive(Clone)]
pub struct Resolved {
    /// The manifest (with `files` filled in from pages if it was paged).
    pub manifest: Manifest,
    /// Raw bytes to store as `.tensorseed/manifest.json`.
    pub raw: Vec<u8>,
    /// True if the signature was verified against the pinned key.
    pub signed: bool,
    /// Tombstone covering this revision, if any.
    pub tombstone: Option<Tombstone>,
    /// Catalogs that took this revision down while another catalog still
    /// lists it (multi catalog resolution only). Front ends report these.
    pub taken_down_in: Vec<String>,
}

/// True when tombstone `t` covers `commit` (an empty list covers all).
#[must_use]
pub fn tombstone_covers(t: &Tombstone, commit: &str) -> bool {
    t.revisions.is_empty() || t.revisions.iter().any(|r| r == commit)
}

/// Resolve a manifest through the configured catalog.
///
/// # Errors
///
/// Fails when the repo is not in the catalog, an entry does not verify, or catalogs disagree on the revision's files.
pub async fn catalog_manifest(
    cfg: &Config,
    repo: &RepoRef,
    revision: Option<&str>,
    no_wait: bool,
    catalog: Option<&str>,
) -> anyhow::Result<Resolved> {
    if let Some(name) = catalog {
        let sub = cfg.subscription(Some(name))?;
        return catalog_manifest_one(&cfg.for_subscription(&sub), repo, revision, no_wait).await;
    }
    let subs = cfg.subscriptions()?;
    if subs.len() <= 1 {
        let view = match subs.first() {
            Some(s) => cfg.for_subscription(s),
            None => cfg.clone(),
        };
        return catalog_manifest_one(&view, repo, revision, no_wait).await;
    }
    resolve_across(cfg, &subs, repo, revision, no_wait).await
}

async fn catalog_manifest_one(
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
        taken_down_in: Vec::new(),
    })
}

/// Resolve through one subscription; `None` when it fails or lacks the
/// revision (the reason is logged).
pub async fn try_one(
    cfg: &Config,
    sub: &crate::config::Subscription,
    repo: &RepoRef,
    revision: Option<&str>,
    no_wait: bool,
) -> Option<Resolved> {
    let view = cfg.for_subscription(sub);
    match catalog_manifest_one(&view, repo, revision, no_wait).await {
        Ok(r) => Some(r),
        Err(e) => {
            let msg = format!("{e:#}");
            if msg.contains("is not in the catalog") {
                None
            } else {
                tracing::warn!(catalog = %sub.name, "catalog read failed: {msg}");
                None
            }
        }
    }
}

async fn resolve_across(
    cfg: &Config,
    subs: &[crate::config::Subscription],
    repo: &RepoRef,
    revision: Option<&str>,
    no_wait: bool,
) -> anyhow::Result<Resolved> {
    let commit = if let Some(r) = revision {
        tensorseed_core::keys::check_commit(r).map_err(|_| {
            anyhow::anyhow!("catalog revisions are full 40 hex commit SHAs, got {r:?} (or use --from-hf for branches and tags)")
        })?;
        r.to_string()
    } else {
        let mut ordered: Vec<&crate::config::Subscription> = subs.iter().collect();
        ordered.sort_by_key(|s| (!s.is_default, s.priority, s.name.as_str()));
        let mut found = None;
        for sub in &ordered {
            if let Some(r) = try_one(cfg, sub, repo, None, no_wait).await {
                found = Some(r.manifest.hf_commit);
                if sub.is_default {
                    break;
                }
                // Not the default: keep the first (default-first, then lowest priority).
                break;
            }
        }
        found.ok_or_else(|| {
            anyhow::anyhow!("{} {} is not in the catalog", repo.repo_type, repo.id)
        })?
    };

    let mut hits: Vec<(crate::config::Subscription, Resolved)> = Vec::new();
    for sub in subs {
        if let Some(r) = try_one(cfg, sub, repo, Some(&commit), no_wait).await {
            hits.push((sub.clone(), r));
        }
    }
    if hits.is_empty() {
        bail!(
            "{} {}@{commit} is not in the catalog",
            repo.repo_type,
            repo.id
        );
    }
    let mut groups: std::collections::BTreeMap<
        String,
        Vec<(crate::config::Subscription, Resolved)>,
    > = std::collections::BTreeMap::new();
    for (s, r) in hits {
        groups
            .entry(crate::follow::revision_fingerprint(&r.manifest))
            .or_default()
            .push((s, r));
    }
    if groups.len() > 1 {
        let summary: Vec<(String, Vec<String>)> = groups
            .iter()
            .map(|(fp, hits)| {
                (
                    fp.clone(),
                    hits.iter().map(|(s, _)| s.display().to_string()).collect(),
                )
            })
            .collect();
        bail!(
            "{}",
            crate::follow::conflict_error(&repo.id, &commit, &summary)
        );
    }
    let agreeing = groups.into_values().next().unwrap();
    let live: Vec<(crate::config::Subscription, Resolved)> = agreeing
        .iter()
        .filter(|(_, r)| match &r.tombstone {
            Some(t) => !tombstone_covers(t, &commit),
            None => true,
        })
        .cloned()
        .collect();
    let taken: Vec<&crate::config::Subscription> = agreeing
        .iter()
        .filter(|(_, r)| {
            r.tombstone
                .as_ref()
                .is_some_and(|t| tombstone_covers(t, &commit))
        })
        .map(|(s, _)| s)
        .collect();
    let (mut resolved, extras) = if live.is_empty() {
        let auth = crate::follow::authority(&agreeing)?;
        (auth.1.clone(), Vec::new())
    } else {
        let auth = crate::follow::authority(&live)?;
        let extras: Vec<Manifest> = live
            .iter()
            .filter(|(s, _)| s.name != auth.0.name)
            .map(|(_, r)| r.manifest.clone())
            .collect();
        let mut r = auth.1.clone();
        r.tombstone = None;
        r.taken_down_in = taken.iter().map(|s| s.display().to_string()).collect();
        (r, extras)
    };
    crate::follow::merge_webseeds(&mut resolved.manifest, &extras);
    Ok(resolved)
}

/// Check that a manifest is for `repo` at `commit`.
///
/// # Errors
///
/// Fails when the repo or commit differs.
pub fn check_manifest_identity(m: &Manifest, repo: &RepoRef, commit: &str) -> anyhow::Result<()> {
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
///
/// # Errors
///
/// Fails when a signed entry does not verify or the bytes are not a manifest, or for paged manifests.
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
            taken_down_in: Vec::new(),
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
        taken_down_in: Vec::new(),
    })
}

/// Download a manifest over HTTP (60 second timeout, at most 64 MiB).
///
/// # Errors
///
/// Fails on a network error, a non success status, or an oversized body.
pub async fn fetch_bytes(url: &str) -> anyhow::Result<Vec<u8>> {
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

/// Write `data` atomically to `<out>/.tensorseed/<name>`.
///
/// # Errors
///
/// Fails when the directory or file cannot be written.
pub fn write_meta(out: &Path, name: &str, data: &[u8]) -> anyhow::Result<()> {
    let meta = out.join(META_DIR);
    std::fs::create_dir_all(&meta)?;
    let tmp = meta.join(format!(".{name}.tmp"));
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, meta.join(name))?;
    Ok(())
}

/// Open the shared job state database (`general.state_db`), creating its
/// directory.
///
/// # Errors
///
/// Fails when the directory cannot be created or the database cannot be opened.
pub fn state_db(cfg: &Config) -> anyhow::Result<Arc<StateDb>> {
    let p = expand(&cfg.general.state_db);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    Ok(Arc::new(
        StateDb::open(&p).with_context(|| format!("opening {}", p.display()))?,
    ))
}

/// Paths recorded as skipped or unselected in a pull, if one exists.
#[must_use]
pub fn allow_missing_from(record: &Option<PullRecord>) -> Vec<String> {
    let Some(r) = record else {
        return Vec::new();
    };
    let mut v = r.skipped.clone();
    for p in &r.unselected {
        if !v.iter().any(|s| s == p) {
            v.push(p.clone());
        }
    }
    v
}

/// Read `<dir>/.tensorseed/pull.json`, if present.
///
/// # Errors
///
/// Fails when the file cannot be read, does not parse, or has another schema.
pub fn read_pull_record(dir: &Path) -> anyhow::Result<Option<PullRecord>> {
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
