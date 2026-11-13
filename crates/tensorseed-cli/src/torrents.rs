// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! The `seed` command (spec section 9). The torrent helpers live in
//! `tensorseed_app::torrents` and are re-exported here.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, bail};
use tensorseed_app::swarm::{LimitHit, SeedLimitAction, SeedLimits, SeedTotals, SeedTracker};
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
    let limits = cfg.torrent.seed_limits()?;
    let _speed = tensorseed_app::swarm::apply_plan(engine.rate_handle(), cfg.torrent.speed_plan()?);
    let mut seeds = Vec::new();
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
                let size = it
                    .torrent
                    .files
                    .iter()
                    .filter(|f| it.only.as_ref().is_none_or(|o| o.contains(&f.path)))
                    .map(|f| f.length)
                    .sum();
                seeds.push(Seeded {
                    name: it.torrent.name.clone(),
                    handle: h,
                    size,
                    tracker: SeedTracker::new(SeedTotals::default()),
                    stopped: None,
                    uploaded: 0,
                });
            }
            Err(e) => {
                failed += 1;
                println!("FAILED   {}: {e}", it.torrent.name);
            }
        }
    }
    if seeds.is_empty() {
        engine.stop().await;
        bail!("no torrent could be seeded");
    }
    if let Some(a) = addr {
        println!(
            "listening on {a}; {} torrent(s), {failed} failed",
            seeds.len()
        );
    }
    if !limits.is_unlimited() {
        println!(
            "seed limits: {}; then {}",
            describe_limits(&limits),
            match limits.action {
                SeedLimitAction::Pause => "pause",
                SeedLimitAction::Remove => "remove",
            }
        );
    }
    let report = |seeds: &[Seeded]| {
        for s in seeds {
            let up = if s.stopped.is_some() {
                s.uploaded
            } else {
                s.handle.progress().uploaded
            };
            match &s.stopped {
                Some(hit) => println!("  {}: uploaded {up} bytes (stopped: {hit})", s.name),
                None => println!("  {}: uploaded {up} bytes", s.name),
            }
        }
    };
    let deadline = duration.map(|d| tokio::time::Instant::now() + d);
    let mut tick = tokio::time::interval(Duration::from_secs(300));
    tick.tick().await;
    let mut limit_tick = tokio::time::interval(Duration::from_secs(5));
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = async {
                match deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending().await,
                }
            } => break,
            _ = tick.tick() => report(&seeds),
            _ = limit_tick.tick(), if !limits.is_unlimited() => {
                if enforce_limits(&engine, &limits, &mut seeds).await {
                    println!("every torrent reached its seed limit");
                    break;
                }
            }
        }
    }
    report(&seeds);
    engine.stop().await;
    Ok(if failed > 0 {
        ExitCode::from(crate::consumer::EXIT_VERIFY_FAILED)
    } else {
        ExitCode::SUCCESS
    })
}

struct Seeded {
    name: String,
    handle: tensorseed_torrent::engine::Handle,
    size: u64,
    tracker: SeedTracker,
    stopped: Option<LimitHit>,
    uploaded: u64,
}

fn describe_limits(l: &SeedLimits) -> String {
    let mut v = Vec::new();
    if let Some(r) = l.ratio {
        v.push(format!("ratio {r}"));
    }
    if let Some(t) = l.time {
        v.push(format!("{}s seeding", t.as_secs()));
    }
    v.join(", ")
}

/// Pause or remove torrents that reached a limit. True when none is left
/// seeding.
async fn enforce_limits(engine: &Engine, limits: &SeedLimits, seeds: &mut [Seeded]) -> bool {
    let now = std::time::Instant::now();
    for s in seeds.iter_mut().filter(|s| s.stopped.is_none()) {
        let p = s.handle.progress();
        let totals = s
            .tracker
            .tick(now, p.live && !s.handle.is_paused(), p.uploaded);
        let Some(hit) = limits.check_totals(&totals, s.size) else {
            continue;
        };
        let r = match limits.action {
            SeedLimitAction::Pause => engine.pause(&s.handle).await,
            SeedLimitAction::Remove => engine.remove(&s.handle).await,
        };
        match r {
            Ok(()) => println!(
                "{} {}: {hit}",
                match limits.action {
                    SeedLimitAction::Pause => "paused  ",
                    SeedLimitAction::Remove => "removed ",
                },
                s.name
            ),
            Err(e) => println!("FAILED   stopping {}: {e}", s.name),
        }
        s.uploaded = totals.uploaded_bytes;
        s.stopped = Some(hit);
    }
    seeds.iter().all(|s| s.stopped.is_some())
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
