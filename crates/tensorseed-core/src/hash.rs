// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Streaming hash helpers.
//!
//! Model files can be tens of gigabytes, so hashing always streams through a
//! fixed size buffer and never loads a whole file into memory.
//!
//! Two digest kinds exist. Plain SHA-256 is what manifests record and what
//! Hugging Face publishes as the LFS oid of large files. Small files that are
//! not in LFS only have a git blob id on Hugging Face (SHA-1 over
//! `"blob <size>\0" || content`), so [`ExpectedDigest::GitBlobSha1`] lets
//! `verify --hf` check those too.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::{Error, Result};

/// Default read buffer size for streaming hashes (1 MiB).
pub const DEFAULT_BUF_SIZE: usize = 1 << 20;

/// Output of a streaming hash: lowercase hex digest plus bytes read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashOutput {
    /// Lowercase 64 character hex SHA-256 digest.
    pub sha256: String,
    /// Total number of bytes hashed.
    pub size: u64,
}

/// Hash everything from `reader` with SHA-256 using a buffer of `buf_size` bytes.
///
/// # Errors
///
/// Any I/O error from `reader`.
pub fn sha256_reader_with_buf<R: Read>(mut reader: R, buf_size: usize) -> io::Result<HashOutput> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; buf_size.max(1)];
    let mut size: u64 = 0;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    Ok(HashOutput {
        sha256: hex::encode(hasher.finalize()),
        size,
    })
}

/// Hash everything from `reader` with SHA-256 using [`DEFAULT_BUF_SIZE`].
///
/// # Errors
///
/// Any I/O error from `reader`.
pub fn sha256_reader<R: Read>(reader: R) -> io::Result<HashOutput> {
    sha256_reader_with_buf(reader, DEFAULT_BUF_SIZE)
}

/// Hash a file on disk with SHA-256.
///
/// # Errors
///
/// Any I/O error opening or reading `path`.
pub fn sha256_file(path: impl AsRef<Path>) -> io::Result<HashOutput> {
    sha256_reader(File::open(path)?)
}

/// Hash a byte slice with SHA-256 and return the lowercase hex digest.
///
/// # Examples
///
/// ```
/// assert_eq!(
///     tensorseed_core::hash::sha256_bytes(b"abc"),
///     "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
/// );
/// ```
pub fn sha256_bytes(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Hash `path` and compare with `expected` (hex, case insensitive).
///
/// Returns the hash output on success, or [`Error::HashMismatch`].
///
/// # Errors
///
/// [`Error::Invalid`] for a malformed `expected`, [`Error::Io`] when the file
/// cannot be read, and [`Error::HashMismatch`] when the digest differs.
pub fn verify_file_sha256(path: impl AsRef<Path>, expected: &str) -> Result<HashOutput> {
    let path = path.as_ref();
    let out = sha256_file(path)?;
    if !out.sha256.eq_ignore_ascii_case(expected) {
        return Err(Error::HashMismatch {
            path: path.display().to_string(),
            expected: expected.to_ascii_lowercase(),
            actual: out.sha256,
        });
    }
    Ok(out)
}

/// What a file's content is expected to hash to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ExpectedDigest {
    /// Plain SHA-256 of the content (64 lowercase hex).
    Sha256(String),
    /// Git blob id: SHA-1 of `"blob <size>\0" || content` (40 lowercase hex).
    GitBlobSha1(String),
}

impl ExpectedDigest {
    /// The expected lowercase hex digest.
    pub fn hex(&self) -> &str {
        match self {
            Self::Sha256(h) | Self::GitBlobSha1(h) => h,
        }
    }

    /// Short algorithm label for reports, `sha256` or `git-sha1`.
    pub fn algorithm(&self) -> &'static str {
        match self {
            Self::Sha256(_) => "sha256",
            Self::GitBlobSha1(_) => "git-sha1",
        }
    }

    /// Validate the hex length for the algorithm and lowercase it.
    ///
    /// # Errors
    ///
    /// [`Error::Invalid`] when the digest is not hex or has the wrong length
    /// for its algorithm.
    pub fn normalized(self) -> Result<Self> {
        let (len, field) = match &self {
            Self::Sha256(_) => (64, "sha256"),
            Self::GitBlobSha1(_) => (40, "git blob sha1"),
        };
        let hex = self.hex().to_ascii_lowercase();
        if !is_lower_hex(&hex, len) {
            return Err(Error::invalid(
                field,
                format!("bad digest {:?}", self.hex()),
            ));
        }
        Ok(match self {
            Self::Sha256(_) => Self::Sha256(hex),
            Self::GitBlobSha1(_) => Self::GitBlobSha1(hex),
        })
    }

    /// Start an incremental hasher for content of `size` bytes.
    pub fn hasher(&self, size: u64) -> StreamHasher {
        match self {
            Self::Sha256(_) => StreamHasher::Sha256(Sha256::new()),
            Self::GitBlobSha1(_) => {
                let mut h = Sha1::new();
                h.update(format!("blob {size}\0").as_bytes());
                StreamHasher::GitSha1(h)
            }
        }
    }

    /// True if `actual_hex` (from [`StreamHasher::finalize_hex`]) matches.
    pub fn matches(&self, actual_hex: &str) -> bool {
        self.hex().eq_ignore_ascii_case(actual_hex)
    }
}

