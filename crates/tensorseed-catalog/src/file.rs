// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Signed static-file catalog backend (spec section 6.8). No chain needed.
//!
//! The catalog is a set of [`SignedEntry`] values (same keys and documents as
//! every other backend) published as append-only files:
//!
//! ```text
//! <base_url>/<catalog>/head.json            (+ .minisig)  rewritten each revision
//! <base_url>/<catalog>/deltas/<rev>.json    (+ .minisig)  one per revision, never rewritten
//! <base_url>/<catalog>/snapshots/<rev>.json (+ .minisig)  full state, every N revisions
//! ```
//!
//! Each revision `n` has a root
//! `sha256(canonical {"catalog","prev_root","revision","schema":"ts.fileroot/1","state_hash"})`
//! where `prev_root` is the root of revision `n - 1` (all zeros before revision
//! 1). Clients pin the publisher key and remember the highest `(revision,
//! root)` they accepted, which makes rollback, replay, gaps, and forks
//! detectable.
//!
//! Status: signature verification, hash chain checks, HTTPS sync with
//! mirror fallback, persisted trust anchor, publishing to a directory or an
//! one or more S3 compatible buckets, snapshots, and head re-signing are implemented and tested
//! against local HTTP servers. The catalog snapshot torrent is pending the
//! M0 BitTorrent spike.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tensorseed_core::canonical::to_canonical_vec;
use tensorseed_core::hash::sha256_bytes;

use crate::anchor::{CHAIN_PROOF_PATH, is_reserved_key};
use crate::minisign::{PublicKey, SecretKey};
use crate::signed::SignedEntry;
use crate::sink::{CachePolicy, ObjectSink};
use crate::{
    CatalogBackend, CatalogError, CatalogId, CatalogOp, Generation, PendingCommit, Result,
    SyncStatus,
};

pub use tensorseed_verify::file::{
    ArtifactKind, ChainState, DELTA_SCHEMA, Delta, DeltaOp, HEAD_PATH, HEAD_SCHEMA, Head,
    HeadStatus, MAX_ARTIFACT_BYTES, ROOT_SCHEMA, SNAPSHOT_SCHEMA, SignedFile, Snapshot,
    SnapshotRef, ZERO_ROOT, artifact_comment, compute_root, delta_path, snapshot_path, state_hash,
    unix_now,
};

fn chain_err(msg: impl Into<String>) -> CatalogError {
    CatalogError::Chain(msg.into())
}

/// Schema tag of the local state file.
pub const STATE_SCHEMA: &str = "ts.filestate/1";

/// Settings for the file backend.
#[derive(Debug, Clone)]
pub struct FileCatalogConfig {
    /// Logical catalog name (also the directory under `base_url`).
    pub catalog: String,
    /// HTTPS base, for example `https://catalog.tensorseed.dev/`.
    pub base_url: String,
    /// Read mirrors tried after `base_url`, same layout.
    pub mirror_urls: Vec<String>,
    /// Pinned publisher key.
    pub publisher: PublicKey,
    /// Where the verified state and trust anchor are kept. `None` keeps
    /// state in memory only.
    pub state_dir: Option<PathBuf>,
    /// Publisher: write a full snapshot every N revisions (and at revision 1).
    pub snapshot_every: u64,
    /// Publisher: head validity in seconds.
    pub head_ttl_secs: u64,
}

impl FileCatalogConfig {
    /// Config with defaults: no mirrors, no state dir, snapshot every 64
    /// revisions, 7 day head TTL.
    pub fn new(catalog: &str, base_url: &str, publisher: PublicKey) -> Self {
        Self {
            catalog: catalog.into(),
            base_url: base_url.into(),
            mirror_urls: Vec::new(),
            publisher,
            state_dir: None,
            snapshot_every: 64,
            head_ttl_secs: 7 * 24 * 3600,
        }
    }
}

/// Local state file (`<state_dir>/<catalog>/state.json`): the verified
/// entries plus the trust anchor `(revision, root)` and newest snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistedState {
    /// Always [`STATE_SCHEMA`].
    pub schema: String,
    /// Logical catalog name.
    pub catalog: String,
    /// Trust anchor revision; clients never accept anything below it.
    pub revision: u64,
    /// Trust anchor root.
    pub root: String,
    /// Root of `revision - 1`.
    pub prev_root: String,
    /// Newest snapshot named by the accepted head.
    pub snapshot: Option<SnapshotRef>,
    /// Entries sorted by key.
    pub entries: Vec<SignedEntry>,
}

