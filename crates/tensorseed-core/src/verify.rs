// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Local verification: re-hash files in a directory against expected
//! digests and report a result per file (used by `tensorseed verify` and at
//! the end of `tensorseed pull`).
//!
//! No network code. Hashing runs on a small pool of OS threads because model
//! files are large and hashing is CPU and disk bound.

use std::collections::BTreeSet;
use std::fs::File;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::hash::{ExpectedDigest, digest_reader};
use crate::manifest::FileEntry;
use crate::{Error, Result};

/// Directory (inside an output directory) where Tensorseed keeps its own
/// metadata and partial downloads. Never part of a repo.
pub const META_DIR: &str = ".tensorseed";

/// One file a directory should contain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedFile {
    /// Relative path with `/` separators.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// Expected digest.
    pub digest: ExpectedDigest,
    /// Pickle based format (may be skipped on download by policy).
    pub pickle_risk: bool,
}

impl ExpectedFile {
    /// Build from a manifest file entry (SHA-256).
    pub fn from_manifest(f: &FileEntry) -> Self {
        Self {
            path: f.path.clone(),
            size: f.size,
            digest: ExpectedDigest::Sha256(f.sha256.clone()),
            pickle_risk: f.pickle_risk,
        }
    }
}

/// Result for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileStatus {
    /// Size and digest match.
    Ok,
    /// File is absent.
    Missing,
    /// Pickle file absent, and absence was allowed (skipped by policy).
    SkippedPickle,
    /// File absent, and absence was allowed (unselected or otherwise skipped).
    Skipped,
    /// Size differs.
    SizeMismatch {
        /// Expected size.
        expected: u64,
        /// Actual size.
        actual: u64,
    },
    /// Digest differs.
    HashMismatch {
        /// Expected digest (with algorithm).
        expected: String,
        /// Actual hex digest.
        actual: String,
    },
    /// Could not read the file.
    Error(String),
}

impl FileStatus {
    /// True for outcomes that do not fail verification.
    pub fn is_pass(&self) -> bool {
        matches!(self, Self::Ok | Self::SkippedPickle | Self::Skipped)
    }

    /// Short label for reports.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Missing => "MISSING",
            Self::SkippedPickle => "skipped (pickle)",
            Self::Skipped => "skipped",
            Self::SizeMismatch { .. } => "SIZE MISMATCH",
            Self::HashMismatch { .. } => "HASH MISMATCH",
            Self::Error(_) => "ERROR",
        }
    }
}

/// Result for one expected file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileReport {
    /// Relative path.
    pub path: String,
    /// Outcome.
    pub status: FileStatus,
}

/// Result for a whole directory.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VerifyReport {
    /// One report per expected file, sorted by path.
    pub files: Vec<FileReport>,
    /// Files present in the directory but not expected (informational).
    pub extra: Vec<String>,
}

impl VerifyReport {
    /// True when every expected file passed.
    pub fn passed(&self) -> bool {
        self.files.iter().all(|f| f.status.is_pass())
    }

    /// Number of failing files.
    pub fn failures(&self) -> usize {
        self.files.iter().filter(|f| !f.status.is_pass()).count()
    }
}

/// Options for [`verify_dir`].
#[derive(Debug, Clone)]
pub struct VerifyOptions {
    /// Treat absent pickle files as skipped instead of missing.
    pub allow_missing_pickle: bool,
    /// Absent files whose path is in this list are reported as [`FileStatus::Skipped`].
    pub allow_missing: Vec<String>,
    /// Hashing threads (at least 1).
    pub threads: usize,
}

impl Default for VerifyOptions {
    fn default() -> Self {
        Self {
            allow_missing_pickle: true,
            allow_missing: Vec::new(),
            threads: 4,
        }
    }
}

