// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Torrent glue for the CLI: build a revision's torrent, fetch and check a
//! manifest's torrent, and the `seed` command (spec sections 7.1 and 9).
//!
//! Layout: a revision's `.torrent` sits next to its content directory, at
//! `t/<content name>.torrent` beside `t/<content name>/`, both locally and on
//! every storage target, so any webseed base also serves the torrent. A pulled
//! directory keeps its copy at `.tensorseed/torrent`.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, bail};
use tensorseed_core::manifest::{FileEntry, Manifest, encode_path};
use tensorseed_core::verify::META_DIR;
use tensorseed_torrent::engine::{Engine, EngineOptions};
use tensorseed_torrent::{BuildParams, InputFile, Metainfo, build_torrent};

use crate::config::{Config, expand};

/// File name of the torrent kept in a pulled directory's `.tensorseed/`.
pub const PULLED_TORRENT: &str = "torrent";

/// URLs of `<content name>.torrent` under each webseed base.
pub fn torrent_file_urls(webseeds: &[String], name: &str) -> Vec<String> {
    webseeds
        .iter()
        .map(|b| format!("{b}{}.torrent", encode_path(name)))
        .collect()
}

/// Build the torrent for a revision whose files are in `content_dir`, and
/// record each file's BEP 52 root in `files`. Hashing runs on a blocking
/// thread pool.
pub async fn build_for_revision(
    cfg: &Config,
    content_dir: &Path,
    name: &str,
    files: &mut [FileEntry],
    webseeds: &[String],
) -> anyhow::Result<Metainfo> {
    let total: u64 = files.iter().map(|f| f.size).sum();
    let params = BuildParams {
        name: name.to_owned(),
        piece_length: cfg.torrent.piece_length_for(total),
        hybrid: cfg.torrent.hybrid,
        trackers: cfg.torrent.trackers.clone(),
        webseeds: webseeds.to_vec(),
    };
    let inputs: Vec<InputFile> = files
        .iter()
        .map(|f| InputFile {
            path: f.path.clone(),
            length: f.size,
        })
        .collect();
    let dir = content_dir.to_owned();
    let threads = cfg.general.max_concurrent_downloads as usize;
    let t = tokio::task::spawn_blocking(move || build_torrent(&dir, &inputs, &params, threads))
        .await
        .context("torrent hashing task failed")??;
    for f in files.iter_mut() {
        let tf = t.file(&f.path).context("built torrent lost a file")?;
        if let Some(sha) = tf.sha256
            && hex::encode(sha) != f.sha256
        {
            bail!(
                "{} changed on disk (SHA-256 differs from the verified download)",
                f.path
            );
        }
        f.bt_v2_pieces_root = tf.pieces_root.map(hex::encode);
    }
    Ok(t)
}

/// Download a manifest's torrent from its `torrent_file_urls`, then from
/// `<base><content name>.torrent` for each extra webseed base, and check it
/// describes exactly the manifest's files.
pub async fn fetch_for_manifest(m: &Manifest, bases: &[String]) -> anyhow::Result<Metainfo> {
    let t = m.torrent.as_ref().context(
        "this revision has no torrent in its manifest (published before torrents existed)",
    )?;
    let mut urls = t.torrent_file_urls.clone();
    for u in torrent_file_urls(bases, &t.name) {
        if !urls.contains(&u) {
            urls.push(u);
        }
    }
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;
    let mut errors = Vec::new();
    for u in &urls {
        let r = async {
            let resp = http.get(u).send().await?;
            if !resp.status().is_success() {
                bail!("HTTP {}", resp.status());
            }
            let b = resp.bytes().await?;
            let meta = Metainfo::parse(&b)?;
            meta.check_manifest(m, &m.files)?;
            Ok(meta)
        }
        .await;
        match r {
            Ok(meta) => return Ok(meta),
            Err(e) => errors.push(format!("{u}: {e:#}")),
        }
    }
    bail!("no usable torrent for {}: {}", t.name, errors.join("; "))
}

/// Engine options from `[torrent]`. `listen = false` makes a download only
/// session.
pub fn engine_options(cfg: &Config, listen: bool, dir: &Path) -> anyhow::Result<EngineOptions> {
    let ip: IpAddr = cfg.torrent.listen_addr.parse()?;
    Ok(EngineOptions {
        listen: listen.then(|| SocketAddr::new(ip, cfg.torrent.listen_port)),
        dht: cfg.torrent.dht,
        trackers: true,
        lsd: cfg.torrent.lsd,
        upload_bps: match cfg.torrent.upload_limit_kbps {
            0 => None,
            k => Some(u32::try_from(k.saturating_mul(1024)).unwrap_or(u32::MAX)),
        },
        download_bps: None,
        utp: false,
        tcp: true,
        default_dir: dir.to_owned(),
    })
}

