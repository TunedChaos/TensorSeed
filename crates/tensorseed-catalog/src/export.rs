// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Backend neutral export bundles for `tensorseed catalog export` and
//! `tensorseed catalog import` (spec section 6.10).
//!
//! A bundle is the full set of [`SignedEntry`] values of one logical catalog.
//! It is not signed as a whole: every entry already carries the publisher
//! signature, and import verifies each one against the pinned key, so a
//! bundle can be moved between backends (Chia to file, file to a future
//! chain) without re-signing anything.

use serde::{Deserialize, Serialize};

use crate::minisign::PublicKey;
use crate::signed::{SignedEntry, check_catalog_name};
use crate::{CatalogBackend, CatalogError, CatalogId, Result};

/// Schema tag for [`ExportBundle`].
pub const EXPORT_SCHEMA: &str = "ts.export/1";

/// A portable catalog export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportBundle {
    /// Always [`EXPORT_SCHEMA`].
    pub schema: String,
    /// Logical catalog name the entries are bound to.
    pub catalog: String,
    /// Publisher public key (minisign base64), informational; import uses
    /// the key pinned in config, never this field.
    pub publisher_pubkey: String,
    /// Backend the export came from, for example `chia-datalayer`.
    pub source_kind: String,
    /// Backend specific id of the source (Chia store id, file catalog name).
    pub source_id: String,
    /// Entries sorted by key.
    pub entries: Vec<SignedEntry>,
}

impl ExportBundle {
    /// Verify every entry against the pinned key and catalog name, and check
    /// that keys are unique and sorted.
    pub fn verify(&self, pinned: &PublicKey) -> Result<()> {
        if self.schema != EXPORT_SCHEMA {
            return Err(CatalogError::Backend(format!(
                "unknown export schema {:?}",
                self.schema
            )));
        }
        check_catalog_name(&self.catalog)?;
        for pair in self.entries.windows(2) {
            if pair[0].key >= pair[1].key {
                return Err(CatalogError::Backend(format!(
                    "export entries not sorted or duplicated at {}",
                    pair[1].key
                )));
            }
        }
        for e in &self.entries {
            e.verify(pinned, &self.catalog)?;
        }
        Ok(())
    }
}

/// Read every `ts/` entry from `backend` and build a verified bundle.
pub async fn export(
    backend: &dyn CatalogBackend,
    source: &CatalogId,
    catalog: &str,
    pinned: &PublicKey,
) -> Result<ExportBundle> {
    let mut entries = Vec::new();
    for key in backend.keys(source, "ts/").await? {
        let entry = backend
            .get_signed(source, &key)
            .await?
            .ok_or_else(|| CatalogError::Backend(format!("key {key} vanished during export")))?;
        entries.push(entry);
    }
    entries.sort_by(|a, b| a.key.cmp(&b.key));
    let bundle = ExportBundle {
        schema: EXPORT_SCHEMA.into(),
        catalog: catalog.into(),
        publisher_pubkey: pinned.to_base64(),
        source_kind: backend.kind().into(),
        source_id: source.0.clone(),
        entries,
    };
    bundle.verify(pinned)?;
    Ok(bundle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CatalogOp;
    use crate::file::{FileCatalog, FileCatalogConfig};
    use crate::minisign::tests::test_key;

    fn backend(seed: u8) -> FileCatalog {
        let sk = test_key(seed);
        FileCatalog::new(FileCatalogConfig::new("main", "", sk.public_key()))
            .unwrap()
            .with_signer(sk)
            .unwrap()
    }

    #[tokio::test]
    async fn export_then_import_keeps_signatures() {
        let pk = test_key(5).public_key();
        let src = backend(5);
        let id = CatalogId("main".into());
        src.stage(
            &id,
            &[
                CatalogOp::Upsert {
                    key: "ts/v1/catalog".into(),
                    value: br#"{"schema":"ts.catalog/1"}"#.to_vec(),
                },
                CatalogOp::Upsert {
                    key: "ts/v1/index/model/org/a".into(),
                    value: br#"{"schema":"ts.index/1"}"#.to_vec(),
                },
            ],
        )
        .await
        .unwrap();
        src.commit(&id).await.unwrap();

        let bundle = export(&src, &id, "main", &pk).await.unwrap();
        assert_eq!(bundle.entries.len(), 2);
        let json = serde_json::to_vec(&bundle).unwrap();
        let back: ExportBundle = serde_json::from_slice(&json).unwrap();

        // Import into a fresh read only backend with the same pinned key: no
        // secret key needed, signatures carried over byte for byte.
        let dst = FileCatalog::new(FileCatalogConfig::new("main", "", pk.clone())).unwrap();
        dst.stage_signed(&back.entries).unwrap();

        // A different pinned key refuses the bundle.
        assert!(back.verify(&test_key(6).public_key()).is_err());
    }

    #[tokio::test]
    async fn import_rejects_tampered_entry() {
        let pk = test_key(5).public_key();
        let src = backend(5);
        let id = CatalogId("main".into());
        src.stage(
            &id,
            &[CatalogOp::Upsert {
                key: "ts/v1/catalog".into(),
                value: br#"{"schema":"ts.catalog/1"}"#.to_vec(),
            }],
        )
        .await
        .unwrap();
        src.commit(&id).await.unwrap();
        let mut bundle = export(&src, &id, "main", &pk).await.unwrap();
        bundle.entries[0].value = r#"{"schema":"ts.evil/1"}"#.into();
        assert!(bundle.verify(&pk).is_err());
    }
}
