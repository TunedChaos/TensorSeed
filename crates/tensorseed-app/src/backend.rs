// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Opening the configured catalog backend and reading verified entries.

use std::sync::Arc;

use anyhow::{Context, bail};
use tensorseed_catalog::chia::{ChiaAnchor, ChiaConfig, ChiaDataLayer};
use tensorseed_catalog::file::{FileCatalog, FileCatalogConfig};
use tensorseed_catalog::minisign::{PublicKey, SecretKey};
use tensorseed_catalog::signed::SignedEntry;
use tensorseed_catalog::sink::{DirSink, MultiSink, ObjectSink, S3Config, S3Settings, S3Sink};
use tensorseed_catalog::{CatalogBackend, CatalogId};

use crate::config::{Config, expand};

/// The pinned publisher key; required for anything that reads the catalog.
///
/// # Errors
///
/// Fails when `catalog.publisher_pubkey` is missing or not a valid minisign key.
pub fn pinned_key(cfg: &Config) -> anyhow::Result<PublicKey> {
    if cfg.catalog.publisher_pubkey.is_empty() {
        bail!(
            "catalog.publisher_pubkey must be set (the minisign public key the catalog is signed with)"
        );
    }
    Ok(PublicKey::parse(&cfg.catalog.publisher_pubkey)?)
}

/// Every enabled `[[storage.target]]` with `upload = true`, as sinks.
///
/// # Errors
///
/// Fails when a target is misconfigured or its sink cannot be built.
pub fn upload_sinks(cfg: &Config) -> anyhow::Result<Vec<(String, Arc<dyn ObjectSink>)>> {
    let mut out: Vec<(String, Arc<dyn ObjectSink>)> = Vec::new();
    for t in cfg.storage.upload_targets() {
        let sink: Arc<dyn ObjectSink> = if t.provider == "local" {
            Arc::new(DirSink::new(expand(&t.dir)))
        } else {
            let r = t.resolve_s3()?;
            let s3 = S3Config::from_env(&S3Settings {
                endpoint: r.endpoint,
                region: r.region,
                bucket: t.bucket.clone(),
                prefix: t.prefix.clone(),
                path_style: r.path_style,
                access_key_env: t.access_key_env.clone(),
                secret_key_env: t.secret_key_env.clone(),
            })
            .with_context(|| format!("storage target {:?} ({})", t.name, t.provider))?;
            Arc::new(S3Sink::new(s3)?)
        };
        out.push((t.name.clone(), sink));
    }
    Ok(out)
}

/// Remote upload targets only, fanned out; `None` when there are none.
///
/// # Errors
///
/// Fails when a target is misconfigured or its sink cannot be built.
pub fn remote_sink(cfg: &Config) -> anyhow::Result<Option<Arc<dyn ObjectSink>>> {
    let sinks: Vec<_> = upload_sinks(cfg)?.into_iter().map(|(_, s)| s).collect();
    Ok(match sinks.len() {
        0 => None,
        1 => sinks.into_iter().next(),
        _ => Some(Arc::new(MultiSink::new(sinks)?)),
    })
}

/// Publish destination: the local directory plus every upload target.
///
/// # Errors
///
/// Fails when a target is misconfigured or its sink cannot be built.
pub fn sink(cfg: &Config) -> anyhow::Result<Arc<dyn ObjectSink>> {
    let local: Arc<dyn ObjectSink> = Arc::new(DirSink::new(expand(&cfg.storage.local_dir)));
    Ok(match remote_sink(cfg)? {
        None => local,
        Some(remote) => Arc::new(MultiSink::new(vec![local, remote])?),
    })
}

/// One line per destination, for `catalog create` and logs.
pub fn describe_storage(cfg: &Config) -> String {
    let mut parts = vec![format!(
        "local {}",
        expand(&cfg.storage.local_dir).display()
    )];
    for t in cfg.storage.upload_targets() {
        parts.push(format!("{} ({})", t.name, t.provider));
    }
    parts.join(", ")
}

