// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Streaming SHA-256 helpers.
//!
//! Model files can be tens of gigabytes, so hashing always streams through a
//! fixed size buffer and never loads a whole file into memory.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

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
pub fn sha256_reader<R: Read>(reader: R) -> io::Result<HashOutput> {
    sha256_reader_with_buf(reader, DEFAULT_BUF_SIZE)
}

/// Hash a file on disk with SHA-256.
pub fn sha256_file(path: impl AsRef<Path>) -> io::Result<HashOutput> {
    sha256_reader(File::open(path)?)
}

/// Hash a byte slice with SHA-256 and return the lowercase hex digest.
pub fn sha256_bytes(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Hash `path` and compare with `expected` (hex, case insensitive).
///
/// Returns the hash output on success, or [`Error::HashMismatch`].
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
    fn hex_check() {
        assert!(is_lower_hex(ABC, 64));
        assert!(!is_lower_hex(&ABC.to_uppercase(), 64));
        assert!(!is_lower_hex("abc", 64));
    }
}
