// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Hugging Face Hub API client for Tensorseed (spec section 5).
//!
//! Endpoints used (field names confirmed against live responses on
//! 2026-11-12):
//!
//! - `GET /api/{models|datasets}/{repo_id}[/revision/{rev}]`: `sha`, `gated`
//!   (`false`, `"auto"`, or `"manual"`), `lastModified`, `cardData.license`
//!   (a string or a list), `tags` (`license:<id>`).
//! - `GET /api/{models|datasets}/{repo_id}/tree/{commit}?recursive=true`:
//!   entries with `type`, `path`, `size`, `oid` (git blob id) and, for LFS
//!   files, `lfs.oid` (SHA-256) and `lfs.size`. Paginated with a
//!   `Link: <...>; rel="next"` header (1000 entries per page).
//! - `GET /{repo_id}/resolve/{commit}/{path}` (datasets: `/datasets/...`):
//!   file bytes, usually through a redirect to a CDN.
//! - `GET /api/{models|datasets}?author=..&filter=..&sort=downloads`: repo
//!   listings for harvest discovery (`id`, `downloads`, `tags`, `gated`),
//!   paginated the same way.
//!
//! HTTP 429 and 5xx responses are retried with backoff, honoring
//! `Retry-After`, so harvest stays polite under the Hub's rate limits.
//!
//! The token (env `HF_TOKEN` by default) is only ever sent to the configured
//! endpoint's origin. Downloads are done by `tensorseed-fetch`, which applies
//! the same rule.
//!
//! Why not the `hf-hub` crate: version 1.0 pulls in `hf-xet` (over 550
//! crates in our dependency graph), and its API is built around the Hugging
//! Face cache layout. Tensorseed needs the raw tree (with LFS SHA-256 and git
//! blob ids), its own output layout, streaming verification, and resumable
//! jobs, which take only a few small calls over `reqwest`. Xet stored files
//! are still served by the plain resolve endpoint.
//!
//! # Where this fits
//!
//! Only publishers (`tensorseed harvest`, `mirror`, `pull --from-hf`) talk
//! to the Hub. They turn a repo's tree into a signed manifest; [`policy`]
//! decides whether a revision may be mirrored at all (license and gating).
//! Clients of a catalog never need the Hub, which is the point: the
//! content stays available if the Hub copy is lost.
//!
//! # Examples
//!
//! Look up a repo's files at a commit (needs network):
//!
//! ```no_run
//! use tensorseed_core::keys::{RepoRef, RepoType};
//! use tensorseed_hf::{DEFAULT_ENDPOINT, HfClient};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let hf = HfClient::from_env(DEFAULT_ENDPOINT, "HF_TOKEN")?;
//! let repo = RepoRef::new(RepoType::Model, "org/name")?;
//! let info = hf.repo_info(&repo, None).await?;
//! for f in hf.files(&repo, &info.sha).await? {
//!     println!("{} {} bytes", f.path, f.content_size());
//! }
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

pub mod policy;

use std::time::Duration;

use serde::Deserialize;
use tensorseed_core::hash::ExpectedDigest;
use tensorseed_core::keys::{RepoRef, RepoType};
use tensorseed_core::manifest::encode_path;

/// Default Hub endpoint.
pub const DEFAULT_ENDPOINT: &str = "https://huggingface.co";

/// Maximum number of tree pages followed (1000 entries each).
pub const MAX_TREE_PAGES: usize = 10_000;

/// User agent for every request.
pub const USER_AGENT: &str = concat!("tensorseed/", env!("CARGO_PKG_VERSION"));

/// Errors from the HF client.
#[derive(Debug, thiserror::Error)]
pub enum HfError {
    /// HTTP transport failure.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    /// The repo does not exist, is private, or is gated and needs a token.
    #[error(
        "{url}: not found or not accessible (HTTP {status}); private or gated repos need HF_TOKEN"
    )]
    NotAccessible {
        /// Requested URL.
        url: String,
        /// HTTP status.
        status: u16,
    },
    /// Other non-success status.
    #[error("{url}: HTTP {status}: {message}")]
    Status {
        /// Requested URL.
        url: String,
        /// HTTP status.
        status: u16,
        /// Error text from the Hub, if any.
        message: String,
    },
    /// Response did not have the expected shape.
    #[error("invalid Hub response: {0}")]
    Invalid(String),
    /// Bad argument.
    #[error("{0}")]
    Arg(String),
}

