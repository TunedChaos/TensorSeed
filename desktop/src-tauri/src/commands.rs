// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Tauri commands over `tensorseed-app`: the followed catalogs, one
//! catalog's entries, entry pages (manifest, signature and chain anchor
//! status, license, files, local copy), local copies, the `[torrent]`
//! settings, and the pull, verify, and seed actions.
//!
//! Results use camelCase field names to match `@tensorseed/ui` types
//! (`ui/src/source.ts`). Errors cross the bridge as display strings.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Serialize;
use tauri::{AppHandle, State};
use tensorseed_app::links::Route;
use tensorseed_app::pull::{
    catalog_manifest, default_out, parse_manifest_bytes, read_pull_record, tombstone_covers,
};
use tensorseed_core::keys::{RepoRef, RepoType};
use tensorseed_core::verify::META_DIR;

use crate::copies::CatalogRef;
use crate::jobs::Jobs;
use crate::seeder::Seeder;
use tensorseed_app::backend::{Backend, file_catalog, open_reader, pinned_key};
use tensorseed_app::config::{self, ChainCheck, Config, Subscription};
use tensorseed_catalog::CatalogId;
use tensorseed_core::keys::index_prefix;
use tensorseed_core::manifest::{IndexEntry, SCHEMA_INDEX};

/// Same lookup as the CLI: `TENSORSEED_CONFIG`, else the default path.
pub fn config_path() -> PathBuf {
    std::env::var_os("TENSORSEED_CONFIG")
        .filter(|v| !v.is_empty())
        .map_or_else(config::default_path, PathBuf::from)
}

/// Load and validate the shared config.
///
/// # Errors
///
/// Fails when the config does not load or validate.
pub fn load_config() -> Result<(PathBuf, Config), String> {
    let path = config_path();
    let cfg = config::load(&path)
        .with_context(|| format!("loading {}", path.display()))
        .map_err(|e| format!("{e:#}"))?;
    Ok((path, cfg))
}

fn mode_str(m: ChainCheck) -> &'static str {
    match m {
        ChainCheck::Strict => "strict",
        ChainCheck::Warn => "warn",
        ChainCheck::Off => "off",
    }
}

/// One followed catalog (`CatalogSummary` in `ui/src/source.ts`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogSummary {
    name: String,
    label: String,
    base_url: String,
    store_id: String,
    backend: String,
    chain_check: &'static str,
    priority: i64,
    is_default: bool,
    local_revision: Option<u64>,
    error: Option<String>,
}

fn summary(cfg: &Config, sub: &Subscription) -> CatalogSummary {
    let view = cfg.for_subscription(sub);
    let mut out = CatalogSummary {
        name: sub.name.clone(),
        label: sub.display().to_string(),
        base_url: sub.base_url.clone(),
        store_id: sub.store_id.clone(),
        backend: view.catalog.backend.clone(),
        chain_check: mode_str(sub.chain_check),
        priority: sub.priority,
        is_default: sub.is_default,
        local_revision: None,
        error: None,
    };
    if view.catalog.backend == "file" {
        // Local state only; no network until the user syncs.
        match file_catalog(&view, None) {
            Ok(cat) => {
                let rev = cat.state().revision;
                out.local_revision = (rev > 0).then_some(rev);
            }
            Err(e) => out.error = Some(format!("{e:#}")),
        }
    }
    out
}

/// `catalogs_list` result.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogsList {
    config_path: String,
    catalogs: Vec<CatalogSummary>,
}

/// List the followed catalogs from the shared config, default first. No
/// network requests.
///
/// # Errors
///
/// Fails when the config cannot be loaded or its catalogs do not resolve.
#[tauri::command]
pub async fn catalogs_list() -> Result<CatalogsList, String> {
    let (path, cfg) = load_config()?;
    let mut subs = cfg.subscriptions().map_err(|e| format!("{e:#}"))?;
    subs.sort_by_key(|s| (!s.is_default, s.priority, s.name.clone()));
    Ok(CatalogsList {
        config_path: path.display().to_string(),
        catalogs: subs.iter().map(|s| summary(&cfg, s)).collect(),
    })
}

