// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Resumable job state in SQLite (`general.state_db`).
//!
//! The partial file on disk is the source of truth for how many bytes were
//! downloaded; the database records which jobs exist, what each file is
//! expected to hash to, which source finished it, and the size and mtime of
//! each finished file so an interrupted `pull` can skip re-hashing files it
//! already verified.

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};

use crate::{FetchError, Result};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS pull_jobs (
    id TEXT PRIMARY KEY,
    label TEXT NOT NULL,
    out_dir TEXT NOT NULL,
    status TEXT NOT NULL,
    files_total INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    created_unix INTEGER NOT NULL,
    updated_unix INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS pull_files (
    job_id TEXT NOT NULL REFERENCES pull_jobs(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    size INTEGER NOT NULL,
    digest TEXT NOT NULL,
    status TEXT NOT NULL,
    bytes_done INTEGER NOT NULL DEFAULT 0,
    source TEXT,
    mtime_ns INTEGER,
    error TEXT,
    updated_unix INTEGER NOT NULL,
    PRIMARY KEY (job_id, path)
);
CREATE TABLE IF NOT EXISTS harvest_revisions (
    repo_type TEXT NOT NULL,
    repo TEXT NOT NULL,
    commit_sha TEXT NOT NULL,
    status TEXT NOT NULL,
    bytes INTEGER NOT NULL DEFAULT 0,
    attempts INTEGER NOT NULL DEFAULT 0,
    detail TEXT,
    updated_unix INTEGER NOT NULL,
    PRIMARY KEY (repo_type, repo, commit_sha)
);
";

/// One harvested (or attempted) repo revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarvestRecord {
    /// `model` or `dataset`.
    pub repo_type: String,
    /// Repo id.
    pub repo: String,
    /// Commit SHA.
    pub commit: String,
    /// `done` or `failed`.
    pub status: String,
    /// Bytes kept on disk for this revision.
    pub bytes: u64,
    /// Attempts so far.
    pub attempts: u64,
    /// Error or note.
    pub detail: Option<String>,
    /// Last update (Unix seconds).
    pub updated_unix: i64,
}

/// Status of a job or file.
pub mod status {
    /// Not started or interrupted.
    pub const PENDING: &str = "pending";
    /// Bytes are being transferred.
    pub const RUNNING: &str = "running";
    /// Verified and in place.
    pub const DONE: &str = "done";
    /// All sources failed.
    pub const FAILED: &str = "failed";
}

/// A job row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRecord {
    /// Job id.
    pub id: String,
    /// Human readable label, for example `model org/name@<commit>`.
    pub label: String,
    /// Output directory.
    pub out_dir: String,
    /// Status.
    pub status: String,
    /// Number of files.
    pub files_total: u64,
    /// Files done.
    pub files_done: u64,
    /// Last error.
    pub error: Option<String>,
    /// Last update (unix seconds).
    pub updated_unix: i64,
}

/// A file row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    /// Relative path.
    pub path: String,
    /// Expected size.
    pub size: u64,
    /// Expected digest (`algorithm:hex`).
    pub digest: String,
    /// Status.
    pub status: String,
    /// Bytes downloaded so far (informational).
    pub bytes_done: u64,
    /// Source URL that completed the file.
    pub source: Option<String>,
    /// mtime (ns since epoch) of the finished file.
    pub mtime_ns: Option<i64>,
    /// Last error.
    pub error: Option<String>,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn db_err(e: rusqlite::Error) -> FetchError {
    FetchError::State(e.to_string())
}

/// SQLite job store.
#[derive(Debug)]
pub struct StateDb {
    conn: Mutex<Connection>,
}

