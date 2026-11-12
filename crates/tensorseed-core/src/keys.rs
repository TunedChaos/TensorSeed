// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Catalog key layout (spec section 6.3). Keys are backend independent; the
//! Chia backend hex encodes them on the wire.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::hash::is_lower_hex;
use crate::{Error, Result};

/// Prefix for every schema v1 key.
pub const PREFIX: &str = "ts/v1";

/// Hugging Face repository type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RepoType {
    /// A model repository.
    Model,
    /// A dataset repository.
    Dataset,
}

impl RepoType {
    /// The lowercase name used in keys and URLs.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Dataset => "dataset",
        }
    }
}

impl fmt::Display for RepoType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for RepoType {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "model" => Ok(Self::Model),
            "dataset" => Ok(Self::Dataset),
            other => Err(Error::invalid(
                "repo type",
                format!("unknown type {other:?}"),
            )),
        }
    }
}

/// A validated Hugging Face repository reference (`org/name` plus type).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepoRef {
    /// Repository type.
    pub repo_type: RepoType,
    /// Repository id in `org/name` form.
    pub id: String,
}

impl RepoRef {
    /// Validate and build a repo reference.
    pub fn new(repo_type: RepoType, id: impl Into<String>) -> Result<Self> {
        let id = id.into();
        let mut parts = id.split('/');
        let (org, name) = match (parts.next(), parts.next(), parts.next()) {
            (Some(o), Some(n), None) => (o, n),
            _ => return Err(Error::invalid("repo id", "expected org/name")),
        };
        let ok = |s: &str| {
            !s.is_empty()
                && s != "."
                && s != ".."
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        };
        if !ok(org) || !ok(name) {
            return Err(Error::invalid(
                "repo id",
                format!("invalid characters in {id:?}"),
            ));
        }
        Ok(Self { repo_type, id })
    }

    fn base(&self) -> String {
        format!("{PREFIX}/repo/{}/{}", self.repo_type, self.id)
    }

    /// `ts/v1/repo/{type}/{org}/{name}/latest`
    pub fn latest_key(&self) -> String {
        format!("{}/latest", self.base())
    }

    /// `ts/v1/repo/{type}/{org}/{name}/rev/{commit}`
    pub fn manifest_key(&self, commit: &str) -> Result<String> {
        check_commit(commit)?;
        Ok(format!("{}/rev/{commit}", self.base()))
    }

    /// `ts/v1/repo/{type}/{org}/{name}/rev/{commit}/files/{page:06}`
    pub fn file_page_key(&self, commit: &str, page: u32) -> Result<String> {
        Ok(format!("{}/files/{page:06}", self.manifest_key(commit)?))
    }

    /// `ts/v1/repo/{type}/{org}/{name}/tombstone`
    pub fn tombstone_key(&self) -> String {
        format!("{}/tombstone", self.base())
    }

    /// `ts/v1/index/{type}/{org}/{name}`
    pub fn index_key(&self) -> String {
        format!("{PREFIX}/index/{}/{}", self.repo_type, self.id)
    }
}

/// `ts/v1/catalog`
pub fn catalog_info_key() -> String {
    format!("{PREFIX}/catalog")
}

/// `ts/v1/index/` prefix, used to list catalog entries.
pub fn index_prefix() -> String {
    format!("{PREFIX}/index/")
}

/// Check that `commit` is a full 40 character lowercase hex HF commit SHA.
pub fn check_commit(commit: &str) -> Result<()> {
    if is_lower_hex(commit, 40) {
        Ok(())
    } else {
        Err(Error::invalid(
            "commit",
            "expected 40 lowercase hex characters",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn key_layout() {
        let r = RepoRef::new(RepoType::Model, "org/name").unwrap();
        assert_eq!(r.latest_key(), "ts/v1/repo/model/org/name/latest");
        assert_eq!(
            r.manifest_key(C).unwrap(),
            format!("ts/v1/repo/model/org/name/rev/{C}")
        );
        assert_eq!(
            r.file_page_key(C, 7).unwrap(),
            format!("ts/v1/repo/model/org/name/rev/{C}/files/000007")
        );
        assert_eq!(r.tombstone_key(), "ts/v1/repo/model/org/name/tombstone");
        assert_eq!(r.index_key(), "ts/v1/index/model/org/name");
        assert_eq!(catalog_info_key(), "ts/v1/catalog");
    }

    #[test]
    fn rejects_bad_input() {
        assert!(RepoRef::new(RepoType::Dataset, "noslash").is_err());
        assert!(RepoRef::new(RepoType::Dataset, "a/b/c").is_err());
        assert!(RepoRef::new(RepoType::Dataset, "../x").is_err());
        assert!(RepoRef::new(RepoType::Dataset, "a/b c").is_err());
        let r = RepoRef::new(RepoType::Dataset, "a/b").unwrap();
        assert!(r.manifest_key("main").is_err());
        assert!(r.manifest_key(&C.to_uppercase()).is_err());
    }

    #[test]
    fn repo_type_parse() {
        assert_eq!("model".parse::<RepoType>().unwrap(), RepoType::Model);
        assert!("space".parse::<RepoType>().is_err());
    }
}
