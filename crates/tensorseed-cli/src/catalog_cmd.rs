// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! `catalog` subcommands.

use std::path::Path;
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
    config_path: &Path,
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
            println!(
                "  publisher_seckey_file = {}",
                toml_string(&sec.to_string_lossy())
            );
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
        CatalogCmd::Info { no_wait, catalog } => info(&cfg()?, *no_wait, catalog.as_deref()).await,
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
            catalog,
        } => {
            let cfg = cfg()?;
            let (view, default_backend) = export_view(&cfg, catalog.as_deref())?;
            let pk = pinned_key(&view)?;
            let (b, id) = open_reader(
                &view,
                backend.map(BackendArg::as_str).or(default_backend),
                *no_wait,
            )
            .await?;
            let bundle = export(b.as_dyn(), &id, &view.catalog.name, &pk).await?;
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
        CatalogCmd::UpdateInfo { dry_run } => update_info(&cfg()?, *dry_run).await,
        CatalogCmd::Anchor => anchor(&cfg()?).await,
        CatalogCmd::ChainStatus { no_wait, catalog } => {
            chain_status(&cfg()?, *no_wait, catalog.as_deref()).await
        }
        CatalogCmd::List => list_subscriptions(&cfg()?).await,
        CatalogCmd::Add {
            name,
            publisher_pubkey,
            base_url,
            store_id,
            label,
            chain_check,
            priority,
        } => add_subscription(
            config_path,
            name,
            publisher_pubkey,
            base_url,
            store_id.as_deref(),
            label.as_deref(),
            chain_check.as_deref(),
            *priority,
        ),
        CatalogCmd::Remove { name } => remove_subscription(config_path, name),
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

/// The config view `catalog export` reads: the named subscription, or the
/// default one when `[[catalogs]]` is used, else the top-level catalog.
/// A catalog added with `catalog add` is a file catalog, so its backend
/// defaults to `file` whatever `catalog.backend` says.
fn export_view(
    cfg: &Config,
    catalog: Option<&str>,
) -> anyhow::Result<(Config, Option<&'static str>)> {
    if catalog.is_none() && cfg.catalogs.is_empty() {
        return Ok((cfg.clone(), None));
    }
    let sub = cfg.subscription(catalog)?;
    let backend = if sub.is_default { None } else { Some("file") };
    Ok((cfg.for_subscription(&sub), backend))
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

/// How `catalog info` shows a catalog's publisher. Empty fields are named
/// as missing instead of printed as blanks (an empty contact used to print
/// as `<>`).
fn publisher_lines(p: &Publisher) -> Vec<String> {
    let or_none = |v: &str, what: &str| {
        if v.trim().is_empty() {
            format!("none published ({what})")
        } else {
            v.to_string()
        }
    };
    vec![
        format!(
            "published by: {}",
            or_none(&p.display_name, "catalog.publisher_name")
        ),
        format!(
            "contact:   {}",
            or_none(&p.contact, "catalog.publisher_contact")
        ),
        format!(
            "policy:    {}",
            or_none(&p.policy_url, "catalog.publisher_policy_url")
        ),
    ]
}

/// Warnings for publisher fields a catalog should not publish empty:
/// SPEC section 11 needs a contact and a policy URL for takedown requests.
fn publisher_warnings(p: &Publisher) -> Vec<String> {
    let mut out = Vec::new();
    for (v, key, why) in [
        (
            &p.display_name,
            "publisher_name",
            "who publishes the catalog",
        ),
        (
            &p.contact,
            "publisher_contact",
            "where takedown requests go (SPEC section 11)",
        ),
        (
            &p.policy_url,
            "publisher_policy_url",
            "your mirroring and takedown policy (SPEC section 11)",
        ),
    ] {
        if v.trim().is_empty() {
            out.push(format!(
                "warning: catalog.{key} is empty; set it to say {why}, then run `tensorseed catalog update-info`"
            ));
        }
    }
    out
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
            let info = catalog_info(cfg);
            for w in publisher_warnings(&info.publisher) {
                eprintln!("{w}");
            }
            let value = tensorseed_core::canonical::to_canonical_vec(&info)?;
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

async fn update_info(cfg: &Config, dry_run: bool) -> anyhow::Result<ExitCode> {
    if cfg.catalog.backend != "file" {
        bail!("this command is for the file backend (catalog.backend = \"file\")");
    }
    let pk = pinned_key(cfg)?;
    let (b, id) = open_reader(cfg, None, false).await?;
    let current =
        crate::backend::read_verified(b.as_dyn(), &id, &catalog_info_key(), &pk, &cfg.catalog.name)
            .await?
            .map(|e| serde_json::from_str::<CatalogInfo>(&e.value))
            .transpose()
            .context("parsing the current ts/v1/catalog value")?;
    let Some(current) = current else {
        bail!(
            "catalog {:?} has no ts/v1/catalog entry; run `tensorseed catalog create` first",
            cfg.catalog.name
        );
    };
    let mut next = catalog_info(cfg);
    next.created_at = current.created_at.clone();
    let cur_bytes = tensorseed_core::canonical::to_canonical_vec(&current)?;
    let value = tensorseed_core::canonical::to_canonical_vec(&next)?;
    println!("current:  {}", String::from_utf8_lossy(&cur_bytes));
    println!("proposed: {}", String::from_utf8_lossy(&value));
    for w in publisher_warnings(&next.publisher) {
        eprintln!("{w}");
    }
    if cur_bytes == value {
        println!("no change; nothing to publish");
        return Ok(ExitCode::SUCCESS);
    }
    if dry_run {
        println!("dry run: nothing signed or uploaded");
        return Ok(ExitCode::SUCCESS);
    }
    let (cat, chain) = publisher_file_catalog(cfg).await?;
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
        "updated catalog info for {:?}: revision {} root {}",
        cfg.catalog.name, g.number, g.root
    );
    println!("written to {}", crate::backend::describe_storage(cfg));
    Ok(ExitCode::SUCCESS)
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

async fn chain_status(
    cfg: &Config,
    no_wait: bool,
    catalog: Option<&str>,
) -> anyhow::Result<ExitCode> {
    let subs = if let Some(name) = catalog {
        vec![cfg.subscription(Some(name))?]
    } else {
        cfg.subscriptions()?
    };
    if subs.is_empty() {
        bail!("no catalog is configured");
    }
    if subs.len() == 1 && catalog.is_none() && cfg.catalogs.is_empty() {
        return chain_status_one(cfg, no_wait).await;
    }
    let mut failed = false;
    for (i, sub) in subs.iter().enumerate() {
        if i > 0 {
            println!();
        }
        println!("catalog:    {} ({})", sub.display(), sub.name);
        let view = cfg.for_subscription(sub);
        match chain_status_one(&view, no_wait).await {
            Ok(code) if code == ExitCode::SUCCESS => {}
            Ok(_) => failed = true,
            Err(e) => {
                println!("chain:      FAILED: {e:#}");
                failed = true;
            }
        }
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

async fn chain_status_one(cfg: &Config, no_wait: bool) -> anyhow::Result<ExitCode> {
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

async fn info(cfg: &Config, no_wait: bool, catalog: Option<&str>) -> anyhow::Result<ExitCode> {
    if catalog.is_some() || !cfg.catalogs.is_empty() {
        let sub = cfg.subscription(catalog)?;
        let view = cfg.for_subscription(&sub);
        return info_one(&view, no_wait).await;
    }
    info_one(cfg, no_wait).await
}

async fn info_one(cfg: &Config, no_wait: bool) -> anyhow::Result<ExitCode> {
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
        for l in publisher_lines(&ci.publisher) {
            println!("{l}");
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

async fn list_subscriptions(cfg: &Config) -> anyhow::Result<ExitCode> {
    let subs = cfg.subscriptions()?;
    if subs.is_empty() {
        println!("no catalogs configured");
        return Ok(ExitCode::SUCCESS);
    }
    let mut failed = false;
    for sub in &subs {
        let view = cfg.for_subscription(sub);
        let def = if sub.is_default { "  (default)" } else { "" };
        print!(
            "{}  {}  priority {}  chain {:?}{def}",
            sub.name,
            if sub.label.is_empty() {
                "-"
            } else {
                sub.label.as_str()
            },
            sub.priority,
            sub.chain_check
        );
        match open_reader(&view, None, true).await {
            Ok((Backend::File(f), _)) => {
                let st = f.state();
                println!("  revision {}", st.revision);
            }
            Ok((Backend::Chia(_), id)) => {
                println!("  store {id}");
            }
            Err(e) => {
                println!("  error: {e:#}");
                failed = true;
            }
        }
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn toml_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

#[allow(clippy::too_many_arguments)] // one argument per `catalog add` flag
fn add_subscription(
    path: &Path,
    name: &str,
    publisher_pubkey: &str,
    base_url: &str,
    store_id: Option<&str>,
    label: Option<&str>,
    chain_check: Option<&str>,
    priority: Option<i64>,
) -> anyhow::Result<ExitCode> {
    tensorseed_catalog::signed::check_catalog_name(name)?;
    tensorseed_catalog::minisign::PublicKey::parse(publisher_pubkey).context("publisher_pubkey")?;
    if !(base_url.starts_with("https://") || base_url.starts_with("http://")) {
        bail!("base_url must be an http(s) URL");
    }
    if let Some(m) = chain_check
        && !["strict", "warn", "off"].contains(&m)
    {
        bail!("chain_check must be \"strict\", \"warn\", or \"off\"");
    }
    let mut body = String::new();
    if path.exists() {
        body =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        if !body.is_empty() && !body.ends_with('\n') {
            body.push('\n');
        }
    } else if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // If this name already exists with a different key, say so after load.
    if path.exists()
        && let Ok(cfg) = crate::config::load(path)
        && let Ok(subs) = cfg.subscriptions()
        && let Some(s) = subs.iter().find(|s| s.name == name)
        && s.publisher_pubkey != publisher_pubkey
    {
        bail!(
            "catalog {name:?} is already subscribed with a different publisher key; remove it first"
        );
    }
    body.push('\n');
    body.push_str("[[catalogs]]\n");
    body.push_str(&format!("name = {}\n", toml_string(name)));
    if let Some(l) = label.filter(|s| !s.is_empty()) {
        body.push_str(&format!("label = {}\n", toml_string(l)));
    }
    body.push_str(&format!(
        "publisher_pubkey = {}\n",
        toml_string(publisher_pubkey)
    ));
    if let Some(id) = store_id.filter(|s| !s.is_empty()) {
        body.push_str(&format!("store_id = {}\n", toml_string(id)));
    }
    body.push_str(&format!("base_url = {}\n", toml_string(base_url)));
    if let Some(m) = chain_check {
        body.push_str(&format!("chain_check = {}\n", toml_string(m)));
    }
    if let Some(p) = priority {
        body.push_str(&format!("priority = {p}\n"));
    }
    std::fs::write(path, body).with_context(|| format!("writing {}", path.display()))?;
    println!("added catalog {name} to {}", path.display());
    Ok(ExitCode::SUCCESS)
}

fn remove_subscription(path: &Path, name: &str) -> anyhow::Result<ExitCode> {
    if !path.exists() {
        bail!("config file {} does not exist", path.display());
    }
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let (new_text, removed) = strip_catalogs_table(&text, name)?;
    if !removed {
        // Might be the top-level [catalog] only.
        if let Ok(cfg) = crate::config::load(path)
            && cfg.catalog.name == name
            && cfg.catalogs.iter().all(|c| c.name != name)
        {
            bail!(
                "catalog {name:?} is the top-level [catalog] block; edit {} to remove it",
                path.display()
            );
        }
        bail!("no [[catalogs]] entry named {name:?} in {}", path.display());
    }
    std::fs::write(path, new_text).with_context(|| format!("writing {}", path.display()))?;
    println!("removed catalog {name} from {}", path.display());
    println!("verified file catalog state on disk was kept");
    Ok(ExitCode::SUCCESS)
}

/// Drop every `[[catalogs]]` table whose `name` equals `want`.
fn strip_catalogs_table(text: &str, want: &str) -> anyhow::Result<(String, bool)> {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let mut out = String::new();
    let mut i = 0;
    let mut removed = false;
    while i < lines.len() {
        let trim = lines[i].trim();
        if trim == "[[catalogs]]" {
            let start = i;
            i += 1;
            while i < lines.len() {
                let t = lines[i].trim();
                if t.starts_with('[') {
                    break;
                }
                i += 1;
            }
            let block = lines[start..i].join("");
            let is_match = block.lines().any(|l| {
                let l = l.trim();
                l.starts_with("name")
                    && l.split_once('=')
                        .is_some_and(|(_, v)| unquote(v.trim()) == want)
            });
            if is_match {
                removed = true;
                continue;
            }
            out.push_str(&block);
            continue;
        }
        out.push_str(lines[i]);
        i += 1;
    }
    Ok((out, removed))
}

fn unquote(v: &str) -> String {
    let v = v.trim().trim_end_matches(',').trim();
    if let Some(s) = v.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        s.replace("\\\"", "\"").replace("\\\\", "\\")
    } else {
        v.to_string()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn empty_publisher_fields_are_named_not_blank() {
        use super::{Publisher, publisher_lines, publisher_warnings};
        let empty = Publisher {
            display_name: "Example Publisher".into(),
            contact: String::new(),
            policy_url: " ".into(),
        };
        let lines = publisher_lines(&empty);
        assert_eq!(lines[0], "published by: Example Publisher");
        assert_eq!(
            lines[1],
            "contact:   none published (catalog.publisher_contact)"
        );
        assert!(lines[2].contains("none published"));
        assert!(lines.iter().all(|l| !l.contains("<>")));
        let w = publisher_warnings(&empty);
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w[0].contains("catalog.publisher_contact"));
        assert!(w[1].contains("catalog.publisher_policy_url"));
        let full = Publisher {
            display_name: "Example Publisher".into(),
            contact: "takedown@example.org".into(),
            policy_url: "https://example.org/policy".into(),
        };
        assert!(publisher_warnings(&full).is_empty());
        assert_eq!(publisher_lines(&full)[1], "contact:   takedown@example.org");
    }

    use super::toml_string;

    #[test]
    fn toml_string_round_trips_paths() {
        for p in [
            r"C:\Users\me\AppData\Roaming\tensorseed\keys\pub.key",
            "/home/me/.config/tensorseed/keys/pub.key",
            r"C:\Users\O'Neil\keys\pub.key",
            "/tmp/odd \"quoted\" dir/pub.key",
        ] {
            let doc = format!("publisher_seckey_file = {}", toml_string(p));
            let v: toml::Table = toml::from_str(&doc).unwrap_or_else(|e| panic!("{doc}: {e}"));
            assert_eq!(v["publisher_seckey_file"].as_str(), Some(p), "{doc}");
        }
    }
}
