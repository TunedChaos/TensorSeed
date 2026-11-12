// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! `catalog` subcommands.

use std::process::ExitCode;

use anyhow::{Context, bail};
use tensorseed_catalog::export::{ExportBundle, export};
use tensorseed_catalog::{CatalogBackend, CatalogId, CatalogOp};
use tensorseed_core::keys::catalog_info_key;
use tensorseed_core::manifest::{CatalogInfo, Publisher, SCHEMA_CATALOG};

use crate::backend::{Backend, chia, file_catalog, open_reader, pinned_key};
use crate::cli::{BackendArg, CatalogCmd};
use crate::config::Config;
use crate::keys::{PASSWORD_ENV, default_key_dir, keygen, load_secret};

/// Run a `catalog` subcommand. `cfg` is loaded lazily because `keygen`
/// works without a config file.
pub async fn run(
    cmd: &CatalogCmd,
    cfg: impl FnOnce() -> anyhow::Result<Config>,
) -> anyhow::Result<ExitCode> {
    match cmd {
        CatalogCmd::Keygen {
            out_dir,
            name,
            no_password,
            force,
        } => {
            let dir = out_dir.clone().unwrap_or_else(default_key_dir);
            let (sec, pubp, sk) = keygen(&dir, name, *no_password, *force)?;
            let pk = sk.public_key();
            println!(
                "secret key: {}{}",
                sec.display(),
                if *no_password { " (UNENCRYPTED)" } else { "" }
            );
            println!("public key: {}", pubp.display());
            println!("key id:     {}", pk.key_id_hex());
            println!();
            println!("Add to your config:");
            println!("  [catalog]");
            println!("  publisher_pubkey = \"{}\"", pk.to_base64());
            println!("  publisher_seckey_file = \"{}\"", sec.display());
            if !*no_password {
                println!();
                println!(
                    "The password is read from {PASSWORD_ENV} or prompted for; it is not stored."
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        CatalogCmd::Create => create(&cfg()?).await,
        CatalogCmd::Info { no_wait } => info(&cfg()?, *no_wait).await,
        CatalogCmd::AddMirror { url, amount, fee } => {
            let cfg = cfg()?;
            if cfg.catalog.backend != "chia" {
                bail!(
                    "the file backend has no on-chain mirror discovery; list mirrors in [file] mirror_urls"
                );
            }
            let c = chia(&cfg)?;
            let id = store_id(&cfg)?;
            c.add_mirror_with(
                &id,
                url,
                amount.unwrap_or(cfg.chia.mirror_amount_mojos),
                fee.unwrap_or(cfg.chia.fee_mojos),
            )
            .await?;
            println!("mirror advertised for store {}: {}", id, url.join(", "));
            Ok(ExitCode::SUCCESS)
        }
        CatalogCmd::Export {
            out,
            backend,
            no_wait,
        } => {
            let cfg = cfg()?;
            let pk = pinned_key(&cfg)?;
            let (b, id) = open_reader(&cfg, backend.map(BackendArg::as_str), *no_wait).await?;
            let bundle = export(b.as_dyn(), &id, &cfg.catalog.name, &pk).await?;
            let tmp = out.with_extension("tmp");
            std::fs::write(&tmp, serde_json::to_vec_pretty(&bundle)?)?;
            std::fs::rename(&tmp, out)?;
            println!(
                "exported {} entries from {} ({}) to {}",
                bundle.entries.len(),
                bundle.source_kind,
                bundle.source_id,
                out.display()
            );
            Ok(ExitCode::SUCCESS)
        }
        CatalogCmd::Import {
            bundle,
            backend,
            dry_run,
        } => {
            let cfg = cfg()?;
            import(&cfg, bundle, backend.map(BackendArg::as_str), *dry_run).await
        }
        CatalogCmd::ResignHead => {
            let cfg = cfg()?;
            let cat = publisher_file_catalog(&cfg).await?;
            let head = cat.resign_head().await?;
            println!(
                "head re-signed: revision {} root {} expires at {}",
                head.revision,
                head.root,
                tensorseed_core::time::rfc3339(head.expires_unix)
            );
            Ok(ExitCode::SUCCESS)
        }
        CatalogCmd::Snapshot => {
            let cfg = cfg()?;
            let cat = publisher_file_catalog(&cfg).await?;
            let s = cat.snapshot_now().await?;
            println!(
                "snapshot written: revision {} at {} (sha256 {})",
                s.revision, s.path, s.sha256
            );
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn store_id(cfg: &Config) -> anyhow::Result<CatalogId> {
    if cfg.chia.catalog_store_id.is_empty() {
        bail!("chia.catalog_store_id is not set (run `tensorseed catalog create` first)");
    }
    Ok(CatalogId(cfg.chia.catalog_store_id.clone()))
}

async fn publisher_file_catalog(
    cfg: &Config,
) -> anyhow::Result<tensorseed_catalog::file::FileCatalog> {
    if cfg.catalog.backend != "file" {
        bail!("this command is for the file backend (catalog.backend = \"file\")");
    }
    let cat = file_catalog(cfg, Some(load_secret(cfg)?))?;
    cat.sync_for_publish()
        .await
        .context("syncing before publishing")?;
    Ok(cat)
}

fn catalog_info(cfg: &Config) -> CatalogInfo {
    let bases = cfg.storage.webseed_bases();
    CatalogInfo {
        schema: SCHEMA_CATALOG.into(),
        name: cfg.catalog.name.clone(),
        publisher: Publisher {
            display_name: cfg.catalog.publisher_name.clone(),
            contact: cfg.catalog.publisher_contact.clone(),
            policy_url: cfg.catalog.publisher_policy_url.clone(),
        },
        webseed_bases: bases,
        trackers: cfg.torrent.trackers.clone(),
        created_at: tensorseed_core::time::now_rfc3339(),
    }
}

async fn create(cfg: &Config) -> anyhow::Result<ExitCode> {
    match cfg.catalog.backend.as_str() {
        "file" => {
            let cat = publisher_file_catalog(cfg).await?;
            if cat.state().revision != 0 {
                bail!(
                    "catalog {:?} already exists at revision {}",
                    cfg.catalog.name,
                    cat.state().revision
                );
            }
            let id = CatalogId(cfg.catalog.name.clone());
            let value = tensorseed_core::canonical::to_canonical_vec(&catalog_info(cfg))?;
            cat.stage(
                &id,
                &[CatalogOp::Upsert {
                    key: catalog_info_key(),
                    value,
                }],
            )
            .await?;
            let p = cat.commit(&id).await?;
            let g = cat.wait_confirmed(&p).await?;
            println!(
                "created file catalog {:?}: revision {} root {}",
                cfg.catalog.name, g.number, g.root
            );
            println!("written to {}", crate::backend::sink(cfg)?.describe());
            Ok(ExitCode::SUCCESS)
        }
        "chia" => {
            if !cfg.chia.catalog_store_id.is_empty() {
                bail!(
                    "chia.catalog_store_id is already set ({})",
                    cfg.chia.catalog_store_id
                );
            }
            let c = chia(cfg)?;
            let id = c.create_catalog().await?;
            println!("created DataLayer store {id}");
            println!(
                "Set chia.catalog_store_id = \"{id}\" in your config. Once the store's first root"
            );
            println!("confirms, the catalog info entry is written by the first import or publish.");
            Ok(ExitCode::SUCCESS)
        }
        other => bail!("unknown backend {other:?}"),
    }
}

async fn info(cfg: &Config, no_wait: bool) -> anyhow::Result<ExitCode> {
    let pk = pinned_key(cfg)?;
    println!("catalog:   {}", cfg.catalog.name);
    println!("publisher: {} (key id {})", pk.to_base64(), pk.key_id_hex());
    let (b, id) = open_reader(cfg, None, no_wait).await?;
    match &b {
        Backend::File(f) => {
            let st = f.state();
            println!("backend:   file");
            println!("revision:  {}", st.revision);
            println!("root:      {}", st.root);
            println!("entries:   {}", st.entries.len());
            if let Some(s) = f.last_snapshot() {
                println!("snapshot:  revision {} ({})", s.revision, s.path);
            }
            println!("sources:   {}", f.sources().join(", "));
        }
        Backend::Chia(c) => {
            let st = c.sync_status(&id).await?;
            let (root, confirmed) = c.root(&id).await?;
            println!("backend:   chia ({id})");
            println!(
                "generation: {} of {}",
                st.local_generation, st.target_generation
            );
            println!(
                "root:      {root}{}",
                if confirmed { "" } else { " (unconfirmed)" }
            );
        }
    }
    if let Some(e) =
        crate::backend::read_verified(b.as_dyn(), &id, &catalog_info_key(), &pk, &cfg.catalog.name)
            .await?
    {
        let ci: CatalogInfo = serde_json::from_str(&e.value)?;
        if !ci.publisher.display_name.is_empty() {
            println!(
                "published by: {} <{}>",
                ci.publisher.display_name, ci.publisher.contact
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

async fn import(
    cfg: &Config,
    bundle: &std::path::Path,
    backend: Option<&str>,
    dry_run: bool,
) -> anyhow::Result<ExitCode> {
    let pinned =
        pinned_key(cfg).context("catalog.publisher_pubkey must be set to verify a bundle")?;
    let parsed: ExportBundle = serde_json::from_slice(&std::fs::read(bundle)?)?;
    if parsed.catalog != cfg.catalog.name {
        bail!(
            "bundle is for catalog {:?}, config catalog.name is {:?}",
            parsed.catalog,
            cfg.catalog.name
        );
    }
    parsed.verify(&pinned)?;
    println!(
        "ok: {} entries from {} ({}) verify against pinned key {}",
        parsed.entries.len(),
        parsed.source_kind,
        parsed.source_id,
        pinned.key_id_hex()
    );
    if dry_run {
        return Ok(ExitCode::SUCCESS);
    }
    if parsed.entries.is_empty() {
        bail!("bundle has no entries");
    }
    match backend.unwrap_or(&cfg.catalog.backend) {
        "file" => {
            let mut c = cfg.clone();
            c.catalog.backend = "file".into();
            let cat = publisher_file_catalog(&c).await?;
            let id = CatalogId(cfg.catalog.name.clone());
            cat.stage_signed(&parsed.entries)?;
            let p = cat.commit(&id).await?;
            let g = cat.wait_confirmed(&p).await?;
            println!(
                "imported into the file catalog: revision {} root {}",
                g.number, g.root
            );
        }
        "chia" => {
            let c = chia(cfg)?;
            let id = store_id(cfg)?;
            c.stage_signed(&id, &parsed.entries).await?;
            let p = c.commit(&id).await?;
            println!(
                "submitted root (tx {}); waiting for confirmation",
                p.tx_id.as_deref().unwrap_or("?")
            );
            let g = c.wait_confirmed(&p).await?;
            println!(
                "imported into DataLayer store {id}: generation {} root {}",
                g.number, g.root
            );
        }
        other => bail!("unknown backend {other:?}"),
    }
    Ok(ExitCode::SUCCESS)
}
