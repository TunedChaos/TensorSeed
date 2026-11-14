// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Torrent activity for the tray and the status bar: the in-app seeder,
//! running pulls, and a `tensorseed serve` on this machine (read through
//! its local health endpoint, `serve.health_listen`).

use std::net::{SocketAddr, ToSocketAddrs};
use std::time::Duration;

use serde::Serialize;
use tensorseed_app::config::Config;

use crate::jobs::{JobEvent, JobKind, Jobs};
use crate::seeder::{SeedStatus, Seeder};

/// Event carrying a [`Status`] every few seconds.
pub const STATUS_EVENT: &str = "status://update";

/// What a running `tensorseed serve` reports.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ServeView {
    /// Revisions it seeds.
    pub torrents: usize,
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
}

impl Status {
    /// Copies seeding, in Desktop and in `serve`.
    pub fn seeding(&self) -> usize {
        self.desktop.seeding + self.serve.as_ref().map_or(0, |s| s.torrents)
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
        if self.desktop.paused > 0 {
            parts.push(format!("{} paused", self.desktop.paused));
        }
        if self.desktop.running || self.serve.is_some() {
            parts.push(format!("up {} down {}", rate(up), rate(down)));
        }
        if self.serve.is_some() {
            parts.push("serve running".into());
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
    Some(ServeView {
        torrents: v["torrents"].as_array().map_or(0, Vec::len),
        upload_mibps: sw["upload_mibps"].as_f64().unwrap_or(0.0),
        download_mibps: sw["download_mibps"].as_f64().unwrap_or(0.0),
        peers: sw["peers_live"]
            .as_u64()
            .and_then(|p| u32::try_from(p).ok())
            .unwrap_or(0),
        status: v["status"].as_str().unwrap_or("ok").to_string(),
    })
}

/// Ask a local `tensorseed serve` for its health document.
pub async fn probe_serve(cfg: &Config) -> Option<ServeView> {
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
    }

    #[test]
    fn health_documents() {
        let v = serde_json::json!({
            "schema": tensorseed_serve::health::HEALTH_SCHEMA,
            "status": "ok",
            "swarm": { "upload_mibps": 2.0, "download_mibps": 0.5, "peers_live": 4 },
            "torrents": [{}, {}],
        });
        let s = parse_health(&v).unwrap();
        assert_eq!((s.torrents, s.peers), (2, 4));
        assert!(parse_health(&serde_json::json!({ "schema": "other" })).is_none());
        assert_eq!(
            health_addr("0.0.0.0:6880").unwrap(),
            "127.0.0.1:6880".parse().unwrap()
        );
        assert!(health_addr("").is_none());
    }
}