/// Result alias.
pub type Result<T, E = HfError> = std::result::Result<T, E>;

/// HF reports `gated` as `false` or a string mode.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Gated {
    /// Boolean form (normally `false`).
    Flag(bool),
    /// Gated with a mode such as `"auto"` or `"manual"`.
    Mode(String),
    /// Field missing.
    #[default]
    Missing,
}

impl Gated {
    /// True if the repo is gated in any mode.
    pub fn is_gated(&self) -> bool {
        matches!(self, Self::Flag(true) | Self::Mode(_))
    }
}

/// `cardData.license`: a string or a list of strings.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum LicenseField {
    /// Single id.
    One(String),
    /// Several ids.
    Many(Vec<String>),
}

/// Subset of the model card metadata.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct CardData {
    /// License id(s) from the card front matter.
    #[serde(default)]
    pub license: Option<LicenseField>,
}

/// Subset of `GET /api/{models|datasets}/{repo_id}`.
#[derive(Debug, Clone, Deserialize)]
pub struct RepoInfo {
    /// Repo id.
    pub id: String,
    /// Commit SHA of the requested revision.
    pub sha: String,
    /// Gated status.
    #[serde(default)]
    pub gated: Gated,
    /// Private repo.
    #[serde(default)]
    pub private: bool,
    /// Disabled by the Hub.
    #[serde(default)]
    pub disabled: bool,
    /// Last modified time.
    #[serde(rename = "lastModified", default)]
    pub last_modified: Option<String>,
    /// Card metadata.
    #[serde(rename = "cardData", default)]
    pub card_data: Option<CardData>,
    /// Tags, including `license:<id>`.
    #[serde(default)]
    pub tags: Vec<String>,
}

impl RepoInfo {
    /// License ids, lowercased: from `cardData.license`, else from
    /// `license:<id>` tags.
    pub fn licenses(&self) -> Vec<String> {
        let from_card = match self.card_data.as_ref().and_then(|c| c.license.as_ref()) {
            Some(LicenseField::One(s)) => vec![s.clone()],
            Some(LicenseField::Many(v)) => v.clone(),
            None => Vec::new(),
        };
        let raw = if from_card.is_empty() {
            self.tags
                .iter()
                .filter_map(|t| t.strip_prefix("license:").map(str::to_owned))
                .collect()
        } else {
            from_card
        };
        raw.into_iter()
            .map(|s| s.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

/// LFS pointer info in a tree entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LfsInfo {
    /// SHA-256 of the file content.
    pub oid: String,
    /// File size.
    pub size: u64,
}

/// One entry of the recursive tree listing.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct TreeEntry {
    /// `"file"` or `"directory"`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Path relative to repo root.
    pub path: String,
    /// Size in bytes.
    #[serde(default)]
    pub size: u64,
    /// Git object id (blob SHA-1 for files).
    #[serde(default)]
    pub oid: Option<String>,
    /// LFS info when the file is stored in LFS.
    #[serde(default)]
    pub lfs: Option<LfsInfo>,
}

impl TreeEntry {
    /// True for files.
    pub fn is_file(&self) -> bool {
        self.kind == "file"
    }

    /// What the content must hash to: LFS SHA-256 when present, otherwise
    /// the git blob SHA-1.
    ///
    /// # Errors
    ///
    /// [`HfError::Invalid`] when the entry has neither a valid LFS SHA-256 nor
    /// a git blob id.
    pub fn expected_digest(&self) -> Result<ExpectedDigest> {
        let d = match (&self.lfs, &self.oid) {
            (Some(lfs), _) => ExpectedDigest::Sha256(lfs.oid.clone()),
            (None, Some(oid)) => ExpectedDigest::GitBlobSha1(oid.clone()),
            (None, None) => {
                return Err(HfError::Invalid(format!("no oid for {}", self.path)));
            }
        };
        d.normalized()
            .map_err(|e| HfError::Invalid(format!("{}: {e}", self.path)))
    }

