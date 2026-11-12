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

use crate::minisign::{PublicKey, SecretKey};
use crate::signed::{SignedEntry, check_catalog_name};
use crate::sink::{CachePolicy, ObjectSink};
use crate::{
    CatalogBackend, CatalogError, CatalogId, CatalogOp, Generation, PendingCommit, Result,
    SyncStatus,
};

/// `prev_root` of revision 1.
pub const ZERO_ROOT: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// Schema tags.
pub const HEAD_SCHEMA: &str = "ts.filehead/1";
/// Delta schema tag.
pub const DELTA_SCHEMA: &str = "ts.filedelta/1";
/// Snapshot schema tag.
pub const SNAPSHOT_SCHEMA: &str = "ts.filesnapshot/1";
/// Root preimage schema tag.
pub const ROOT_SCHEMA: &str = "ts.fileroot/1";

/// Kind of signed file artifact, used in trusted comments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactKind {
    /// `head.json`
    Head,
    /// `deltas/<rev>.json`
    Delta,
    /// `snapshots/<rev>.json`
    Snapshot,
}

impl ArtifactKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Head => "head",
            Self::Delta => "delta",
            Self::Snapshot => "snapshot",
        }
    }
}

/// The exact trusted comment for a file artifact signature.
pub fn artifact_comment(kind: ArtifactKind, catalog: &str, revision: u64, root: &str) -> String {
    format!(
        "ts.file/1 kind={} catalog={catalog} rev={revision} root={root}",
        kind.as_str()
    )
}

/// Pointer from the head to the newest snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotRef {
    /// Revision the snapshot captures.
    pub revision: u64,
    /// Path relative to the catalog directory.
    pub path: String,
    /// SHA-256 of the snapshot file bytes.
    pub sha256: String,
}

/// `head.json`: the newest revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Head {
    /// Always [`HEAD_SCHEMA`].
    pub schema: String,
    /// Logical catalog name.
    pub catalog: String,
    /// Newest revision.
    pub revision: u64,
    /// Root of `revision`.
    pub root: String,
    /// Root of `revision - 1`.
    pub prev_root: String,
    /// Newest snapshot, if any.
    pub snapshot: Option<SnapshotRef>,
    /// Unix seconds after which clients treat the head as stale (freeze
    /// attack defense). Publishers re-sign the head before it expires.
    pub expires_unix: u64,
}

/// One change inside a delta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase", deny_unknown_fields)]
pub enum DeltaOp {
    /// Insert or replace a signed entry.
    Upsert {
        /// The signed entry (carries its own key).
        entry: SignedEntry,
    },
    /// Remove a key from the current state (history keeps it).
    Delete {
        /// Key to remove.
        key: String,
    },
}

/// `deltas/<rev>.json`: changes from `revision - 1` to `revision`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delta {
    /// Always [`DELTA_SCHEMA`].
    pub schema: String,
    /// Logical catalog name.
    pub catalog: String,
    /// Revision this delta produces.
    pub revision: u64,
    /// Root of the previous revision.
    pub prev_root: String,
    /// State hash after applying `ops`.
    pub state_hash: String,
    /// Root of `revision`.
    pub root: String,
    /// Changes, applied in order.
    pub ops: Vec<DeltaOp>,
}

/// `snapshots/<rev>.json`: the full state at `revision`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    /// Always [`SNAPSHOT_SCHEMA`].
    pub schema: String,
    /// Logical catalog name.
    pub catalog: String,
    /// Revision captured.
    pub revision: u64,
    /// Root of the previous revision.
    pub prev_root: String,
    /// State hash of `entries`.
    pub state_hash: String,
    /// Root of `revision`.
    pub root: String,
    /// All entries, sorted by key.
    pub entries: Vec<SignedEntry>,
}

/// A body plus its detached minisign signature, as published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedFile {
    /// Canonical JSON bytes of the artifact.
    pub body: Vec<u8>,
    /// Contents of the `.minisig` file.
    pub sig: String,
}