/// Per entry signature result (`ListedEntry.signature`).
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum SignatureView {
    /// Verified against the pinned publisher key and catalog name.
    Ok {
        /// Always true.
        ok: bool,
    },
    /// Did not verify.
    Failed {
        /// Always false.
        ok: bool,
        /// Why.
        error: String,
    },
}

/// One index entry (`ListedEntry` in `ui/src/types.ts`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryView {
    id: String,
    repo_type: &'static str,
    index_key: String,
    latest_key: String,
    index: IndexEntry,
    /// Commercial serving mark for the index license (client side, not legal advice).
    commercial: tensorseed_core::commercial::Assessment,
    signature: SignatureView,
}

/// Chain anchor status (`ChainView` in `ui/src/source.ts`).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ChainView {
    /// `chain_check = "off"`.
    Off,
    /// The accepted revision and root match the last chain check.
    #[serde(rename_all = "camelCase")]
    Anchored {
        /// Chia store id.
        store_id: String,
        /// DataLayer root that includes the anchor.
        datalayer_root: String,
        /// Store coin.
        coin_id: String,
        /// `current`, `recently-spent`, or `stale`.
        coin_state: String,
        /// Where the proof came from.
        via: String,
        /// Unix time of the check.
        checked_unix: u64,
    },
    /// Entries read straight from a Chia DataLayer store.
    #[serde(rename_all = "camelCase")]
    Datalayer {
        /// Chia store id.
        store_id: String,
    },
    /// Not anchored for this revision.
    Unchecked {
        /// Why.
        reason: String,
    },
}

/// `entries_list` result (`CatalogEntries` in `ui/src/source.ts`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntries {
    catalog: CatalogSummary,
    revision: u64,
    root: String,
    synced: bool,
    sync_error: Option<String>,
    chain: ChainView,
    entries: Vec<EntryView>,
    /// Index values that did not parse (shown, never trusted).
    problems: Vec<String>,
}

fn chain_view(view: &Config, backend: &Backend, sync_error: Option<&str>) -> ChainView {
    let mode = view.chain_check();
    match backend {
        Backend::Chia(_) => ChainView::Datalayer {
            store_id: view.chia.catalog_store_id.clone(),
        },
        Backend::File(_) if mode == ChainCheck::Off => ChainView::Off,
        Backend::File(f) => {
            let st = f.state();
            match tensorseed_app::chain::load_status(view) {
                Some(c) if c.revision == st.revision && c.root == st.root => ChainView::Anchored {
                    store_id: c.store_id,
                    datalayer_root: c.datalayer_root,
                    coin_id: c.coin_id,
                    coin_state: c.coin_state,
                    via: c.via,
                    checked_unix: c.checked_unix,
                },
                Some(c) => ChainView::Unchecked {
                    reason: match sync_error {
                        Some(e) => format!("chain check failed: {e}"),
                        None => format!(
                            "the last chain check covered revision {}, not revision {}",
                            c.revision, st.revision
                        ),
                    },
                },
                None => ChainView::Unchecked {
                    reason: sync_error.map_or_else(
                        || "no chain check has been recorded for this catalog yet".to_string(),
                        |e| format!("chain check failed: {e}"),
                    ),
                },
            }
        }
    }
}

