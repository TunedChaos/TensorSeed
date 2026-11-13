// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Errors from catalog verification (signatures, hash chain, and proofs).
//! These variants do not depend on HTTP or the filesystem, so this crate
//! can compile to WebAssembly.

/// Errors from signature checks, file catalog chain checks, and chain proofs.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// JSON failure.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// Local I/O failure, for example reading a key file.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The backend returned an error response.
    #[error("backend error: {0}")]
    Backend(String),
    /// Invalid configuration.
    #[error("config error: {0}")]
    Config(String),
    /// Core type or canonical JSON failure.
    #[error("core error: {0}")]
    Core(#[from] tensorseed_core::Error),
    /// Signature missing, malformed, from the wrong key, or for the wrong context.
    #[error("signature error: {0}")]
    Signature(String),
    /// Hash chain is broken (gap, bad root, wrong catalog, stale head).
    #[error("catalog chain error: {0}")]
    Chain(String),
    /// Offered state is older than state already accepted.
    #[error("rollback detected: already accepted revision {seen}, offered {offered}")]
    Rollback {
        /// Highest accepted revision.
        seen: u64,
        /// Revision offered by the source.
        offered: u64,
    },
    /// Two different states claim the same revision.
    #[error("fork detected at revision {revision}")]
    Fork {
        /// Revision with conflicting roots.
        revision: u64,
    },
    /// No chain endpoint could be reached to check the catalog against the
    /// chain (spec 6.14).
    #[error("chain unreachable: {0}")]
    ChainUnreachable(String),
    /// The catalog (or a requested object) does not exist at any source.
    #[error("not found: {0}")]
    NotFound(String),
    /// Operation does not exist for this backend.
    #[error("not supported by this backend: {0}")]
    Unsupported(&'static str),
    /// Feature not implemented yet.
    #[error("not implemented yet: {0}")]
    NotImplemented(&'static str),
}

/// Result alias for verify operations.
pub type Result<T, E = Error> = std::result::Result<T, E>;
