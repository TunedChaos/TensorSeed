// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos LLC

//! Catalog JSON value types (spec section 6.4).
//!
//! Timestamps are RFC 3339 UTC strings. They are kept as strings in this
//! scaffold; a typed timestamp may replace them once a time crate is chosen.

use serde::{Deserialize, Serialize};

use crate::hash::is_lower_hex;
use crate::keys::{RepoType, check_commit};
use crate::{Error, Result};

/// Schema tag for [`CatalogInfo`].
pub const SCHEMA_CATALOG: &str = "ts.catalog/1";
/// Schema tag for [`LatestPointer`].
pub const SCHEMA_LATEST: &str = "ts.latest/1";
/// Schema tag for [`Manifest`].
pub const SCHEMA_MANIFEST: &str = "ts.manifest/1";
/// Schema tag for [`FilePage`].
pub const SCHEMA_FILEPAGE: &str = "ts.filepage/1";
/// Schema tag for [`Tombstone`].
pub const SCHEMA_TOMBSTONE: &str = "ts.tombstone/1";
/// Schema tag for [`IndexEntry`].
pub const SCHEMA_INDEX: &str = "ts.index/1";

/// File extensions treated as pickle based (can execute code on load).
pub const PICKLE_EXTENSIONS: &[&str] = &["bin", "pt", "pth", "pkl", "pickle", "ckpt", "joblib"];

/// Publisher identity inside [`CatalogInfo`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Publisher {
    /// Display name, for example "Tuned Chaos LLC".
    pub display_name: String,
    /// Contact address for takedown and policy questions.
    pub contact: String,
    /// URL of the published mirroring and takedown policy.
    pub policy_url: String,
}

/// `ts/v1/catalog` value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogInfo {
    /// Always [`SCHEMA_CATALOG`].
    pub schema: String,
    /// Human readable catalog name.
    pub name: String,
    /// Publisher identity.
    pub publisher: Publisher,
    /// Base URLs of HTTP webseeds.
    pub webseed_bases: Vec<String>,
    /// BitTorrent tracker announce URLs.
    pub trackers: Vec<String>,
    /// Creation time (RFC 3339 UTC).
    pub created_at: String,
}

/// `.../latest` value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatestPointer {
    /// Always [`SCHEMA_LATEST`].
    pub schema: String,
    /// Newest mirrored HF commit (40 hex).
    pub commit: String,
    /// Key of the manifest for that commit.
    pub manifest_key: String,
    /// Update time (RFC 3339 UTC).
    pub updated_at: String,
}

/// Repository identity inside a manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoInfo {
    /// Model or dataset.
    #[serde(rename = "type")]
    pub repo_type: RepoType,
    /// `org/name`.
    pub id: String,
    /// Upstream URL on the Hugging Face Hub.
    pub hf_url: String,
}

/// License class used by the publishing policy (spec section 11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LicenseClass {
    /// Permissive or open license on the allowlist.
    Open,
    /// Redistribution allowed with attribution handling (Llama style).
    Attribution,
    /// Not redistributable.
    Restricted,
    /// Missing or unrecognized license.
    Unknown,
}

/// License details recorded in a manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicenseInfo {
    /// Normalized SPDX id where one exists.
    pub spdx: Option<String>,
    /// Raw HF license tag.
    pub hf_tag: Option<String>,
    /// Policy class.
    pub class: LicenseClass,
    /// License files mirrored with the repo.
    pub license_files: Vec<String>,
    /// Required attribution text, for attribution class licenses.
    pub attribution: Option<String>,
    /// Whether the upstream repo was gated.
    pub gated_upstream: bool,
}

/// File and byte totals for a revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Totals {
    /// Number of files.
    pub files: u64,
    /// Sum of file sizes in bytes.
    pub bytes: u64,
}

/// One file in a revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Path relative to the repo root, using `/` separators.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// Plain SHA-256 of the whole file (64 lowercase hex). Always present.
    pub sha256: String,
    /// HF LFS oid (SHA-256) if the file is in LFS. Must equal `sha256`.
    pub hf_lfs_oid: Option<String>,
    /// BEP 52 per file Merkle root. None for empty files.
    pub bt_v2_pieces_root: Option<String>,
    /// Detected format, for example "safetensors" or "gguf".
    pub format: Option<String>,
    /// True for pickle based formats.
    pub pickle_risk: bool,
}

