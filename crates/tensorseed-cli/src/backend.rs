// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Opening the configured catalog backend and reading verified entries.

use std::sync::Arc;

use anyhow::{Context, bail};
use tensorseed_catalog::chia::{ChiaConfig, ChiaDataLayer};
use tensorseed_catalog::file::{FileCatalog, FileCatalogConfig};
use tensorseed_catalog::minisign::{PublicKey, SecretKey};
use tensorseed_catalog::signed::SignedEntry;
use tensorseed_catalog::sink::{DirSink, MultiSink, ObjectSink, S3Config, S3Settings, S3Sink};
use tensorseed_catalog::{CatalogBackend, CatalogId};

use crate::config::{Config, expand};

/// The pinned publisher key; required for anything that reads the catalog.
pub fn pinned_key(cfg: &Config) -> anyhow::Result<PublicKey> {
    if cfg.catalog.publisher_pubkey.is_empty() {
        bail!(
            "catalog.publisher_pubkey must be set (the minisign public key the catalog is signed with)"
        );
    }
    Ok(PublicKey::parse(&cfg.catalog.publisher_pubkey)?)
}

/// Publish destination from `[storage]`: the local directory, or every S3
/// target.
pub fn sink(cfg: &Config) -> anyhow::Result<Arc<dyn ObjectSink>> {
    let st = &cfg.storage;
    Ok(match st.backend.as_str() {
        "local" => Arc::new(DirSink::new(expand(&st.local_dir))),
        "s3" => {
            let mut sinks: Vec<Arc<dyn ObjectSink>> = Vec::new();
            for t in &st.targets {
                let s3 = S3Config::from_env(&S3Settings {
                    endpoint: t.endpoint.clone(),
                    region: t.region.clone(),
                    bucket: t.bucket.clone(),
                    prefix: t.prefix.clone(),
                    path_style: t.path_style,
                    access_key_env: t.access_key_env.clone(),
                    secret_key_env: t.secret_key_env.clone(),
                })
                .with_context(|| format!("storage target {:?}", t.name))?;
                sinks.push(Arc::new(S3Sink::new(s3)?));
            }
            Arc::new(MultiSink::new(sinks)?)
        }
        other => bail!("unknown storage.backend {other:?} (use local or s3)"),
    })
}

/// Build the file backend from config. With a signer, the upload sink is
/// attached as well.
pub fn file_catalog(cfg: &Config, signer: Option<SecretKey>) -> anyhow::Result<FileCatalog> {
    let mut fc = FileCatalogConfig::new(&cfg.catalog.name, &cfg.file.base_url, pinned_key(cfg)?);
    fc.mirror_urls = cfg.file.mirror_urls.clone();
    fc.state_dir = Some(expand(&cfg.file.state_dir));
    fc.snapshot_every = cfg.file.snapshot_every;
    fc.head_ttl_secs = cfg.file.head_ttl_secs;
    let mut cat = FileCatalog::new(fc).context("loading file catalog state")?;
    if let Some(sk) = signer {
        cat = cat.with_signer(sk)?;
        cat = cat.with_sink(sink(cfg)?);
    }
    Ok(cat)
}

/// Build the Chia backend from config.
pub fn chia(cfg: &Config) -> anyhow::Result<ChiaDataLayer> {
    let c = &cfg.chia;
    let mut chia = ChiaDataLayer::new(ChiaConfig {
        rpc_url: c.rpc_url.clone(),
        ssl_cert: expand(&c.ssl_cert),
        ssl_key: expand(&c.ssl_key),
        ca_cert: c.ca_cert.as_deref().map(expand),
        fee_mojos: c.fee_mojos,
    })?;
    chia = chia.with_catalog(&cfg.catalog.name, pinned_key(cfg)?);
    chia = chia.with_confirm_policy(c.confirm_poll_secs, c.confirm_timeout_secs);
    Ok(chia)
}

/// An opened backend.
#[allow(clippy::large_enum_variant)] // built once per command
pub enum Backend {
    /// Signed static files.
    File(FileCatalog),
    /// Chia DataLayer.
    Chia(ChiaDataLayer),
}

impl Backend {
    /// As the trait object.
    pub fn as_dyn(&self) -> &dyn CatalogBackend {
        match self {
            Self::File(f) => f,
            Self::Chia(c) => c,
        }
    }
}

/// Open the backend named `name` (or `catalog.backend`) for reading, synced
/// unless `no_wait`. Returns the backend and its catalog id.
pub async fn open_reader(
    cfg: &Config,
    name: Option<&str>,
    no_wait: bool,
) -> anyhow::Result<(Backend, CatalogId)> {
    match name.unwrap_or(&cfg.catalog.backend) {
        "file" => {
            let cat = file_catalog(cfg, None)?;
            if !no_wait {
                let r = cat
                    .sync()
                    .await
                    .context("syncing the file catalog (use --no-wait to use the local copy)")?;
                for s in &r.skipped {
                    tracing::warn!("catalog source skipped: {s}");
                }
                tracing::info!(from = r.from, to = r.to, source = %r.source, "catalog synced");
            } else if cat.state().revision == 0 {
                bail!("no local catalog copy yet; run without --no-wait first");
            }
            let id = CatalogId(cfg.catalog.name.clone());
            Ok((Backend::File(cat), id))
        }
        "chia" => {
            if cfg.chia.catalog_store_id.is_empty() {
                bail!("chia.catalog_store_id is not set");
            }
            let c = chia(cfg)?;
            let id = CatalogId(cfg.chia.catalog_store_id.clone());
            if !no_wait {
                let st = c
                    .sync_status(&id)
                    .await
                    .context("DataLayer get_sync_status")?;
                if !st.is_synced() {
                    bail!(
                        "local DataLayer copy is at generation {} of {}; wait for sync or use --no-wait",
                        st.local_generation,
                        st.target_generation
                    );
                }
            }
            Ok((Backend::Chia(c), id))
        }
        other => bail!("unknown backend {other:?}"),
    }
}

/// Read one entry and verify it against the pinned key and catalog name.
pub async fn read_verified(
    backend: &dyn CatalogBackend,
    id: &CatalogId,
    key: &str,
    pk: &PublicKey,
    catalog: &str,
) -> anyhow::Result<Option<SignedEntry>> {
    match backend.get_signed(id, key).await? {
        Some(e) => {
            if e.key != key {
                bail!(
                    "catalog returned entry for {:?} when asked for {key:?}",
                    e.key
                );
            }
            e.verify(pk, catalog)
                .with_context(|| format!("verifying {key}"))?;
            Ok(Some(e))
        }
        None => Ok(None),
    }
}
