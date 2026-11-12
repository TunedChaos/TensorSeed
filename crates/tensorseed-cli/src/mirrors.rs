// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Mirror lists: the local `mirrors.toml`, the publisher signed list served
//! as `<catalog>/mirrors.toml`, `file.mirror_urls`, and the public URLs of
//! `[[storage.target]]` blocks, merged into one priority ordered list used
//! for catalog reads and webseed downloads.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use tensorseed_catalog::minisign::PublicKey;
use tensorseed_catalog::mirrors::{
    MAX_MIRRORS_BYTES, MIRRORS_PATH, Mirror, MirrorList, sign_list, verify_list,
};
use tensorseed_catalog::sink::CachePolicy;

use crate::backend::sink;
use crate::cli::MirrorCmd;
use crate::config::{Config, expand};
use crate::keys::load_secret;

/// One usable HTTP mirror after merging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ranked {
    /// Lower is tried first.
    pub priority: i64,
    /// Base URL with trailing `/`.
    pub base: String,
    /// Name (may be empty).
    pub name: String,
    /// Where it came from: `local`, `published`, `config`, `storage`.
    pub origin: &'static str,
}

fn local_path(cfg: &Config) -> PathBuf {
    expand(&cfg.mirrors.file)
}

fn cache_paths(cfg: &Config) -> (PathBuf, PathBuf) {
    let dir = expand(&cfg.file.state_dir).join(&cfg.catalog.name);
    (
        dir.join("mirrors.published.toml"),
        dir.join("mirrors.published.toml.minisig"),
    )
}

fn pinned(cfg: &Config) -> Option<PublicKey> {
    PublicKey::parse(&cfg.catalog.publisher_pubkey).ok()
}

