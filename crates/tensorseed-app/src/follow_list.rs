// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Following signed `ts.follow-list/1` lists
//! ([`tensorseed_verify::follow_list`]): reading a list and its signature
//! from a URL or a file, and planning the `[[catalogs]]` changes a new
//! revision makes. The CLI shows the plan and writes it.
//!
//! The rules, so a list can never quietly take over a catalog you chose
//! yourself:
//!
//! - Only entries tagged `follow_list = "<list>"` are added, changed, or
//!   removed. A catalog you added by hand (or through another list) with
//!   the same name and key is kept as it is; with a different key it is a
//!   conflict and is left alone.
//! - A listed catalog whose publisher key changed is not applied unless
//!   the follower allows key changes.
//! - When an entry is updated, the follower's own settings (chain check,
//!   priority, state directory, mirrors, coin APIs) are kept.

use std::time::Duration;

use anyhow::{Context, bail};
use tensorseed_verify::follow_list::{
    FollowList, FollowedCatalog, MAX_FOLLOW_LIST_BYTES, Seen, Update, check_update, verify_list,
};
use tensorseed_verify::minisign::PublicKey;

use crate::config::{CatalogSub, Config, FollowListSub};

/// Largest signature file read.
const MAX_SIG_BYTES: usize = 4096;

/// True for an http(s) source.
#[must_use]
pub fn is_url(source: &str) -> bool {
    source.starts_with("https://") || source.starts_with("http://")
}

async fn read_one(source: &str, max: usize) -> anyhow::Result<Vec<u8>> {
    if !is_url(source) {
        let meta = std::fs::metadata(source).with_context(|| format!("reading {source}"))?;
        if meta.len() > max as u64 {
            bail!("{source} is larger than {max} bytes");
        }
        return std::fs::read(source).with_context(|| format!("reading {source}"));
    }
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(3))
        .user_agent(concat!("tensorseed/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let mut r = http
        .get(source)
        .send()
        .await
        .with_context(|| format!("fetching {source}"))?;
    if !r.status().is_success() {
        bail!("fetching {source}: HTTP {}", r.status());
    }
    let mut buf = Vec::new();
    while let Some(c) = r.chunk().await? {
        if buf.len() + c.len() > max {
            bail!("{source} is larger than {max} bytes");
        }
        buf.extend_from_slice(&c);
    }
    Ok(buf)
}

/// Read a list and its signature (`sig`, else `<source>.minisig`).
///
/// # Errors
///
/// Fails when either cannot be read, is too large, or the signature is not
/// UTF-8.
pub async fn read_signed(source: &str, sig: Option<&str>) -> anyhow::Result<(Vec<u8>, String)> {
    let body = read_one(source, MAX_FOLLOW_LIST_BYTES).await?;
    let sig_src = sig.map_or_else(|| format!("{source}.minisig"), str::to_string);
    let sig = read_one(&sig_src, MAX_SIG_BYTES).await?;
    let sig = String::from_utf8(sig).with_context(|| format!("{sig_src} is not text"))?;
    Ok((body, sig))
}

/// Read and verify a list against `pubkey`.
///
/// # Errors
///
/// Fails when it cannot be read or does not verify.
pub async fn fetch_verified(
    source: &str,
    sig: Option<&str>,
    pubkey: &str,
) -> anyhow::Result<(FollowList, Vec<u8>)> {
    let pk = PublicKey::parse(pubkey).context("list publisher key")?;
    let (body, sig) = read_signed(source, sig).await?;
    let list = verify_list(&pk, &body, &sig).with_context(|| format!("verifying {source}"))?;
    Ok((list, body))
}

/// A listed catalog that is not applied, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    /// The listed catalog.
    pub catalog: FollowedCatalog,
    /// Why it is left alone.
    pub reason: String,
    /// True when this is a conflict the follower should look at (another
    /// key, or a key change that was not allowed); false when the catalog
    /// is simply already followed.
    pub conflict: bool,
}