impl StateDb {
    /// Open (creating if needed) the database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        Self::init(Connection::open(path).map_err(db_err)?)
    }

    /// In-memory database (tests).
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory().map_err(db_err)?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(10))
            .map_err(db_err)?;
        conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")
            .map_err(db_err)?;
        conn.execute_batch(SCHEMA).map_err(db_err)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Create or refresh a job.
    pub fn upsert_job(&self, id: &str, label: &str, out_dir: &str, files_total: u64) -> Result<()> {
        let t = now();
        self.conn()
            .execute(
                "INSERT INTO pull_jobs (id, label, out_dir, status, files_total, created_unix, updated_unix)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
                 ON CONFLICT(id) DO UPDATE SET label = ?2, out_dir = ?3, status = ?4,
                     files_total = ?5, error = NULL, updated_unix = ?6",
                params![id, label, out_dir, status::RUNNING, files_total as i64, t],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Set a job's final status.
    pub fn set_job_status(&self, id: &str, st: &str, error: Option<&str>) -> Result<()> {
        self.conn()
            .execute(
                "UPDATE pull_jobs SET status = ?2, error = ?3, updated_unix = ?4 WHERE id = ?1",
                params![id, st, error, now()],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Register a file. If its expected size or digest changed, any previous
    /// progress is discarded.
    pub fn upsert_file(&self, job: &str, path: &str, size: u64, digest: &str) -> Result<()> {
        self.conn()
            .execute(
                "INSERT INTO pull_files (job_id, path, size, digest, status, updated_unix)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(job_id, path) DO UPDATE SET
                     status = CASE WHEN size = ?3 AND digest = ?4 THEN status ELSE ?5 END,
                     mtime_ns = CASE WHEN size = ?3 AND digest = ?4 THEN mtime_ns ELSE NULL END,
                     bytes_done = CASE WHEN size = ?3 AND digest = ?4 THEN bytes_done ELSE 0 END,
                     size = ?3, digest = ?4, updated_unix = ?6",
                params![job, path, size as i64, digest, status::PENDING, now()],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Read one file row.
    pub fn file(&self, job: &str, path: &str) -> Result<Option<FileRecord>> {
        self.conn()
            .query_row(
                "SELECT path, size, digest, status, bytes_done, source, mtime_ns, error
                 FROM pull_files WHERE job_id = ?1 AND path = ?2",
                params![job, path],
                row_to_file,
            )
            .optional()
            .map_err(db_err)
    }

    /// Record progress.
    pub fn file_progress(&self, job: &str, path: &str, bytes_done: u64) -> Result<()> {
        self.conn()
            .execute(
                "UPDATE pull_files SET status = ?3, bytes_done = ?4, updated_unix = ?5
                 WHERE job_id = ?1 AND path = ?2",
                params![job, path, status::RUNNING, bytes_done as i64, now()],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Record a verified, renamed file.
    pub fn file_done(
        &self,
        job: &str,
        path: &str,
        source: &str,
        mtime_ns: Option<i64>,
    ) -> Result<()> {
        self.conn()
            .execute(
                "UPDATE pull_files SET status = ?3, bytes_done = size, source = ?4, mtime_ns = ?5,
                     error = NULL, updated_unix = ?6
                 WHERE job_id = ?1 AND path = ?2",
                params![job, path, status::DONE, source, mtime_ns, now()],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// Record a failure.
    pub fn file_failed(&self, job: &str, path: &str, error: &str) -> Result<()> {
        self.conn()
            .execute(
                "UPDATE pull_files SET status = ?3, error = ?4, updated_unix = ?5
                 WHERE job_id = ?1 AND path = ?2",
                params![job, path, status::FAILED, error, now()],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// All jobs, most recently updated first.
    pub fn jobs(&self) -> Result<Vec<JobRecord>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT j.id, j.label, j.out_dir, j.status, j.files_total,
                        (SELECT COUNT(*) FROM pull_files f WHERE f.job_id = j.id AND f.status = 'done'),
                        j.error, j.updated_unix
                 FROM pull_jobs j ORDER BY j.updated_unix DESC, j.id",
            )
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok(JobRecord {
                    id: r.get(0)?,
                    label: r.get(1)?,
                    out_dir: r.get(2)?,
                    status: r.get(3)?,
                    files_total: r.get::<_, i64>(4)? as u64,
                    files_done: r.get::<_, i64>(5)? as u64,
                    error: r.get(6)?,
                    updated_unix: r.get(7)?,
                })
            })
            .map_err(db_err)?;
        rows.collect::<std::result::Result<_, _>>().map_err(db_err)
    }

    /// Files of one job, sorted by path.
    pub fn job_files(&self, job: &str) -> Result<Vec<FileRecord>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT path, size, digest, status, bytes_done, source, mtime_ns, error
                 FROM pull_files WHERE job_id = ?1 ORDER BY path",
            )
            .map_err(db_err)?;
        let rows = stmt.query_map(params![job], row_to_file).map_err(db_err)?;
        rows.collect::<std::result::Result<_, _>>().map_err(db_err)
    }

    /// Record the outcome of harvesting a revision (`done` or `failed`).
    pub fn harvest_set(
        &self,
        repo_type: &str,
        repo: &str,
        commit: &str,
        st: &str,
        bytes: u64,
        detail: Option<&str>,
    ) -> Result<()> {
        self.conn()
            .execute(
                "INSERT INTO harvest_revisions (repo_type, repo, commit_sha, status, bytes, attempts, detail, updated_unix)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7)
                 ON CONFLICT(repo_type, repo, commit_sha) DO UPDATE SET status = ?4, bytes = ?5,
                     attempts = attempts + 1, detail = ?6, updated_unix = ?7",
                params![repo_type, repo, commit, st, bytes as i64, detail, now()],
            )
            .map_err(db_err)?;
        Ok(())
    }

    /// The record for one revision.
    pub fn harvest_get(
        &self,
        repo_type: &str,
        repo: &str,
        commit: &str,
    ) -> Result<Option<HarvestRecord>> {
        self.conn()
            .query_row(
                "SELECT repo_type, repo, commit_sha, status, bytes, attempts, detail, updated_unix
                 FROM harvest_revisions WHERE repo_type = ?1 AND repo = ?2 AND commit_sha = ?3",
                params![repo_type, repo, commit],
                row_to_harvest,
            )
            .optional()
            .map_err(db_err)
    }

    /// Every harvest record, newest first.
    pub fn harvest_list(&self) -> Result<Vec<HarvestRecord>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare(
                "SELECT repo_type, repo, commit_sha, status, bytes, attempts, detail, updated_unix
                 FROM harvest_revisions ORDER BY updated_unix DESC, repo",
            )
            .map_err(db_err)?;
        let rows = stmt.query_map([], row_to_harvest).map_err(db_err)?;
        rows.collect::<std::result::Result<_, _>>().map_err(db_err)
    }

    /// Bytes held by finished harvests (for the disk budget).
    pub fn harvest_bytes_done(&self) -> Result<u64> {
        let n: i64 = self
            .conn()
            .query_row(
                "SELECT COALESCE(SUM(bytes), 0) FROM harvest_revisions WHERE status = 'done'",
                [],
                |r| r.get(0),
            )
            .map_err(db_err)?;
        Ok(n as u64)
    }

    /// Delete a job and its file rows. Returns true if it existed.
    pub fn clear_job(&self, job: &str) -> Result<bool> {
        let n = self
            .conn()
            .execute("DELETE FROM pull_jobs WHERE id = ?1", params![job])
            .map_err(db_err)?;
        Ok(n > 0)
    }
}

fn row_to_harvest(r: &rusqlite::Row<'_>) -> rusqlite::Result<HarvestRecord> {
    Ok(HarvestRecord {
        repo_type: r.get(0)?,
        repo: r.get(1)?,
        commit: r.get(2)?,
        status: r.get(3)?,
        bytes: r.get::<_, i64>(4)? as u64,
        attempts: r.get::<_, i64>(5)? as u64,
        detail: r.get(6)?,
        updated_unix: r.get(7)?,
    })
}

fn row_to_file(r: &rusqlite::Row<'_>) -> rusqlite::Result<FileRecord> {
    Ok(FileRecord {
        path: r.get(0)?,
        size: r.get::<_, i64>(1)? as u64,
        digest: r.get(2)?,
        status: r.get(3)?,
        bytes_done: r.get::<_, i64>(4)? as u64,
        source: r.get(5)?,
        mtime_ns: r.get(6)?,
        error: r.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harvest_records() {
        let db = StateDb::open_in_memory().unwrap();
        db.harvest_set("model", "o/a", "c1", "failed", 0, Some("boom"))
            .unwrap();
        db.harvest_set("model", "o/a", "c1", "done", 10, None)
            .unwrap();
        db.harvest_set("model", "o/b", "c2", "done", 5, None)
            .unwrap();
        let r = db.harvest_get("model", "o/a", "c1").unwrap().unwrap();
        assert_eq!((r.status.as_str(), r.attempts, r.bytes), ("done", 2, 10));
        assert!(db.harvest_get("dataset", "o/a", "c1").unwrap().is_none());
        assert_eq!(db.harvest_bytes_done().unwrap(), 15);
        assert_eq!(db.harvest_list().unwrap().len(), 2);
    }

    #[test]
    fn job_lifecycle() {
        let db = StateDb::open_in_memory().unwrap();
        db.upsert_job("j1", "model o/n@c", "/out", 2).unwrap();
        db.upsert_file("j1", "a", 3, "sha256:aa").unwrap();
        db.upsert_file("j1", "b", 4, "sha256:bb").unwrap();
        db.file_progress("j1", "a", 2).unwrap();
        db.file_done("j1", "a", "http://x/a", Some(42)).unwrap();
        db.file_failed("j1", "b", "boom").unwrap();
        let jobs = db.jobs().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].files_done, 1);
        assert_eq!(jobs[0].files_total, 2);
        let a = db.file("j1", "a").unwrap().unwrap();
        assert_eq!(a.status, status::DONE);
        assert_eq!(a.bytes_done, 3);
        assert_eq!(a.mtime_ns, Some(42));
        // Same expectation keeps progress; a new digest resets it.
        db.upsert_file("j1", "a", 3, "sha256:aa").unwrap();
        assert_eq!(db.file("j1", "a").unwrap().unwrap().status, status::DONE);
        db.upsert_file("j1", "a", 3, "sha256:cc").unwrap();
        let a = db.file("j1", "a").unwrap().unwrap();
        assert_eq!(a.status, status::PENDING);
        assert_eq!(a.mtime_ns, None);
        assert_eq!(db.job_files("j1").unwrap().len(), 2);
        assert!(db.clear_job("j1").unwrap());
        assert!(db.job_files("j1").unwrap().is_empty());
        assert!(!db.clear_job("j1").unwrap());
    }

    #[test]
    fn opens_on_disk_and_creates_parent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nested/state.sqlite");
        let db = StateDb::open(&p).unwrap();
        db.upsert_job("j", "l", "/o", 0).unwrap();
        drop(db);
        assert_eq!(StateDb::open(&p).unwrap().jobs().unwrap().len(), 1);
    }
}
