// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Torrent glue: build a revision's torrent, fetch and check a manifest's
//! torrent, engine options, and seed directory scanning (spec sections 7.1
//! and 9). The `seed` command itself lives in the CLI.
//!
//! Layout: a revision's `.torrent` sits next to its content directory, at
//! `t/<content name>.torrent` beside `t/<content name>/`, both locally and on
//! every storage target, so any webseed base also serves the torrent. A pulled
//! directory keeps its copy at `.tensorseed/torrent`.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use tensorseed_core::manifest::{FileEntry, Manifest, encode_path};
use tensorseed_core::verify::META_DIR;
use tensorseed_torrent::engine::EngineOptions;
use tensorseed_torrent::{BuildParams, InputFile, Metainfo, build_torrent};

use crate::config::{Config, data_local_dir};

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
///
/// # Errors
///
/// Fails when a file cannot be read or the torrent cannot be built.
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
///
/// # Errors
///
/// Fails when no source serves a torrent that matches the manifest.
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

/// Where the DHT routing table is saved between runs:
/// `~/.local/share/tensorseed/dht.json` on Unix,
/// `%LOCALAPPDATA%\tensorseed\dht.json` on Windows.
pub fn dht_persistence_path() -> PathBuf {
    data_local_dir().join("tensorseed").join("dht.json")
}

/// Engine options from `[torrent]`. `listen = false` makes a download only
/// session (outbound connections, no listen port): that is what `pull`
/// uses, so a pull does not require an open port and does not seed unless
/// `--seed-after` starts a listening seeder afterwards.
///
/// # Errors
///
/// Fails when a configured address or peer is invalid or the data directory cannot be resolved.
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
        dht_persistence: Some(dht_persistence_path()),
        ipv4_only: ip.is_ipv4(),
    })
}

/// Parse `30s`, `10m`, `2h`, `1d`, or `forever` (also `0`).
///
/// # Errors
///
/// Fails on an unknown unit or a malformed number.
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
///
/// # Errors
///
/// Fails when the root cannot be read or a torrent file does not parse.
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
///
/// # Errors
///
/// Fails when `.tensorseed/torrent` is missing or does not parse.
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
