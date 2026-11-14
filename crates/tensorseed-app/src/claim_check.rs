// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! `tensorseed check-claim`: everything a buyer or marketplace checks about
//! a seller's `ts.served-weights/1` claim before trusting it
//! (docs/plans/verified-serving.md, section 3.2).
//!
//! The checks themselves are pure and live in the verify-only crate
//! ([`tensorseed_verify::claim_check`], re-exported here), so the CLI, the
//! desktop app, and the WebAssembly package run the same code. This module
//! adds the part that needs a configured, synced catalog:
//! [`resolve_catalog`] finds the claimed entry in a catalog the checker
//! trusts, and [`check_claim_bytes`] runs everything.

use tensorseed_core::keys::RepoRef;
pub use tensorseed_verify::claim_check::*;

use crate::config::Config;
use crate::inference::signed_value_sha256;
use crate::signing::ServedClaim;

/// Look up the claimed entry in the trusted catalog: the configured
/// subscription named `catalog` (or the one the claim names), synced
/// unless `no_wait`.
///
/// # Errors
///
/// `Err(Ok(check))` is a failed check (the catalog is not trusted, or the
/// entry is not in it); `Err(Err(e))` means the catalog could not be read,
/// so the run is inconclusive.
pub async fn resolve_catalog(
    cfg: &Config,
    c: &ServedClaim,
    catalog: Option<&str>,
    no_wait: bool,
) -> Result<CatalogMatch, Result<Check, anyhow::Error>> {
    let subs = cfg.subscriptions().map_err(Err)?;
    if subs.is_empty() {
        return Err(Err(anyhow::anyhow!(
            "no catalog is configured; set catalog.publisher_pubkey or add one with `tensorseed catalog add`"
        )));
    }
    let name = catalog.unwrap_or(&c.catalog.name);
    let Some(sub) = subs.iter().find(|s| s.name == name) else {
        let have: Vec<&str> = subs.iter().map(|s| s.name.as_str()).collect();
        return Err(Ok(check(
            "catalog",
            Status::Fail,
            format!(
                "the claim names catalog {name}, which is not one you trust (configured: {}); add it with `tensorseed catalog add` only if you trust its publisher",
                have.join(", ")
            ),
        )));
    };
    let view = cfg.for_subscription(sub);
    let key = crate::backend::pinned_key(&view).map_err(Err)?;
    let repo = RepoRef::new(c.repo_type, c.repo.clone()).map_err(|e| {
        Ok(check(
            "manifest",
            Status::Fail,
            format!("bad repo id {:?}: {e}", c.repo),
        ))
    })?;
    if tensorseed_core::keys::check_commit(&c.revision).is_err() {
        return Err(Ok(check(
            "manifest",
            Status::Fail,
            format!("revision {:?} is not a 40 hex commit", c.revision),
        )));
    }
    let res = match crate::pull::catalog_manifest(
        &view,
        &repo,
        Some(&c.revision),
        no_wait,
        Some(&sub.name),
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            let msg = format!("{e:#}");
            if msg.contains("is not in the catalog") {
                return Err(Ok(check(
                    "manifest",
                    Status::Fail,
                    format!("{}@{} is not in catalog {}", c.repo, c.revision, sub.name),
                )));
            }
            return Err(Err(e.context(format!("reading catalog {}", sub.name))));
        }
    };
    if !res.signed {
        return Err(Ok(check(
            "manifest",
            Status::Fail,
            format!(
                "the {} entry did not verify against the pinned key",
                sub.name
            ),
        )));
    }
    let revision = (view.catalog.backend == "file")
        .then(|| crate::backend::file_catalog(&view, None).ok())
        .flatten()
        .map(|cat| cat.state().revision);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let chain = crate::chain::load_status(&view).map(|s| ChainSeen {
        revision: s.revision,
        coin_state: s.coin_state,
        age_secs: now.saturating_sub(s.checked_unix),
    });
    Ok(CatalogMatch {
        name: sub.name.clone(),
        publisher_key_id: key.key_id_hex(),
        manifest_key: repo
            .manifest_key(&res.manifest.hf_commit)
            .map_err(|e| Err(anyhow::Error::from(e)))?,
        manifest_sha256: signed_value_sha256(&res.raw).map_err(|e| Err(anyhow::Error::from(e)))?,
        manifest: res.manifest,
        tombstone: res.tombstone,
        freshness: Freshness {
            synced: !no_wait,
            revision,
            chain_check: crate::config::chain_check_name(view.chain_check()).into(),
            chain,
        },
    })
}

/// Run every check on `raw`, read from `origin`. `revocations` is what the
/// caller fetched (the seller's list, a marketplace list, or nothing).
#[allow(clippy::too_many_arguments)]
pub async fn check_claim_bytes(
    cfg: &Config,
    source: &str,
    raw: &[u8],
    origin: Origin,
    catalog: Option<&str>,
    no_wait: bool,
    revocations: &RevocationInput,
    opts: &ClaimCheckOptions,
) -> ClaimReport {
    let c = match parse_claim(raw) {
        Ok(c) => c,
        Err(e) => return unparsable_report(source, &origin, &e.to_string()),
    };
    let cat = match resolve_catalog(cfg, &c, catalog, no_wait).await {
        Ok(m) => CatalogResult::Matched(Box::new(m)),
        Err(Ok(failed)) => CatalogResult::Failed(failed),
        Err(Err(e)) => CatalogResult::Unavailable(format!("{e:#}")),
    };
    build_report(source, &origin, &c, &cat, revocations, opts)
}