    /// Content size (LFS size when present).
    pub fn content_size(&self) -> u64 {
        self.lfs.as_ref().map_or(self.size, |l| l.size)
    }
}

/// Hub API client.
#[derive(Debug, Clone)]
pub struct HfClient {
    endpoint: reqwest::Url,
    token: Option<String>,
    http: reqwest::Client,
    retries: u32,
    retry_base: Duration,
}

/// Longest wait honored from a `Retry-After` header.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(300);

/// One entry of a repo listing (`GET /api/models?author=...`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RepoSummary {
    /// Repo id.
    pub id: String,
    /// Download count (recent window, as reported by the Hub).
    #[serde(default)]
    pub downloads: u64,
    /// Tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Gated status.
    #[serde(default)]
    pub gated: Gated,
    /// Private repo.
    #[serde(default)]
    pub private: bool,
    /// Last modified time, if listed.
    #[serde(rename = "lastModified", default)]
    pub last_modified: Option<String>,
}

/// Hub listing page size limit.
pub const LIST_PAGE_LIMIT: usize = 1000;

fn status_error(url: &str, status: reqwest::StatusCode, body: &str) -> HfError {
    if matches!(status.as_u16(), 401 | 403 | 404) {
        return HfError::NotAccessible {
            url: url.into(),
            status: status.as_u16(),
        };
    }
    let message = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned))
        .unwrap_or_else(|| body.chars().take(200).collect());
    HfError::Status {
        url: url.into(),
        status: status.as_u16(),
        message,
    }
}

/// Seconds from a `Retry-After` header (delta seconds form), capped.
fn retry_after(h: &reqwest::header::HeaderMap) -> Option<Duration> {
    h.get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(|s| Duration::from_secs(s).min(MAX_RETRY_AFTER))
}

/// Parse the `rel="next"` target out of a `Link` header.
pub fn next_link(header: &str) -> Option<String> {
    header.split(',').find_map(|part| {
        let mut pieces = part.split(';');
        let target = pieces.next()?.trim();
        let is_next = pieces.any(|p| {
            let p = p.trim();
            p == "rel=\"next\"" || p == "rel=next"
        });
        if is_next {
            target
                .strip_prefix('<')
                .and_then(|t| t.strip_suffix('>'))
                .map(str::to_owned)
        } else {
            None
        }
    })
}

impl HfClient {
    /// Create a client. `token` is read by the caller from the env var named
    /// in config (`huggingface.token_env`, default `HF_TOKEN`).
    ///
    /// # Errors
    ///
    /// [`HfError::Arg`] for an invalid endpoint URL and [`HfError::Http`] if
    /// the HTTP client cannot be built.
    pub fn new(endpoint: &str, token: Option<String>) -> Result<Self> {
        let mut endpoint = reqwest::Url::parse(endpoint)
            .map_err(|e| HfError::Arg(format!("bad Hub endpoint {endpoint:?}: {e}")))?;
        if !endpoint.path().ends_with('/') {
            let p = format!("{}/", endpoint.path());
            endpoint.set_path(&p);
        }
        let http = reqwest::Client::builder().user_agent(USER_AGENT).build()?;
        Ok(Self {
            endpoint,
            token: token.filter(|t| !t.is_empty()),
            http,
            retries: 4,
            retry_base: Duration::from_secs(1),
        })
    }

    /// Retry policy for rate limiting (HTTP 429) and server errors (5xx):
    /// up to `retries` retries, waiting `Retry-After` when the Hub sends it
    /// (capped at [`MAX_RETRY_AFTER`]) and `base * 2^attempt` otherwise.
    pub fn with_retry(mut self, retries: u32, base: Duration) -> Self {
        self.retries = retries;
        self.retry_base = base;
        self
    }

    /// Create a client reading the token from env var `token_env`.
    ///
    /// # Errors
    ///
    /// Same as [`Self::new`].
    pub fn from_env(endpoint: &str, token_env: &str) -> Result<Self> {
        Self::new(endpoint, std::env::var(token_env).ok())
    }

