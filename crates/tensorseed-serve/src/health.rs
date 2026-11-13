// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! The `serve` health endpoint: a JSON status document (`ts.serve.health/1`)
//! on a local address (127.0.0.1 by default).
//!
//! - `GET /health` (and `/`): the full [`Status`] as JSON. HTTP 200 while
//!   the daemon is healthy, 503 when the last catalog sync failed and the
//!   catalog head is expired or was never loaded.
//! - `GET /healthz`: `ok` or `degraded` as plain text, same status codes.

use std::future::Future;
use std::sync::{Arc, RwLock};

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Serialize;

use crate::http::HttpStatsSnapshot;

/// Schema id of the health document.
pub const HEALTH_SCHEMA: &str = "ts.serve.health/1";

/// Catalog state.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CatalogStatus {
    /// Catalog name.
    pub name: String,
    /// Backend (`file` or `chia`).
    pub backend: String,
    /// Local verified revision (file) or generation (Chia).
    pub revision: u64,
    /// Root hash of the local revision (file backend).
    pub root: Option<String>,
    /// When the published head expires (unix seconds, file backend).
    pub head_expires_unix: Option<u64>,
    /// Source that served the last accepted head.
    pub source: Option<String>,
    /// Last sync attempt (unix seconds).
    pub last_sync_unix: Option<u64>,
    /// Last successful sync (unix seconds).
    pub last_sync_ok_unix: Option<u64>,
    /// Error from the last sync, if it failed.
    pub last_sync_error: Option<String>,
    /// Catalog files are mirrored and served over HTTP.
    pub serving: bool,
}

/// One seeded revision.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TorrentStatus {
    /// Content name (`org__name@commit12`).
    pub name: String,
    /// `model` or `dataset`.
    pub repo_type: String,
    /// Repo id.
    pub repo_id: String,
    /// Commit SHA.
    pub commit: String,
    /// Directory the files are served from.
    pub dir: String,
    /// Payload bytes.
    pub bytes: u64,
    /// v1 info hash (hex).
    pub info_hash_v1: String,
    /// Bytes uploaded to peers in this run.
    pub uploaded_bytes: u64,
    /// Connected peers.
    pub peers: u32,
    /// Engine state (`live`, `error: ...`).
    pub state: String,
    /// The daemon downloaded this revision itself (prunable).
    pub owned: bool,
}

/// Swarm totals.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SwarmStatus {
    /// Listening address.
    pub listen: Option<String>,
    /// uTP enabled next to TCP.
    pub utp: bool,
    /// DHT enabled.
    pub dht: bool,
    /// Bytes uploaded in this run.
    pub uploaded_bytes: u64,
    /// Bytes downloaded from peers in this run.
    pub downloaded_bytes: u64,
    /// Uploaded bytes including earlier runs (from the state file).
    pub uploaded_bytes_total: u64,
    /// Downloaded bytes including earlier runs.
    pub downloaded_bytes_total: u64,
    /// Upload speed (MiB/s).
    pub upload_mibps: f64,
    /// Download speed (MiB/s).
    pub download_mibps: f64,
    /// Connected peers over all torrents.
    pub peers_live: u32,
    /// Peers being connected.
    pub peers_connecting: u32,
    /// Upload cap in bytes per second (0 = unlimited).
    pub upload_limit_bps: u64,
    /// Download cap in bytes per second (0 = unlimited).
    pub download_limit_bps: u64,
}

/// NAT port mapping state.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PortMapStatus {
    /// `[serve] port_mapping`.
    pub enabled: bool,
    /// `nat-pmp` or `upnp` once a mapping works.
    pub method: Option<String>,
    /// Router's external address, when known.
    pub external_ip: Option<String>,
    /// Mapped external port (TCP and UDP).
    pub external_port: Option<u16>,
    /// Protocols mapped (`tcp`, `udp`).
    pub protocols: Vec<String>,
    /// Lease renewal time (unix seconds) of the current mapping.
    pub renewed_unix: Option<u64>,
    /// Last error.
    pub error: Option<String>,
}

/// HTTP serving state.
#[derive(Debug, Clone, Default, Serialize)]
pub struct HttpStatus {
    /// Listening address, when enabled.
    pub listen: Option<String>,
    /// Revisions are served as a webseed (`t/`).
    pub webseed: bool,
    /// Counters.
    pub stats: HttpStatsSnapshot,
    /// Upload cap in bytes per second (0 = unlimited).
    pub upload_limit_bps: u64,
}

/// Something selected but not seeded, or a pending download.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Note {
    /// `org/name@commit12` or a content name.
    pub item: String,
    /// What is going on.
    pub detail: String,
}