/// What following a verified list would change.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    /// The verified list.
    pub list: FollowList,
    /// How it relates to the revision accepted before.
    pub update: Update,
    /// New `[[catalogs]]` entries.
    pub add: Vec<CatalogSub>,
    /// Entries from this list that change: (current, new).
    pub change: Vec<(CatalogSub, CatalogSub)>,
    /// Entries from this list it no longer names.
    pub remove: Vec<CatalogSub>,
    /// Listed catalogs left alone.
    pub skipped: Vec<Skipped>,
}

impl Plan {
    /// True when no `[[catalogs]]` entry changes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.add.is_empty() && self.change.is_empty() && self.remove.is_empty()
    }

    /// True when some listed catalog conflicts with the follower's config.
    #[must_use]
    pub fn has_conflicts(&self) -> bool {
        self.skipped.iter().any(|s| s.conflict)
    }
}

fn entry_for(list: &str, c: &FollowedCatalog, keep: Option<&CatalogSub>) -> CatalogSub {
    let mut e = keep.cloned().unwrap_or_default();
    e.name.clone_from(&c.name);
    e.label.clone_from(&c.label);
    e.publisher_pubkey.clone_from(&c.publisher_pubkey);
    e.base_url.clone_from(&c.base_url);
    e.store_id.clone_from(&c.store_id);
    e.follow_list = list.to_string();
    e
}

/// The `[[follow_lists]]` entry for `name`, if followed.
#[must_use]
pub fn followed<'a>(cfg: &'a Config, name: &str) -> Option<&'a FollowListSub> {
    cfg.follow_lists.iter().find(|l| l.name == name)
}