/// Show one catalog's entries. With `sync`, the file catalog is synced
/// first, gated by the chain check exactly as `tensorseed list` does; if that
/// fails, the last accepted local copy is shown with the error. Every entry
/// signature is checked one by one so a bad entry is flagged, not hidden.
///
/// # Errors
///
/// Fails when the config or catalog cannot be loaded and there is no local
/// copy to fall back to.
#[tauri::command]
pub async fn entries_list(name: String, sync: bool) -> Result<CatalogEntries, String> {
    let (_, cfg) = load_config()?;
    let sub = cfg
        .subscription(Some(&name))
        .map_err(|e| format!("{e:#}"))?;
    let view = cfg.for_subscription(&sub);
    let pk = pinned_key(&view).map_err(|e| format!("{e:#}"))?;

    let mut sync_error = None;
    let opened = if sync {
        match open_reader(&view, None, false).await {
            Ok(v) => Ok(v),
            Err(e) => {
                sync_error = Some(format!("{e:#}"));
                open_reader(&view, None, true).await
            }
        }
    } else {
        open_reader(&view, None, true).await
    };
    let (backend, id) = opened.map_err(|e| match &sync_error {
        Some(s) => format!("{s}; and no local copy: {e:#}"),
        None => format!("{e:#}"),
    })?;

    let chain = chain_view(&view, &backend, sync_error.as_deref());
    let (revision, root) = match &backend {
        Backend::File(f) => {
            let st = f.state();
            (st.revision, st.root)
        }
        Backend::Chia(_) => (0, String::new()),
    };
    let (entries, problems) = read_entries(&backend, &id, &pk, &view.catalog.name)
        .await
        .map_err(|e| format!("{e:#}"))?;

    Ok(CatalogEntries {
        catalog: summary(&cfg, &sub),
        revision,
        root,
        synced: sync && sync_error.is_none(),
        sync_error,
        chain,
        entries,
        problems,
    })
}

async fn read_entries(
    backend: &Backend,
    id: &CatalogId,
    pk: &tensorseed_catalog::minisign::PublicKey,
    catalog: &str,
) -> anyhow::Result<(Vec<EntryView>, Vec<String>)> {
    let b = backend.as_dyn();
    let prefix = index_prefix();
    let mut entries = Vec::new();
    let mut problems = Vec::new();
    for key in b.keys(id, &prefix).await? {
        let Some(e) = b.get_signed(id, &key).await? else {
            continue;
        };
        let signature = if e.key == key {
            match e.verify(pk, catalog) {
                Ok(()) => SignatureView::Ok { ok: true },
                Err(err) => SignatureView::Failed {
                    ok: false,
                    error: err.to_string(),
                },
            }
        } else {
            SignatureView::Failed {
                ok: false,
                error: format!("catalog returned entry {:?} for {key:?}", e.key),
            }
        };
        let ix: IndexEntry = match serde_json::from_str(&e.value) {
            Ok(ix) => ix,
            Err(err) => {
                problems.push(format!("{key}: {err}"));
                continue;
            }
        };
        if ix.schema != SCHEMA_INDEX {
            problems.push(format!("{key}: schema {:?}", ix.schema));
            continue;
        }
        let rest = &key[prefix.len()..];
        let (ty, repo_id) = rest.split_once('/').unwrap_or(("model", rest));
        let repo_type = if ty == "dataset" { "dataset" } else { "model" };
        entries.push(EntryView {
            id: repo_id.to_string(),
            repo_type,
            latest_key: format!("ts/v1/repo/{repo_type}/{repo_id}/latest"),
            index_key: key.clone(),
            commercial: tensorseed_core::commercial::assess(ix.license_spdx.as_deref(), None),
            index: ix,
            signature,
        });
    }
    entries.sort_by_key(|a| a.id.to_lowercase());
    Ok((entries, problems))
}

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

fn parse_type(t: &str) -> Result<RepoType, String> {
    match t {
        "model" => Ok(RepoType::Model),
        "dataset" => Ok(RepoType::Dataset),
        other => Err(format!("unknown repo type {other:?} (model or dataset)")),
    }
}

/// A local copy under `general.download_dir` (`LocalCopy` in
/// `src/lib/native.ts`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalCopy {
    /// `model` or `dataset`.
    repo_type: &'static str,
    /// Repo id.
    id: String,
    /// Directory.
    dir: String,
    /// A finished pull is recorded (`.tensorseed/pull.json`).
    pulled: bool,
    /// This copy's own commit (from its pull record).
    commit: Option<String>,
    /// When it was pulled (RFC 3339 UTC).
    pulled_at: Option<String>,
    /// The pull used a signature verified manifest.
    signed: bool,
    /// `catalog`, `manifest-file`, `manifest-url`, or `huggingface`.
    source: Option<String>,
    /// Files this copy holds (downloaded, not skipped).
    files: usize,
    /// Bytes of those files.
    bytes: u64,
    /// Files the pull record lists, including skipped ones.
    record_files: usize,
    /// Files deliberately not downloaded.
    skipped: usize,
    /// How it compares with the catalog entry.
    status: Option<crate::copies::CopyStatus>,
    /// It has a torrent, so it can be seeded.
    has_torrent: bool,
    /// The in-app seeder is seeding it.
    seeding: bool,
    /// Why the record could not be read.
    error: Option<String>,
}