/// The local list; a missing file is an empty list.
pub fn load_local(cfg: &Config) -> anyhow::Result<MirrorList> {
    let p = local_path(cfg);
    match std::fs::read_to_string(&p) {
        Ok(t) => MirrorList::parse(&t).with_context(|| format!("reading {}", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(MirrorList::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", p.display())),
    }
}

fn save_local(cfg: &Config, l: &MirrorList) -> anyhow::Result<()> {
    let p = local_path(cfg);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = p.with_extension("toml.tmp");
    std::fs::write(&tmp, l.to_toml()?)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

/// The last verified published list, re-verified from the cache.
pub fn load_cached_published(cfg: &Config) -> Option<MirrorList> {
    let pk = pinned(cfg)?;
    let (b, s) = cache_paths(cfg);
    let body = std::fs::read(b).ok()?;
    let sig = std::fs::read_to_string(s).ok()?;
    verify_list(&pk, &cfg.catalog.name, &body, &sig).ok()
}

async fn get(http: &reqwest::Client, url: &str) -> anyhow::Result<Option<Vec<u8>>> {
    let r = http.get(url).send().await?;
    if r.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let r = r.error_for_status()?;
    let b = r.bytes().await?;
    if b.len() > MAX_MIRRORS_BYTES {
        anyhow::bail!("{url}: larger than 1 MiB");
    }
    Ok(Some(b.to_vec()))
}

/// Fetch the published list from the first base that serves a valid one and
/// cache it. `Ok(None)` if no base serves one.
pub async fn fetch_published(cfg: &Config, bases: &[String]) -> anyhow::Result<Option<MirrorList>> {
    let Some(pk) = pinned(cfg) else {
        return Ok(None);
    };
    let http = reqwest::Client::builder()
        .user_agent(concat!("tensorseed/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let cat = &cfg.catalog.name;
    for base in bases {
        let url = format!("{base}{cat}/{MIRRORS_PATH}");
        let res: anyhow::Result<Option<(Vec<u8>, String, MirrorList)>> = async {
            let Some(body) = get(&http, &url).await? else {
                return Ok(None);
            };
            let sig = get(&http, &format!("{url}.minisig"))
                .await?
                .ok_or_else(|| anyhow::anyhow!("{url}: missing .minisig"))?;
            let sig = String::from_utf8(sig)?;
            let l = verify_list(&pk, cat, &body, &sig)?;
            Ok(Some((body, sig, l)))
        }
        .await;
        match res {
            Ok(Some((body, sig, l))) => {
                let (b, s) = cache_paths(cfg);
                if let Some(d) = b.parent() {
                    std::fs::create_dir_all(d)?;
                }
                std::fs::write(&s, &sig)?;
                std::fs::write(&b, &body)?;
                tracing::info!(%url, mirrors = l.mirrors.len(), "published mirror list verified");
                return Ok(Some(l));
            }
            Ok(None) => tracing::debug!(%url, "no published mirror list"),
            Err(e) => tracing::warn!(%url, "ignoring published mirror list: {e:#}"),
        }
    }
    Ok(None)
}

/// Merge every source into one list: usable HTTP mirrors only (enabled,
/// pinned to the configured key if pinned at all), deduplicated by URL
/// keeping the lowest priority, sorted by priority (stable).
pub fn ranked(cfg: &Config, published: Option<&MirrorList>) -> Vec<Ranked> {
    let pk = pinned(cfg);
    let mut all: Vec<Ranked> = Vec::new();
    let push_list = |l: &MirrorList, origin: &'static str, all: &mut Vec<Ranked>| {
        for m in l.mirrors.iter() {
            if m.is_http() && m.usable_for(pk.as_ref()) {
                all.push(Ranked {
                    priority: m.priority,
                    base: m.base(),
                    name: m.name.clone(),
                    origin,
                });
            }
        }
    };
    match load_local(cfg) {
        Ok(l) => push_list(&l, "local", &mut all),
        Err(e) => tracing::warn!("ignoring local mirror list: {e:#}"),
    }
    if let Some(l) = published {
        push_list(l, "published", &mut all);
    }
    for u in &cfg.file.mirror_urls {
        all.push(Ranked {
            priority: 100,
            base: Mirror::http("", u).base(),
            name: String::new(),
            origin: "config",
        });
    }
    for (priority, base) in cfg.storage.public_bases() {
        all.push(Ranked {
            priority,
            base,
            name: String::new(),
            origin: "storage",
        });
    }
    all.sort_by_key(|r| r.priority);
    let mut seen = std::collections::BTreeSet::new();
    all.retain(|r| seen.insert(r.base.clone()));
    all
}

/// Refresh the published list (unless `offline` or disabled) and return the
/// merged ranking. Network failures fall back to the cached list.
pub async fn refresh(cfg: &Config, offline: bool) -> Vec<Ranked> {
    let cached = load_cached_published(cfg);
    let published = if cfg.mirrors.fetch_published && !offline {
        let mut bases: Vec<String> = Vec::new();
        if !cfg.file.base_url.is_empty() {
            bases.push(Mirror::http("", &cfg.file.base_url).base());
        }
        bases.extend(ranked(cfg, cached.as_ref()).into_iter().map(|r| r.base));
        match fetch_published(cfg, &bases).await {
            Ok(Some(l)) => Some(l),
            Ok(None) => cached,
            Err(e) => {
                tracing::warn!("mirror list refresh failed: {e:#}");
                cached
            }
        }
    } else {
        cached
    };
    ranked(cfg, published.as_ref())
}

/// Catalog read bases after `file.base_url`, in priority order.
pub fn catalog_bases(cfg: &Config) -> Vec<String> {
    ranked(cfg, load_cached_published(cfg).as_ref())
        .into_iter()
        .map(|r| r.base)
        .collect()
}

/// Merge a manifest's webseeds (priority 100, publisher order) with the
/// mirror ranking (`<base>t/`), lowest priority first, deduplicated.
pub fn webseed_bases(cfg: &Config, manifest_webseeds: &[String]) -> Vec<String> {
    let mut v: Vec<(i64, String)> = manifest_webseeds.iter().map(|w| (100, w.clone())).collect();
    for r in ranked(cfg, load_cached_published(cfg).as_ref()) {
        v.push((r.priority, format!("{}t/", r.base)));
    }
    v.sort_by_key(|(p, _)| *p);
    let mut seen = std::collections::BTreeSet::new();
    v.into_iter()
        .map(|(_, b)| b)
        .filter(|b| seen.insert(b.clone()))
        .collect()
}

/// `tensorseed mirror ...`
pub async fn run(cfg: &Config, cmd: MirrorCmd) -> anyhow::Result<ExitCode> {
    match cmd {
        MirrorCmd::Add {
            url,
            name,
            provider,
            priority,
            magnet,
            label,
            pubkey,
            disabled,
        } => {
            let mut l = load_local(cfg)?;
            let m = Mirror {
                name: name.unwrap_or_default(),
                provider,
                url: url.unwrap_or_default(),
                magnet: magnet.unwrap_or_default(),
                label: label.unwrap_or_default(),
                priority,
                enabled: !disabled,
                publisher_pubkey: pubkey,
            };
            l.upsert(m)?;
            save_local(cfg, &l)?;
            println!("saved {}", local_path(cfg).display());
        }
        MirrorCmd::Remove { name_or_url } => {
            let mut l = load_local(cfg)?;
            if !l.remove(&name_or_url) {
                anyhow::bail!("no mirror named or at {name_or_url:?}");
            }
            save_local(cfg, &l)?;
            println!("removed {name_or_url}");
        }
        MirrorCmd::List { offline } => {
            let ranking = refresh(cfg, offline).await;
            if ranking.is_empty() {
                println!("no mirrors (file.base_url is still used)");
            }
            for r in ranking {
                println!(
                    "{:>5}  {:<9}  {:<16}  {}",
                    r.priority,
                    r.origin,
                    if r.name.is_empty() { "-" } else { &r.name },
                    r.base
                );
            }
        }
        MirrorCmd::Publish { file } => {
            let l = match file {
                Some(p) => MirrorList::parse(&std::fs::read_to_string(&p)?)
                    .with_context(|| format!("reading {}", p.display()))?,
                None => load_local(cfg)?,
            };
            let sk = load_secret(cfg)?;
            let (body, sig) = sign_list(&sk, &cfg.catalog.name, &l)?;
            let key = format!("{}/{MIRRORS_PATH}", cfg.catalog.name);
            let out = sink(cfg)?;
            out.put(
                &format!("{key}.minisig"),
                sig.into_bytes(),
                "text/plain",
                CachePolicy::Mutable,
            )
            .await?;
            out.put(&key, body, "application/toml", CachePolicy::Mutable)
                .await?;
            println!(
                "published {} mirrors to {}",
                l.mirrors.len(),
                crate::backend::describe_storage(cfg)
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}