/// Join a manifest relative path onto `dir`, refusing anything that could
/// escape it (absolute paths, `..`, empty components, backslashes).
///
/// # Errors
///
/// [`Error::Invalid`] for any path that is absolute or
/// contains `..`, empty components, or backslashes.
///
/// # Examples
///
/// ```
/// use std::path::Path;
/// use tensorseed_core::verify::safe_join;
///
/// let dir = Path::new("out");
/// assert_eq!(safe_join(dir, "a/b.bin")?, Path::new("out/a/b.bin"));
/// assert!(safe_join(dir, "../etc/passwd").is_err());
/// assert!(safe_join(dir, "/abs").is_err());
/// # Ok::<(), tensorseed_core::Error>(())
/// ```
pub fn safe_join(dir: &Path, rel: &str) -> Result<PathBuf> {
    let bad = || Error::invalid("file path", format!("unsafe path {rel:?}"));
    if rel.is_empty() || rel.starts_with('/') || rel.contains('\\') || rel.contains('\0') {
        return Err(bad());
    }
    let mut out = dir.to_path_buf();
    for part in rel.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(bad());
        }
        let p = Path::new(part);
        if p.components().count() != 1
            || !matches!(p.components().next(), Some(Component::Normal(_)))
        {
            return Err(bad());
        }
        out.push(part);
    }
    if rel.split('/').next() == Some(META_DIR) {
        return Err(bad());
    }
    Ok(out)
}

/// Check one file. `path` must already be safe.
pub fn verify_file(path: &Path, expected: &ExpectedFile) -> FileStatus {
    let meta = match std::fs::metadata(path) {
        Ok(m) if m.is_file() => m,
        Ok(_) => return FileStatus::Error("not a regular file".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return FileStatus::Missing,
        Err(e) => return FileStatus::Error(e.to_string()),
    };
    if meta.len() != expected.size {
        return FileStatus::SizeMismatch {
            expected: expected.size,
            actual: meta.len(),
        };
    }
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => return FileStatus::Error(e.to_string()),
    };
    match digest_reader(file, &expected.digest, expected.size) {
        Ok((hex, _)) if expected.digest.matches(&hex) => FileStatus::Ok,
        Ok((hex, _)) => FileStatus::HashMismatch {
            expected: expected.digest.to_string(),
            actual: hex,
        },
        Err(e) => FileStatus::Error(e.to_string()),
    }
}

/// Verify every expected file under `dir`.
///
/// # Errors
///
/// [`Error::Invalid`] for an unsafe path. Missing or
/// mismatching files are reported in the [`VerifyReport`], not as errors.
pub fn verify_dir(
    dir: &Path,
    expected: &[ExpectedFile],
    opts: VerifyOptions,
) -> Result<VerifyReport> {
    let mut sorted: Vec<&ExpectedFile> = expected.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let paths = sorted
        .iter()
        .map(|e| safe_join(dir, &e.path))
        .collect::<Result<Vec<_>>>()?;
    let results: Vec<Mutex<Option<FileStatus>>> = sorted.iter().map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    let threads = opts.threads.clamp(1, 64).min(sorted.len().max(1));
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= sorted.len() {
                        break;
                    }
                    let mut st = verify_file(&paths[i], sorted[i]);
                    if st == FileStatus::Missing
                        && opts.allow_missing.iter().any(|p| p == &sorted[i].path)
                    {
                        st = FileStatus::Skipped;
                    } else if st == FileStatus::Missing
                        && sorted[i].pickle_risk
                        && opts.allow_missing_pickle
                    {
                        st = FileStatus::SkippedPickle;
                    }
                    *results[i]
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(st);
                }
            });
        }
    });
    let files = sorted
        .iter()
        .zip(results)
        .map(|(e, r)| FileReport {
            path: e.path.clone(),
            status: r
                .into_inner()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .unwrap_or_else(|| FileStatus::Error("not checked".into())),
        })
        .collect();
    let wanted: BTreeSet<&str> = sorted.iter().map(|e| e.path.as_str()).collect();
    let mut extra = Vec::new();
    list_files(dir, dir, &mut extra)?;
    extra.retain(|p| !wanted.contains(p.as_str()));
    extra.sort();
    Ok(VerifyReport { files, extra })
}

