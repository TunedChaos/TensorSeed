// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Verified HTTP downloads for Tensorseed (`pull`, spec section 8.2).
//!
//! Every file is downloaded into `<out>/.tensorseed/partial/`, hashed while
//! it streams, and only renamed into place once its size and digest match
//! what the signed manifest (or the Hub's published hashes) says. Sources
//! are tried in order (webseeds first, then Hugging Face as a fallback); a
//! source that serves wrong bytes is dropped for that file. Interrupted
//! downloads resume with HTTP range requests, and job state lives in SQLite
//! so `tensorseed jobs` can show it and reruns skip verified files.
//!
//! This is the HTTP source path, and Tensorseed's BEP 19 webseed client.
//! Swarm downloads (`pull --swarm`, `tensorseed-torrent`) run first and
//! write in place; a fetcher run afterwards hashes those files and keeps
//! only the ones that match.

pub mod state;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use tensorseed_core::hash::{ExpectedDigest, StreamHasher, sha256_bytes};
use tensorseed_core::verify::{META_DIR, safe_join};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Semaphore;

pub use state::StateDb;

/// User agent for downloads.
pub const USER_AGENT: &str = concat!("tensorseed/", env!("CARGO_PKG_VERSION"));

/// Record progress in the database every this many bytes.
const PROGRESS_EVERY: u64 = 64 << 20;

/// Errors from the fetch layer.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// Local I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// HTTP client failure.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    /// Job state database failure.
    #[error("state database error: {0}")]
    State(String),
    /// Invalid input (for example an unsafe path).
    #[error("invalid: {0}")]
    Invalid(String),
}

/// Result alias.
pub type Result<T, E = FetchError> = std::result::Result<T, E>;

/// Where a source comes from (for reports and token scoping).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// BEP 19 webseed (any HTTP mirror: local web server or object storage).
    Webseed,
    /// Hugging Face resolve URL (fallback).
    HuggingFace,
    /// Anything else (for example `--manifest-url` relative sources).
    Other,
}

impl SourceKind {
    /// Label for reports.
    pub fn label(self) -> &'static str {
        match self {
            Self::Webseed => "webseed",
            Self::HuggingFace => "huggingface",
            Self::Other => "http",
        }
    }
}

/// One place a file can be downloaded from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// Full URL of the file.
    pub url: String,
    /// Source kind.
    pub kind: SourceKind,
    /// Bearer token to send. Callers set this only for the Hub endpoint's
    /// own origin; `reqwest` drops it on cross-origin redirects.
    pub bearer: Option<String>,
}

/// One file to download and verify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileJob {
    /// Relative path inside the output directory.
    pub path: String,
    /// Expected size.
    pub size: u64,
    /// Expected digest.
    pub digest: ExpectedDigest,
    /// Sources in preference order.
    pub sources: Vec<Source>,
}

/// Identity of a pull job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobSpec {
    /// Stable id (see [`job_id`]).
    pub id: String,
    /// Label, for example `model org/name@<commit>`.
    pub label: String,
    /// Output directory.
    pub out_dir: PathBuf,
}

/// Stable job id for a label and output directory.
pub fn job_id(label: &str, out_dir: &Path) -> String {
    sha256_bytes(format!("{label}\n{}", out_dir.display()).as_bytes())[..16].to_string()
}

/// Options for a run.
#[derive(Debug, Clone, Copy)]
pub struct FetchOptions {
    /// Files downloaded at the same time (at least 1).
    pub concurrency: usize,
    /// Attempts per source for transient errors (each attempt resumes).
    pub attempts_per_source: u32,
    /// Delay between attempts.
    pub retry_delay: Duration,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            concurrency: 4,
            attempts_per_source: 3,
            retry_delay: Duration::from_millis(500),
        }
    }
}

/// What happened to one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Downloaded and verified.
    Downloaded {
        /// URL that served the verified bytes.
        source: String,
        /// Source kind.
        kind: SourceKind,
        /// Bytes already on disk from an earlier attempt or run.
        resumed_from: u64,
    },
    /// Already present and verified; nothing downloaded.
    AlreadyPresent,
    /// Every source failed.
    Failed,
}

