// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Torrent activity for the tray and the status bar: the in-app seeder,
//! running pulls, and another seeder on this machine. A `tensorseed serve`
//! that holds the machine's seeder lock is attached through its local
//! control endpoint (address and token in its owner only `seeder.json`),
//! so Desktop can show and pause what it seeds. A serve running under
//! another state folder (a service account) is still shown read only
//! through its health endpoint, `serve.health_listen`.

use std::net::{SocketAddr, ToSocketAddrs};
use std::time::Duration;

use serde::Serialize;
use tensorseed_app::config::Config;
use tensorseed_app::seeder_lock::{self, Control};

use crate::jobs::{JobEvent, JobKind, Jobs};
use crate::seeder::{SeedStatus, Seeder};

/// Event carrying a [`Status`] every few seconds.
pub const STATUS_EVENT: &str = "status://update";

/// One revision a `tensorseed serve` seeds.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ServeTorrent {
    /// Revision name (`org__name@commit12`).
    pub name: String,
    /// `live`, `checking`, `paused: ...`, or `error: ...`.
    pub state: String,
    /// Payload bytes.
    pub bytes: u64,
    /// Bytes uploaded in this run.
    pub uploaded: u64,
    /// Connected peers.
    pub peers: u32,
}

/// Another seeder on this machine: a `tensorseed serve` or `tensorseed seed`.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ServeView {
    /// `serve` or `seed`.
    pub kind: String,
    /// Its process id, when it holds the seeder lock.
    pub pid: Option<u32>,
    /// Desktop can pause and resume it (attached over the control endpoint).
    pub control: bool,
    /// Revisions it seeds.
    pub torrents: usize,
    /// Revisions paused (by the user or a stop rule).
    pub paused: usize,
    /// Bytes uploaded in this run.
    pub uploaded: u64,
    /// Each revision (empty when unknown).
    pub list: Vec<ServeTorrent>,
    /// Upload speed (MiB/s).
    pub upload_mibps: f64,
    /// Download speed (MiB/s).
    pub download_mibps: f64,
    /// Connected peers.
    pub peers: u32,
    /// `ok` or `degraded`.
    pub status: String,
}

/// Everything the tray and status bar show.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// The in-app seeder.
    pub desktop: SeedStatus,
    /// A `tensorseed serve` on this machine, if one answers.
    pub serve: Option<ServeView>,
    /// Running pull and verify jobs.
    pub jobs: Vec<JobEvent>,
    /// Running pulls.
    pub downloading: usize,
    /// Pulls stopped by Pause all.
    pub paused_pulls: usize,
}

impl Status {
    /// Copies seeding, in Desktop and in `serve`.
    pub fn seeding(&self) -> usize {
        self.desktop.seeding
            + self
                .serve
                .as_ref()
                .map_or(0, |s| s.torrents.saturating_sub(s.paused))
    }

    /// Seeds and pulls that Pause all can pause (an attached serve's
    /// count, not a read only one's).
    pub fn pausable(&self) -> usize {
        let serve = self
            .serve
            .as_ref()
            .filter(|s| s.control)
            .map_or(0, |s| s.torrents.saturating_sub(s.paused));
        self.desktop.seeding + self.downloading + serve
    }

    /// Seeds and pulls paused, here and in an attached serve.
    pub fn paused(&self) -> usize {
        self.desktop.paused + self.paused_pulls + self.serve.as_ref().map_or(0, |s| s.paused)
    }

    /// One line for the tray menu and tooltip.
    pub fn summary(&self) -> String {
        let up = self.desktop.upload_mibps + self.serve.as_ref().map_or(0.0, |s| s.upload_mibps);
        let down =
            self.desktop.download_mibps + self.serve.as_ref().map_or(0.0, |s| s.download_mibps);
        let mut parts = vec![format!(
            "Seeding {}, downloading {}",
            self.seeding(),
            self.downloading
        )];
        if self.paused() > 0 {
            parts.push(format!("{} paused", self.paused()));
        }
        if self.desktop.running || self.serve.is_some() {
            parts.push(format!("up {} down {}", rate(up), rate(down)));
        }
        if let Some(s) = &self.serve {
            parts.push(format!(
                "{} running",
                if s.kind == "seed" {
                    "tensorseed seed"
                } else {
                    "serve"
                }
            ));
        }
        parts.join(", ")
    }
}

fn rate(mibps: f64) -> String {
    if mibps >= 1.0 {
        format!("{mibps:.1} MiB/s")
    } else {
        format!("{:.0} KiB/s", mibps * 1024.0)
    }
}

fn health_addr(listen: &str) -> Option<SocketAddr> {
    let listen = listen.trim();
    if listen.is_empty() {
        return None;
    }
    let a: SocketAddr = listen
        .parse()
        .ok()
        .or_else(|| listen.to_socket_addrs().ok().and_then(|mut a| a.next()))?;
    Some(if a.ip().is_unspecified() {
        match a {
            SocketAddr::V4(_) => SocketAddr::from(([127, 0, 0, 1], a.port())),
            SocketAddr::V6(_) => SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, a.port())),
        }
    } else {
        a
    })
}