/// Hash of a set of entries: SHA-256 of the canonical JSON array of
/// `[key, sha256(canonical entry bytes)]` pairs sorted by key.
pub fn state_hash(entries: &BTreeMap<String, SignedEntry>) -> Result<String> {
    let mut pairs = Vec::with_capacity(entries.len());
    for (k, e) in entries {
        pairs.push((k.as_str(), sha256_bytes(&e.to_bytes()?)));
    }
    Ok(sha256_bytes(&to_canonical_vec(&pairs)?))
}

/// Root of a revision.
pub fn compute_root(
    catalog: &str,
    revision: u64,
    prev_root: &str,
    state_hash: &str,
) -> Result<String> {
    let pre = serde_json::json!({
        "schema": ROOT_SCHEMA,
        "catalog": catalog,
        "revision": revision,
        "prev_root": prev_root,
        "state_hash": state_hash,
    });
    Ok(sha256_bytes(&to_canonical_vec(&pre)?))
}

fn chain_err(msg: impl Into<String>) -> CatalogError {
    CatalogError::Chain(msg.into())
}

fn sign_artifact<T: Serialize>(
    sk: &SecretKey,
    body: &T,
    kind: ArtifactKind,
    catalog: &str,
    revision: u64,
    root: &str,
) -> Result<SignedFile> {
    let body = to_canonical_vec(body)?;
    let sig = sk.sign(&body, &artifact_comment(kind, catalog, revision, root))?;
    Ok(SignedFile { body, sig })
}

fn verify_artifact<T: for<'de> Deserialize<'de>>(
    pk: &PublicKey,
    file: &SignedFile,
    kind: ArtifactKind,
    fields: impl Fn(&T) -> (&str, u64, &str),
) -> Result<T> {
    let tc = pk.verify(&file.body, &file.sig)?;
    let parsed: T = serde_json::from_slice(&file.body)?;
    let (catalog, revision, root) = fields(&parsed);
    let expected = artifact_comment(kind, catalog, revision, root);
    if tc != expected {
        return Err(CatalogError::Signature(format!(
            "{} signature context mismatch: got {tc:?}, expected {expected:?}",
            kind.as_str()
        )));
    }
    Ok(parsed)
}

/// What a verified head says relative to local state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeadStatus {
    /// Local state is at the head.
    UpToDate,
    /// The head is ahead; fetch deltas `local + 1 ..= target`.
    Behind {
        /// Head revision.
        target: u64,
    },
}

/// Verified local catalog state for one logical catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainState {
    /// Logical catalog name.
    pub catalog: String,
    /// Highest accepted revision (0 means nothing published yet).
    pub revision: u64,
    /// Root of `revision` ([`ZERO_ROOT`] at revision 0).
    pub root: String,
    /// Root of `revision - 1`.
    pub prev_root: String,
    /// Current entries by key.
    pub entries: BTreeMap<String, SignedEntry>,
}

impl ChainState {
    /// Empty state before revision 1.
    pub fn empty(catalog: &str) -> Result<Self> {
        check_catalog_name(catalog)?;
        Ok(Self {
            catalog: catalog.into(),
            revision: 0,
            root: ZERO_ROOT.into(),
            prev_root: ZERO_ROOT.into(),
            entries: BTreeMap::new(),
        })
    }

    /// Check a signed head against local state. Detects rollback, forks, and
    /// stale heads.
    pub fn check_head(
        &self,
        pk: &PublicKey,
        file: &SignedFile,
        now_unix: u64,
    ) -> Result<HeadStatus> {
        let head: Head = verify_artifact(pk, file, ArtifactKind::Head, |h: &Head| {
            (h.catalog.as_str(), h.revision, h.root.as_str())
        })?;
        if head.schema != HEAD_SCHEMA {
            return Err(chain_err(format!("unknown head schema {:?}", head.schema)));
        }
        if head.catalog != self.catalog {
            return Err(chain_err(format!(
                "head is for catalog {:?}, expected {:?}",
                head.catalog, self.catalog
            )));
        }
        if head.expires_unix <= now_unix {
            return Err(chain_err(format!(
                "head for revision {} expired at {} (possible freeze attack)",
                head.revision, head.expires_unix
            )));
        }
        if head.revision < self.revision {
            return Err(CatalogError::Rollback {
                seen: self.revision,
                offered: head.revision,
            });
        }
        if head.revision == self.revision {
            if head.root != self.root {
                return Err(CatalogError::Fork {
                    revision: head.revision,
                });
            }
            return Ok(HeadStatus::UpToDate);
        }
        Ok(HeadStatus::Behind {
            target: head.revision,
        })
    }