    /// Whether an auth token is configured.
    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }

    /// `GET /api/whoami-v2`. Returns the username. The token is never
    /// included in the result or in error messages.
    ///
    /// # Errors
    ///
    /// [`HfError::Arg`] when no token is set, [`HfError::NotAccessible`]
    /// when the Hub rejects the token, and the usual HTTP errors.
    pub async fn whoami(&self) -> Result<String> {
        if self.token.is_none() {
            return Err(HfError::Arg("no Hugging Face token configured".into()));
        }
        let url = format!("{}/api/whoami-v2", self.base());
        let v: serde_json::Value = self.get(&url).await?.json().await?;
        v.get("name")
            .and_then(|n| n.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| HfError::Invalid("whoami-v2 response has no username".into()))
    }

    /// The token, for callers that download from the same origin.
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// The endpoint URL.
    pub fn endpoint(&self) -> &reqwest::Url {
        &self.endpoint
    }

    /// True if `url` has the endpoint's origin (the only place the token goes).
    pub fn is_endpoint_origin(&self, url: &str) -> bool {
        reqwest::Url::parse(url)
            .map(|u| u.origin() == self.endpoint.origin())
            .unwrap_or(false)
    }

    fn base(&self) -> String {
        self.endpoint.as_str().trim_end_matches('/').to_string()
    }

    fn api_base(&self, repo: &RepoRef) -> String {
        let kind = match repo.repo_type {
            RepoType::Model => "models",
            RepoType::Dataset => "datasets",
        };
        format!("{}/api/{kind}/{}", self.base(), repo.id)
    }

    /// URL for repo info, optionally at a revision (branch, tag, or commit).
    pub fn info_url(&self, repo: &RepoRef, revision: Option<&str>) -> String {
        match revision {
            Some(rev) => format!("{}/revision/{}", self.api_base(repo), encode_rev(rev)),
            None => self.api_base(repo),
        }
    }

    /// URL for the recursive file tree at a commit.
    pub fn tree_url(&self, repo: &RepoRef, commit: &str) -> String {
        format!(
            "{}/tree/{}?recursive=true",
            self.api_base(repo),
            encode_rev(commit)
        )
    }

    /// Download URL for one file at a revision.
    pub fn resolve_url(&self, repo: &RepoRef, revision: &str, path: &str) -> String {
        let prefix = match repo.repo_type {
            RepoType::Model => String::new(),
            RepoType::Dataset => "datasets/".into(),
        };
        format!(
            "{}/{prefix}{}/resolve/{}/{}",
            self.base(),
            repo.id,
            encode_rev(revision),
            encode_path(path)
        )
    }

    /// GET a Hub URL with the token, retrying 429 and 5xx responses. Refuses
    /// any URL outside the configured endpoint's origin so the token never
    /// leaks to another host.
    async fn get(&self, url: &str) -> Result<reqwest::Response> {
        if !self.is_endpoint_origin(url) {
            return Err(HfError::Invalid(format!(
                "refusing to follow {url}: not the Hub endpoint origin"
            )));
        }
        let mut attempt = 0u32;
        loop {
            let mut req = self.http.get(url);
            if let Some(t) = &self.token {
                req = req.bearer_auth(t);
            }
            let resp = req.send().await;
            let retry_wait = match &resp {
                Ok(r) if r.status().as_u16() == 429 || r.status().is_server_error() => Some(
                    retry_after(r.headers())
                        .unwrap_or_else(|| self.retry_base.saturating_mul(1 << attempt.min(16))),
                ),
                Ok(_) => None,
                Err(e) if e.is_timeout() || e.is_connect() => {
                    Some(self.retry_base.saturating_mul(1 << attempt.min(16)))
                }
                Err(_) => None,
            };
            if let Some(wait) = retry_wait
                && attempt < self.retries
            {
                attempt += 1;
                tracing::warn!(url, attempt, ?wait, "Hub busy or rate limited; backing off");
                tokio::time::sleep(wait).await;
                continue;
            }
            let resp = resp?;
            let status = resp.status();
            if status.is_success() {
                return Ok(resp);
            }
            let body = resp.text().await.unwrap_or_default();
            return Err(status_error(url, status, &body));
        }
    }

    /// List repos of a type by author, optionally requiring tags, sorted by
    /// downloads (highest first), up to `limit` entries.
    ///
    /// # Errors
    ///
    /// [`HfError::Http`] on transport failure, [`HfError::Status`] for an error
    /// response, and [`HfError::Invalid`] for an unexpected body.
    pub async fn list_repos(
        &self,
        repo_type: RepoType,
        author: &str,
        tags: &[String],
        limit: usize,
    ) -> Result<Vec<RepoSummary>> {
        let kind = match repo_type {
            RepoType::Model => "models",
            RepoType::Dataset => "datasets",
        };
        let mut url = reqwest::Url::parse(&format!("{}/api/{kind}", self.base()))
            .map_err(|e| HfError::Arg(e.to_string()))?;
        {
            let mut q = url.query_pairs_mut();
            q.append_pair("author", author);
            for t in tags {
                q.append_pair("filter", t);
            }
            q.append_pair("sort", "downloads");
            q.append_pair("direction", "-1");
            q.append_pair("limit", &limit.clamp(1, LIST_PAGE_LIMIT).to_string());
        }
        let mut url = url.to_string();
        let mut out: Vec<RepoSummary> = Vec::new();
        for _ in 0..MAX_TREE_PAGES {
            let resp = self.get(&url).await?;
            let next = resp
                .headers()
                .get(reqwest::header::LINK)
                .and_then(|v| v.to_str().ok())
                .and_then(next_link);
            let page: Vec<RepoSummary> = resp.json().await?;
            out.extend(page);
            match next {
                Some(n) if out.len() < limit => url = n,
                _ => break,
            }
        }
        out.truncate(limit);
        Ok(out)
    }

    /// Fetch repo info.
    ///
    /// # Errors
    ///
    /// [`HfError::NotAccessible`] for a missing, private, or gated repo,
    /// [`HfError::Status`] for other error responses, and [`HfError::Http`] or
    /// [`HfError::Invalid`] for transport or shape problems.
    pub async fn repo_info(&self, repo: &RepoRef, revision: Option<&str>) -> Result<RepoInfo> {
        let url = self.info_url(repo, revision);
        let info: RepoInfo = self.get(&url).await?.json().await?;
        if !tensorseed_core::hash::is_lower_hex(&info.sha, 40) {
            return Err(HfError::Invalid(format!(
                "repo {} returned commit {:?}",
                repo.id, info.sha
            )));
        }
        Ok(info)
    }

    /// Fetch the full recursive tree at `commit`, following pagination.
    ///
    /// # Errors
    ///
    /// The same errors as [`Self::repo_info`], plus [`HfError::Arg`] for an
    /// invalid commit.
    pub async fn tree(&self, repo: &RepoRef, commit: &str) -> Result<Vec<TreeEntry>> {
        let mut url = self.tree_url(repo, commit);
        let mut out = Vec::new();
        for _ in 0..MAX_TREE_PAGES {
            let resp = self.get(&url).await?;
            let next = resp
                .headers()
                .get(reqwest::header::LINK)
                .and_then(|v| v.to_str().ok())
                .and_then(next_link);
            let page: Vec<TreeEntry> = resp.json().await?;
            out.extend(page);
            match next {
                Some(n) => url = n,
                None => return Ok(out),
            }
        }
        Err(HfError::Invalid(format!(
            "tree for {} has more than {MAX_TREE_PAGES} pages",
            repo.id
        )))
    }

    /// Files only (no directories), sorted by path, with safe paths.
    ///
    /// # Errors
    ///
    /// The errors of [`Self::tree`], plus [`HfError::Invalid`] when a path is
    /// unsafe.
    pub async fn files(&self, repo: &RepoRef, commit: &str) -> Result<Vec<TreeEntry>> {
        let mut files: Vec<TreeEntry> = self
            .tree(repo, commit)
            .await?
            .into_iter()
            .filter(TreeEntry::is_file)
            .collect();
        for f in &files {
            tensorseed_core::verify::safe_join(std::path::Path::new("."), &f.path)
                .map_err(|e| HfError::Invalid(format!("unsafe path from Hub: {e}")))?;
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(files)
    }
}