impl FileEntry {
    /// Return true if `path` has a pickle based extension.
    pub fn path_has_pickle_risk(path: &str) -> bool {
        path.rsplit_once('.')
            .map(|(_, ext)| {
                let ext = ext.to_ascii_lowercase();
                PICKLE_EXTENSIONS.contains(&ext.as_str())
            })
            .unwrap_or(false)
    }

    /// Validate field rules from spec section 6.4.
    pub fn validate(&self) -> Result<()> {
        if self.path.is_empty()
            || self.path.starts_with('/')
            || self
                .path
                .split('/')
                .any(|c| c.is_empty() || c == "." || c == "..")
        {
            return Err(Error::invalid(
                "file path",
                format!("unsafe path {:?}", self.path),
            ));
        }
        if !is_lower_hex(&self.sha256, 64) {
            return Err(Error::invalid(
                "sha256",
                format!("bad digest for {}", self.path),
            ));
        }
        if let Some(oid) = &self.hf_lfs_oid
            && oid != &self.sha256
        {
            return Err(Error::invalid(
                "hf_lfs_oid",
                format!("does not match sha256 for {}", self.path),
            ));
        }
        if let Some(root) = &self.bt_v2_pieces_root
            && !is_lower_hex(root, 64)
        {
            return Err(Error::invalid(
                "bt_v2_pieces_root",
                format!("bad root for {}", self.path),
            ));
        }
        if self.size == 0 && self.bt_v2_pieces_root.is_some() {
            return Err(Error::invalid(
                "bt_v2_pieces_root",
                "empty files have no pieces root",
            ));
        }
        if self.pickle_risk != Self::path_has_pickle_risk(&self.path) {
            return Err(Error::invalid(
                "pickle_risk",
                format!("flag inconsistent with extension for {}", self.path),
            ));
        }
        Ok(())
    }
}

/// Torrent details for a revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TorrentInfo {
    /// BEP 52 v2 info hash (64 hex).
    pub infohash_v2: String,
    /// v1 info hash for hybrid torrents (40 hex).
    pub infohash_v1: Option<String>,
    /// `magnet:?xt=urn:btmh:1220...` link.
    pub magnet: String,
    /// Torrent name, `{org}__{name}@{commit12}`.
    pub name: String,
    /// Piece length in bytes; power of two, at least 16 KiB.
    pub piece_length: u64,
    /// Where the `.torrent` file can be downloaded.
    pub torrent_file_urls: Vec<String>,
}

impl TorrentInfo {
    /// Torrent name for a repo and commit, per spec section 7.1.
    pub fn name_for(repo_id: &str, commit: &str) -> String {
        let short = commit.get(..12).unwrap_or(commit);
        format!("{}@{short}", repo_id.replace('/', "__"))
    }
}

/// Cold backup location.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupLocation {
    /// Backup kind, for example "b2" or "sia".
    pub kind: String,
    /// Locator URI, for example `b2://bucket/prefix/`.
    pub locator: String,
}

/// `.../rev/{commit}` value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Always [`SCHEMA_MANIFEST`].
    pub schema: String,
    /// Upstream repository.
    pub repo: RepoInfo,
    /// HF commit SHA (40 hex).
    pub hf_commit: String,
    /// Upstream last modified time.
    pub hf_last_modified: Option<String>,
    /// License details.
    pub license: LicenseInfo,
    /// Totals across all files, including paged ones.
    pub totals: Totals,
    /// Files, empty when `file_pages > 0`.
    pub files: Vec<FileEntry>,
    /// Number of [`FilePage`] values.
    pub file_pages: u32,
    /// Torrent details.
    pub torrent: TorrentInfo,
    /// BEP 19 webseed base URLs.
    pub webseeds: Vec<String>,
    /// Cold backups.
    pub backups: Vec<BackupLocation>,
    /// Publish time (RFC 3339 UTC).
    pub published_at: String,
    /// Tool and version that published this manifest.
    pub publisher_tool: String,
}