fn list_files(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for ent in rd {
        let ent = ent?;
        let path = ent.path();
        let ft = ent.file_type()?;
        let rel = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        if rel == META_DIR {
            continue;
        }
        if ft.is_dir() {
            list_files(root, &path, out)?;
        } else {
            out.push(rel);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::{git_blob_sha1_bytes, sha256_bytes};

    fn exp(path: &str, data: &[u8]) -> ExpectedFile {
        ExpectedFile {
            path: path.into(),
            size: data.len() as u64,
            digest: ExpectedDigest::Sha256(sha256_bytes(data)),
            pickle_risk: FileEntry::path_has_pickle_risk(path),
        }
    }

    #[test]
    fn safe_join_rejects_escapes() {
        let d = Path::new("/tmp/x");
        assert_eq!(
            safe_join(d, "a/b.json").unwrap(),
            Path::new("/tmp/x/a/b.json")
        );
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../b",
            "a//b",
            "a\\b",
            ".tensorseed/x",
            "./a",
        ] {
            assert!(safe_join(d, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn reports_each_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::create_dir_all(d.join("sub")).unwrap();
        std::fs::write(d.join("good.json"), b"{}").unwrap();
        std::fs::write(d.join("sub/bad.safetensors"), b"XXXX").unwrap();
        std::fs::write(d.join("short.txt"), b"ab").unwrap();
        std::fs::write(d.join("small.md"), b"hello\n").unwrap();
        std::fs::write(d.join("unexpected.txt"), b"?").unwrap();
        std::fs::create_dir_all(d.join(META_DIR)).unwrap();
        std::fs::write(d.join(META_DIR).join("state"), b"x").unwrap();
        let expected = vec![
            exp("good.json", b"{}"),
            exp("sub/bad.safetensors", b"YYYY"),
            exp("short.txt", b"abc"),
            exp("missing.gguf", b"m"),
            exp("pytorch_model.bin", b"p"),
            ExpectedFile {
                path: "small.md".into(),
                size: 6,
                digest: ExpectedDigest::GitBlobSha1(git_blob_sha1_bytes(b"hello\n")),
                pickle_risk: false,
            },
        ];
        let r = verify_dir(d, &expected, VerifyOptions::default()).unwrap();
        let by: std::collections::BTreeMap<_, _> = r
            .files
            .iter()
            .map(|f| (f.path.as_str(), &f.status))
            .collect();
        assert_eq!(by["good.json"], &FileStatus::Ok);
        assert_eq!(by["small.md"], &FileStatus::Ok);
        assert!(matches!(
            by["sub/bad.safetensors"],
            FileStatus::HashMismatch { .. }
        ));
        assert!(matches!(
            by["short.txt"],
            FileStatus::SizeMismatch {
                expected: 3,
                actual: 2
            }
        ));
        assert_eq!(by["missing.gguf"], &FileStatus::Missing);
        assert_eq!(by["pytorch_model.bin"], &FileStatus::SkippedPickle);
        assert_eq!(r.extra, vec!["unexpected.txt".to_string()]);
        assert!(!r.passed());
        assert_eq!(r.failures(), 3);

        let strict = VerifyOptions {
            allow_missing_pickle: false,
            allow_missing: Vec::new(),
            threads: 1,
        };
        let r = verify_dir(d, &expected[..1], strict).unwrap();
        assert!(r.passed());

        let allow = VerifyOptions {
            allow_missing_pickle: false,
            allow_missing: vec!["missing.gguf".into()],
            threads: 1,
        };
        let r = verify_dir(d, &expected, allow).unwrap();
        assert_eq!(by_status(&r)["missing.gguf"], &FileStatus::Skipped);
        assert_eq!(by_status(&r)["pytorch_model.bin"], &FileStatus::Missing);
    }

    fn by_status(r: &VerifyReport) -> std::collections::BTreeMap<&str, &FileStatus> {
        r.files
            .iter()
            .map(|f| (f.path.as_str(), &f.status))
            .collect()
    }
}
