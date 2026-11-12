// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos LLC

//! Pluggable catalog backends for Tensorseed (spec section 6.0).
//!
//! The catalog format (keys and canonical JSON) lives in `tensorseed-core` and
//! is chain agnostic. Every chain specific operation goes through
//! [`CatalogBackend`]. The first implementation is [`chia::ChiaDataLayer`].

use async_trait::async_trait;

pub mod chia;

/// Errors from catalog backends.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    /// HTTP or TLS failure talking to the backend.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    /// Local I/O failure, for example reading TLS material.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON failure.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    /// The backend returned an error response.
    #[error("backend error: {0}")]
    Backend(String),
    /// Invalid configuration.
    #[error("config error: {0}")]
    Config(String),
    /// Feature not implemented yet.
    #[error("not implemented yet: {0}")]
    NotImplemented(&'static str),
}

/// Result alias for catalog operations.
pub type Result<T, E = CatalogError> = std::result::Result<T, E>;

/// Backend specific catalog identifier (Chia: 64 hex store ID).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CatalogId(pub String);

impl std::fmt::Display for CatalogId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One staged change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogOp {
    /// Insert or replace a value (canonical JSON bytes).
    Upsert {
        /// Key, for example `ts/v1/catalog`.
        key: String,
        /// Value bytes.
        value: Vec<u8>,
    },
    /// Delete a key.
    Delete {
        /// Key to delete.
        key: String,
    },
}

/// A submitted but not yet confirmed commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCommit {
    /// Catalog the commit belongs to.
    pub catalog: CatalogId,
    /// Expected root hash (hex) or backend equivalent, if known.
    pub expected_root: Option<String>,
    /// Backend transaction id, if any.
    pub tx_id: Option<String>,
}

/// A confirmed catalog generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generation {
    /// Generation number.
    pub number: u64,
    /// Root hash (hex) or backend equivalent.
    pub root: String,
}

/// Local versus remote state of a subscribed catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncStatus {
    /// Generation available locally.
    pub local_generation: u64,
    /// Latest generation confirmed by the backend.
    pub target_generation: u64,
}

impl SyncStatus {
    /// True when the local copy is up to date.
    pub fn is_synced(&self) -> bool {
        self.local_generation >= self.target_generation
    }
}

/// Chain agnostic catalog interface. See spec section 6.0 for the contract.
#[async_trait]
pub trait CatalogBackend: Send + Sync {
    /// Short stable identifier, for example `chia-datalayer`.
    fn kind(&self) -> &'static str;
    /// Create a new catalog and return its identifier.
    async fn create_catalog(&self) -> Result<CatalogId>;
    /// Read one value by key from the latest synced generation.
    async fn get(&self, catalog: &CatalogId, key: &str) -> Result<Option<Vec<u8>>>;
    /// List keys that start with `prefix`.
    async fn keys(&self, catalog: &CatalogId, prefix: &str) -> Result<Vec<String>>;
    /// Stage a batch of changes without publishing.
    async fn stage(&self, catalog: &CatalogId, batch: &[CatalogOp]) -> Result<()>;
    /// Publish everything staged as one atomic generation.
    async fn commit(&self, catalog: &CatalogId) -> Result<PendingCommit>;
    /// Wait until a commit is confirmed.
    async fn wait_confirmed(&self, pending: &PendingCommit) -> Result<Generation>;
    /// Subscribe to a catalog published by someone else.
    async fn subscribe(&self, catalog: &CatalogId, mirror_urls: &[String]) -> Result<()>;
    /// Sync state for a subscribed catalog.
    async fn sync_status(&self, catalog: &CatalogId) -> Result<SyncStatus>;
    /// Advertise a mirror for this catalog where supported.
    async fn add_mirror(&self, catalog: &CatalogId, urls: &[String]) -> Result<()>;
}

/// Backends known to this build, as accepted by `catalog.backend` in config.
pub const KNOWN_BACKENDS: &[&str] = &["chia"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_status() {
        let s = SyncStatus {
            local_generation: 3,
            target_generation: 4,
        };
        assert!(!s.is_synced());
    }

    #[test]
    fn trait_is_object_safe() {
        fn takes(_: Option<&dyn CatalogBackend>) {}
        takes(None);
    }
}