    /// Verify and apply the next delta. Every entry signature, the state
    /// hash, and the root are recomputed.
    pub fn apply_delta(&mut self, pk: &PublicKey, file: &SignedFile) -> Result<()> {
        let delta: Delta = verify_artifact(pk, file, ArtifactKind::Delta, |d: &Delta| {
            (d.catalog.as_str(), d.revision, d.root.as_str())
        })?;
        if delta.schema != DELTA_SCHEMA {
            return Err(chain_err(format!(
                "unknown delta schema {:?}",
                delta.schema
            )));
        }
        if delta.catalog != self.catalog {
            return Err(chain_err("delta is for another catalog"));
        }
        let next = self.revision + 1;
        if delta.revision < next {
            return Err(CatalogError::Rollback {
                seen: self.revision,
                offered: delta.revision,
            });
        }
        if delta.revision > next {
            return Err(chain_err(format!(
                "gap: have revision {}, got delta {}",
                self.revision, delta.revision
            )));
        }
        if delta.prev_root != self.root {
            return Err(CatalogError::Fork {
                revision: delta.revision,
            });
        }
        let mut entries = self.entries.clone();
        for op in &delta.ops {
            match op {
                DeltaOp::Upsert { entry } => {
                    entry.verify(pk, &self.catalog)?;
                    entries.insert(entry.key.clone(), entry.clone());
                }
                DeltaOp::Delete { key } => {
                    entries.remove(key);
                }
            }
        }
        let sh = state_hash(&entries)?;
        if sh != delta.state_hash {
            return Err(chain_err("delta state_hash does not match applied state"));
        }
        let root = compute_root(&self.catalog, delta.revision, &delta.prev_root, &sh)?;
        if root != delta.root {
            return Err(chain_err("delta root does not match recomputed root"));
        }
        self.prev_root = std::mem::replace(&mut self.root, root);
        self.revision = delta.revision;
        self.entries = entries;
        Ok(())
    }

    /// Load a verified snapshot. Callers that already trust a higher revision
    /// must not go backwards; this is checked against `self`.
    pub fn load_snapshot(&mut self, pk: &PublicKey, file: &SignedFile) -> Result<()> {
        let snap: Snapshot = verify_artifact(pk, file, ArtifactKind::Snapshot, |s: &Snapshot| {
            (s.catalog.as_str(), s.revision, s.root.as_str())
        })?;
        if snap.schema != SNAPSHOT_SCHEMA || snap.catalog != self.catalog {
            return Err(chain_err("snapshot schema or catalog mismatch"));
        }
        if snap.revision < self.revision {
            return Err(CatalogError::Rollback {
                seen: self.revision,
                offered: snap.revision,
            });
        }
        if snap.revision == self.revision && snap.root != self.root {
            return Err(CatalogError::Fork {
                revision: snap.revision,
            });
        }
        let mut entries = BTreeMap::new();
        for e in &snap.entries {
            e.verify(pk, &self.catalog)?;
            if entries.insert(e.key.clone(), e.clone()).is_some() {
                return Err(chain_err(format!("duplicate key {} in snapshot", e.key)));
            }
        }
        let sh = state_hash(&entries)?;
        let root = compute_root(&self.catalog, snap.revision, &snap.prev_root, &sh)?;
        if sh != snap.state_hash || root != snap.root {
            return Err(chain_err("snapshot hashes do not match its entries"));
        }
        self.revision = snap.revision;
        self.root = root;
        self.prev_root = snap.prev_root;
        self.entries = entries;
        Ok(())
    }