/// Result of [`FileCatalog::sync`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    /// Revision before the sync.
    pub from: u64,
    /// Revision after the sync.
    pub to: u64,
    /// Base URL that served the accepted head.
    pub source: String,
    /// Errors from sources that were skipped.
    pub skipped: Vec<String>,
}

/// Signed static-file catalog backend.
pub struct FileCatalog {
    cfg: FileCatalogConfig,
    signer: Option<SecretKey>,
    state: Mutex<ChainState>,
    last_snapshot: Mutex<Option<SnapshotRef>>,
    staged: Mutex<Vec<DeltaOp>>,
    outbox: Mutex<Vec<SignedFile>>,
    sink: Option<Arc<dyn ObjectSink>>,
    http: reqwest::Client,
    /// When set, sync only accepts a head with exactly this revision and
    /// root (the chain anchor, spec 6.14).
    expected_head: Mutex<Option<(u64, String)>>,
    /// Chain proof uploaded with the next commit, before the head.
    chain_proof: Mutex<Option<Vec<u8>>>,
}

impl std::fmt::Debug for FileCatalog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileCatalog")
            .field("cfg", &self.cfg)
            .field("signer", &self.signer)
            .field("sink", &self.sink)
            .finish_non_exhaustive()
    }
}

/// Reserved chain side keys never enter the file catalog.
fn reject_reserved(key: &str) -> Result<()> {
    if is_reserved_key(key) {
        return Err(CatalogError::Config(format!(
            "{key} is reserved for the chain anchor and cannot be stored in the file catalog"
        )));
    }
    Ok(())
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn normalize_base(b: &str) -> String {
    if b.ends_with('/') {
        b.to_string()
    } else {
        format!("{b}/")
    }
}

impl FileCatalog {
    /// Build a read only (subscriber) backend. Loads and re-verifies the
    /// persisted state when `state_dir` is set.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Config`] for invalid settings, and [`CatalogError::Io`],
    /// [`CatalogError::Json`], or verification errors when the persisted state
    /// cannot be loaded.
    pub fn new(cfg: FileCatalogConfig) -> Result<Self> {
        let mut state = ChainState::empty(&cfg.catalog)?;
        let mut last_snapshot = None;
        if let Some(p) = Self::state_file(&cfg) {
            match std::fs::read(&p) {
                Ok(bytes) => {
                    let (st, snap) = load_state(&cfg, &bytes)
                        .map_err(|e| CatalogError::Chain(format!("{}: {e}", p.display())))?;
                    state = st;
                    last_snapshot = snap;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        let http = reqwest::Client::builder()
            .user_agent(concat!("tensorseed/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(std::time::Duration::from_secs(20))
            .timeout(std::time::Duration::from_secs(300))
            .build()?;
        Ok(Self {
            cfg,
            signer: None,
            state: Mutex::new(state),
            last_snapshot: Mutex::new(last_snapshot),
            staged: Mutex::new(Vec::new()),
            outbox: Mutex::new(Vec::new()),
            sink: None,
            http,
            expected_head: Mutex::new(None),
            chain_proof: Mutex::new(None),
        })
    }

    /// Attach the publisher key. It must match the pinned public key.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Signature`] if `sk` does not match the pinned public
    /// key.
    pub fn with_signer(mut self, sk: SecretKey) -> Result<Self> {
        if sk.public_key() != self.cfg.publisher {
            return Err(CatalogError::Config(
                "publisher secret key does not match pinned catalog.publisher_pubkey".into(),
            ));
        }
        self.signer = Some(sk);
        Ok(self)
    }

    /// Attach the destination for published files. Without a sink, commits
    /// keep their signed deltas in an outbox (see [`Self::take_outbox`]).
    pub fn with_sink(mut self, sink: Arc<dyn ObjectSink>) -> Self {
        self.sink = Some(sink);
        self
    }

    /// The config.
    pub fn config(&self) -> &FileCatalogConfig {
        &self.cfg
    }

    fn state_file(cfg: &FileCatalogConfig) -> Option<PathBuf> {
        cfg.state_dir
            .as_ref()
            .map(|d| d.join(&cfg.catalog).join("state.json"))
    }

    fn check_id(&self, catalog: &CatalogId) -> Result<()> {
        if catalog.0 != self.cfg.catalog {
            return Err(CatalogError::Config(format!(
                "file backend serves catalog {:?}, not {:?}",
                self.cfg.catalog, catalog.0
            )));
        }
        Ok(())
    }

    fn signer(&self) -> Result<&SecretKey> {
        self.signer
            .as_ref()
            .ok_or_else(|| CatalogError::Config("no publisher key loaded".into()))
    }

    /// Snapshot of the verified local state.
    pub fn state(&self) -> ChainState {
        lock(&self.state).clone()
    }

    /// Newest snapshot named by the accepted head.
    pub fn last_snapshot(&self) -> Option<SnapshotRef> {
        lock(&self.last_snapshot).clone()
    }

    /// Signed delta files produced by `commit` without a sink.
    pub fn take_outbox(&self) -> Vec<SignedFile> {
        std::mem::take(&mut *lock(&self.outbox))
    }

    /// Stage already signed entries (for `catalog import`). Every entry is
    /// verified against the pinned key first; signatures are kept as is.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Signature`] if any entry fails verification; nothing is
    /// staged in that case.
    pub fn stage_signed(&self, entries: &[SignedEntry]) -> Result<()> {
        for e in entries {
            reject_reserved(&e.key)?;
            e.verify(&self.cfg.publisher, &self.cfg.catalog)?;
        }
        lock(&self.staged).extend(
            entries
                .iter()
                .cloned()
                .map(|entry| DeltaOp::Upsert { entry }),
        );
        Ok(())
    }

    /// Apply a verified delta fetched from a mirror (subscriber side).
    ///
    /// # Errors
    ///
    /// The verification errors of [`ChainState::apply_delta`].
    pub fn apply_remote_delta(&self, file: &SignedFile) -> Result<()> {
        lock(&self.state).apply_delta(&self.cfg.publisher, file)?;
        self.persist()
    }

    /// The state the staged changes would produce, without committing or
    /// uploading anything: `(next state, staged ops)`. The publisher uses it
    /// to write the chain anchor for the next revision first (spec 6.14).
    ///
    /// # Errors
    ///
    /// [`CatalogError::Backend`] when nothing is staged, and
    /// [`CatalogError::Core`] if encoding fails.
    pub fn preview_staged(&self) -> Result<(ChainState, Vec<DeltaOp>)> {
        let ops = lock(&self.staged).clone();
        if ops.is_empty() {
            return Err(CatalogError::Backend("nothing staged".into()));
        }
        let mut next = self.state();
        for op in &ops {
            match op {
                DeltaOp::Upsert { entry } => {
                    next.entries.insert(entry.key.clone(), entry.clone());
                }
                DeltaOp::Delete { key } => {
                    next.entries.remove(key);
                }
            }
        }
        next.revision += 1;
        next.prev_root = std::mem::take(&mut next.root);
        let sh = state_hash(&next.entries)?;
        next.root = compute_root(&next.catalog, next.revision, &next.prev_root, &sh)?;
        Ok((next, ops))
    }

    /// Only accept a head with exactly this revision and root during sync
    /// (`None` accepts any valid head). Used for the chain check.
    pub fn set_expected_head(&self, expected: Option<(u64, String)>) {
        *lock(&self.expected_head) = expected;
    }

    /// Upload this chain proof with the next commit, before the head.
    pub fn set_chain_proof(&self, body: Option<Vec<u8>>) {
        *lock(&self.chain_proof) = body;
    }

    /// Upload `chain-proof.json` now (mutable, short cache).
    ///
    /// # Errors
    ///
    /// [`CatalogError::Config`] without an upload destination, and any error from
    /// the sink.
    pub async fn upload_chain_proof(&self, body: Vec<u8>) -> Result<()> {
        let key = format!("{}/{CHAIN_PROOF_PATH}", self.cfg.catalog);
        self.sink()?
            .put(&key, body, "application/json", CachePolicy::Mutable)
            .await
    }

    /// Fetch `chain-proof.json` from one source. `Ok(None)` on 404.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Http`] on transport failure and
    /// [`CatalogError::Backend`] for a non success status other than 404.
    pub async fn fetch_chain_proof(&self, base: &str) -> Result<Option<Vec<u8>>> {
        self.fetch(&normalize_base(base), CHAIN_PROOF_PATH).await
    }

    /// Write the verified state and trust anchor to `state_dir` (atomic).
    ///
    /// # Errors
    ///
    /// [`CatalogError::Io`] or [`CatalogError::Json`] if the state cannot be
    /// written.
    pub fn persist(&self) -> Result<()> {
        let Some(path) = Self::state_file(&self.cfg) else {
            return Ok(());
        };
        let st = self.state();
        let doc = PersistedState {
            schema: STATE_SCHEMA.into(),
            catalog: st.catalog.clone(),
            revision: st.revision,
            root: st.root.clone(),
            prev_root: st.prev_root.clone(),
            snapshot: self.last_snapshot(),
            entries: st.entries.values().cloned().collect(),
        };
        let bytes = to_canonical_vec(&doc)?;
        let dir = path
            .parent()
            .ok_or_else(|| CatalogError::Config("bad state path".into()))?;
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join("state.json.tmp");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Base URLs to read from: `base_url` first, then mirrors.
    pub fn sources(&self) -> Vec<String> {
        std::iter::once(&self.cfg.base_url)
            .chain(self.cfg.mirror_urls.iter())
            .filter(|b| !b.is_empty())
            .map(|b| normalize_base(b))
            .collect()
    }

    /// GET `<base><catalog>/<rel>`. `Ok(None)` on 404.
    async fn fetch(&self, base: &str, rel: &str) -> Result<Option<Vec<u8>>> {
        let url = format!("{base}{}/{rel}", self.cfg.catalog);
        let resp = self.http.get(&url).send().await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(CatalogError::Backend(format!(
                "{url}: HTTP {}",
                resp.status()
            )));
        }
        if resp
            .content_length()
            .is_some_and(|n| n > MAX_ARTIFACT_BYTES)
        {
            return Err(CatalogError::Backend(format!("{url}: too large")));
        }
        let bytes = resp.bytes().await?;
        if bytes.len() as u64 > MAX_ARTIFACT_BYTES {
            return Err(CatalogError::Backend(format!("{url}: too large")));
        }
        Ok(Some(bytes.to_vec()))
    }

    async fn fetch_signed(&self, base: &str, rel: &str) -> Result<Option<SignedFile>> {
        let Some(body) = self.fetch(base, rel).await? else {
            return Ok(None);
        };
        let Some(sig) = self.fetch(base, &format!("{rel}.minisig")).await? else {
            return Err(CatalogError::Signature(format!(
                "{base}{}/{rel}: missing .minisig",
                self.cfg.catalog
            )));
        };
        let sig = String::from_utf8(sig)
            .map_err(|_| CatalogError::Signature(format!("{rel}.minisig is not UTF-8")))?;
        Ok(Some(SignedFile { body, sig }))
    }

    /// Fetch `rel` from `preferred` first, then every other source.
    async fn fetch_signed_any(&self, preferred: &str, rel: &str) -> Result<SignedFile> {
        let mut errors = Vec::new();
        let mut order = vec![preferred.to_string()];
        order.extend(self.sources().into_iter().filter(|s| s != preferred));
        for base in order {
            match self.fetch_signed(&base, rel).await {
                Ok(Some(f)) => return Ok(f),
                Ok(None) => errors.push(format!("{base}: {rel} not found")),
                Err(e) => errors.push(format!("{base}: {e}")),
            }
        }
        Err(CatalogError::Backend(format!(
            "could not fetch {rel}: {}",
            errors.join("; ")
        )))
    }

    /// Bring the local state up to the newest valid head, trying `base_url`
    /// and then each mirror. Rollback, stale, unsigned, or unreachable
    /// sources are skipped; a fork (two signed chains) aborts.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Fork`] when two validly signed chains disagree,
    /// [`CatalogError::NotFound`] when no source serves a head at all, and
    /// [`CatalogError::Backend`] when no offered head is usable.
    pub async fn sync(&self) -> Result<SyncReport> {
        self.sync_at(unix_now()).await
    }

    /// [`Self::sync`] with an explicit clock (tests).
    ///
    /// # Errors
    ///
    /// Same as [`Self::sync`].
    pub async fn sync_at(&self, now_unix: u64) -> Result<SyncReport> {
        let from = self.state().revision;
        let mut skipped = Vec::new();
        let mut not_found = 0usize;
        let sources = self.sources();
        if sources.is_empty() {
            return Err(CatalogError::Config(
                "no file.base_url or mirror_urls configured".into(),
            ));
        }
        for base in &sources {
            let head_file = match self.fetch_signed(base, HEAD_PATH).await {
                Ok(Some(f)) => f,
                Ok(None) => {
                    not_found += 1;
                    skipped.push(format!("{base}: no head.json"));
                    continue;
                }
                Err(e) => {
                    skipped.push(format!("{base}: {e}"));
                    continue;
                }
            };
            match self.advance_from(base, &head_file, now_unix).await {
                Ok(to) => {
                    return Ok(SyncReport {
                        from,
                        to,
                        source: base.clone(),
                        skipped,
                    });
                }
                Err(e @ CatalogError::Fork { .. }) => return Err(e),
                Err(e) => skipped.push(format!("{base}: {e}")),
            }
        }
        if not_found == sources.len() {
            return Err(CatalogError::NotFound(format!(
                "no head.json for catalog {:?} at {}",
                self.cfg.catalog,
                sources.join(", ")
            )));
        }
        Err(CatalogError::Backend(format!(
            "no usable catalog source: {}",
            skipped.join("; ")
        )))
    }

    /// Verify `head_file` from `base` and walk the local state forward to it,
    /// fetching deltas (or a snapshot when the gap is too large) from the same
    /// source. Works on a copy and only swaps it in once the whole chain
    /// verified, so a bad source never leaves half applied state. When the
    /// chain check pinned an expected head, any other head is refused.
    /// Returns the revision reached.
    async fn advance_from(&self, base: &str, head_file: &SignedFile, now_unix: u64) -> Result<u64> {
        let pk = &self.cfg.publisher;
        let mut work = self.state();
        let status = work.check_head(pk, head_file, now_unix)?;
        let head: Head = serde_json::from_slice(&head_file.body)?;
        if let Some((rev, root)) = lock(&self.expected_head).clone()
            && (head.revision != rev || head.root != root)
        {
            return Err(chain_err(format!(
                "head revision {} root {} is not the chain anchored revision {rev} root {root}",
                head.revision, head.root
            )));
        }
        if let HeadStatus::Behind { target } = status {
            if work.revision == 0
                && let Some(snap) = &head.snapshot
            {
                if snap.path != snapshot_path(snap.revision) || snap.revision > target {
                    return Err(chain_err("head names an invalid snapshot"));
                }
                let file = self.fetch_signed_any(base, &snap.path).await?;
                if sha256_bytes(&file.body) != snap.sha256 {
                    return Err(chain_err("snapshot sha256 does not match head"));
                }
                work.load_snapshot(pk, &file)?;
            }
            for rev in work.revision + 1..=target {
                let file = self.fetch_signed_any(base, &delta_path(rev)).await?;
                work.apply_delta(pk, &file)?;
            }
            if work.root != head.root {
                return Err(CatalogError::Fork {
                    revision: head.revision,
                });
            }
        }
        let to = work.revision;
        *lock(&self.state) = work;
        *lock(&self.last_snapshot) = head.snapshot.clone();
        self.persist()?;
        Ok(to)
    }

    /// Fetch and check the newest head without applying anything.
    ///
    /// # Errors
    ///
    /// [`CatalogError::NotFound`] when no source serves a head, and the
    /// verification errors of [`ChainState::check_head`].
    pub async fn remote_status(&self) -> Result<SyncStatus> {
        let now = unix_now();
        let local = self.state();
        let mut errors = Vec::new();
        for base in self.sources() {
            match self.fetch_signed(&base, HEAD_PATH).await {
                Ok(Some(f)) => match local
                    .check_head(&self.cfg.publisher, &f, now)
                    .map_err(CatalogError::from)
                {
                    Ok(HeadStatus::UpToDate) => {
                        return Ok(SyncStatus {
                            local_generation: local.revision,
                            target_generation: local.revision,
                        });
                    }
                    Ok(HeadStatus::Behind { target }) => {
                        return Ok(SyncStatus {
                            local_generation: local.revision,
                            target_generation: target,
                        });
                    }
                    Err(e @ CatalogError::Fork { .. }) => return Err(e),
                    Err(e) => errors.push(format!("{base}: {e}")),
                },
                Ok(None) => errors.push(format!("{base}: no head.json")),
                Err(e) => errors.push(format!("{base}: {e}")),
            }
        }
        Err(CatalogError::Backend(format!(
            "no usable catalog source: {}",
            errors.join("; ")
        )))
    }

    /// Publisher: sync with the published catalog first. When no source
    /// serves a head at all (never published, or local only storage that
    /// nothing exposes at `base_url`), continue from the verified local
    /// state. Any other error, including a fork, stops publishing.
    ///
    /// # Errors
    ///
    /// Any sync error except a missing head, including [`CatalogError::Fork`].
    pub async fn sync_for_publish(&self) -> Result<()> {
        match self.sync().await {
            Ok(_) => Ok(()),
            Err(CatalogError::NotFound(m)) => {
                let rev = self.state().revision;
                if rev > 0 {
                    tracing::warn!(
                        revision = rev,
                        "no published head reachable ({m}); continuing from local state"
                    );
                }
                Ok(())
            }
            Err(CatalogError::Backend(m)) if m.starts_with("no usable catalog source") => {
                Err(CatalogError::Backend(format!(
                    "{m}. The publisher reads the current published head from file.base_url \
                 (or file.mirror_urls) before writing, so it never forks the public catalog; \
                 make sure that URL is reachable (serve storage.local_dir there) and try again"
                )))
            }
            Err(e) => Err(e),
        }
    }

    fn sink(&self) -> Result<&Arc<dyn ObjectSink>> {
        self.sink
            .as_ref()
            .ok_or_else(|| CatalogError::Config("no upload destination configured".into()))
    }

    async fn upload(&self, rel: &str, file: &SignedFile, cache: CachePolicy) -> Result<()> {
        let sink = self.sink()?;
        let key = format!("{}/{rel}", self.cfg.catalog);
        sink.put(&key, file.body.clone(), "application/json", cache)
            .await?;
        sink.put(
            &format!("{key}.minisig"),
            file.sig.clone().into_bytes(),
            "text/plain; charset=utf-8",
            cache,
        )
        .await
    }

    /// Publisher: re-sign `head.json` for the current revision with a fresh
    /// expiry and upload it. Run this before `head_ttl_secs` lapses.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Config`] without a publisher key or upload destination, and any signing
    /// or upload error.
    pub async fn resign_head(&self) -> Result<Head> {
        self.resign_head_at(unix_now()).await
    }

    /// [`Self::resign_head`] with an explicit clock (tests).
    ///
    /// # Errors
    ///
    /// Same as [`Self::resign_head`].
    pub async fn resign_head_at(&self, now_unix: u64) -> Result<Head> {
        let sk = self.signer()?;
        let st = self.state();
        if st.revision == 0 {
            return Err(CatalogError::Backend("nothing published yet".into()));
        }
        let snap = self.last_snapshot();
        let expires = now_unix.saturating_add(self.cfg.head_ttl_secs);
        let file = st.build_head(sk, snap, expires)?;
        self.upload(HEAD_PATH, &file, CachePolicy::Mutable).await?;
        Ok(serde_json::from_slice(&file.body)?)
    }

    /// Publisher: write a snapshot of the current revision now, then a head
    /// that points at it.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Config`] without a publisher key or upload destination, and any signing
    /// or upload error.
    pub async fn snapshot_now(&self) -> Result<SnapshotRef> {
        let sk = self.signer()?;
        let st = self.state();
        if st.revision == 0 {
            return Err(CatalogError::Backend("nothing published yet".into()));
        }
        let snap = self.upload_snapshot(sk, &st).await?;
        *lock(&self.last_snapshot) = Some(snap.clone());
        self.resign_head().await?;
        self.persist()?;
        Ok(snap)
    }

    async fn upload_snapshot(&self, sk: &SecretKey, st: &ChainState) -> Result<SnapshotRef> {
        let file = st.build_snapshot(sk)?;
        let rel = snapshot_path(st.revision);
        self.upload(&rel, &file, CachePolicy::Immutable).await?;
        Ok(SnapshotRef {
            revision: st.revision,
            path: rel,
            sha256: sha256_bytes(&file.body),
        })
    }
}

/// Parse a persisted state file and re-verify it from scratch: every entry
/// signature, the state hash, and the root. A tampered state directory is
/// treated like any other untrusted source.
fn load_state(cfg: &FileCatalogConfig, bytes: &[u8]) -> Result<(ChainState, Option<SnapshotRef>)> {
    let doc: PersistedState = serde_json::from_slice(bytes)?;
    if doc.schema != STATE_SCHEMA || doc.catalog != cfg.catalog {
        return Err(chain_err("state file schema or catalog mismatch"));
    }
    let mut entries = BTreeMap::new();
    for e in doc.entries {
        e.verify(&cfg.publisher, &cfg.catalog)?;
        entries.insert(e.key.clone(), e);
    }
    let root = if doc.revision == 0 {
        if !entries.is_empty() {
            return Err(chain_err("state at revision 0 has entries"));
        }
        ZERO_ROOT.to_string()
    } else {
        compute_root(
            &cfg.catalog,
            doc.revision,
            &doc.prev_root,
            &state_hash(&entries)?,
        )?
    };
    if root != doc.root {
        return Err(chain_err("state file entries do not match its root"));
    }
    Ok((
        ChainState {
            catalog: doc.catalog,
            revision: doc.revision,
            root: doc.root,
            prev_root: doc.prev_root,
            entries,
        },
        doc.snapshot,
    ))
}

#[async_trait]
impl CatalogBackend for FileCatalog {
    fn kind(&self) -> &'static str {
        "signed-file"
    }

    async fn create_catalog(&self) -> Result<CatalogId> {
        // No chain step: the catalog exists once revision 1 is uploaded.
        self.signer()?;
        Ok(CatalogId(self.cfg.catalog.clone()))
    }

    async fn get(&self, catalog: &CatalogId, key: &str) -> Result<Option<Vec<u8>>> {
        self.check_id(catalog)?;
        Ok(lock(&self.state)
            .entries
            .get(key)
            .map(|e| e.value.clone().into_bytes()))
    }

    async fn get_signed(&self, catalog: &CatalogId, key: &str) -> Result<Option<SignedEntry>> {
        self.check_id(catalog)?;
        Ok(lock(&self.state).entries.get(key).cloned())
    }

    async fn keys(&self, catalog: &CatalogId, prefix: &str) -> Result<Vec<String>> {
        self.check_id(catalog)?;
        Ok(lock(&self.state)
            .entries
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect())
    }

    async fn stage(&self, catalog: &CatalogId, batch: &[CatalogOp]) -> Result<()> {
        self.check_id(catalog)?;
        let sk = self.signer()?;
        let mut ops = Vec::with_capacity(batch.len());
        for op in batch {
            match op {
                CatalogOp::Upsert { key, .. } | CatalogOp::Delete { key } => reject_reserved(key)?,
            }
            ops.push(match op {
                CatalogOp::Upsert { key, value } => DeltaOp::Upsert {
                    entry: SignedEntry::sign(sk, &self.cfg.catalog, key, value)?,
                },
                CatalogOp::Delete { key } => DeltaOp::Delete { key: key.clone() },
            });
        }
        lock(&self.staged).extend(ops);
        Ok(())
    }

    async fn commit(&self, catalog: &CatalogId) -> Result<PendingCommit> {
        self.check_id(catalog)?;
        let sk = self.signer()?;
        let ops = std::mem::take(&mut *lock(&self.staged));
        if ops.is_empty() {
            return Err(CatalogError::Backend("nothing staged".into()));
        }
        let mut next = self.state();
        let file = next.build_delta(sk, ops)?;
        next.apply_delta(&self.cfg.publisher, &file)?;
        if self.sink.is_some() {
            // Upload order: delta, snapshot (if due), then head, so readers
            // never see a head whose delta is missing. Local state only
            // advances after every upload succeeded.
            self.upload(&delta_path(next.revision), &file, CachePolicy::Immutable)
                .await?;
            let every = self.cfg.snapshot_every.max(1);
            let mut snap = self.last_snapshot();
            if next.revision == 1 || next.revision.is_multiple_of(every) {
                snap = Some(self.upload_snapshot(sk, &next).await?);
            }
            // The chain proof goes up before the head, so a reader that sees
            // the new head finds the matching proof.
            let proof = lock(&self.chain_proof).take();
            if let Some(body) = proof {
                self.upload_chain_proof(body).await?;
            }
            let head = next.build_head(
                sk,
                snap.clone(),
                unix_now().saturating_add(self.cfg.head_ttl_secs),
            )?;
            self.upload(HEAD_PATH, &head, CachePolicy::Mutable).await?;
            *lock(&self.last_snapshot) = snap;
        } else {
            lock(&self.outbox).push(file);
        }
        let root = next.root.clone();
        *lock(&self.state) = next;
        self.persist()?;
        Ok(PendingCommit {
            catalog: catalog.clone(),
            expected_root: Some(root),
            tx_id: None,
            previous_root: None,
        })
    }

    async fn wait_confirmed(&self, pending: &PendingCommit) -> Result<Generation> {
        // A file catalog revision is final once its head is uploaded.
        let state = lock(&self.state);
        match &pending.expected_root {
            Some(r) if *r == state.root => Ok(Generation {
                number: state.revision,
                root: state.root.clone(),
            }),
            _ => Err(chain_err("pending root is not the local head")),
        }
    }

    async fn subscribe(&self, catalog: &CatalogId, _mirror_urls: &[String]) -> Result<()> {
        self.check_id(catalog)?;
        self.sync().await.map(|_| ())
    }

    async fn sync_status(&self, catalog: &CatalogId) -> Result<SyncStatus> {
        self.check_id(catalog)?;
        self.remote_status().await
    }

    async fn add_mirror(&self, _catalog: &CatalogId, _urls: &[String]) -> Result<()> {
        Err(CatalogError::Unsupported(
            "the file backend has no on-chain mirror discovery; list mirrors in [file] mirror_urls",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_key;

    const CAT: &str = "main";
    const DOC_A: &[u8] = br#"{"schema":"ts.catalog/1"}"#;

    fn backend(sk: SecretKey) -> FileCatalog {
        FileCatalog::new(FileCatalogConfig::new(
            CAT,
            "https://catalog.tensorseed.dev/",
            sk.public_key(),
        ))
        .unwrap()
        .with_signer(sk)
        .unwrap()
    }

    #[tokio::test]
    async fn backend_stage_commit_get() {
        let b = backend(test_key(9));
        let id = b.create_catalog().await.unwrap();
        b.stage(
            &id,
            &[CatalogOp::Upsert {
                key: "ts/v1/catalog".into(),
                value: DOC_A.to_vec(),
            }],
        )
        .await
        .unwrap();
        let pending = b.commit(&id).await.unwrap();
        let generation = b.wait_confirmed(&pending).await.unwrap();
        assert_eq!(generation.number, 1);
        assert_eq!(b.get(&id, "ts/v1/catalog").await.unwrap().unwrap(), DOC_A);
        assert_eq!(b.keys(&id, "ts/v1/").await.unwrap(), vec!["ts/v1/catalog"]);
        assert_eq!(b.take_outbox().len(), 1);
        assert!(matches!(
            b.add_mirror(&id, &[]).await,
            Err(CatalogError::Unsupported(_))
        ));
    }

    #[tokio::test]
    async fn preview_matches_commit_and_reserved_keys_are_refused() {
        let sk = test_key(9);
        let b = backend(sk);
        let id = CatalogId(CAT.into());
        b.stage(
            &id,
            &[CatalogOp::Upsert {
                key: "ts/v1/a".into(),
                value: br#"{"a":1}"#.to_vec(),
            }],
        )
        .await
        .unwrap();
        let (next, ops) = b.preview_staged().unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(b.state().revision, 0, "preview does not commit");
        let p = b.commit(&id).await.unwrap();
        let g = b.wait_confirmed(&p).await.unwrap();
        assert_eq!((g.number, g.root), (next.revision, next.root.clone()));
        assert_eq!(b.state(), next);
        assert!(b.preview_staged().is_err(), "nothing staged");
        let reserved = [CatalogOp::Upsert {
            key: crate::anchor::FILE_HEAD_KEY.into(),
            value: b"{}".to_vec(),
        }];
        assert!(b.stage(&id, &reserved).await.is_err());
    }

    #[test]
    fn signer_must_match_pinned_key() {
        let b =
            FileCatalog::new(FileCatalogConfig::new(CAT, "", test_key(1).public_key())).unwrap();
        assert!(b.with_signer(test_key(2)).is_err());
    }
}