fn parse_health(v: &serde_json::Value) -> Option<ServeView> {
    if v.get("schema").and_then(|s| s.as_str()) != Some(tensorseed_serve::health::HEALTH_SCHEMA) {
        return None;
    }
    let sw = &v["swarm"];
    let list: Vec<ServeTorrent> = v["torrents"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|t| ServeTorrent {
                    name: t["name"].as_str().unwrap_or_default().to_string(),
                    state: t["state"].as_str().unwrap_or_default().to_string(),
                    bytes: t["bytes"].as_u64().unwrap_or(0),
                    uploaded: t["uploaded_bytes"].as_u64().unwrap_or(0),
                    peers: t["peers"]
                        .as_u64()
                        .and_then(|p| u32::try_from(p).ok())
                        .unwrap_or(0),
                })
                .collect()
        })
        .unwrap_or_default();
    Some(ServeView {
        kind: "serve".into(),
        torrents: list.len(),
        paused: list
            .iter()
            .filter(|t| t.state.starts_with("paused"))
            .count(),
        uploaded: sw["uploaded_bytes"].as_u64().unwrap_or(0),
        list,
        upload_mibps: sw["upload_mibps"].as_f64().unwrap_or(0.0),
        download_mibps: sw["download_mibps"].as_f64().unwrap_or(0.0),
        peers: sw["peers_live"]
            .as_u64()
            .and_then(|p| u32::try_from(p).ok())
            .unwrap_or(0),
        status: v["status"].as_str().unwrap_or("ok").to_string(),
        ..ServeView::default()
    })
}

/// The control client of the `tensorseed serve` holding this machine's
/// seeder lock, if any.
pub fn serve_control(cfg: &Config) -> Option<Control> {
    let info = seeder_lock::holder(cfg)?;
    (info.pid != std::process::id() && info.kind == "serve")
        .then(|| Control::of(&info))
        .flatten()
}

/// Another seeder on this machine: the holder of the seeder lock (attached
/// over the control endpoint when it is a serve), else a serve answering
/// on `serve.health_listen`.
pub async fn probe_serve(cfg: &Config) -> Option<ServeView> {
    if let Some(info) = seeder_lock::holder(cfg)
        && info.pid != std::process::id()
    {
        if let Some(c) = Control::of(&info)
            && let Ok(v) = c.status().await
            && let Some(mut s) = parse_health(&v)
        {
            s.pid = Some(info.pid);
            s.control = true;
            return Some(s);
        }
        // `tensorseed seed`, or a serve whose endpoint does not answer.
        return Some(ServeView {
            kind: info.kind,
            pid: Some(info.pid),
            status: "ok".into(),
            ..ServeView::default()
        });
    }
    probe_health(cfg).await
}

/// Ask a local `tensorseed serve` for its health document (read only).
async fn probe_health(cfg: &Config) -> Option<ServeView> {
    let addr = health_addr(&cfg.serve.health_listen)?;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_millis(800))
        .no_proxy()
        .build()
        .ok()?;
    let r = http
        .get(format!("http://{addr}/health"))
        .send()
        .await
        .ok()?;
    parse_health(&r.json::<serde_json::Value>().await.ok()?)
}

/// Gather the status now.
pub async fn gather(seeder: &Seeder, jobs: &Jobs, cfg: Option<&Config>) -> Status {
    let serve = match cfg {
        Some(c) => probe_serve(c).await,
        None => None,
    };
    Status {
        desktop: seeder.status().await,
        serve,
        jobs: jobs.list(),
        downloading: jobs.count(JobKind::Pull),
        paused_pulls: crate::pulls::paused_count(jobs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_lines() {
        let mut s = Status::default();
        assert_eq!(s.summary(), "Seeding 0, downloading 0");
        s.desktop.running = true;
        s.desktop.seeding = 2;
        s.desktop.paused = 1;
        s.desktop.upload_mibps = 1.5;
        s.downloading = 1;
        assert_eq!(
            s.summary(),
            "Seeding 2, downloading 1, 1 paused, up 1.5 MiB/s down 0 KiB/s"
        );
        s.serve = Some(ServeView {
            torrents: 3,
            upload_mibps: 0.25,
            ..ServeView::default()
        });
        assert_eq!(
            s.summary(),
            "Seeding 5, downloading 1, 1 paused, up 1.8 MiB/s down 0 KiB/s, serve running"
        );
        s.serve.as_mut().unwrap().paused = 1;
        assert_eq!(
            s.summary(),
            "Seeding 4, downloading 1, 2 paused, up 1.8 MiB/s down 0 KiB/s, serve running"
        );
    }

    #[test]
    fn health_documents() {
        let v = serde_json::json!({
            "schema": tensorseed_serve::health::HEALTH_SCHEMA,
            "status": "ok",
            "swarm": { "upload_mibps": 2.0, "download_mibps": 0.5, "peers_live": 4 },
            "torrents": [
                { "name": "o__a@0123456789ab", "state": "live", "bytes": 10, "uploaded_bytes": 3, "peers": 1 },
                { "name": "o__b@0123456789ab", "state": "paused: by the user" },
            ],
        });
        let s = parse_health(&v).unwrap();
        assert_eq!((s.torrents, s.peers, s.paused), (2, 4, 1));
        assert_eq!(s.list[0].uploaded, 3);
        assert_eq!(s.list[1].state, "paused: by the user");
        assert!(!s.control, "read only until attached");
        assert!(parse_health(&serde_json::json!({ "schema": "other" })).is_none());
        assert_eq!(
            health_addr("0.0.0.0:6880").unwrap(),
            "127.0.0.1:6880".parse().unwrap()
        );
        assert!(health_addr("").is_none());
    }
}