/// Build the file backend from config. With a signer, the upload sink is
/// attached as well.
///
/// # Errors
///
/// Fails when the pinned key, the catalog name, or a sink is invalid.
pub fn file_catalog(cfg: &Config, signer: Option<SecretKey>) -> anyhow::Result<FileCatalog> {
    let mut fc = FileCatalogConfig::new(&cfg.catalog.name, &cfg.file.base_url, pinned_key(cfg)?);
    // Signed mirror lists, file.mirror_urls and storage public URLs, by
    // priority (spec 6.12). Mirrors are untrusted; everything is verified.
    fc.mirror_urls = crate::mirrors::catalog_bases(cfg);
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
///
/// # Errors
///
/// Fails when the Chia settings are incomplete or the client cannot be built.
pub fn chia(cfg: &Config) -> anyhow::Result<ChiaDataLayer> {
    let c = &cfg.chia;
    let cc = ChiaConfig {
        rpc_url: c.rpc_url.clone(),
        ssl_cert: expand(&c.ssl_cert),
        ssl_key: expand(&c.ssl_key),
        ca_cert: c.ca_cert.as_deref().map(expand),
        fee_mojos: c.fee_mojos,
    };
    // The DataLayer RPC itself is always HTTPS with mutual TLS. A plain
    // http:// URL is only accepted on the loopback interface (tests, or a
    // local proxy that adds the client certificate).
    let mut chia = if let Some(rest) = c.rpc_url.strip_prefix("http://") {
        let host = rest.split(['/', ':']).next().unwrap_or("");
        if !["127.0.0.1", "localhost", "[::1]"].contains(&host) && !rest.starts_with("[::1]") {
            bail!("chia.rpc_url: plain http:// is only allowed for localhost");
        }
        ChiaDataLayer::with_client(cc, reqwest::Client::new())
    } else {
        ChiaDataLayer::new(cc)?
    };
    chia = chia.with_catalog(&cfg.catalog.name, pinned_key(cfg)?);
    chia = chia.with_confirm_policy(c.confirm_poll_secs, c.confirm_timeout_secs);
    Ok(chia)
}

fn chia_anchor_path(cfg: &Config, id: &CatalogId) -> std::path::PathBuf {
    expand(&cfg.file.state_dir)
        .join(&cfg.catalog.name)
        .join(format!("chia-{}.json", id.0))
}

/// The last accepted on-chain state for a Chia store, if any.
pub fn load_chia_anchor(cfg: &Config, id: &CatalogId) -> Option<ChiaAnchor> {
    let raw = std::fs::read(chia_anchor_path(cfg, id)).ok()?;
    serde_json::from_slice(&raw).ok()
}

/// Persist the accepted on-chain state for a Chia store (atomic rename).
///
/// # Errors
///
/// Fails when the state file cannot be written.
pub fn save_chia_anchor(cfg: &Config, anchor: &ChiaAnchor) -> anyhow::Result<()> {
    let path = chia_anchor_path(cfg, &CatalogId(anchor.store_id.clone()));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(anchor)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
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
///
/// # Errors
///
/// Fails on an unknown backend, a bad config, or a sync or chain check error.
pub async fn open_reader(
    cfg: &Config,
    name: Option<&str>,
    no_wait: bool,
) -> anyhow::Result<(Backend, CatalogId)> {
    match name.unwrap_or(&cfg.catalog.backend) {
        "file" => {
            crate::mirrors::refresh(cfg, no_wait).await;
            let cat = file_catalog(cfg, None)?;
            if !no_wait {
                // Gated by the chain check (catalog.chain_check, spec 6.14).
                let r = crate::chain::sync_checked(cfg, &cat)
                    .await
                    .context("syncing the file catalog (use --no-wait to use the local copy)")?;
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
            // Rollback and fork check against the last accepted on-chain
            // state, then advance the anchor (spec section 6.13).
            let prev = load_chia_anchor(cfg, &id);
            let anchor = c
                .check_anchor(&id, prev.as_ref())
                .await
                .context("checking the DataLayer store against the local anchor")?;
            if prev.as_ref() != Some(&anchor) {
                save_chia_anchor(cfg, &anchor)?;
            }
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
///
/// # Errors
///
/// Fails when the entry cannot be read or does not verify.
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