impl Manifest {
    /// Validate structural rules from spec section 6.4. File pages, if any,
    /// must be validated separately.
    pub fn validate(&self) -> Result<()> {
        if self.schema != SCHEMA_MANIFEST {
            return Err(Error::invalid(
                "schema",
                format!("expected {SCHEMA_MANIFEST}"),
            ));
        }
        check_commit(&self.hf_commit)?;
        crate::keys::RepoRef::new(self.repo.repo_type, self.repo.id.clone())?;
        if self.file_pages > 0 && !self.files.is_empty() {
            return Err(Error::invalid("files", "must be empty when file_pages > 0"));
        }
        for f in &self.files {
            f.validate()?;
        }
        if self.file_pages == 0 {
            let bytes: u64 = self.files.iter().map(|f| f.size).sum();
            if self.totals.files != self.files.len() as u64 || self.totals.bytes != bytes {
                return Err(Error::invalid("totals", "do not match file list"));
            }
        }
        let pl = self.torrent.piece_length;
        if pl < 16 * 1024 || !pl.is_power_of_two() {
            return Err(Error::invalid(
                "piece_length",
                "must be a power of two >= 16 KiB",
            ));
        }
        if !is_lower_hex(&self.torrent.infohash_v2, 64) {
            return Err(Error::invalid("infohash_v2", "expected 64 lowercase hex"));
        }
        if let Some(v1) = &self.torrent.infohash_v1
            && !is_lower_hex(v1, 40)
        {
            return Err(Error::invalid("infohash_v1", "expected 40 lowercase hex"));
        }
        Ok(())
    }

    /// Parse and validate a manifest from JSON bytes.
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        let m: Self = serde_json::from_slice(bytes)?;
        m.validate()?;
        Ok(m)
    }

    /// Canonical JSON bytes for storage in the catalog.
    pub fn to_canonical_json(&self) -> Result<Vec<u8>> {
        crate::canonical::to_canonical_vec(self)
    }
}

/// `.../files/{page:06}` value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilePage {
    /// Always [`SCHEMA_FILEPAGE`].
    pub schema: String,
    /// Zero based page number.
    pub page: u32,
    /// Files in this page.
    pub files: Vec<FileEntry>,
}

/// Reason for a takedown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TombstoneReason {
    /// License problem.
    License,
    /// Legal request.
    Legal,
    /// Publisher decision.
    Publisher,
    /// Security issue in the weights.
    Security,
}

/// `.../tombstone` value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tombstone {
    /// Always [`SCHEMA_TOMBSTONE`].
    pub schema: String,
    /// Reason class.
    pub reason: TombstoneReason,
    /// Free form detail.
    pub detail: String,
    /// When the takedown takes effect (RFC 3339 UTC).
    pub effective_at: String,
    /// Affected commits; empty means all revisions.
    pub revisions: Vec<String>,
}

