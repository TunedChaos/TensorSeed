// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Hugging Face Hub API client for Tensorseed (spec section 5).
//!
//! Status: stub. URL builders and response types are in place; network calls
//! return [`HfError::NotImplemented`] until M0 confirms field names (tree
//! `lfs.oid`, `gated`, `cardData.license`) against live responses.

use serde::Deserialize;
use tensorseed_core::keys::{RepoRef, RepoType};

/// Default Hub endpoint.
pub const DEFAULT_ENDPOINT: &str = "https://huggingface.co";

/// Errors from the HF client.
#[derive(Debug, thiserror::Error)]
pub enum HfError {
    /// HTTP failure.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    /// Feature not implemented yet.
    #[error("not implemented yet: {0}")]
    NotImplemented(&'static str),
}

/// Subset of `GET /api/{models|datasets}/{repo_id}` (fields unverified, M0).
#[derive(Debug, Clone, Deserialize)]
pub struct RepoInfo {
    /// Repo id.
    pub id: String,
    /// Commit SHA of the requested revision.
    pub sha: String,
    /// Gated status: `false`, `"auto"`, or `"manual"` in practice.
    #[serde(default)]
    pub gated: serde_json_value::Gated,
    /// Last modified time.
    #[serde(rename = "lastModified", default)]
    pub last_modified: Option<String>,
}

/// Helper types for loosely typed HF fields.
pub mod serde_json_value {
    use serde::Deserialize;

    /// HF reports `gated` as `false` or a string mode.
    #[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
    #[serde(untagged)]
    pub enum Gated {
        /// Not gated (or a boolean value).
        Flag(bool),
        /// Gated with a mode such as "auto" or "manual".
        Mode(String),
        /// Field missing.
        #[default]
        #[serde(skip)]
        Missing,
    }

    impl Gated {
        /// True if the repo is gated in any mode.
        pub fn is_gated(&self) -> bool {
            matches!(self, Self::Flag(true) | Self::Mode(_))
        }
    }
}

/// LFS pointer info in a tree entry.
#[derive(Debug, Clone, Deserialize)]
pub struct LfsInfo {
    /// SHA-256 of the file content.
    pub oid: String,
    /// File size.
    pub size: u64,
}

/// One entry of `GET /api/{models|datasets}/{repo_id}/tree/{commit}?recursive=true`.
#[derive(Debug, Clone, Deserialize)]
pub struct TreeEntry {
    /// "file" or "directory".
    #[serde(rename = "type")]
    pub kind: String,
    /// Path relative to repo root.
    pub path: String,
    /// Size in bytes.
    #[serde(default)]
    pub size: u64,
    /// LFS info when the file is stored in LFS.
    #[serde(default)]
    pub lfs: Option<LfsInfo>,
}

/// Hub API client.
#[derive(Debug, Clone)]
pub struct HfClient {
    endpoint: String,
    token: Option<String>,
    #[allow(dead_code)]
    http: reqwest::Client,
}

impl HfClient {
    /// Create a client. `token` is read by the caller from the env var named in config.
    pub fn new(endpoint: impl Into<String>, token: Option<String>) -> Self {
        Self {
            endpoint: endpoint.into().trim_end_matches('/').to_string(),
            token,
            http: reqwest::Client::new(),
        }
    }

    /// Whether an auth token is configured.
    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    fn api_base(&self, repo: &RepoRef) -> String {
        let kind = match repo.repo_type {
            RepoType::Model => "models",
            RepoType::Dataset => "datasets",
        };
        format!("{}/api/{kind}/{}", self.endpoint, repo.id)
    }

    /// URL for repo info, optionally at a revision.
    pub fn info_url(&self, repo: &RepoRef, revision: Option<&str>) -> String {
        match revision {
            Some(rev) => format!("{}/revision/{rev}", self.api_base(repo)),
            None => self.api_base(repo),
        }
    }

    /// URL for the recursive file tree at a commit.
    pub fn tree_url(&self, repo: &RepoRef, commit: &str) -> String {
        format!("{}/tree/{commit}?recursive=true", self.api_base(repo))
    }

    /// Fetch repo info. TODO(M1): implement after M0 field confirmation.
    pub async fn repo_info(
        &self,
        _repo: &RepoRef,
        _revision: Option<&str>,
    ) -> Result<RepoInfo, HfError> {
        Err(HfError::NotImplemented("HfClient::repo_info"))
    }

    /// Fetch the file tree. TODO(M1): implement with pagination.
    pub async fn tree(&self, _repo: &RepoRef, _commit: &str) -> Result<Vec<TreeEntry>, HfError> {
        Err(HfError::NotImplemented("HfClient::tree"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        let c = HfClient::new("https://huggingface.co/", None);
        let m = RepoRef::new(RepoType::Model, "org/name").unwrap();
        let d = RepoRef::new(RepoType::Dataset, "org/data").unwrap();
        assert_eq!(
            c.info_url(&m, None),
            "https://huggingface.co/api/models/org/name"
        );
        assert_eq!(
            c.info_url(&d, Some("abc")),
            "https://huggingface.co/api/datasets/org/data/revision/abc"
        );
        assert_eq!(
            c.tree_url(&m, "abc"),
            "https://huggingface.co/api/models/org/name/tree/abc?recursive=true"
        );
    }

    #[test]
    fn parses_tree_and_gated() {
        let entries: Vec<TreeEntry> = serde_json::from_str(
            r#"[{"type":"file","path":"a.safetensors","size":5,"lfs":{"oid":"ab","size":5}},
                {"type":"directory","path":"sub"}]"#,
        )
        .unwrap();
        assert_eq!(entries[0].lfs.as_ref().unwrap().oid, "ab");
        assert!(entries[1].lfs.is_none());

        let info: RepoInfo =
            serde_json::from_str(r#"{"id":"o/n","sha":"abc","gated":"manual"}"#).unwrap();
        assert!(info.gated.is_gated());
        let info: RepoInfo =
            serde_json::from_str(r#"{"id":"o/n","sha":"abc","gated":false}"#).unwrap();
        assert!(!info.gated.is_gated());
    }
}
