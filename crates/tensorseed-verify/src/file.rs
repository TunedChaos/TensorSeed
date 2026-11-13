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

use serde::{Deserialize, Serialize};
use tensorseed_core::canonical::to_canonical_vec;
use tensorseed_core::hash::sha256_bytes;

use crate::minisign::{PublicKey, SecretKey};
use crate::signed::{SignedEntry, check_catalog_name};
use crate::{Error as CatalogError, Result};

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
///
/// # Errors
///
/// [`CatalogError::Core`] if an entry cannot be encoded as canonical JSON.
pub fn state_hash(entries: &BTreeMap<String, SignedEntry>) -> Result<String> {
    let mut pairs = Vec::with_capacity(entries.len());
    for (k, e) in entries {
        pairs.push((k.as_str(), sha256_bytes(&e.to_bytes()?)));
    }
    Ok(sha256_bytes(&to_canonical_vec(&pairs)?))
}

/// Root of a revision.
///
/// # Errors
///
/// [`CatalogError::Core`] if canonical JSON encoding fails.
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    ///
    /// # Errors
    ///
    /// [`CatalogError::Core`] if `catalog` is not a valid catalog name or
    /// encoding fails.
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
    ///
    /// # Errors
    ///
    /// [`CatalogError::Signature`] for a bad signature,
    /// [`CatalogError::Rollback`] for a head older than local state,
    /// [`CatalogError::Fork`] for a different root at an accepted revision, and
    /// [`CatalogError::Chain`] for a malformed or expired head.
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
    ///
    /// # Errors
    ///
    /// [`CatalogError::Signature`] when the delta or any entry is not signed by
    /// `pk`, and [`CatalogError::Chain`] when the delta does not follow the
    /// current revision or its state hash or root does not match.
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
    ///
    /// # Errors
    ///
    /// [`CatalogError::Signature`] for a bad signature, [`CatalogError::Chain`]
    /// when the recomputed root does not match, and [`CatalogError::Rollback`]
    /// when the snapshot is older than `self`.
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
    ///
    /// # Errors
    ///
    /// [`CatalogError::Core`] if encoding fails and [`CatalogError::Signature`]
    /// if signing fails.
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
    ///
    /// # Errors
    ///
    /// [`CatalogError::Core`] if encoding fails and [`CatalogError::Signature`]
    /// if signing fails.
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
    ///
    /// # Errors
    ///
    /// [`CatalogError::Core`] if encoding fails and [`CatalogError::Signature`]
    /// if signing fails.
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

/// Current unix time in seconds.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Error as CatalogError;
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
}