/// Per file result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileResult {
    /// Relative path.
    pub path: String,
    /// Size.
    pub size: u64,
    /// Outcome.
    pub outcome: Outcome,
    /// Errors from sources that were tried and dropped (also on success).
    pub errors: Vec<String>,
}

/// Result of a run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FetchReport {
    /// Results sorted by path.
    pub files: Vec<FileResult>,
}

impl FetchReport {
    /// True if no file failed.
    pub fn ok(&self) -> bool {
        self.files.iter().all(|f| f.outcome != Outcome::Failed)
    }

    /// Count of (downloaded, already present, failed).
    pub fn counts(&self) -> (usize, usize, usize) {
        let mut c = (0, 0, 0);
        for f in &self.files {
            match f.outcome {
                Outcome::Downloaded { .. } => c.0 += 1,
                Outcome::AlreadyPresent => c.1 += 1,
                Outcome::Failed => c.2 += 1,
            }
        }
        c
    }
}

/// Error from one download attempt.
#[derive(Debug)]
enum AttemptError {
    /// Worth retrying the same source (network error, short read, 5xx).
    Transient(String),
    /// Drop this source (4xx, wrong bytes, bad range).
    Drop(String),
}

/// Downloader.
#[derive(Debug, Clone)]
pub struct Fetcher {
    http: reqwest::Client,
    db: Option<Arc<StateDb>>,
}