async fn local_copy(
    seeder: &Seeder,
    repo_type: RepoType,
    id: &str,
    dir: &Path,
    catalog: Option<&CatalogRef>,
) -> LocalCopy {
    let (record, error) = match read_pull_record(dir) {
        Ok(r) => (r, None),
        Err(e) => (None, Some(err(e))),
    };
    let status = record.as_ref().map(|r| crate::copies::classify(r, catalog));
    LocalCopy {
        repo_type: repo_type.as_str(),
        id: record
            .as_ref()
            .map_or_else(|| id.to_string(), |r| r.repo.clone()),
        dir: dir.display().to_string(),
        pulled: record.is_some(),
        commit: record.as_ref().map(|r| r.commit.clone()),
        pulled_at: record.as_ref().map(|r| r.pulled_at.clone()),
        signed: record.as_ref().is_some_and(|r| r.signed),
        source: record.as_ref().map(|r| r.source.clone()),
        files: status.as_ref().map_or(0, |s| s.present_files),
        bytes: status.as_ref().map_or(0, |s| s.present_bytes),
        record_files: record.as_ref().map_or(0, |r| r.files.len()),
        skipped: record.as_ref().map_or(0, |r| r.skipped.len()),
        status,
        has_torrent: dir
            .join(META_DIR)
            .join(tensorseed_app::torrents::PULLED_TORRENT)
            .is_file(),
        seeding: seeder.is_seeding(dir).await,
        error,
    }
}

/// Pulled copies under `general.download_dir`: `<org>/<name>` for models
/// and `datasets/<org>/<name>` for datasets, each with a `pull.json`.
fn scan_local(cfg: &Config) -> Vec<(RepoType, String, PathBuf)> {
    let base = config::expand(&cfg.general.download_dir);
    let mut out = Vec::new();
    let mut scan = |root: &Path, ty: RepoType, skip: Option<&str>| {
        let Ok(orgs) = std::fs::read_dir(root) else {
            return;
        };
        for org in orgs.filter_map(Result::ok) {
            let on = org.file_name().to_string_lossy().into_owned();
            if skip == Some(on.as_str()) || !org.path().is_dir() {
                continue;
            }
            let Ok(names) = std::fs::read_dir(org.path()) else {
                continue;
            };
            for n in names.filter_map(Result::ok) {
                let d = n.path();
                if d.join(META_DIR).join("pull.json").is_file() {
                    let id = format!("{on}/{}", n.file_name().to_string_lossy());
                    out.push((ty, id, d));
                }
            }
        }
    };
    scan(&base, RepoType::Model, Some("datasets"));
    scan(&base.join("datasets"), RepoType::Dataset, None);
    out.sort_by_key(|a| a.1.to_lowercase());
    out
}

/// The signed catalog entry a copy is compared with, from the local
/// catalog copy only (no network requests). Only the file backend keeps a
/// local copy; other backends leave copies unchecked here.
async fn offline_catalog_ref(
    cfg: &Config,
    ty: RepoType,
    id: &str,
    dir: &Path,
) -> Option<CatalogRef> {
    if cfg.catalog.backend != "file" {
        return None;
    }
    let repo_id = read_pull_record(dir)
        .ok()
        .flatten()
        .map_or_else(|| id.to_string(), |r| r.repo);
    let repo = RepoRef::new(ty, repo_id).ok()?;
    let r = catalog_manifest(cfg, &repo, None, true, None).await.ok()?;
    catalog_ref(&r)
}

/// The comparison view of a resolved catalog manifest (signed only).
fn catalog_ref(r: &tensorseed_app::pull::Resolved) -> Option<CatalogRef> {
    r.signed.then(|| CatalogRef {
        commit: r.manifest.hf_commit.clone(),
        paths: r.manifest.files.iter().map(|f| f.path.clone()).collect(),
    })
}

