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
        CatalogCmd::Create { no_wait } => create(&cfg()?, *no_wait).await,
        CatalogCmd::Subscribe { url } => {
            let cfg = cfg()?;
            if cfg.catalog.backend != "chia" {
                bail!("subscribe is for the Chia backend; file catalogs sync from [file] base_url");
            }
            let c = chia(&cfg)?;
            let id = store_id(&cfg)?;
            let urls = if url.is_empty() {
                cfg.chia.catalog_mirror_urls.clone()
            } else {
                url.clone()
            };
            c.subscribe(&id, &urls).await?;
            println!("subscribed to DataLayer store {id}");
            if !urls.is_empty() {
                println!("extra mirrors: {}", urls.join(", "));
            }
            println!(
                "Mirrors advertised on chain are added automatically. Check progress with `tensorseed catalog info`."
            );
            Ok(ExitCode::SUCCESS)
        }
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
            let (cat, _) = publisher_file_catalog(&cfg).await?;
            let head = cat.resign_head().await?;
            println!(
                "head re-signed: revision {} root {} expires at {}",
                head.revision,
                head.root,
                tensorseed_core::time::rfc3339(head.expires_unix)
            );
            Ok(ExitCode::SUCCESS)
        }
        CatalogCmd::Anchor => anchor(&cfg()?).await,
        CatalogCmd::ChainStatus { no_wait } => chain_status(&cfg()?, *no_wait).await,
        CatalogCmd::Snapshot => {
            let cfg = cfg()?;
            let (cat, _) = publisher_file_catalog(&cfg).await?;
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
) -> anyhow::Result<(
    tensorseed_catalog::file::FileCatalog,
    Option<crate::chain::ChainWriter>,
)> {
    if cfg.catalog.backend != "file" {
        bail!("this command is for the file backend (catalog.backend = \"file\")");
    }
    let sk = load_secret(cfg)?;
    let chain = crate::chain::ChainWriter::open(cfg, &sk)?;
    let cat = file_catalog(cfg, Some(sk))?;
    cat.sync_for_publish()
        .await
        .context("syncing before publishing")?;
    Ok((cat, chain))
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

async fn create(cfg: &Config, no_wait: bool) -> anyhow::Result<ExitCode> {
    match cfg.catalog.backend.as_str() {
        "file" => {
            let (cat, chain) = publisher_file_catalog(cfg).await?;
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
            let g = crate::chain::commit_file(&cat, &id, chain.as_ref()).await?;
            println!(
                "created file catalog {:?}: revision {} root {}",
                cfg.catalog.name, g.number, g.root
            );
            println!("written to {}", crate::backend::describe_storage(cfg));
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
            println!("Set chia.catalog_store_id = \"{id}\" in your config.");
            if no_wait {
                println!(
                    "The store must confirm on chain before the first import or publish (about one to ten minutes)."
                );
            } else {
                println!("waiting for the store to confirm on chain...");
                c.wait_store_ready(&id).await?;
                println!("store confirmed; run `tensorseed catalog import` or publish next.");
            }
            Ok(ExitCode::SUCCESS)
        }
        other => bail!("unknown backend {other:?}"),
    }
}

async fn anchor(cfg: &Config) -> anyhow::Result<ExitCode> {
    let (cat, chain) = publisher_file_catalog(cfg).await?;
    let Some(chain) = chain else {
        bail!(
            "chia.catalog_store_id is not set; create the store first (see docs/CHIA-RUNBOOK.md)"
        );
    };
    let st = cat.state();
    if st.revision == 0 {
        bail!("the file catalog has nothing published yet");
    }
    let proof = chain.write(&st, &st, None).await?;
    cat.upload_chain_proof(proof.to_bytes()?).await?;
    println!(
        "Chia store {} anchors file revision {} root {}",
        chain.store(),
        st.revision,
        st.root
    );
    println!(
        "chain-proof.json written to {}",
        crate::backend::describe_storage(cfg)
    );
    Ok(ExitCode::SUCCESS)
}

async fn chain_status(cfg: &Config, no_wait: bool) -> anyhow::Result<ExitCode> {
    if cfg.catalog.backend != "file" {
        bail!("chain-status checks the file catalog (catalog.backend = \"file\")");
    }
    if cfg.chia.catalog_store_id.trim().is_empty() {
        bail!("chia.catalog_store_id is not set; there is no chain to check against");
    }
    crate::mirrors::refresh(cfg, no_wait).await;
    let cat = file_catalog(cfg, None)?;
    if !no_wait {
        // Plain sync: this command reports, it does not gate.
        if let Err(e) = cat.sync().await {
            println!("file sync: {e}");
        }
    }
    let st = cat.state();
    println!("mode:       {:?}", cfg.chain_check());
    println!("source:     {}", cfg.chia.chain_source);
    println!("store:      {}", cfg.chia.catalog_store_id);
    println!("file:       revision {} root {}", st.revision, st.root);
    let r = match crate::chain::chain_anchor(cfg, &cat).await {
        Ok(r) => r,
        Err(e) => {
            println!("chain:      FAILED: {e:#}");
            return Ok(ExitCode::FAILURE);
        }
    };
    let a = &r.proof.anchor;
    println!("chain:      revision {} root {}", a.revision, a.root);
    println!("datalayer:  root {} coin {}", r.proof.root, r.proof.coin_id);
    println!(
        "coin:       {:?} (created at height {})",
        r.state, r.confirmed_block_index
    );
    println!("proof from: {}", r.via);
    if r.endpoints.is_empty() {
        println!("checked by: local DataLayer node (trusted)");
    } else {
        println!("checked by: {}", r.endpoints.join(", "));
    }
    if a.matches(st.revision, &st.root) {
        println!("result:     OK, the file catalog matches the chain");
        Ok(ExitCode::SUCCESS)
    } else {
        println!("result:     MISMATCH");
        Ok(ExitCode::FAILURE)
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
            println!("chain:     {:?}", cfg.chain_check());
            if let Some(c) = crate::chain::load_status(cfg) {
                println!(
                    "anchored:  revision {} in Chia store {} (coin {}, checked {})",
                    c.revision,
                    c.store_id,
                    c.coin_state,
                    tensorseed_core::time::rfc3339(c.checked_unix)
                );
            }
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
            if let Some(a) = crate::backend::load_chia_anchor(cfg, &id) {
                println!("anchor:    generation {} root {}", a.generation, a.root);
            }
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
    // The chain anchor (spec 6.14) describes a file catalog revision; it is
    // never imported as an entry. Older Chia exports may contain it.
    let entries: Vec<_> = parsed
        .entries
        .iter()
        .filter(|e| !tensorseed_catalog::anchor::is_reserved_key(&e.key))
        .cloned()
        .collect();
    if entries.is_empty() {
        bail!("bundle has no entries");
    }
    match backend.unwrap_or(&cfg.catalog.backend) {
        "file" => {
            let mut c = cfg.clone();
            c.catalog.backend = "file".into();
            let (cat, chain) = publisher_file_catalog(&c).await?;
            let id = CatalogId(cfg.catalog.name.clone());
            cat.stage_signed(&entries)?;
            let g = crate::chain::commit_file(&cat, &id, chain.as_ref()).await?;
            println!(
                "imported into the file catalog: revision {} root {}",
                g.number, g.root
            );
        }
        "chia" => {
            let c = chia(cfg)?;
            let id = store_id(cfg)?;
            let changed = c.stage_signed(&id, &entries).await?;
            let p = c.commit(&id).await?;
            match &p.tx_id {
                Some(tx) => println!(
                    "staged {changed} changed entries; submitted root (tx {tx}); waiting for confirmation"
                ),
                None if changed == 0 && p.previous_root.is_none() => {
                    println!("every entry is already in the store; nothing to submit")
                }
                None => println!("a root is already submitted; waiting for confirmation"),
            }
            let g = c.wait_confirmed(&p).await?;
            println!(
                "DataLayer store {id}: generation {} root {}",
                g.number, g.root
            );
        }
        other => bail!("unknown backend {other:?}"),
    }
    Ok(ExitCode::SUCCESS)
}