impl Fetcher {
    /// Build a fetcher. `db` enables resumable job records.
    pub fn new(db: Option<Arc<StateDb>>) -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(120))
            .build()?;
        Ok(Self { http, db })
    }

    /// Download and verify every file of a job.
    pub async fn run(
        &self,
        job: &JobSpec,
        files: Vec<FileJob>,
        opts: FetchOptions,
    ) -> Result<FetchReport> {
        let partial_dir = job.out_dir.join(META_DIR).join("partial");
        tokio::fs::create_dir_all(&partial_dir).await?;
        for f in &files {
            safe_join(&job.out_dir, &f.path).map_err(|e| FetchError::Invalid(e.to_string()))?;
        }
        if let Some(db) = &self.db {
            db.upsert_job(
                &job.id,
                &job.label,
                &job.out_dir.display().to_string(),
                files.len() as u64,
            )?;
            for f in &files {
                db.upsert_file(&job.id, &f.path, f.size, &f.digest.to_string())?;
            }
        }
        let sem = Arc::new(Semaphore::new(opts.concurrency.max(1)));
        let mut set = tokio::task::JoinSet::new();
        for f in files {
            let this = self.clone();
            let job = job.clone();
            let sem = sem.clone();
            set.spawn(async move {
                let _permit = sem.acquire_owned().await.ok();
                this.fetch_one(&job, f, opts).await
            });
        }
        let mut results = Vec::new();
        while let Some(r) = set.join_next().await {
            let r =
                r.map_err(|e| FetchError::Invalid(format!("download task panicked: {e}")))??;
            results.push(r);
        }
        results.sort_by(|a, b| a.path.cmp(&b.path));
        let report = FetchReport { files: results };
        if let Some(db) = &self.db {
            let (_, _, failed) = report.counts();
            if failed == 0 {
                db.set_job_status(&job.id, state::status::DONE, None)?;
            } else {
                db.set_job_status(
                    &job.id,
                    state::status::FAILED,
                    Some(&format!("{failed} file(s) failed")),
                )?;
            }
        }
        // Best effort: remove the partial dir if it is empty.
        let _ = tokio::fs::remove_dir(&partial_dir).await;
        Ok(report)
    }

    async fn fetch_one(&self, job: &JobSpec, f: FileJob, opts: FetchOptions) -> Result<FileResult> {
        let final_path =
            safe_join(&job.out_dir, &f.path).map_err(|e| FetchError::Invalid(e.to_string()))?;
        let mut result = FileResult {
            path: f.path.clone(),
            size: f.size,
            outcome: Outcome::Failed,
            errors: Vec::new(),
        };
        if self.already_present(job, &f, &final_path).await? {
            result.outcome = Outcome::AlreadyPresent;
            return Ok(result);
        }
        let part = job.out_dir.join(META_DIR).join("partial").join(format!(
            "{}-{}.part",
            f.digest.hex(),
            &sha256_bytes(f.path.as_bytes())[..16]
        ));
        for src in &f.sources {
            let mut attempt = 0;
            loop {
                attempt += 1;
                let resumed_from = tokio::fs::metadata(&part)
                    .await
                    .map(|m| m.len())
                    .unwrap_or(0);
                match self.attempt(job, &f, src, &part).await {
                    Ok(()) => {
                        if let Some(parent) = final_path.parent() {
                            tokio::fs::create_dir_all(parent).await?;
                        }
                        tokio::fs::rename(&part, &final_path).await?;
                        let mtime = mtime_ns(&final_path).await;
                        if let Some(db) = &self.db {
                            db.file_done(&job.id, &f.path, &src.url, mtime)?;
                        }
                        tracing::info!(path = %f.path, source = %src.url, "verified");
                        result.outcome = Outcome::Downloaded {
                            source: src.url.clone(),
                            kind: src.kind,
                            resumed_from: resumed_from.min(f.size),
                        };
                        return Ok(result);
                    }
                    Err(AttemptError::Transient(e)) if attempt < opts.attempts_per_source => {
                        tracing::warn!(path = %f.path, source = %src.url, attempt, error = %e, "retrying");
                        tokio::time::sleep(opts.retry_delay).await;
                    }
                    Err(AttemptError::Transient(e) | AttemptError::Drop(e)) => {
                        tracing::warn!(path = %f.path, source = %src.url, error = %e, "source dropped");
                        result
                            .errors
                            .push(format!("{} ({}): {e}", src.url, src.kind.label()));
                        break;
                    }
                }
            }
        }
        if let Some(db) = &self.db {
            let msg = if result.errors.is_empty() {
                "no sources".to_string()
            } else {
                result.errors.join("; ")
            };
            db.file_failed(&job.id, &f.path, &msg)?;
        }
        if f.sources.is_empty() {
            result.errors.push("no sources available".into());
        }
        Ok(result)
    }

    /// True if the final file is already there and verified.
    async fn already_present(&self, job: &JobSpec, f: &FileJob, path: &Path) -> Result<bool> {
        let Ok(meta) = tokio::fs::metadata(path).await else {
            return Ok(false);
        };
        if !meta.is_file() || meta.len() != f.size {
            return Ok(false);
        }
        let mtime = mtime_ns(path).await;
        if let Some(db) = &self.db
            && let Some(rec) = db.file(&job.id, &f.path)?
            && rec.status == state::status::DONE
            && rec.mtime_ns.is_some()
            && rec.mtime_ns == mtime
        {
            return Ok(true);
        }
        let p = path.to_path_buf();
        let digest = f.digest.clone();
        let size = f.size;
        let ok = tokio::task::spawn_blocking(move || {
            std::fs::File::open(&p)
                .and_then(|file| tensorseed_core::hash::digest_reader(file, &digest, size))
                .map(|(hex, _)| digest.matches(&hex))
                .unwrap_or(false)
        })
        .await
        .map_err(|e| FetchError::Invalid(format!("hash task failed: {e}")))?;
        if ok && let Some(db) = &self.db {
            db.file_done(&job.id, &f.path, "local", mtime)?;
        }
        Ok(ok)
    }

    async fn attempt(
        &self,
        job: &JobSpec,
        f: &FileJob,
        src: &Source,
        part: &Path,
    ) -> Result<(), AttemptError> {
        let io = |e: std::io::Error| AttemptError::Transient(format!("local I/O: {e}"));
        let mut have = tokio::fs::metadata(part)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        if have > f.size {
            tokio::fs::remove_file(part).await.map_err(io)?;
            have = 0;
        }
        let mut hasher = rehash_prefix(part, have, &f.digest, f.size)
            .await
            .map_err(io)?;
        if have == f.size {
            return finish(hasher, &f.digest, part).await;
        }
        let mut req = self.http.get(&src.url);
        if have > 0 {
            req = req.header(reqwest::header::RANGE, format!("bytes={have}-"));
        }
        if let Some(t) = &src.bearer {
            req = req.bearer_auth(t);
        }
        let mut resp = req
            .send()
            .await
            .map_err(|e| AttemptError::Transient(e.to_string()))?;
        let status = resp.status();
        let mut file;
        if status == reqwest::StatusCode::PARTIAL_CONTENT && have > 0 {
            let start = resp
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(content_range_start);
            if start != Some(have) {
                tokio::fs::remove_file(part).await.map_err(io)?;
                return Err(AttemptError::Transient(format!(
                    "server answered range {have}- with start {start:?}; restarting"
                )));
            }
            file = tokio::fs::OpenOptions::new()
                .append(true)
                .open(part)
                .await
                .map_err(io)?;
        } else if status.is_success() {
            // Full body (server ignored or did not get a range): start over.
            hasher = f.digest.hasher(f.size);
            have = 0;
            file = tokio::fs::File::create(part).await.map_err(io)?;
        } else if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
            tokio::fs::remove_file(part).await.map_err(io)?;
            return Err(AttemptError::Transient(
                "range not satisfiable; restarting".into(),
            ));
        } else if status.is_server_error() || status.as_u16() == 429 {
            return Err(AttemptError::Transient(format!("HTTP {status}")));
        } else {
            return Err(AttemptError::Drop(format!("HTTP {status}")));
        }
        let mut since_progress = 0u64;
        while let Some(chunk) = resp
            .chunk()
            .await
            .map_err(|e| AttemptError::Transient(e.to_string()))?
        {
            if have + chunk.len() as u64 > f.size {
                drop(file);
                let _ = tokio::fs::remove_file(part).await;
                return Err(AttemptError::Drop(format!(
                    "served more than the expected {} bytes",
                    f.size
                )));
            }
            hasher.update(&chunk);
            file.write_all(&chunk).await.map_err(io)?;
            have += chunk.len() as u64;
            since_progress += chunk.len() as u64;
            if since_progress >= PROGRESS_EVERY {
                since_progress = 0;
                file.flush().await.map_err(io)?;
                if let Some(db) = &self.db {
                    let _ = db.file_progress(&job.id, &f.path, have);
                }
            }
        }
        file.flush().await.map_err(io)?;
        file.sync_all().await.map_err(io)?;
        drop(file);
        if have < f.size {
            return Err(AttemptError::Transient(format!(
                "connection ended at {have} of {} bytes",
                f.size
            )));
        }
        finish(hasher, &f.digest, part).await
    }
}

