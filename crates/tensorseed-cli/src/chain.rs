// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! The publisher's Chia first write path for the file catalog (spec 6.14).
//! The client chain check lives in `tensorseed_app::chain` and is
//! re-exported here so `crate::chain` paths keep working.

use anyhow::{Context, bail};
pub use tensorseed_app::chain::*;
use tensorseed_catalog::anchor::{ChainProof, FileAnchor, chain_deletes, norm_hex};
use tensorseed_catalog::chia::ChiaDataLayer;
use tensorseed_catalog::file::{ChainState, DeltaOp, FileCatalog};
use tensorseed_catalog::minisign::SecretKey;
use tensorseed_catalog::signed::SignedEntry;
use tensorseed_catalog::{CatalogBackend, CatalogId, Generation};

use crate::backend::chia;
use crate::config::Config;

/// Publisher side: writes the chain half of each file catalog revision.
#[derive(Debug)]
pub struct ChainWriter {
    chia: ChiaDataLayer,
    id: CatalogId,
    sk: SecretKey,
    catalog: String,
}

impl ChainWriter {
    /// `Some` when `chia.catalog_store_id` is set: every file catalog
    /// revision is then written to Chia first.
    pub fn open(cfg: &Config, sk: &SecretKey) -> anyhow::Result<Option<Self>> {
        if cfg.chia.catalog_store_id.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(Self {
            chia: chia(cfg)?,
            id: CatalogId(store_id(cfg)?),
            sk: sk.clone(),
            catalog: cfg.catalog.name.clone(),
        }))
    }

    /// Store id.
    pub fn store(&self) -> &CatalogId {
        &self.id
    }

    /// Write `next` to the Chia store and wait for it to confirm, then
    /// return the proof of its anchor.
    ///
    /// With `ops` (a normal publish) only the changed keys and the anchor
    /// are written, and the store must already anchor `current`. Without
    /// `ops` (`catalog anchor`) the store is made to hold exactly
    /// `next.entries` plus the anchor. If the store already anchors `next`
    /// (an interrupted publish), nothing is written.
    pub async fn write(
        &self,
        current: &ChainState,
        next: &ChainState,
        ops: Option<&[DeltaOp]>,
    ) -> anyhow::Result<ChainProof> {
        let existing = self
            .chia
            .file_anchor(&self.id)
            .await
            .context("reading the current chain anchor")?;
        let already = existing
            .as_ref()
            .is_some_and(|a| a.matches(next.revision, &next.root));
        if already {
            tracing::info!(
                revision = next.revision,
                "Chia store already anchors this revision; not writing it again"
            );
        } else {
            let mut entries: Vec<SignedEntry> = Vec::new();
            let mut deletes: Vec<String> = Vec::new();
            let incremental = ops.is_some()
                && existing
                    .as_ref()
                    .is_some_and(|a| a.matches(current.revision, &current.root));
            if let (true, Some(ops)) = (incremental, ops) {
                for op in ops {
                    match op {
                        DeltaOp::Upsert { entry } => entries.push(entry.clone()),
                        DeltaOp::Delete { key } => deletes.push(key.clone()),
                    }
                }
            } else {
                if ops.is_some() && current.revision > 0 {
                    bail!(
                        "the Chia store {} anchors {}, but the file catalog is at revision {}; \
                         run `tensorseed catalog anchor` to bring the store up to date first",
                        self.id,
                        existing.as_ref().map_or("nothing".to_string(), |a| format!(
                            "revision {}",
                            a.revision
                        )),
                        current.revision
                    );
                }
                let keys = self.chia.keys(&self.id, "ts/").await?;
                deletes = chain_deletes(&keys, &next.entries);
                entries.extend(next.entries.values().cloned());
            }
            entries.push(FileAnchor::new(&self.catalog, next.revision, &next.root).sign(&self.sk)?);
            let changed = self
                .chia
                .stage_signed_with_deletes(&self.id, &entries, &deletes)
                .await?;
            let p = self.chia.commit(&self.id).await?;
            println!(
                "chia     staged {changed} changes for file revision {}; waiting for the store {} to confirm",
                next.revision, self.id
            );
            let g: Generation = self.chia.wait_confirmed(&p).await?;
            println!("chia     generation {} root {} confirmed", g.number, g.root);
        }
        let proof = self
            .chia
            .chain_proof(&self.id)
            .await?
            .context("the chain anchor is missing right after it confirmed")?;
        let v = proof.verify(&self.sk.public_key(), &self.catalog, &self.id.0)?;
        if !v.anchor.matches(next.revision, &next.root) {
            bail!(
                "the DataLayer proof anchors revision {}, expected {}",
                v.anchor.revision,
                next.revision
            );
        }
        let (root, confirmed) = self.chia.root(&self.id).await?;
        if !confirmed || norm_hex(&root) != v.root {
            bail!(
                "the DataLayer proof root {} is not the confirmed store root {root}",
                v.root
            );
        }
        Ok(proof)
    }
}

/// Commit what is staged in the file catalog. With a [`ChainWriter`] the
/// revision goes to Chia first and the file catalog (with its chain proof)
/// is only uploaded after the chain confirmed it.
pub async fn commit_file(
    cat: &FileCatalog,
    id: &CatalogId,
    chain: Option<&ChainWriter>,
) -> anyhow::Result<Generation> {
    if let Some(w) = chain {
        let (next, ops) = cat.preview_staged()?;
        let proof = w.write(&cat.state(), &next, Some(&ops)).await?;
        cat.set_chain_proof(Some(proof.to_bytes()?));
    }
    let p = cat.commit(id).await?;
    Ok(cat.wait_confirmed(&p).await?)
}