/// `local_list` result.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalList {
    download_dir: String,
    copies: Vec<LocalCopy>,
}

/// Pulled copies on this machine. No network requests.
///
/// # Errors
///
/// Fails when the config cannot be loaded.
#[tauri::command]
pub async fn local_list(seeder: State<'_, Seeder>) -> Result<LocalList, String> {
    let (_, cfg) = load_config()?;
    let mut copies = Vec::new();
    for (ty, id, dir) in scan_local(&cfg) {
        let catalog = offline_catalog_ref(&cfg, ty, &id, &dir).await;
        copies.push(local_copy(&seeder, ty, &id, &dir, catalog.as_ref()).await);
    }
    Ok(LocalList {
        download_dir: config::expand(&cfg.general.download_dir)
            .display()
            .to_string(),
        copies,
    })
}

/// `entry_show` result (`EntryPage` in `src/lib/native.ts`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryPage {
    id: String,
    repo_type: &'static str,
    /// Catalog the manifest was resolved through.
    catalog: Option<String>,
    catalog_label: Option<String>,
    /// The manifest (file pages merged), as `Manifest` in `ui/src/types.ts`.
    manifest: Option<serde_json::Value>,
    /// `catalog` or `local` (the copy's stored manifest).
    manifest_source: Option<&'static str>,
    manifest_error: Option<String>,
    /// The manifest signature verified against the pinned publisher key.
    signed: bool,
    /// A tombstone covering this revision.
    tombstone: Option<serde_json::Value>,
    taken_down: bool,
    taken_down_in: Vec<String>,
    /// Chain anchor status of the catalog revision the manifest came from.
    chain: Option<ChainView>,
    commercial: Option<tensorseed_core::commercial::Assessment>,
    synced: bool,
    sync_error: Option<String>,
    local: LocalCopy,
    /// The `tensorseed` CLI that runs pulls was found.
    cli_found: bool,
}

/// One entry: its signed manifest, signature and chain anchor status,
/// license and commercial serving mark, files, and the local copy. With
/// `sync`, the catalog is synced first (gated by the chain check as in the
/// CLI); without it only the local catalog copy is read. When the catalog
/// has no manifest, a pulled copy's stored manifest is shown instead (its
/// signature checked the same way).
///
/// # Errors
///
/// Fails when the config cannot be loaded or the type or id are invalid.
#[tauri::command]
pub async fn entry_show(
    seeder: State<'_, Seeder>,
    repo_type: String,
    id: String,
    revision: Option<String>,
    catalog: Option<String>,
    sync: bool,
) -> Result<EntryPage, String> {
    let (_, cfg) = load_config()?;
    let ty = parse_type(&repo_type)?;
    let route = Route::entry(ty, &id, revision.as_deref(), catalog.as_deref()).map_err(err)?;
    let Route::Entry { revision, .. } = route else {
        return Err("internal error: not an entry route".into());
    };
    let repo = RepoRef::new(ty, id.clone()).map_err(|e| e.to_string())?;
    let sub = cfg.subscription(catalog.as_deref()).ok();
    let catalog_name = sub.as_ref().map(|s| s.name.clone());

    let mut sync_error = None;
    let mut resolved = catalog_manifest(
        &cfg,
        &repo,
        revision.as_deref(),
        !sync,
        catalog_name.as_deref(),
    )
    .await;
    if sync && let Err(e) = &resolved {
        sync_error = Some(err(anyhow::anyhow!("{e:#}")));
        resolved = catalog_manifest(
            &cfg,
            &repo,
            revision.as_deref(),
            true,
            catalog_name.as_deref(),
        )
        .await;
    }
    let dir = default_out(&cfg, &repo);
    let mut source = Some("catalog");
    let mut manifest_error = None;
    let resolved = match resolved {
        Ok(r) => Some(r),
        Err(e) => {
            manifest_error = Some(err(e));
            let stored = dir.join(META_DIR).join("manifest.json");
            source = Some("local");
            std::fs::read(&stored)
                .ok()
                .and_then(|b| parse_manifest_bytes(&cfg, &b, &stored.display().to_string()).ok())
        }
    };
    if resolved.is_none() {
        source = None;
    }

    let chain = match &sub {
        Some(s) if source == Some("catalog") => {
            let view = cfg.for_subscription(s);
            match open_reader(&view, None, true).await {
                Ok((backend, _)) => Some(chain_view(&view, &backend, sync_error.as_deref())),
                Err(_) => None,
            }
        }
        _ => None,
    };
    let catalog_cmp = match (&resolved, source) {
        (Some(r), Some("catalog")) => catalog_ref(r),
        _ => None,
    };
    let local = local_copy(&seeder, ty, &id, &dir, catalog_cmp.as_ref()).await;
    let (manifest, signed, tombstone, taken_down, taken_down_in, commercial) = match resolved {
        Some(r) => {
            let covered = r
                .tombstone
                .as_ref()
                .is_some_and(|t| tombstone_covers(t, &r.manifest.hf_commit));
            (
                serde_json::to_value(&r.manifest).ok(),
                r.signed,
                r.tombstone
                    .as_ref()
                    .and_then(|t| serde_json::to_value(t).ok()),
                covered,
                r.taken_down_in,
                Some(tensorseed_core::commercial::assess_license_info(
                    &r.manifest.license,
                )),
            )
        }
        None => (None, false, None, false, Vec::new(), None),
    };
    Ok(EntryPage {
        id,
        repo_type: ty.as_str(),
        catalog: catalog_name,
        catalog_label: sub.as_ref().map(|s| s.display().to_string()),
        manifest,
        manifest_source: source,
        manifest_error,
        signed,
        tombstone,
        taken_down,
        taken_down_in,
        chain,
        commercial,
        synced: sync && sync_error.is_none(),
        sync_error,
        local,
        cli_found: crate::jobs::cli_path().is_some(),
    })
}