async fn finish(
    hasher: StreamHasher,
    digest: &ExpectedDigest,
    part: &Path,
) -> Result<(), AttemptError> {
    let actual = hasher.finalize_hex();
    if digest.matches(&actual) {
        Ok(())
    } else {
        let _ = tokio::fs::remove_file(part).await;
        Err(AttemptError::Drop(format!(
            "content hash mismatch: expected {digest}, got {actual}"
        )))
    }
}

/// Hash the first `len` bytes of an existing partial file.
async fn rehash_prefix(
    part: &Path,
    len: u64,
    digest: &ExpectedDigest,
    size: u64,
) -> std::io::Result<StreamHasher> {
    let mut hasher = digest.hasher(size);
    if len == 0 {
        return Ok(hasher);
    }
    let mut file = tokio::fs::File::open(part).await?;
    let mut buf = vec![0u8; 1 << 20];
    let mut left = len;
    while left > 0 {
        let want = buf.len().min(left as usize);
        let n = file.read(&mut buf[..want]).await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "partial file shrank",
            ));
        }
        hasher.update(&buf[..n]);
        left -= n as u64;
    }
    Ok(hasher)
}

/// Parse the first byte position of `Content-Range: bytes <start>-<end>/<total>`.
pub fn content_range_start(v: &str) -> Option<u64> {
    v.trim()
        .strip_prefix("bytes ")?
        .split('-')
        .next()?
        .trim()
        .parse()
        .ok()
}

async fn mtime_ns(path: &Path) -> Option<i64> {
    let m = tokio::fs::metadata(path).await.ok()?.modified().ok()?;
    m.duration_since(UNIX_EPOCH)
        .ok()
        .map(|d| d.as_nanos() as i64)
}

#[cfg(test)]
mod tests;
