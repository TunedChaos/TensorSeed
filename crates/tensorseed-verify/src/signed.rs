// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Backend independent signed catalog entries (spec section 6.9).
//!
//! Every value stored under a `ts/v1/` key, in every backend, is a
//! [`SignedEntry`]: the canonical JSON document plus a minisign signature by
//! the publisher key. The trusted comment binds the signature to the logical
//! catalog name and the key, so an entry cannot be replayed under another key
//! or into another catalog. Because the signature does not depend on the
//! backend, entries keep their signatures when a catalog is exported from one
//! backend and imported into another.

use serde::{Deserialize, Serialize};
use tensorseed_core::canonical::to_canonical_vec;
use tensorseed_core::hash::sha256_bytes;

use crate::minisign::{PublicKey, SecretKey};
use crate::{Error as CatalogError, Result};

/// Schema tag for [`SignedEntry`].
pub const SIGNED_ENTRY_SCHEMA: &str = "ts.signed/1";

/// Prefix of the trusted comment for entry signatures.
pub const ENTRY_COMMENT_PREFIX: &str = "ts.entry/1";

/// One signed catalog entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedEntry {
    /// Always [`SIGNED_ENTRY_SCHEMA`].
    pub schema: String,
    /// Catalog key, for example `ts/v1/catalog`.
    pub key: String,
    /// The canonical JSON document, as a string, exactly as signed.
    pub value: String,
    /// Minisign signature over the UTF-8 bytes of `value`.
    pub sig: String,
}

/// Validate a logical catalog name: 1 to 64 of `[a-z0-9-]`.
///
/// # Errors
///
/// [`CatalogError::Config`] when `name` is empty, longer than 64 bytes, or
/// contains other characters.
pub fn check_catalog_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(CatalogError::Config(format!(
            "invalid catalog name {name:?}: use 1 to 64 of a-z, 0-9, -"
        )))
    }
}

fn check_key(key: &str) -> Result<()> {
    if !key.starts_with("ts/") || key.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(CatalogError::Signature(format!(
            "invalid entry key {key:?}"
        )));
    }
    Ok(())
}

/// The exact trusted comment an entry signature must carry.
pub fn entry_comment(catalog: &str, key: &str, value: &str) -> String {
    format!(
        "{ENTRY_COMMENT_PREFIX} catalog={catalog} key={key} sha256={}",
        sha256_bytes(value.as_bytes())
    )
}

impl SignedEntry {
    /// Sign canonical JSON `value` bytes for `key` in `catalog`.
    ///
    /// The value must already be canonical JSON (see
    /// `tensorseed_core::canonical`); this is checked.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Core`] when `value` is not canonical JSON, and
    /// [`CatalogError::Signature`] if signing fails.
    pub fn sign(sk: &SecretKey, catalog: &str, key: &str, value: &[u8]) -> Result<Self> {
        check_catalog_name(catalog)?;
        check_key(key)?;
        let value = std::str::from_utf8(value)
            .map_err(|e| CatalogError::Signature(format!("value is not UTF-8: {e}")))?
            .to_owned();
        check_canonical(&value)?;
        let sig = sk.sign(value.as_bytes(), &entry_comment(catalog, key, &value))?;
        Ok(Self {
            schema: SIGNED_ENTRY_SCHEMA.into(),
            key: key.into(),
            value,
            sig,
        })
    }

    /// Verify against the pinned publisher key and logical catalog name.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Signature`] when the signature does not verify or its
    /// trusted comment names another catalog, key, or value hash.
    pub fn verify(&self, pk: &PublicKey, catalog: &str) -> Result<()> {
        if self.schema != SIGNED_ENTRY_SCHEMA {
            return Err(CatalogError::Signature(format!(
                "unknown entry schema {:?}",
                self.schema
            )));
        }
        check_key(&self.key)?;
        let tc = pk.verify(self.value.as_bytes(), &self.sig)?;
        let expected = entry_comment(catalog, &self.key, &self.value);
        if tc != expected {
            return Err(CatalogError::Signature(format!(
                "entry signature context mismatch: got {tc:?}, expected {expected:?}"
            )));
        }
        check_canonical(&self.value)
    }

    /// Canonical JSON bytes of the whole envelope (what backends store).
    ///
    /// # Errors
    ///
    /// [`CatalogError::Core`] if canonical JSON encoding fails.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        Ok(to_canonical_vec(self)?)
    }

    /// Parse an envelope from stored bytes. Does not verify.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Json`] when `bytes` is not a signed entry envelope.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }
}

fn check_canonical(value: &str) -> Result<()> {
    let parsed: serde_json::Value = serde_json::from_str(value)?;
    if to_canonical_vec(&parsed)? != value.as_bytes() {
        return Err(CatalogError::Signature(
            "entry value is not canonical JSON".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::minisign::tests::test_key;

    const VALUE: &[u8] = br#"{"name":"Tensorseed catalog","schema":"ts.catalog/1"}"#;

    #[test]
    fn sign_and_verify() {
        let sk = test_key(3);
        let e = SignedEntry::sign(&sk, "main", "ts/v1/catalog", VALUE).unwrap();
        e.verify(&sk.public_key(), "main").unwrap();
        let round = SignedEntry::from_bytes(&e.to_bytes().unwrap()).unwrap();
        assert_eq!(round, e);
    }

    #[test]
    fn bound_to_key_and_catalog() {
        let sk = test_key(3);
        let pk = sk.public_key();
        let e = SignedEntry::sign(&sk, "main", "ts/v1/catalog", VALUE).unwrap();
        assert!(e.verify(&pk, "other").is_err());
        let mut moved = e.clone();
        moved.key = "ts/v1/index/model/org/name".into();
        assert!(moved.verify(&pk, "main").is_err());
        let mut changed = e;
        changed.value = r#"{"name":"evil","schema":"ts.catalog/1"}"#.into();
        assert!(changed.verify(&pk, "main").is_err());
    }

    #[test]
    fn rejects_non_canonical_and_bad_names() {
        let sk = test_key(3);
        assert!(SignedEntry::sign(&sk, "main", "ts/v1/catalog", br#"{"b":1, "a":2}"#).is_err());
        assert!(SignedEntry::sign(&sk, "Main", "ts/v1/catalog", VALUE).is_err());
        assert!(SignedEntry::sign(&sk, "main", "ts/v1/has space", VALUE).is_err());
    }
}
