// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! The `seed` command (spec section 9). The torrent helpers live in
//! `tensorseed_app::torrents` and are re-exported here.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, bail};
pub use tensorseed_app::torrents::*;
use tensorseed_core::paths::native_path;
use tensorseed_core::verify::META_DIR;
use tensorseed_torrent::engine::Engine;

use crate::config::{Config, expand};

/// Seed `items` until `duration` passes (or forever, or Ctrl-C).
pub async fn run_seeder(
    cfg: &Config,
    items: Vec<SeedItem>,
    duration: Option<Duration>,
) -> anyhow::Result<ExitCode> {
    if items.is_empty() {
        println!("nothing to seed");
        return Ok(ExitCode::SUCCESS);
    }
    let state = expand(&cfg.general.state_db);
    let scratch = state
        .parent()
        .map(Path::to_owned)
        .unwrap_or_else(std::env::temp_dir);
    let engine = Engine::start(engine_options(cfg, true, &scratch)?)
        .await
        .context("starting the BitTorrent engine")?;
    let addr = engine.listen_addr();
    let mut handles = Vec::new();
    let mut failed = 0;
    for it in &items {
        match engine
            .seed(
                &it.torrent,
                &it.dir,
                it.only.as_deref(),
                Duration::from_secs(24 * 3600),
            )
            .await
        {
            Ok(h) => {
                println!(
                    "seeding {} from {} (btih {}, btmh 1220{})",
                    it.torrent.name,
                    it.dir.display(),
                    it.torrent.info_hash_v1_hex().unwrap_or_default(),
                    it.torrent.info_hash_v2_hex()
                );
                handles.push((it.torrent.name.clone(), h));
            }
            Err(e) => {
                failed += 1;
                println!("FAILED   {}: {e}", it.torrent.name);
            }
        }
    }
    if handles.is_empty() {
        engine.stop().await;
        bail!("no torrent could be seeded");
    }
    if let Some(a) = addr {
        println!(
            "listening on {a}; {} torrent(s), {failed} failed",
            handles.len()
        );
    }
    let report = |handles: &[(String, tensorseed_torrent::engine::Handle)]| {
        for (n, h) in handles {
            let p = h.progress();
            println!("  {n}: uploaded {} bytes", p.uploaded);
        }
    };
    let deadline = duration.map(|d| tokio::time::Instant::now() + d);
    let mut tick = tokio::time::interval(Duration::from_secs(300));
    tick.tick().await;
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = async {
                match deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending().await,
                }
            } => break,
            _ = tick.tick() => report(&handles),
        }
    }
    report(&handles);
    engine.stop().await;
    Ok(if failed > 0 {
        ExitCode::from(crate::consumer::EXIT_VERIFY_FAILED)
    } else {
        ExitCode::SUCCESS
    })
}

/// `tensorseed seed`.
pub async fn seed(
    cfg: &Config,
    dirs: &[PathBuf],
    duration: Option<&str>,
    serve_catalog: bool,
) -> anyhow::Result<ExitCode> {
    // Serve the publisher's catalog files next to the swarm (spec 6.7).
    let _catalog = if serve_catalog {
        Some(crate::serve::serve_local_catalog(cfg).await?)
    } else {
        None
    };
    let duration = duration.map(parse_duration).transpose()?.flatten();
    let mut items = Vec::new();
    if dirs.is_empty() {
        for root in cfg.storage.seed_roots() {
            items.extend(scan_root(&expand(&root))?);
        }
    } else {
        for d in dirs {
            let d = &native_path(d);
            if d.join(META_DIR).join(PULLED_TORRENT).exists() {
                items.push(pulled_dir(d)?);
            } else {
                items.extend(scan_root(d)?);
            }
        }
    }
    run_seeder(cfg, items, duration).await
}
