// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! BitTorrent v2 content layer for Tensorseed (spec section 7).
//!
//! Status: stub. No torrent library is linked yet.
//!
//! **M0 must verify `librqbit` before this crate takes a dependency on it:**
//! BEP 52 v2 download and seeding, v2 and hybrid torrent creation, and BEP 19
//! webseeds (`url-list`) against an R2 bucket with HTTP range requests. As of
//! 2026-11-12 the librqbit v2 tracking issue is still open and BEP 19 support
//! is unconfirmed. If any check fails, fall back to libtorrent-rasterbar 2.x
//! (FFI or sidecar) or our own BEP 52 builder plus a v2 capable seeder.

use tensorseed_core::manifest::TorrentInfo;

/// Minimum BEP 52 piece length (16 KiB).
pub const MIN_PIECE_LENGTH: u64 = 16 * 1024;

/// Errors from the torrent layer.
#[derive(Debug, thiserror::Error)]
pub enum TorrentError {
    /// Invalid parameter.
    #[error("invalid: {0}")]
    Invalid(String),
    /// Feature not implemented yet.
    #[error("not implemented yet: {0}")]
    NotImplemented(&'static str),
}

/// Parameters for building a torrent for one (repo, commit).
#[derive(Debug, Clone)]
pub struct BuildParams {
    /// Torrent name, see [`TorrentInfo::name_for`].
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

/// Build a BEP 52 torrent from a directory. TODO(M1): implement after M0.
pub fn build_torrent(
    _dir: &std::path::Path,
    params: &BuildParams,
) -> Result<TorrentInfo, TorrentError> {
    params.validate()?;
    Err(TorrentError::NotImplemented("BEP 52 torrent creation"))
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

    #[test]
    fn build_is_stubbed() {
        let err = build_torrent(std::path::Path::new("."), &params()).unwrap_err();
        assert!(matches!(err, TorrentError::NotImplemented(_)));
    }
}