/// `settings_get` result (`SettingsView` in `src/lib/native.ts`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    config_path: String,
    /// The file exists (else defaults are shown and saving creates it).
    exists: bool,
    /// `[torrent]` from the file over the defaults (no environment).
    torrent: config::Torrent,
    /// The defaults, so the form can mark changed fields.
    defaults: config::Torrent,
    /// `TENSORSEED_TORRENT__*` variables that override the file.
    env_overrides: Vec<String>,
    /// `bind_interface` works on this platform.
    bind_interface_supported: bool,
}

/// The `[torrent]` settings from the shared config file.
///
/// # Errors
///
/// Fails when the file is not valid TOML or `[torrent]` does not parse.
#[tauri::command]
pub async fn settings_get() -> Result<SettingsView, String> {
    let path = config_path();
    let torrent = tensorseed_app::settings::read_torrent(&path).map_err(err)?;
    Ok(SettingsView {
        config_path: path.display().to_string(),
        exists: path.is_file(),
        torrent,
        defaults: config::Torrent::default(),
        env_overrides: tensorseed_app::settings::torrent_env_overrides(),
        bind_interface_supported: !cfg!(windows),
    })
}

/// Validate `[torrent]` settings without saving (the same checks the CLI
/// runs when it loads the config).
///
/// # Errors
///
/// Fails with the first validation error.
#[tauri::command]
pub async fn settings_check(torrent: config::Torrent) -> Result<(), String> {
    torrent.validate().map_err(err)
}

/// `settings_set` result.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsSaved {
    config_path: String,
    /// The running seeder took the new caps, schedule, and stop rules.
    applied_to_seeder: bool,
    env_overrides: Vec<String>,
}

