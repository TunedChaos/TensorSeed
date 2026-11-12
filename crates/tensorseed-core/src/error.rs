// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Shared error type for Tensorseed core operations.

use std::io;

/// Convenience result alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Errors produced by core helpers.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Underlying I/O failure, for example while hashing a file.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// JSON encoding or decoding failure.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// A value did not have the expected format.
    #[error("invalid {field}: {reason}")]
    Invalid {
        /// Name of the offending field.
        field: &'static str,
        /// Human readable explanation.
        reason: String,
    },

    /// A file's SHA-256 did not match the expected value.
    #[error("hash mismatch for {path}: expected {expected}, got {actual}")]
    HashMismatch {
        /// Path of the file that failed verification.
        path: String,
        /// Expected lowercase hex digest.
        expected: String,
        /// Actual lowercase hex digest.
        actual: String,
    },
}

impl Error {
    /// Build an [`Error::Invalid`].
    pub fn invalid(field: &'static str, reason: impl Into<String>) -> Self {
        Self::Invalid {
            field,
            reason: reason.into(),
        }
    }
}
