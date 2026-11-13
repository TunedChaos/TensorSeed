// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Chia as the source of truth for the file catalog (spec 6.14): the client
//! chain check that gates file catalog syncs, and the publisher's Chia first
//! write path.

use std::time::Duration;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use tensorseed_catalog::anchor::{
    ChainProof, CoinApi, CoinState, FileAnchor, VerifiedProof, chain_deletes, norm_hex,
};
use tensorseed_catalog::chia::ChiaDataLayer;
use tensorseed_catalog::file::{ChainState, DeltaOp, FileCatalog, SyncReport, unix_now};
use tensorseed_catalog::minisign::SecretKey;
use tensorseed_catalog::signed::SignedEntry;
use tensorseed_catalog::{CatalogBackend, CatalogError, CatalogId, Generation};

use crate::backend::{chia, pinned_key};
use crate::config::{ChainCheck, Config, expand};

/// A file anchor proven against the chain.
#[derive(Debug, Clone)]
pub struct ChainReport {
    /// What the proof establishes offline.
    pub proof: VerifiedProof,
    /// Where the proven root sits on chain.
    pub state: CoinState,
    /// Height the store coin was created at (0 when unknown).
    pub confirmed_block_index: u64,
    /// Where the proof came from.
    pub via: String,
    /// Chain endpoints that confirmed the coin (empty when the local
    /// DataLayer node was trusted instead).
    pub endpoints: Vec<String>,
}

/// Last chain check result, kept for `catalog info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainStatusFile {
    /// Always `ts.chaincheck/1`.
    pub schema: String,
    /// Store id.
    pub store_id: String,
    /// Anchored file revision.
    pub revision: u64,
    /// Anchored file root.
    pub root: String,
    /// DataLayer root that includes the anchor.
    pub datalayer_root: String,
    /// Store coin.
    pub coin_id: String,
    /// `current`, `recently-spent`, or `stale`.
    pub coin_state: String,
    /// Where the proof came from.
    pub via: String,
    /// Unix time of the check.
    pub checked_unix: u64,
}

fn state_label(s: &CoinState) -> String {
    match s {
        CoinState::Current => "current".into(),
        CoinState::RecentlySpent { blocks_ago } => {
            format!("recently-spent ({blocks_ago} blocks ago)")
        }
        CoinState::Stale { blocks_ago } => format!("stale (spent {blocks_ago} blocks ago)"),
    }
}

fn status_path(cfg: &Config) -> std::path::PathBuf {
    expand(&cfg.file.state_dir)
        .join(&cfg.catalog.name)
        .join("chain-check.json")
}

/// The last successful chain check, if any.
pub fn load_status(cfg: &Config) -> Option<ChainStatusFile> {
    serde_json::from_slice(&std::fs::read(status_path(cfg)).ok()?).ok()
}

fn save_status(cfg: &Config, r: &ChainReport) {
    let doc = ChainStatusFile {
        schema: "ts.chaincheck/1".into(),
        store_id: r.proof.store_id.clone(),
        revision: r.proof.anchor.revision,
        root: r.proof.anchor.root.clone(),
        datalayer_root: r.proof.root.clone(),
        coin_id: r.proof.coin_id.clone(),
        coin_state: state_label(&r.state),
        via: r.via.clone(),
        checked_unix: unix_now(),
    };
    let path = status_path(cfg);
    let res = (|| -> anyhow::Result<()> {
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&doc)?)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    })();
    if let Err(e) = res {
        tracing::warn!("could not save {}: {e:#}", path.display());
    }
}

fn store_id(cfg: &Config) -> anyhow::Result<String> {
    let s = norm_hex(cfg.chia.catalog_store_id.trim());
    if s.is_empty() {
        bail!("chia.catalog_store_id is not set");
    }
    Ok(s)
}