/// Validate and save `[torrent]` into the shared config file, keeping
/// comments, other sections, and untouched keys. Nothing is written when
/// validation fails. A running seeder picks up the new caps, schedule, and
/// stop rules; other options apply when seeding starts again.
///
/// # Errors
///
/// Fails with the validation error, or when the file cannot be written.
#[tauri::command]
pub async fn settings_set(
    seeder: State<'_, Seeder>,
    torrent: config::Torrent,
) -> Result<SettingsSaved, String> {
    let path = config_path();
    tensorseed_app::settings::write_torrent(&path, &torrent).map_err(err)?;
    let applied = match config::load(&path) {
        Ok(cfg) => seeder.reload(&cfg).await.map_err(err)?,
        Err(_) => false,
    };
    Ok(SettingsSaved {
        config_path: path.display().to_string(),
        applied_to_seeder: applied,
        env_overrides: tensorseed_app::settings::torrent_env_overrides(),
    })
}

/// Start `tensorseed pull` for an entry; progress arrives as job events.
///
/// # Errors
///
/// Fails when the inputs are invalid or the CLI cannot be started.
#[tauri::command]
pub async fn pull_start(
    app: AppHandle,
    jobs: State<'_, Jobs>,
    repo_type: String,
    id: String,
    revision: Option<String>,
    catalog: Option<String>,
) -> Result<u64, String> {
    let (path, cfg) = load_config()?;
    let ty = parse_type(&repo_type)?;
    let route = Route::entry(ty, &id, revision.as_deref(), catalog.as_deref()).map_err(err)?;
    let Route::Entry {
        revision, catalog, ..
    } = route
    else {
        return Err("internal error: not an entry route".into());
    };
    let out = default_out(
        &cfg,
        &RepoRef::new(ty, id.clone()).map_err(|e| e.to_string())?,
    );
    let req = crate::jobs::PullRequest {
        repo_type: ty.as_str().to_string(),
        id,
        revision,
        catalog,
        out,
    };
    crate::jobs::start_pull(app, jobs.inner().clone(), path, &req).map_err(err)
}

/// Verify the local copy of an entry.
///
/// `mode` is `catalog` (against the signed manifest), `record` (against
/// the copy's own pull record), or `auto` (the default): a complete signed
/// copy is checked against the catalog, and a partial, unsigned, or other
/// revision copy against its own record, so it is not reported as a broken
/// catalog entry.
///
/// # Errors
///
/// Fails when the inputs are invalid or there is no local copy.
#[tauri::command]
pub async fn verify_start(
    app: AppHandle,
    jobs: State<'_, Jobs>,
    repo_type: String,
    id: String,
    catalog: Option<String>,
    mode: Option<String>,
) -> Result<u64, String> {
    let (path, cfg) = load_config()?;
    let ty = parse_type(&repo_type)?;
    let repo = RepoRef::new(ty, id).map_err(|e| e.to_string())?;
    let dir = default_out(&cfg, &repo);
    let Some(record) = read_pull_record(&dir).map_err(err)? else {
        return Err(format!("{} has no finished pull to verify", dir.display()));
    };
    let use_record = match mode.as_deref().unwrap_or("auto") {
        "record" => true,
        "catalog" => false,
        "auto" => {
            let cmp = catalog_manifest(&cfg, &repo, None, true, catalog.as_deref())
                .await
                .ok()
                .and_then(|r| catalog_ref(&r));
            crate::copies::classify(&record, cmp.as_ref()).verify_default == "record"
        }
        other => return Err(format!("unknown verify mode {other:?}")),
    };
    if use_record {
        return Ok(crate::jobs::start_verify_record(
            app,
            jobs.inner().clone(),
            repo,
            dir,
            record,
        ));
    }
    crate::jobs::start_verify(app, jobs.inner().clone(), path, cfg, repo, dir, catalog).map_err(err)
}

/// Seed the local copy of an entry with the in-app seeder.
///
/// # Errors
///
/// Fails when `tensorseed serve` already seeds on this machine, the copy
/// has no torrent, or the files do not match it.
#[tauri::command]
pub async fn seed_start(
    seeder: State<'_, Seeder>,
    repo_type: String,
    id: String,
) -> Result<String, String> {
    let (_, cfg) = load_config()?;
    let ty = parse_type(&repo_type)?;
    let repo = RepoRef::new(ty, id).map_err(|e| e.to_string())?;
    if crate::status::probe_serve(&cfg).await.is_some() {
        return Err(
            "tensorseed serve is running on this machine and seeds there; stop it to seed from Desktop (one seeder per machine)"
                .into(),
        );
    }
    let dir = default_out(&cfg, &repo);
    seeder.seed_dir(&cfg, &dir).await.map_err(err)
}