/// `ts/v1/index/{type}/{org}/{name}` value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEntry {
    /// Always [`SCHEMA_INDEX`].
    pub schema: String,
    /// Newest mirrored commit.
    pub latest_commit: String,
    /// SPDX id of the latest revision's license.
    pub license_spdx: Option<String>,
    /// Total bytes of the latest revision.
    pub total_bytes: u64,
    /// Update time (RFC 3339 UTC).
    pub updated_at: String,
    /// True if a tombstone exists.
    pub taken_down: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const SHA: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn sample() -> Manifest {
        Manifest {
            schema: SCHEMA_MANIFEST.into(),
            repo: RepoInfo {
                repo_type: RepoType::Model,
                id: "org/name".into(),
                hf_url: "https://huggingface.co/org/name".into(),
            },
            hf_commit: COMMIT.into(),
            hf_last_modified: Some("2026-11-01T00:00:00Z".into()),
            license: LicenseInfo {
                spdx: Some("apache-2.0".into()),
                hf_tag: Some("apache-2.0".into()),
                class: LicenseClass::Open,
                license_files: vec!["LICENSE".into()],
                attribution: None,
                gated_upstream: false,
            },
            totals: Totals { files: 1, bytes: 3 },
            files: vec![FileEntry {
                path: "model-00001-of-00004.safetensors".into(),
                size: 3,
                sha256: SHA.into(),
                hf_lfs_oid: Some(SHA.into()),
                bt_v2_pieces_root: Some(SHA.into()),
                format: Some("safetensors".into()),
                pickle_risk: false,
            }],
            file_pages: 0,
            torrent: TorrentInfo {
                infohash_v2: SHA.into(),
                infohash_v1: None,
                magnet: format!("magnet:?xt=urn:btmh:1220{SHA}"),
                name: TorrentInfo::name_for("org/name", COMMIT),
                piece_length: 4 * 1024 * 1024,
                torrent_file_urls: vec![format!("https://models.tensorseed.dev/t/{SHA}.torrent")],
            },
            webseeds: vec![format!("https://models.tensorseed.dev/t/{SHA}/")],
            backups: vec![BackupLocation {
                kind: "b2".into(),
                locator: "b2://bucket/prefix/".into(),
            }],
            published_at: "2026-11-12T00:00:00Z".into(),
            publisher_tool: crate::TOOL_ID.into(),
        }
    }

    #[test]
    fn manifest_round_trip_and_canonical() {
        let m = sample();
        m.validate().unwrap();
        let bytes = m.to_canonical_json().unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(
            text.starts_with(r#"{"backups":"#),
            "keys must be sorted: {text}"
        );
        assert!(text.contains(r#""type":"model""#));
        assert!(text.contains(r#""class":"open""#));
        assert!(!text.contains('\n'));
        let back = Manifest::from_json(&bytes).unwrap();
        assert_eq!(back, m);
        assert_eq!(
            back.to_canonical_json().unwrap(),
            bytes,
            "canonical form is stable"
        );
    }

    #[test]
    fn parses_spec_example_shape() {
        let json = format!(
            r#"{{
  "schema": "ts.manifest/1",
  "repo": {{ "type": "dataset", "id": "org/data", "hf_url": "https://huggingface.co/datasets/org/data" }},
  "hf_commit": "{COMMIT}",
  "hf_last_modified": null,
  "license": {{ "spdx": null, "hf_tag": "llama3.1", "class": "attribution",
               "license_files": ["LICENSE", "NOTICE"], "attribution": "Built with Llama",
               "gated_upstream": false }},
  "totals": {{ "files": 10, "bytes": 1000 }},
  "files": [],
  "file_pages": 2,
  "torrent": {{ "infohash_v2": "{SHA}", "infohash_v1": null, "magnet": "magnet:?xt=urn:btmh:1220{SHA}",
               "name": "org__data@0123456789ab", "piece_length": 16384, "torrent_file_urls": [] }},
  "webseeds": [],
  "backups": [],
  "published_at": "2026-11-12T00:00:00Z",
  "publisher_tool": "tensorseed 0.1.0"
}}"#
        );
        let m = Manifest::from_json(json.as_bytes()).unwrap();
        assert_eq!(m.repo.repo_type, RepoType::Dataset);
        assert_eq!(m.license.class, LicenseClass::Attribution);
        assert_eq!(m.file_pages, 2);
    }

    #[test]
    fn validation_failures() {
        let mut m = sample();
        m.files[0].hf_lfs_oid = Some("0".repeat(64));
        assert!(m.validate().is_err(), "lfs oid mismatch");

        let mut m = sample();
        m.torrent.piece_length = 3 * 16384;
        assert!(m.validate().is_err(), "non power of two piece length");

        let mut m = sample();
        m.files[0].path = "../evil".into();
        assert!(m.validate().is_err(), "path traversal");

        let mut m = sample();
        m.totals.bytes = 4;
        assert!(m.validate().is_err(), "totals mismatch");

        let mut m = sample();
        m.hf_commit = "main".into();
        assert!(m.validate().is_err(), "branch name instead of commit");
    }

    #[test]
    fn pickle_detection() {
        assert!(FileEntry::path_has_pickle_risk("pytorch_model.bin"));
        assert!(FileEntry::path_has_pickle_risk("x/y.CKPT"));
        assert!(!FileEntry::path_has_pickle_risk("model.safetensors"));
        assert!(!FileEntry::path_has_pickle_risk("README"));
        let mut m = sample();
        m.files[0].pickle_risk = true;
        assert!(m.validate().is_err(), "flag must match extension");
    }

    #[test]
    fn other_values_round_trip() {
        let t = Tombstone {
            schema: SCHEMA_TOMBSTONE.into(),
            reason: TombstoneReason::Legal,
            detail: "request #1".into(),
            effective_at: "2026-11-12T00:00:00Z".into(),
            revisions: vec![COMMIT.into()],
        };
        let s = crate::canonical::to_canonical_string(&t).unwrap();
        assert!(s.contains(r#""reason":"legal""#));
        assert_eq!(serde_json::from_str::<Tombstone>(&s).unwrap(), t);

        let i = IndexEntry {
            schema: SCHEMA_INDEX.into(),
            latest_commit: COMMIT.into(),
            license_spdx: Some("mit".into()),
            total_bytes: 42,
            updated_at: "2026-11-12T00:00:00Z".into(),
            taken_down: false,
        };
        let s = serde_json::to_string(&i).unwrap();
        assert_eq!(serde_json::from_str::<IndexEntry>(&s).unwrap(), i);
    }

    #[test]
    fn torrent_name() {
        assert_eq!(
            TorrentInfo::name_for("org/name", COMMIT),
            "org__name@0123456789ab"
        );
    }
}