/// Percent encode a revision as one path segment (`refs/pr/1` becomes
/// `refs%2Fpr%2F1`).
pub fn encode_rev(rev: &str) -> String {
    encode_path(rev).replace('/', "%2F")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const C: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn urls() {
        let c = HfClient::new("https://huggingface.co/", None).unwrap();
        let m = RepoRef::new(RepoType::Model, "org/name").unwrap();
        let d = RepoRef::new(RepoType::Dataset, "org/data").unwrap();
        assert_eq!(
            c.info_url(&m, None),
            "https://huggingface.co/api/models/org/name"
        );
        assert_eq!(
            c.info_url(&d, Some("refs/pr/1")),
            "https://huggingface.co/api/datasets/org/data/revision/refs%2Fpr%2F1"
        );
        assert_eq!(
            c.tree_url(&m, "abc"),
            "https://huggingface.co/api/models/org/name/tree/abc?recursive=true"
        );
        assert_eq!(
            c.resolve_url(&m, C, "sub/a b.json"),
            format!("https://huggingface.co/org/name/resolve/{C}/sub/a%20b.json")
        );
        assert_eq!(
            c.resolve_url(&d, C, "x.parquet"),
            format!("https://huggingface.co/datasets/org/data/resolve/{C}/x.parquet")
        );
        assert!(c.is_endpoint_origin("https://huggingface.co/x"));
        assert!(!c.is_endpoint_origin("https://cdn.example/x"));
    }

    #[test]
    fn parses_live_shapes() {
        let entries: Vec<TreeEntry> = serde_json::from_str(
            r#"[{"type":"file","oid":"cdebb9016e0099550c661ad5d7b4b0db174d2da7","size":453864,
                  "lfs":{"oid":"8111d5afb0715dbf5a31396d31432cb56370ba23f6650a035ea0fc8a20b4e500","size":453864,"pointerSize":131},
                  "xetHash":"f8ac","path":"model.safetensors"},
                {"type":"file","oid":"4ff64fe5192d88c0b5dbfc578c775c0ce05dd7d0","size":807,"path":"config.json"},
                {"type":"directory","oid":"aa","size":0,"path":"sub"}]"#,
        )
        .unwrap();
        assert!(matches!(
            entries[0].expected_digest().unwrap(),
            ExpectedDigest::Sha256(_)
        ));
        assert!(matches!(
            entries[1].expected_digest().unwrap(),
            ExpectedDigest::GitBlobSha1(_)
        ));
        assert!(!entries[2].is_file());

        let info: RepoInfo = serde_json::from_str(
            r#"{"id":"o/n","sha":"abc","gated":"manual","cardData":{"license":"Llama3.2"},"tags":["license:other"]}"#,
        )
        .unwrap();
        assert!(info.gated.is_gated());
        assert_eq!(info.licenses(), vec!["llama3.2"]);
        let info: RepoInfo = serde_json::from_str(
            r#"{"id":"o/n","sha":"abc","gated":false,"cardData":{"license":["other"]}}"#,
        )
        .unwrap();
        assert!(!info.gated.is_gated());
        assert_eq!(info.licenses(), vec!["other"]);
        let info: RepoInfo =
            serde_json::from_str(r#"{"id":"o/n","sha":"abc","tags":["license:mit","x"]}"#).unwrap();
        assert_eq!(info.licenses(), vec!["mit"]);
        assert_eq!(info.gated, Gated::Missing);
    }

    #[test]
    fn link_header() {
        let h = r#"<https://huggingface.co/api/x?cursor=abc%3D>; rel="next""#;
        assert_eq!(
            next_link(h).unwrap(),
            "https://huggingface.co/api/x?cursor=abc%3D"
        );
        assert_eq!(next_link(r#"<https://a/b>; rel="prev""#), None);
    }

    #[tokio::test]
    async fn repo_info_and_paginated_tree_over_http() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/models/org/name/revision/main"))
            .and(header("authorization", "Bearer secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "org/name", "sha": C, "gated": false,
                "cardData": {"license": "apache-2.0"}, "lastModified": "2026-01-01T00:00:00.000Z"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let next = format!(
            "{}/api/models/org/name/tree/{C}?recursive=true&cursor=p2",
            server.uri()
        );
        Mock::given(method("GET"))
            .and(path(format!("/api/models/org/name/tree/{C}")))
            .and(query_param("cursor", "p2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"type": "file", "oid": "4ff64fe5192d88c0b5dbfc578c775c0ce05dd7d0", "size": 2, "path": "b.json"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/api/models/org/name/tree/{C}")))
            .and(query_param("recursive", "true"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("link", format!("<{next}>; rel=\"next\"").as_str())
                    .set_body_json(serde_json::json!([
                        {"type": "directory", "oid": "aa", "size": 0, "path": "sub"},
                        {"type": "file", "oid": "4ff64fe5192d88c0b5dbfc578c775c0ce05dd7d0", "size": 1, "path": "a.json"}
                    ])),
            )
            .mount(&server)
            .await;

        let c = HfClient::new(&server.uri(), Some("secret".into())).unwrap();
        let repo = RepoRef::new(RepoType::Model, "org/name").unwrap();
        let info = c.repo_info(&repo, Some("main")).await.unwrap();
        assert_eq!(info.sha, C);
        assert_eq!(info.licenses(), vec!["apache-2.0"]);
        let files = c.files(&repo, C).await.unwrap();
        assert_eq!(
            files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            vec!["a.json", "b.json"]
        );
    }

    #[tokio::test]
    async fn errors_and_foreign_pagination() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/models/org/missing"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_string(r#"{"error":"Invalid username or password."}"#),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/models/org/broken"))
            .respond_with(ResponseTemplate::new(500).set_body_string(r#"{"error":"boom"}"#))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/api/models/org/evil/tree/{C}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("link", "<https://attacker.example/steal>; rel=\"next\"")
                    .set_body_json(serde_json::json!([])),
            )
            .mount(&server)
            .await;
        let c = HfClient::new(&server.uri(), Some("secret".into()))
            .unwrap()
            .with_retry(2, Duration::from_millis(1));
        let r = |id: &str| RepoRef::new(RepoType::Model, id).unwrap();
        let e = c.repo_info(&r("org/missing"), None).await.unwrap_err();
        assert!(
            matches!(e, HfError::NotAccessible { status: 401, .. }),
            "{e}"
        );
        let e = c.repo_info(&r("org/broken"), None).await.unwrap_err();
        assert!(e.to_string().contains("boom"), "{e}");
        let e = c.tree(&r("org/evil"), C).await.unwrap_err();
        assert!(e.to_string().contains("not the Hub endpoint origin"), "{e}");
    }

    #[tokio::test]
    async fn backs_off_on_429_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/models/org/busy"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/models/org/busy"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"id": "org/busy", "sha": C})),
            )
            .mount(&server)
            .await;
        let c = HfClient::new(&server.uri(), None)
            .unwrap()
            .with_retry(3, Duration::from_millis(1));
        let r = RepoRef::new(RepoType::Model, "org/busy").unwrap();
        assert_eq!(c.repo_info(&r, None).await.unwrap().sha, C);
        let none = HfClient::new(&server.uri(), None)
            .unwrap()
            .with_retry(0, Duration::from_millis(1));
        // The 429 mocks are used up; a fresh one shows retries=0 gives up at once.
        Mock::given(method("GET"))
            .and(path("/api/models/org/always"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        let e = none
            .repo_info(&RepoRef::new(RepoType::Model, "org/always").unwrap(), None)
            .await
            .unwrap_err();
        assert!(matches!(e, HfError::Status { status: 429, .. }), "{e}");
    }

    #[tokio::test]
    async fn lists_repos_by_author_with_pagination() {
        use wiremock::matchers::query_param;
        let server = MockServer::start().await;
        let next = format!("{}/api/models?author=org&page=2", server.uri());
        Mock::given(method("GET"))
            .and(path("/api/models"))
            .and(query_param("author", "org"))
            .and(query_param("filter", "safetensors"))
            .and(query_param("sort", "downloads"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("link", format!("<{next}>; rel=\"next\"").as_str())
                    .set_body_json(serde_json::json!([
                        {"id": "org/a", "downloads": 50, "tags": ["safetensors"], "gated": false},
                        {"id": "org/b", "downloads": 10, "tags": ["safetensors"], "gated": "auto"}
                    ])),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/models"))
            .and(query_param("page", "2"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([{"id": "org/c", "downloads": 1}])),
            )
            .mount(&server)
            .await;
        let c = HfClient::new(&server.uri(), None).unwrap();
        let l = c
            .list_repos(RepoType::Model, "org", &["safetensors".into()], 10)
            .await
            .unwrap();
        assert_eq!(
            l.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["org/a", "org/b", "org/c"]
        );
        assert!(l[1].gated.is_gated());
        let l = c
            .list_repos(RepoType::Model, "org", &["safetensors".into()], 1)
            .await
            .unwrap();
        assert_eq!(l.len(), 1);
    }

    #[tokio::test]
    async fn whoami_returns_username_and_not_the_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/whoami-v2"))
            .and(header("authorization", "Bearer secret-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "demo-user",
                "fullname": "Demo User"
            })))
            .mount(&server)
            .await;
        let hf = HfClient::new(&server.uri(), Some("secret-token".into()))
            .unwrap()
            .with_retry(0, Duration::from_millis(1));
        assert_eq!(hf.whoami().await.unwrap(), "demo-user");
        let err = HfClient::new(&server.uri(), None)
            .unwrap()
            .whoami()
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("no Hugging Face token"), "{msg}");
        assert!(!msg.contains("secret-token"), "{msg}");
    }
}