/// Pause every copy the in-app seeder is seeding.
///
/// # Errors
///
/// Fails when the engine rejects a pause.
#[tauri::command]
pub async fn seeds_pause(seeder: State<'_, Seeder>) -> Result<usize, String> {
    seeder.pause_all().await.map_err(err)
}

/// Resume every copy the user paused.
///
/// # Errors
///
/// Fails when the engine rejects a resume.
#[tauri::command]
pub async fn seeds_resume(seeder: State<'_, Seeder>) -> Result<usize, String> {
    seeder.resume_all().await.map_err(err)
}

/// Torrent activity now (the same data the tray shows).
///
/// # Errors
///
/// Never; the result type is for the command bridge.
#[tauri::command]
pub async fn status(
    seeder: State<'_, Seeder>,
    jobs: State<'_, Jobs>,
) -> Result<crate::status::Status, String> {
    let cfg = load_config().ok().map(|(_, c)| c);
    Ok(crate::status::gather(&seeder, &jobs, cfg.as_ref()).await)
}

/// The link Desktop was started with, once (later links arrive as events).
///
/// # Errors
///
/// Never; the result type is for the command bridge.
#[tauri::command]
pub async fn initial_route(
    link: State<'_, crate::InitialLink>,
) -> Result<Option<crate::DeepLink>, String> {
    Ok(link.0.lock().ok().and_then(|mut l| l.take()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The webview reads these shapes through `ui/src/source.ts`; keep the
    // field names and tags in step with it.
    #[test]
    fn chain_view_matches_the_ui_types() {
        let v = serde_json::to_value(ChainView::Anchored {
            store_id: "s".into(),
            datalayer_root: "d".into(),
            coin_id: "c".into(),
            coin_state: "current".into(),
            via: "v".into(),
            checked_unix: 7,
        })
        .unwrap();
        assert_eq!(v["kind"], "anchored");
        assert_eq!(v["storeId"], "s");
        assert_eq!(v["datalayerRoot"], "d");
        assert_eq!(v["checkedUnix"], 7);
        let off = serde_json::to_value(ChainView::Off).unwrap();
        assert_eq!(off, serde_json::json!({ "kind": "off" }));
        let un = serde_json::to_value(ChainView::Unchecked { reason: "r".into() }).unwrap();
        assert_eq!(
            un,
            serde_json::json!({ "kind": "unchecked", "reason": "r" })
        );
    }

    #[test]
    fn signature_view_is_untagged() {
        let ok = serde_json::to_value(SignatureView::Ok { ok: true }).unwrap();
        assert_eq!(ok, serde_json::json!({ "ok": true }));
        let bad = serde_json::to_value(SignatureView::Failed {
            ok: false,
            error: "e".into(),
        })
        .unwrap();
        assert_eq!(bad, serde_json::json!({ "ok": false, "error": "e" }));
    }

    #[test]
    fn entry_view_uses_camel_case() {
        let v = serde_json::to_value(EntryView {
            id: "org/name".into(),
            repo_type: "model",
            index_key: "ts/v1/index/model/org/name".into(),
            latest_key: "ts/v1/repo/model/org/name/latest".into(),
            index: IndexEntry {
                schema: SCHEMA_INDEX.into(),
                latest_commit: "0".repeat(40),
                license_spdx: None,
                total_bytes: 1,
                updated_at: "2026-01-01T00:00:00Z".into(),
                taken_down: false,
            },
            commercial: tensorseed_core::commercial::assess(None, None),
            signature: SignatureView::Ok { ok: true },
        })
        .unwrap();
        assert_eq!(v["repoType"], "model");
        assert_eq!(v["commercial"]["value"], "unknown");
        assert_eq!(v["indexKey"], "ts/v1/index/model/org/name");
        assert_eq!(v["index"]["latest_commit"], "0".repeat(40));
        assert_eq!(v["signature"]["ok"], true);
    }
}