    /// Publisher side: build and sign the next delta from already signed ops,
    /// without changing `self`.
    pub fn build_delta(&self, sk: &SecretKey, ops: Vec<DeltaOp>) -> Result<SignedFile> {
        let mut entries = self.entries.clone();
        for op in &ops {
            match op {
                DeltaOp::Upsert { entry } => {
                    entries.insert(entry.key.clone(), entry.clone());
                }
                DeltaOp::Delete { key } => {
                    entries.remove(key);
                }
            }
        }
        let revision = self.revision + 1;
        let sh = state_hash(&entries)?;
        let root = compute_root(&self.catalog, revision, &self.root, &sh)?;
        let delta = Delta {
            schema: DELTA_SCHEMA.into(),
            catalog: self.catalog.clone(),
            revision,
            prev_root: self.root.clone(),
            state_hash: sh,
            root: root.clone(),
            ops,
        };
        sign_artifact(
            sk,
            &delta,
            ArtifactKind::Delta,
            &self.catalog,
            revision,
            &root,
        )
    }

    /// Publisher side: sign a head for the current state.
    pub fn build_head(
        &self,
        sk: &SecretKey,
        snapshot: Option<SnapshotRef>,
        expires_unix: u64,
    ) -> Result<SignedFile> {
        let head = Head {
            schema: HEAD_SCHEMA.into(),
            catalog: self.catalog.clone(),
            revision: self.revision,
            root: self.root.clone(),
            prev_root: self.prev_root.clone(),
            snapshot,
            expires_unix,
        };
        sign_artifact(
            sk,
            &head,
            ArtifactKind::Head,
            &self.catalog,
            self.revision,
            &self.root,
        )
    }

    /// Publisher side: sign a full snapshot of the current state.
    pub fn build_snapshot(&self, sk: &SecretKey) -> Result<SignedFile> {
        let snap = Snapshot {
            schema: SNAPSHOT_SCHEMA.into(),
            catalog: self.catalog.clone(),
            revision: self.revision,
            prev_root: self.prev_root.clone(),
            state_hash: state_hash(&self.entries)?,
            root: self.root.clone(),
            entries: self.entries.values().cloned().collect(),
        };
        sign_artifact(
            sk,
            &snap,
            ArtifactKind::Snapshot,
            &self.catalog,
            self.revision,
            &self.root,
        )
    }
}

/// Path of the head file inside a catalog directory.
pub const HEAD_PATH: &str = "head.json";

/// Path of a delta inside a catalog directory.
pub fn delta_path(revision: u64) -> String {
    format!("deltas/{revision:010}.json")
}

/// Path of a snapshot inside a catalog directory.
pub fn snapshot_path(revision: u64) -> String {
    format!("snapshots/{revision:010}.json")
}

/// Largest catalog artifact a client accepts (256 MiB).
pub const MAX_ARTIFACT_BYTES: u64 = 256 << 20;

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

