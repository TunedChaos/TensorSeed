// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! `tensorseed mirror ...`. The merging logic lives in
//! `tensorseed_app::mirrors` and is re-exported here.

use std::process::ExitCode;

use anyhow::Context;
pub use tensorseed_app::mirrors::*;
use tensorseed_catalog::mirrors::{MIRRORS_PATH, Mirror, MirrorList, sign_list};
use tensorseed_catalog::sink::CachePolicy;

use crate::backend::sink;
use crate::cli::MirrorCmd;
use crate::config::Config;
use crate::keys::load_secret;

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