/// Check a verified list against what was accepted before and plan the
/// `[[catalogs]]` changes.
///
/// # Errors
///
/// A rollback or a fork against the accepted revision, a list followed
/// under the same name with another key, or a config whose catalogs do
/// not resolve.
pub fn plan(
    cfg: &Config,
    list: &FollowList,
    body: &[u8],
    pubkey: &str,
    allow_key_change: bool,
) -> anyhow::Result<Plan> {
    let prev = followed(cfg, &list.name);
    if let Some(p) = prev
        && p.publisher_pubkey != pubkey
    {
        bail!(
            "follow list {:?} is followed with another key; run `catalog follow-list unfollow {}` first",
            list.name,
            list.name
        );
    }
    let seen = prev.map(|p| Seen {
        rev: p.rev,
        sha256: p.sha256.clone(),
    });
    let update = check_update(seen.as_ref(), list, body)
        .with_context(|| format!("follow list {:?}", list.name))?;
    let subs = cfg.subscriptions()?;
    let mut plan = Plan {
        list: list.clone(),
        update,
        add: Vec::new(),
        change: Vec::new(),
        remove: Vec::new(),
        skipped: Vec::new(),
    };
    for c in &list.catalogs {
        let ours = cfg
            .catalogs
            .iter()
            .find(|e| e.name == c.name && e.follow_list == list.name);
        if let Some(cur) = ours {
            let new = entry_for(&list.name, c, Some(cur));
            if new == *cur {
                continue;
            }
            if new.publisher_pubkey != cur.publisher_pubkey && !allow_key_change {
                plan.skipped.push(Skipped {
                    catalog: c.clone(),
                    reason: format!(
                        "the list changed its publisher key from {} to {}; not applied without --allow-key-change",
                        cur.publisher_pubkey, c.publisher_pubkey
                    ),
                    conflict: true,
                });
                continue;
            }
            plan.change.push((cur.clone(), new));
            continue;
        }
        match subs.iter().find(|s| s.name == c.name) {
            Some(s) if s.publisher_pubkey == c.publisher_pubkey => {
                plan.skipped.push(Skipped {
                    catalog: c.clone(),
                    reason: "already followed with the same key; your entry is kept".into(),
                    conflict: false,
                });
            }
            Some(s) => plan.skipped.push(Skipped {
                catalog: c.clone(),
                reason: format!(
                    "already followed with another key ({}); not changed",
                    s.publisher_pubkey
                ),
                conflict: true,
            }),
            None => plan.add.push(entry_for(&list.name, c, None)),
        }
    }
    plan.remove = cfg
        .catalogs
        .iter()
        .filter(|e| e.follow_list == list.name && list.catalog(&e.name).is_none())
        .cloned()
        .collect();
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tensorseed_verify::follow_list::{FollowList, sign_list};
    use tensorseed_verify::minisign::SecretKey;

    fn key(n: u8) -> SecretKey {
        SecretKey::from_seed(&[n; 32])
    }

    fn cat(name: &str, k: u8) -> FollowedCatalog {
        FollowedCatalog {
            name: name.into(),
            label: String::new(),
            publisher_pubkey: key(k).public_key().to_base64(),
            base_url: format!("https://{name}.example.org/"),
            store_id: String::new(),
            note: String::new(),
        }
    }

    #[test]
    fn plans_touch_only_the_lists_own_entries() {
        let signer = key(9);
        let spk = signer.public_key().to_base64();
        let mut cfg = Config::default();
        cfg.catalogs.push(entry_for("", &cat("mine", 1), None));
        cfg.catalogs.push(entry_for("", &cat("clash", 2), None));
        let mut old = entry_for("l", &cat("gone", 3), None);
        old.priority = Some(7);
        cfg.catalogs.push(old);
        let mut kept = entry_for("l", &cat("moved", 4), None);
        kept.chain_check = Some("strict".into());
        cfg.catalogs.push(kept);
        cfg.catalogs.push(entry_for("l", &cat("rekeyed", 5), None));
        cfg.follow_lists.push(FollowListSub {
            name: "l".into(),
            source: "list.json".into(),
            publisher_pubkey: spk.clone(),
            rev: 1,
            sha256: "00".repeat(32),
        });

        let mut l = FollowList::new("l", "2026-11-14T00:00:00Z");
        l.rev = 2;
        l.catalogs.push(cat("mine", 1));
        l.catalogs.push(cat("clash", 6));
        let mut moved = cat("moved", 4);
        moved.base_url = "https://new.example.org/".into();
        l.catalogs.push(moved);
        l.catalogs.push(cat("rekeyed", 7));
        l.catalogs.push(cat("fresh", 8));
        let (body, _) = sign_list(&signer, &l).unwrap();

        let p = plan(&cfg, &l, &body, &spk, false).unwrap();
        assert_eq!(p.update, Update::Newer);
        assert_eq!(p.add.len(), 1);
        assert_eq!(p.add[0].name, "fresh");
        assert_eq!(p.add[0].follow_list, "l");
        assert_eq!(p.change.len(), 1);
        assert_eq!(p.change[0].1.base_url, "https://new.example.org/");
        // The follower's own settings survive an update.
        assert_eq!(p.change[0].1.chain_check.as_deref(), Some("strict"));
        assert_eq!(p.remove.len(), 1);
        assert_eq!(p.remove[0].name, "gone");
        let reasons: Vec<_> = p
            .skipped
            .iter()
            .map(|s| (s.catalog.name.as_str(), s.conflict))
            .collect();
        assert_eq!(
            reasons,
            [("mine", false), ("clash", true), ("rekeyed", true)]
        );
        assert!(p.has_conflicts());

        // Allowing key changes applies the rekeyed entry.
        let p = plan(&cfg, &l, &body, &spk, true).unwrap();
        assert_eq!(p.change.len(), 2);

        // A rollback, and another signer under the same list name.
        cfg.follow_lists[0].rev = 3;
        assert!(plan(&cfg, &l, &body, &spk, false).is_err());
        cfg.follow_lists[0].rev = 1;
        let other = key(10).public_key().to_base64();
        let e = plan(&cfg, &l, &body, &other, false).unwrap_err();
        assert!(e.to_string().contains("another key"), "{e}");
    }
}
