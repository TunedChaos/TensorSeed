// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Tauri commands over `tensorseed-app`. Read only for now (M3.2): list the
//! followed catalogs and show one catalog's entries with their signature and
//! chain anchor status. Pull and seed move here in M3.3 and M3.4.
//!
//! Results use camelCase field names to match `@tensorseed/ui` types
//! (`ui/src/source.ts`). Errors cross the bridge as display strings.

use std::path::PathBuf;

use anyhow::Context;
use serde::Serialize;
use tensorseed_app::backend::{Backend, file_catalog, open_reader, pinned_key};
use tensorseed_app::config::{self, ChainCheck, Config, Subscription};
use tensorseed_catalog::CatalogId;
use tensorseed_core::keys::index_prefix;
use tensorseed_core::manifest::{IndexEntry, SCHEMA_INDEX};

/// Same lookup as the CLI: `TENSORSEED_CONFIG`, else the default path.
fn config_path() -> PathBuf {
    std::env::var_os("TENSORSEED_CONFIG")
        .filter(|v| !v.is_empty())
        .map_or_else(config::default_path, PathBuf::from)
}

fn load_config() -> Result<(PathBuf, Config), String> {
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
            index: ix,
            signature,
        });
    }
    entries.sort_by_key(|a| a.id.to_lowercase());
    Ok((entries, problems))
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
            signature: SignatureView::Ok { ok: true },
        })
        .unwrap();
        assert_eq!(v["repoType"], "model");
        assert_eq!(v["indexKey"], "ts/v1/index/model/org/name");
        assert_eq!(v["index"]["latest_commit"], "0".repeat(40));
        assert_eq!(v["signature"]["ok"], true);
    }
}