/// The whole health document.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    /// [`HEALTH_SCHEMA`].
    pub schema: &'static str,
    /// `ok` or `degraded`.
    pub status: String,
    /// Tool version.
    pub version: String,
    /// Process start (unix seconds).
    pub started_unix: u64,
    /// Seconds since start.
    pub uptime_secs: u64,
    /// Sync loop period.
    pub sync_interval_secs: u64,
    /// Catalog.
    pub catalog: CatalogStatus,
    /// Swarm totals.
    pub swarm: SwarmStatus,
    /// Seeded revisions.
    pub torrents: Vec<TorrentStatus>,
    /// Bytes of all seeded revisions.
    pub seeded_bytes: u64,
    /// Disk budget (`max_bytes`, 0 = unlimited).
    pub budget_bytes: u64,
    /// Revisions being downloaded.
    pub fetching: Vec<Note>,
    /// Revisions not seeded and why.
    pub skipped: Vec<Note>,
    /// Revisions stopped because of a takedown in this run.
    pub taken_down: Vec<Note>,
    /// HTTP serving.
    pub http: HttpStatus,
    /// Port mapping.
    pub port_mapping: PortMapStatus,
}

impl Status {
    /// A fresh status for `version`, started at `started_unix`.
    pub fn new(version: &str, started_unix: u64) -> Self {
        Self {
            schema: HEALTH_SCHEMA,
            status: "starting".into(),
            version: version.into(),
            started_unix,
            uptime_secs: 0,
            sync_interval_secs: 0,
            catalog: CatalogStatus::default(),
            swarm: SwarmStatus::default(),
            torrents: Vec::new(),
            seeded_bytes: 0,
            budget_bytes: 0,
            fetching: Vec::new(),
            skipped: Vec::new(),
            taken_down: Vec::new(),
            http: HttpStatus::default(),
            port_mapping: PortMapStatus::default(),
        }
    }

    /// Healthy unless the catalog is unusable: the last sync failed and the
    /// head is expired or was never loaded.
    pub fn healthy(&self, now_unix: u64) -> bool {
        if self.catalog.last_sync_error.is_none() {
            return true;
        }
        match self.catalog.head_expires_unix {
            Some(exp) => exp > now_unix,
            None => self.catalog.last_sync_ok_unix.is_some(),
        }
    }
}

/// Shared, updated by the daemon.
pub type SharedStatus = Arc<RwLock<Status>>;

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn snapshot(s: &SharedStatus) -> (bool, Status) {
    let mut st = s.read().unwrap_or_else(|e| e.into_inner()).clone();
    let now = now_unix();
    st.uptime_secs = now.saturating_sub(st.started_unix);
    let ok = st.healthy(now);
    if st.status != "starting" {
        st.status = if ok { "ok" } else { "degraded" }.into();
    }
    (ok, st)
}

async fn health(State(s): State<SharedStatus>) -> Response {
    let (ok, st) = snapshot(&s);
    let body = serde_json::to_string_pretty(&st).unwrap_or_else(|_| "{}".into());
    let code = if ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}

async fn healthz(State(s): State<SharedStatus>) -> Response {
    let (ok, _) = snapshot(&s);
    if ok {
        (StatusCode::OK, "ok\n").into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "degraded\n").into_response()
    }
}

/// The health router.
pub fn router(status: SharedStatus) -> Router {
    Router::new()
        .route("/", get(health))
        .route("/health", get(health))
        .route("/healthz", get(healthz))
        .with_state(status)
}

/// Serve the health endpoint until `shutdown` resolves.
///
/// # Errors
///
/// Any I/O error from accepting connections.
pub async fn serve(
    listener: tokio::net::TcpListener,
    status: SharedStatus,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, router(status))
        .with_graceful_shutdown(shutdown)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_rules() {
        let mut s = Status::new("0.1.0", 1);
        assert!(s.healthy(100));
        s.catalog.last_sync_error = Some("down".into());
        assert!(!s.healthy(100), "never synced and failing");
        s.catalog.last_sync_ok_unix = Some(50);
        assert!(s.healthy(100), "synced before, no expiry known (chia)");
        s.catalog.head_expires_unix = Some(90);
        assert!(!s.healthy(100), "expired head");
        s.catalog.head_expires_unix = Some(200);
        assert!(s.healthy(100));
    }

    #[tokio::test]
    async fn endpoint_serves_json() {
        let st: SharedStatus = Arc::new(RwLock::new(Status::new("0.1.0", now_unix())));
        {
            let mut w = st.write().unwrap();
            w.status = "ok".into();
            w.catalog.revision = 3;
            w.swarm.uploaded_bytes = 42;
            w.torrents.push(TorrentStatus {
                name: "org__n@0123456789ab".into(),
                peers: 2,
                ..Default::default()
            });
        }
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(serve(l, st.clone(), async {
            let _ = rx.await;
        }));
        let r = reqwest::get(format!("http://{addr}/health")).await.unwrap();
        assert_eq!(r.status(), 200);
        let v: serde_json::Value = serde_json::from_str(&r.text().await.unwrap()).unwrap();
        assert_eq!(v["schema"], HEALTH_SCHEMA);
        assert_eq!(v["status"], "ok");
        assert_eq!(v["catalog"]["revision"], 3);
        assert_eq!(v["swarm"]["uploaded_bytes"], 42);
        assert_eq!(v["torrents"][0]["peers"], 2);
        st.write().unwrap().catalog.last_sync_error = Some("x".into());
        let r = reqwest::get(format!("http://{addr}/healthz"))
            .await
            .unwrap();
        assert_eq!(r.status(), 503);
        let _ = tx.send(());
    }
}