/// Current unix time in seconds.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
        })
    }

    /// Attach the publisher key. It must match the pinned public key.
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
    pub fn stage_signed(&self, entries: &[SignedEntry]) -> Result<()> {
        for e in entries {
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
    pub fn apply_remote_delta(&self, file: &SignedFile) -> Result<()> {
        lock(&self.state).apply_delta(&self.cfg.publisher, file)?;
        self.persist()
    }

    /// Write the verified state and trust anchor to `state_dir` (atomic).
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
    pub async fn sync(&self) -> Result<SyncReport> {
        self.sync_at(unix_now()).await
    }

    /// [`Self::sync`] with an explicit clock (tests).
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

    async fn advance_from(&self, base: &str, head_file: &SignedFile, now_unix: u64) -> Result<u64> {
        let pk = &self.cfg.publisher;
        let mut work = self.state();
        let status = work.check_head(pk, head_file, now_unix)?;
        let head: Head = serde_json::from_slice(&head_file.body)?;
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
    pub async fn remote_status(&self) -> Result<SyncStatus> {
        let now = unix_now();
        let local = self.state();
        let mut errors = Vec::new();
        for base in self.sources() {
            match self.fetch_signed(&base, HEAD_PATH).await {
                Ok(Some(f)) => match local.check_head(&self.cfg.publisher, &f, now) {
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
    pub async fn resign_head(&self) -> Result<Head> {
        self.resign_head_at(unix_now()).await
    }

    /// [`Self::resign_head`] with an explicit clock (tests).
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
    use crate::minisign::tests::test_key;

    const CAT: &str = "main";
    const DOC_A: &[u8] = br#"{"schema":"ts.catalog/1"}"#;
    const DOC_B: &[u8] = br#"{"latest_commit":"abc","schema":"ts.index/1"}"#;
    const FAR_FUTURE: u64 = u64::MAX;

    fn entry(sk: &SecretKey, key: &str, doc: &[u8]) -> DeltaOp {
        DeltaOp::Upsert {
            entry: SignedEntry::sign(sk, CAT, key, doc).unwrap(),
        }
    }

    /// Publisher state after `n` revisions, plus every delta file.
    fn publish(sk: &SecretKey, n: u64) -> (ChainState, Vec<SignedFile>) {
        let pk = sk.public_key();
        let mut st = ChainState::empty(CAT).unwrap();
        let mut files = Vec::new();
        for i in 1..=n {
            let key = format!("ts/v1/index/model/org/m{i}");
            let f = st.build_delta(sk, vec![entry(sk, &key, DOC_B)]).unwrap();
            st.apply_delta(&pk, &f).unwrap();
            files.push(f);
        }
        (st, files)
    }

    #[test]
    fn subscriber_replays_chain_to_publisher_root() {
        let sk = test_key(9);
        let pk = sk.public_key();
        let (publisher, deltas) = publish(&sk, 3);
        let mut sub = ChainState::empty(CAT).unwrap();
        for d in &deltas {
            sub.apply_delta(&pk, d).unwrap();
        }
        assert_eq!(sub, publisher);
        assert_eq!(sub.revision, 3);
        let head = publisher.build_head(&sk, None, FAR_FUTURE).unwrap();
        assert_eq!(sub.check_head(&pk, &head, 0).unwrap(), HeadStatus::UpToDate);
    }

    #[test]
    fn head_ahead_reports_behind() {
        let sk = test_key(9);
        let (publisher, _) = publish(&sk, 2);
        let sub = ChainState::empty(CAT).unwrap();
        let head = publisher.build_head(&sk, None, FAR_FUTURE).unwrap();
        assert_eq!(
            sub.check_head(&sk.public_key(), &head, 0).unwrap(),
            HeadStatus::Behind { target: 2 }
        );
    }

    #[test]
    fn detects_rollback_of_head_and_delta_replay() {
        let sk = test_key(9);
        let pk = sk.public_key();
        let (old, _) = publish(&sk, 1);
        let old_head = old.build_head(&sk, None, FAR_FUTURE).unwrap();
        let (mut sub, deltas) = publish(&sk, 2);
        assert!(matches!(
            sub.check_head(&pk, &old_head, 0),
            Err(CatalogError::Rollback {
                seen: 2,
                offered: 1
            })
        ));
        assert!(matches!(
            sub.apply_delta(&pk, &deltas[0]),
            Err(CatalogError::Rollback { .. })
        ));
    }

    #[test]
    fn detects_fork() {
        let sk = test_key(9);
        let pk = sk.public_key();
        let (mut sub, _) = publish(&sk, 1);
        // Publisher (or a stolen key) signs a different revision 1.
        let base = ChainState::empty(CAT).unwrap();
        let mut forked = base.clone();
        let f = base
            .build_delta(&sk, vec![entry(&sk, "ts/v1/catalog", DOC_A)])
            .unwrap();
        forked.apply_delta(&pk, &f).unwrap();
        let fork_head = forked.build_head(&sk, None, FAR_FUTURE).unwrap();
        assert!(matches!(
            sub.check_head(&pk, &fork_head, 0),
            Err(CatalogError::Fork { revision: 1 })
        ));
        // A revision 2 built on the forked revision 1 does not chain.
        let f2 = forked
            .build_delta(&sk, vec![entry(&sk, "ts/v1/catalog", DOC_B)])
            .unwrap();
        assert!(matches!(
            sub.apply_delta(&pk, &f2),
            Err(CatalogError::Fork { revision: 2 })
        ));
    }

    #[test]
    fn detects_gap_and_expired_head() {
        let sk = test_key(9);
        let pk = sk.public_key();
        let (publisher, deltas) = publish(&sk, 2);
        let mut sub = ChainState::empty(CAT).unwrap();
        assert!(matches!(
            sub.apply_delta(&pk, &deltas[1]),
            Err(CatalogError::Chain(_))
        ));
        let head = publisher.build_head(&sk, None, 100).unwrap();
        assert!(matches!(
            sub.check_head(&pk, &head, 100),
            Err(CatalogError::Chain(_))
        ));
    }

    #[test]
    fn rejects_tampered_delta_and_wrong_key() {
        let sk = test_key(9);
        let pk = sk.public_key();
        let (_, deltas) = publish(&sk, 1);
        let mut tampered = deltas[0].clone();
        let text = String::from_utf8(tampered.body).unwrap();
        tampered.body = text.replace("m1", "m9").into_bytes();
        let mut sub = ChainState::empty(CAT).unwrap();
        assert!(matches!(
            sub.apply_delta(&pk, &tampered),
            Err(CatalogError::Signature(_))
        ));
        let other = test_key(10).public_key();
        assert!(matches!(
            sub.apply_delta(&other, &deltas[0]),
            Err(CatalogError::Signature(_))
        ));
        assert_eq!(sub.revision, 0);
    }

    #[test]
    fn rejects_delta_with_entry_from_other_key() {
        // A delta correctly signed by the publisher must still carry entries
        // signed by the publisher.
        let sk = test_key(9);
        let rogue = test_key(11);
        let st = ChainState::empty(CAT).unwrap();
        let f = st
            .build_delta(&sk, vec![entry(&rogue, "ts/v1/catalog", DOC_A)])
            .unwrap();
        let mut sub = ChainState::empty(CAT).unwrap();
        assert!(matches!(
            sub.apply_delta(&sk.public_key(), &f),
            Err(CatalogError::Signature(_))
        ));
    }

    #[test]
    fn snapshot_round_trip_and_delete() {
        let sk = test_key(9);
        let pk = sk.public_key();
        let (mut publisher, _) = publish(&sk, 2);
        let del = publisher
            .build_delta(
                &sk,
                vec![DeltaOp::Delete {
                    key: "ts/v1/index/model/org/m1".into(),
                }],
            )
            .unwrap();
        publisher.apply_delta(&pk, &del).unwrap();
        assert_eq!(publisher.entries.len(), 1);
        let snap = publisher.build_snapshot(&sk).unwrap();
        let mut sub = ChainState::empty(CAT).unwrap();
        sub.load_snapshot(&pk, &snap).unwrap();
        assert_eq!(sub, publisher);
        // An older snapshot cannot roll a newer client back.
        let (old, _) = publish(&sk, 1);
        let old_snap = old.build_snapshot(&sk).unwrap();
        assert!(matches!(
            sub.load_snapshot(&pk, &old_snap),
            Err(CatalogError::Rollback { .. })
        ));
    }

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

    #[test]
    fn signer_must_match_pinned_key() {
        let b =
            FileCatalog::new(FileCatalogConfig::new(CAT, "", test_key(1).public_key())).unwrap();
        assert!(b.with_signer(test_key(2)).is_err());
    }
}