impl std::fmt::Display for ExpectedDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.algorithm(), self.hex())
    }
}

/// Incremental hasher matching an [`ExpectedDigest`] kind.
#[derive(Debug, Clone)]
pub enum StreamHasher {
    /// Plain SHA-256.
    Sha256(Sha256),
    /// Git blob SHA-1 (header already fed).
    GitSha1(Sha1),
}

impl StreamHasher {
    /// Feed bytes.
    pub fn update(&mut self, data: &[u8]) {
        match self {
            Self::Sha256(h) => h.update(data),
            Self::GitSha1(h) => h.update(data),
        }
    }

    /// Finish and return the lowercase hex digest.
    pub fn finalize_hex(self) -> String {
        match self {
            Self::Sha256(h) => hex::encode(h.finalize()),
            Self::GitSha1(h) => hex::encode(h.finalize()),
        }
    }
}

/// Hash `reader` for `expected`'s algorithm, returning (hex digest, bytes read).
///
/// For git blob ids the header uses `size`, the size the file should have;
/// a different actual length then yields a mismatching digest, as it should.
///
/// # Errors
///
/// Any I/O error from `reader`.
pub fn digest_reader<R: Read>(
    mut reader: R,
    expected: &ExpectedDigest,
    size: u64,
) -> io::Result<(String, u64)> {
    let mut hasher = expected.hasher(size);
    let mut buf = vec![0u8; DEFAULT_BUF_SIZE];
    let mut read: u64 = 0;
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        hasher.update(&buf[..n]);
        read += n as u64;
    }
    Ok((hasher.finalize_hex(), read))
}

/// Git blob id of a byte slice (what `git hash-object` prints).
///
/// # Examples
///
/// ```
/// // Same as `printf 'hello\n' | git hash-object --stdin`.
/// assert_eq!(
///     tensorseed_core::hash::git_blob_sha1_bytes(b"hello\n"),
///     "ce013625030ba8dba906f756967f9e9ca394464a"
/// );
/// ```
pub fn git_blob_sha1_bytes(data: &[u8]) -> String {
    let mut h = ExpectedDigest::GitBlobSha1(String::new()).hasher(data.len() as u64);
    h.update(data);
    h.finalize_hex()
}

/// Return true if `s` is a lowercase hex string of exactly `len` characters.
pub fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Write};

    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn empty_input() {
        let out = sha256_reader(Cursor::new(Vec::<u8>::new())).unwrap();
        assert_eq!(out.sha256, EMPTY);
        assert_eq!(out.size, 0);
        assert_eq!(sha256_bytes(b""), EMPTY);
    }

    #[test]
    fn known_vector_abc() {
        assert_eq!(sha256_bytes(b"abc"), ABC);
        let out = sha256_reader(Cursor::new(b"abc")).unwrap();
        assert_eq!(out.sha256, ABC);
        assert_eq!(out.size, 3);
    }

    #[test]
    fn tiny_buffer_matches_one_shot() {
        let data: Vec<u8> = (0..100_003u32).map(|i| (i % 251) as u8).collect();
        let expected = sha256_bytes(&data);
        for buf in [1usize, 7, 4096, DEFAULT_BUF_SIZE] {
            let out = sha256_reader_with_buf(Cursor::new(&data), buf).unwrap();
            assert_eq!(out.sha256, expected, "buf size {buf}");
            assert_eq!(out.size, data.len() as u64);
        }
    }

    #[test]
    fn file_hash_and_verify() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"abc").unwrap();
        f.flush().unwrap();
        let out = sha256_file(f.path()).unwrap();
        assert_eq!(out.sha256, ABC);
        assert!(verify_file_sha256(f.path(), &ABC.to_uppercase()).is_ok());
        let err = verify_file_sha256(f.path(), EMPTY).unwrap_err();
        assert!(matches!(err, Error::HashMismatch { .. }));
    }

    #[test]
    fn git_blob_sha1_matches_git() {
        // `printf 'hello\n' | git hash-object --stdin`
        assert_eq!(
            git_blob_sha1_bytes(b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
        // Empty blob id is well known.
        assert_eq!(
            git_blob_sha1_bytes(b""),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
        let exp = ExpectedDigest::GitBlobSha1("CE013625030BA8DBA906F756967F9E9CA394464A".into())
            .normalized()
            .unwrap();
        let (hex, n) = digest_reader(Cursor::new(b"hello\n"), &exp, 6).unwrap();
        assert!(exp.matches(&hex));
        assert_eq!(n, 6);
        assert!(ExpectedDigest::Sha256("zz".into()).normalized().is_err());
    }

    #[test]
    fn digest_reader_sha256() {
        let exp = ExpectedDigest::Sha256(ABC.into());
        let (hex, n) = digest_reader(Cursor::new(b"abc"), &exp, 3).unwrap();
        assert_eq!(hex, ABC);
        assert_eq!(n, 3);
        assert_eq!(exp.to_string(), format!("sha256:{ABC}"));
    }

    #[test]
    fn hex_check() {
        assert!(is_lower_hex(ABC, 64));
        assert!(!is_lower_hex(&ABC.to_uppercase(), 64));
        assert!(!is_lower_hex("abc", 64));
    }
}
