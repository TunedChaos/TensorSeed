// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! BitTorrent v2 content layer for Tensorseed (spec section 7).
//!
//! Decided in the M0 spike (`docs/M0-TORRENT-SPIKE.md`):
//! - Torrent creation is ours, in pure Rust ([`metainfo`], [`merkle`],
//!   [`bencode`]): BEP 52 v2 and hybrid v1+v2 torrents with BEP 19 `url-list`
//!   webseeds, byte compatible with libtorrent 2.x.
//! - The swarm engine is `librqbit` ([`engine`], feature `engine`, on by
//!   default). librqbit 9.1 speaks the v1 side of hybrid torrents (it cannot
//!   load v2 only torrents, resolve `btmh` only magnets, or use webseeds), so
//!   Tensorseed publishes hybrid torrents, downloads webseeds with its own
//!   verified HTTP fetcher, and checks every finished file against the signed
//!   manifest (SHA-256) and the torrent's v2 Merkle root.

pub mod bencode;
#[cfg(feature = "engine")]
pub mod engine;
pub mod merkle;
pub mod metainfo;

pub use metainfo::{InputFile, Metainfo, TorrentFile, build_torrent};

/// Minimum BEP 52 piece length (16 KiB).
pub const MIN_PIECE_LENGTH: u64 = 16 * 1024;

/// Errors from the torrent layer.
#[derive(Debug, thiserror::Error)]
pub enum TorrentError {
    /// Invalid parameter.
    #[error("invalid: {0}")]
    Invalid(String),
    /// I/O error.
    #[error("io: {0}")]
    Io(String),
    /// Swarm engine error.
    #[error("engine: {0}")]
    Engine(String),
}

/// Parameters for building a torrent for one (repo, commit).
#[derive(Debug, Clone)]
pub struct BuildParams {
    /// Torrent name, see
    /// [`TorrentInfo::name_for`](tensorseed_core::manifest::TorrentInfo::name_for).
    pub name: String,
    /// Piece length; power of two, at least 16 KiB.
    pub piece_length: u64,
    /// Build a hybrid v1+v2 torrent.
    pub hybrid: bool,
    /// Tracker announce URLs.
    pub trackers: Vec<String>,
    /// BEP 19 webseed base URLs (ending in `/`).
    pub webseeds: Vec<String>,
}

impl BuildParams {
    /// Validate parameters.
    pub fn validate(&self) -> Result<(), TorrentError> {
        if self.piece_length < MIN_PIECE_LENGTH || !self.piece_length.is_power_of_two() {
            return Err(TorrentError::Invalid(
                "piece_length must be a power of two >= 16 KiB".into(),
            ));
        }
        if let Some(bad) = self.webseeds.iter().find(|w| !w.ends_with('/')) {
            return Err(TorrentError::Invalid(format!(
                "webseed base must end with '/': {bad}"
            )));
        }
        Ok(())
    }
}

/// BEP 19 object path for a file: `<base><torrent name>/<relative path>`.
pub fn webseed_url(base: &str, torrent_name: &str, rel_path: &str) -> String {
    format!("{base}{torrent_name}/{rel_path}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> BuildParams {
        BuildParams {
            name: "org__name@0123456789ab".into(),
            piece_length: 4 * 1024 * 1024,
            hybrid: true,
            trackers: vec![],
            webseeds: vec!["https://models.tensorseed.dev/t/abc/".into()],
        }
    }

    #[test]
    fn validates_params() {
        assert!(params().validate().is_ok());
        let mut p = params();
        p.piece_length = 1000;
        assert!(p.validate().is_err());
        let mut p = params();
        p.webseeds = vec!["https://x/no-slash".into()];
        assert!(p.validate().is_err());
    }

    #[test]
    fn webseed_layout() {
        assert_eq!(
            webseed_url(
                "https://models.tensorseed.dev/t/abc/",
                "org__n@0123",
                "a/b.gguf"
            ),
            "https://models.tensorseed.dev/t/abc/org__n@0123/a/b.gguf"
        );
    }
}