fn coin_api(cfg: &Config) -> anyhow::Result<CoinApi> {
    let http = reqwest::Client::builder()
        .user_agent(concat!("tensorseed/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .build()?;
    Ok(CoinApi::new(cfg.chia.coin_api_urls.clone(), http))
}

/// Verify one proof: offline first, then the store coin on chain (or, with
/// no chain endpoints and a local DataLayer node, the node's confirmed root).
pub async fn verify_proof(
    cfg: &Config,
    proof: &ChainProof,
    via: &str,
    node: Option<&ChiaDataLayer>,
) -> anyhow::Result<ChainReport> {
    let pk = pinned_key(cfg)?;
    let store = store_id(cfg)?;
    let v = proof.verify(&pk, &cfg.catalog.name, &store)?;
    if !cfg.chia.coin_api_urls.is_empty() {
        let c = coin_api(cfg)?
            .check(&v, cfg.chia.chain_grace_blocks)
            .await?;
        for s in &c.skipped {
            tracing::warn!("chain endpoint skipped: {s}");
        }
        return Ok(ChainReport {
            proof: v,
            state: c.state,
            confirmed_block_index: c.confirmed_block_index,
            via: via.into(),
            endpoints: c.answered,
        });
    }
    let Some(node) = node else {
        bail!(
            "the light chain check needs at least one chia.coin_api_urls endpoint \
             (or chia.chain_source = \"datalayer\" with a local DataLayer node)"
        );
    };
    let id = CatalogId(store);
    let (root, confirmed) = node.root(&id).await.context("DataLayer get_root")?;
    if !confirmed || norm_hex(&root) != v.root {
        return Err(CatalogError::Chain(format!(
            "local DataLayer root {root} (confirmed: {confirmed}) is not the proven root {}",
            v.root
        ))
        .into());
    }
    Ok(ChainReport {
        proof: v,
        state: CoinState::Current,
        confirmed_block_index: 0,
        via: via.into(),
        endpoints: Vec::new(),
    })
}

/// Find and verify the chain anchored file head (`chia.chain_source`).
pub async fn chain_anchor(cfg: &Config, cat: &FileCatalog) -> anyhow::Result<ChainReport> {
    store_id(cfg)?;
    if cfg.chia.chain_source == "datalayer" {
        let node = chia(cfg)?;
        let id = CatalogId(store_id(cfg)?);
        let proof = node
            .chain_proof(&id)
            .await
            .context("reading the file anchor from the local DataLayer")?
            .with_context(|| {
                format!(
                    "DataLayer store {id} has no {} yet",
                    tensorseed_catalog::anchor::FILE_HEAD_KEY
                )
            })?;
        let r = verify_proof(
            cfg,
            &proof,
            &format!("DataLayer {}", cfg.chia.rpc_url),
            Some(&node),
        )
        .await?;
        if let CoinState::Stale { blocks_ago } = r.state {
            bail!(
                "the local DataLayer is behind the chain: its root was replaced {blocks_ago} blocks ago; wait for it to sync"
            );
        }
        return Ok(r);
    }
    let mut errors = Vec::new();
    let mut unreachable = 0usize;
    let mut best: Option<ChainReport> = None;
    let sources = cat.sources();
    for base in &sources {
        let bytes = match cat.fetch_chain_proof(base).await {
            Ok(Some(b)) => b,
            Ok(None) => {
                errors.push(format!("{base}: no chain-proof.json"));
                continue;
            }
            Err(e) => {
                errors.push(format!("{base}: {e}"));
                continue;
            }
        };
        let r = match ChainProof::from_bytes(&bytes) {
            Ok(p) => {
                verify_proof(
                    cfg,
                    &p,
                    &format!("{base}{}/chain-proof.json", cfg.catalog.name),
                    None,
                )
                .await
            }
            Err(e) => Err(e.into()),
        };
        match r {
            Ok(r) if matches!(r.state, CoinState::Stale { .. }) => errors.push(format!(
                "{base}: proof for file revision {} is {}; the chain has a newer state",
                r.proof.anchor.revision,
                state_label(&r.state)
            )),
            Ok(r) => {
                let better = best.as_ref().is_none_or(|b| {
                    let rank = |s: &CoinState| matches!(s, CoinState::Current) as u8;
                    (rank(&r.state), r.proof.anchor.revision)
                        > (rank(&b.state), b.proof.anchor.revision)
                });
                if better {
                    best = Some(r);
                }
                if best.as_ref().is_some_and(|b| b.state == CoinState::Current) {
                    break;
                }
            }
            Err(e) => {
                if matches!(
                    e.downcast_ref::<CatalogError>(),
                    Some(CatalogError::ChainUnreachable(_))
                ) {
                    unreachable += 1;
                }
                errors.push(format!("{base}: {e:#}"));
            }
        }
    }
    if let Some(b) = best {
        return Ok(b);
    }
    if unreachable > 0 && unreachable == errors.len() {
        return Err(CatalogError::ChainUnreachable(errors.join("; ")).into());
    }
    bail!("no verifiable chain proof: {}", errors.join("; "))
}

/// Sync the file catalog, gated by the chain check (`catalog.chain_check`).
///
/// - `off`: plain sync (with a notice when no store id is configured).
/// - `strict`: find the chain anchored `(revision, root)` first, then only
///   accept a head that matches it; any failure, including unreachable chain
///   endpoints, is an error and the local state does not move.
/// - `warn`: the same, but failures are logged and the plain sync result is
///   used.
pub async fn sync_checked(cfg: &Config, cat: &FileCatalog) -> anyhow::Result<SyncReport> {
    let mode = cfg.chain_check();
    if mode == ChainCheck::Off {
        if cfg.chia.catalog_store_id.trim().is_empty() {
            tracing::info!(
                "chain check off: no chia.catalog_store_id configured, so the file catalog \
                 is trusted on its publisher signatures alone"
            );
        } else {
            tracing::warn!("chain check disabled (catalog.chain_check = \"off\")");
        }
        return plain_sync(cat).await;
    }
    let strict = mode == ChainCheck::Strict;
    let report = match chain_anchor(cfg, cat).await {
        Ok(r) => r,
        Err(e) if strict => {
            return Err(e.context(
                "chain check failed (catalog.chain_check = \"strict\"); the file catalog was not updated",
            ));
        }
        Err(e) => {
            tracing::warn!("chain check failed, using the file catalog unchecked: {e:#}");
            return plain_sync(cat).await;
        }
    };
    let a = &report.proof.anchor;
    if let CoinState::RecentlySpent { blocks_ago } = report.state {
        tracing::warn!(
            "the chain moved past file revision {} {blocks_ago} blocks ago; the catalog caches \
             have not caught up yet",
            a.revision
        );
    }
    cat.set_expected_head(Some((a.revision, a.root.clone())));
    let synced = cat.sync().await;
    cat.set_expected_head(None);
    let r = match synced {
        Ok(r) => r,
        Err(e) if strict => {
            return Err(anyhow::Error::from(e).context(format!(
                "no catalog source serves the chain anchored head (revision {} root {})",
                a.revision, a.root
            )));
        }
        Err(e) => {
            tracing::warn!(
                "no catalog source serves the chain anchored head (revision {}): {e}; \
                 using the file catalog unchecked",
                a.revision
            );
            return plain_sync(cat).await;
        }
    };
    for s in &r.skipped {
        tracing::warn!("catalog source skipped: {s}");
    }
    let st = cat.state();
    if !a.matches(st.revision, &st.root) {
        let msg = format!(
            "file catalog is at revision {} root {}, but the chain anchors revision {} root {}",
            st.revision, st.root, a.revision, a.root
        );
        if strict {
            bail!("{msg}");
        }
        tracing::warn!("{msg}");
        return Ok(r);
    }
    tracing::info!(
        revision = a.revision,
        store = %report.proof.store_id,
        coin = %report.proof.coin_id,
        "file catalog matches the Chia store ({})",
        if report.endpoints.is_empty() { "local DataLayer".to_string() } else { report.endpoints.join(", ") }
    );
    save_status(cfg, &report);
    Ok(r)
}

async fn plain_sync(cat: &FileCatalog) -> anyhow::Result<SyncReport> {
    let r = cat.sync().await?;
    for s in &r.skipped {
        tracing::warn!("catalog source skipped: {s}");
    }
    Ok(r)
}

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