/// Parse `30s`, `10m`, `2h`, `1d`, or `forever` (also `0`).
pub fn parse_duration(s: &str) -> anyhow::Result<Option<Duration>> {
    let s = s.trim();
    if s == "forever" || s == "0" {
        return Ok(None);
    }
    let (n, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = n.parse().with_context(|| format!("bad duration {s:?}"))?;
    let secs = match unit {
        "" | "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86400,
        _ => bail!("bad duration {s:?} (use 30s, 10m, 2h, 1d, or forever)"),
    };
    Ok(Some(Duration::from_secs(secs)))
}

/// One thing to seed.
#[derive(Debug)]
pub struct SeedItem {
    /// Directory holding the files.
    pub dir: PathBuf,
    /// The torrent.
    pub torrent: Metainfo,
    /// Only these paths (pulled dirs that skipped files).
    pub only: Option<Vec<String>>,
}

/// Revisions under a seeding root: every `t/<name>.torrent` with a matching
/// `t/<name>/` directory.
pub fn scan_root(root: &Path) -> anyhow::Result<Vec<SeedItem>> {
    let t = root.join("t");
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&t) else {
        return Ok(out);
    };
    let mut paths: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    for p in paths {
        if p.extension().and_then(|e| e.to_str()) != Some("torrent") {
            continue;
        }
        let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let dir = t.join(stem);
        if !dir.is_dir() {
            continue;
        }
        let torrent = Metainfo::parse(&std::fs::read(&p)?)
            .with_context(|| format!("parsing {}", p.display()))?;
        if torrent.name != stem {
            tracing::warn!(
                "{}: torrent name {:?} does not match its file name; skipped",
                p.display(),
                torrent.name
            );
            continue;
        }
        out.push(SeedItem {
            dir,
            torrent,
            only: None,
        });
    }
    Ok(out)
}

/// A directory written by `pull` (needs `.tensorseed/torrent`); skipped
/// files (pickle formats) are left out of the seed.
pub fn pulled_dir(dir: &Path) -> anyhow::Result<SeedItem> {
    let tp = dir.join(META_DIR).join(PULLED_TORRENT);
    let torrent = Metainfo::parse(&std::fs::read(&tp).with_context(|| {
        format!(
            "{} has no torrent (pull it again with --swarm or --seed-after)",
            dir.display()
        )
    })?)?;
    let skipped: Vec<String> = std::fs::read(dir.join(META_DIR).join("pull.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| {
            v.get("skipped").and_then(|s| s.as_array()).map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
        })
        .unwrap_or_default();
    let only = (!skipped.is_empty()).then(|| {
        torrent
            .files
            .iter()
            .map(|f| f.path.clone())
            .filter(|p| !skipped.contains(p))
            .collect()
    });
    Ok(SeedItem {
        dir: dir.to_owned(),
        torrent,
        only,
    })
}

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
    if serve_catalog {
        bail!("--serve-catalog is not implemented yet (see docs/MILESTONES.md)");
    }
    let duration = duration.map(parse_duration).transpose()?.flatten();
    let mut items = Vec::new();
    if dirs.is_empty() {
        for root in cfg.storage.seed_roots() {
            items.extend(scan_root(&expand(&root))?);
        }
    } else {
        for d in dirs {
            if d.join(META_DIR).join(PULLED_TORRENT).exists() {
                items.push(pulled_dir(d)?);
            } else {
                items.extend(scan_root(d)?);
            }
        }
    }
    run_seeder(cfg, items, duration).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("forever").unwrap(), None);
        assert_eq!(parse_duration("0").unwrap(), None);
        assert_eq!(parse_duration("90").unwrap(), Some(Duration::from_secs(90)));
        assert_eq!(
            parse_duration("10m").unwrap(),
            Some(Duration::from_secs(600))
        );
        assert_eq!(
            parse_duration("2h").unwrap(),
            Some(Duration::from_secs(7200))
        );
        assert_eq!(
            parse_duration("1d").unwrap(),
            Some(Duration::from_secs(86400))
        );
        assert!(parse_duration("1w").is_err());
        assert!(parse_duration("m").is_err());
    }

    #[test]
    fn torrent_urls() {
        assert_eq!(
            torrent_file_urls(&["https://m.example/t/".into()], "org__n@0123456789ab"),
            vec!["https://m.example/t/org__n@0123456789ab.torrent".to_string()]
        );
    }

    #[test]
    fn auto_piece_length() {
        let t = crate::config::Torrent::default();
        assert_eq!(t.piece_length_for(1 << 30), 4 << 20);
        assert_eq!(t.piece_length_for(64 << 30), 4 << 20);
        assert_eq!(t.piece_length_for(100 << 30), 8 << 20);
        assert_eq!(t.piece_length_for(1 << 40), 64 << 20);
        let mut fixed = t.clone();
        fixed.piece_length = 1 << 20;
        assert_eq!(fixed.piece_length_for(1 << 40), 1 << 20);
    }
}
