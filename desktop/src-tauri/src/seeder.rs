// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! The in-app seeder: pulled copies seeded with the same engine options,
//! speed schedule, and stop rules as `tensorseed seed` (all from the shared
//! `[torrent]` config). One seeder per machine: when `tensorseed serve` is
//! running it already seeds, so Desktop shows its status read only and
//! does not start a second engine on the same port.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use serde::Serialize;
use tensorseed_app::config::{Config, expand};
use tensorseed_app::swarm::{
    SeedLimitAction, SeedLimits, SeedTotals, SeedTracker, SpeedSchedule, apply_plan,
};
use tensorseed_app::torrents::{engine_options, pulled_dir};
use tensorseed_torrent::engine::{Engine, Handle};
use tokio::sync::Mutex;

/// How long the initial check of local files may take.
const CHECK_TIMEOUT: Duration = Duration::from_secs(24 * 3600);
/// How often the stop rules are checked.
const LIMIT_TICK: Duration = Duration::from_secs(5);

struct Seeded {
    name: String,
    dir: PathBuf,
    size: u64,
    handle: Handle,
    tracker: SeedTracker,
    /// Why a stop rule stopped it.
    stopped: Option<String>,
    /// Upload counter when it stopped.
    uploaded: u64,
}

struct Running {
    engine: Arc<Engine>,
    speed: Option<SpeedSchedule>,
    limits: SeedLimits,
    seeds: Vec<Seeded>,
}

/// The seeder state shared by commands, the tray, and the limit task.
#[derive(Default, Clone)]
pub struct Seeder {
    inner: Arc<Mutex<Option<Running>>>,
}

/// One seeded copy (`SeedView` in `src/lib/native.ts`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeedView {
    /// Content name (`org__name@commit12`).
    pub name: String,
    /// Directory the files are seeded from.
    pub dir: String,
    /// Payload bytes.
    pub bytes: u64,
    /// Bytes uploaded in this run.
    pub uploaded: u64,
    /// Connected peers.
    pub peers: u32,
    /// `seeding`, `paused`, `stopped`, or `error`.
    pub state: &'static str,
    /// Stop rule or error detail.
    pub detail: Option<String>,
}

/// The in-app seeder's status.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeedStatus {
    /// The engine is running.
    pub running: bool,
    /// Listen address.
    pub listen: Option<String>,
    /// Copies seeding now.
    pub seeding: usize,
    /// Copies paused by the user.
    pub paused: usize,
    /// Copies a stop rule stopped.
    pub stopped: usize,
    /// Upload speed (MiB/s).
    pub upload_mibps: f64,
    /// Download speed (MiB/s).
    pub download_mibps: f64,
    /// Connected peers.
    pub peers: u32,
    /// Bytes uploaded in this run.
    pub uploaded: u64,
    /// Each copy.
    pub torrents: Vec<SeedView>,
}

impl Running {
    async fn start(cfg: &Config) -> anyhow::Result<Self> {
        let state = expand(&cfg.general.state_db);
        let scratch = state
            .parent()
            .map(Path::to_owned)
            .unwrap_or_else(std::env::temp_dir);
        std::fs::create_dir_all(&scratch).ok();
        let limits = cfg.torrent.seed_limits()?;
        let plan = cfg.torrent.speed_plan()?;
        let engine = Engine::start(engine_options(cfg, true, &scratch)?)
            .await
            .with_context(|| {
                format!(
                    "starting the BitTorrent engine on port {} (is another program using it?)",
                    cfg.torrent.listen_port
                )
            })?;
        let speed = apply_plan(engine.rate_handle(), plan);
        Ok(Self {
            engine: Arc::new(engine),
            speed: Some(speed),
            limits,
            seeds: Vec::new(),
        })
    }

    /// Pause or remove copies that reached a stop rule.
    async fn enforce(&mut self) {
        if self.limits.is_unlimited() {
            return;
        }
        let now = Instant::now();
        for s in self.seeds.iter_mut().filter(|s| s.stopped.is_none()) {
            let p = s.handle.progress();
            let totals = s
                .tracker
                .tick(now, p.live && !s.handle.is_paused(), p.uploaded);
            let Some(hit) = self.limits.check_totals(&totals, s.size) else {
                continue;
            };
            let r = match self.limits.action {
                SeedLimitAction::Pause => self.engine.pause(&s.handle).await,
                SeedLimitAction::Remove => self.engine.remove(&s.handle).await,
            };
            if let Err(e) = r {
                tracing::warn!("stopping {}: {e}", s.name);
            }
            s.uploaded = totals.uploaded_bytes;
            s.stopped = Some(hit.to_string());
        }
    }
}

impl Seeder {
    /// Seed a pulled copy (it needs `.tensorseed/torrent`). Starts the
    /// engine on first use. Returns a short message.
    ///
    /// # Errors
    ///
    /// Fails when the copy has no torrent, the engine cannot start, or the
    /// local files do not match the torrent.
    pub async fn seed_dir(&self, cfg: &Config, dir: &Path) -> anyhow::Result<String> {
        let item = pulled_dir(dir)?;
        let engine = {
            let mut g = self.inner.lock().await;
            if g.is_none() {
                *g = Some(Running::start(cfg).await?);
                self.spawn_limit_task();
            }
            let Some(r) = g.as_ref() else {
                bail!("the seeder did not start");
            };
            if let Some(s) = r.seeds.iter().find(|s| s.dir == dir && s.stopped.is_none()) {
                if s.handle.is_paused() {
                    r.engine.resume(&s.handle).await?;
                    return Ok(format!("resumed seeding {}", s.name));
                }
                return Ok(format!("already seeding {}", s.name));
            }
            r.engine.clone()
        };
        let handle = engine
            .seed(
                &item.torrent,
                &item.dir,
                item.only.as_deref(),
                CHECK_TIMEOUT,
            )
            .await?;
        let size = item
            .torrent
            .files
            .iter()
            .filter(|f| item.only.as_ref().is_none_or(|o| o.contains(&f.path)))
            .map(|f| f.length)
            .sum();
        let name = item.torrent.name.clone();
        let mut g = self.inner.lock().await;
        let Some(r) = g.as_mut() else {
            bail!("the seeder stopped while checking {name}");
        };
        r.seeds.retain(|s| s.dir != dir);
        r.seeds.push(Seeded {
            name: name.clone(),
            dir: dir.to_owned(),
            size,
            handle,
            tracker: SeedTracker::new(SeedTotals::default()),
            stopped: None,
            uploaded: 0,
        });
        Ok(match r.engine.listen_addr() {
            Some(a) => format!("seeding {name} (listening on {a})"),
            None => format!("seeding {name}"),
        })
    }

    fn spawn_limit_task(&self) {
        let inner = Arc::clone(&self.inner);
        tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(LIMIT_TICK).await;
                let mut g = inner.lock().await;
                match g.as_mut() {
                    Some(r) => r.enforce().await,
                    None => break,
                }
            }
        });
    }

    /// Pause every seeding copy. Returns how many were paused.
    ///
    /// # Errors
    ///
    /// Fails when the engine rejects a pause.
    pub async fn pause_all(&self) -> anyhow::Result<usize> {
        let g = self.inner.lock().await;
        let Some(r) = g.as_ref() else { return Ok(0) };
        let mut n = 0;
        for s in r.seeds.iter().filter(|s| s.stopped.is_none()) {
            if !s.handle.is_paused() {
                r.engine.pause(&s.handle).await?;
                n += 1;
            }
        }
        Ok(n)
    }

    /// Resume every copy the user paused (not ones a stop rule stopped).
    ///
    /// # Errors
    ///
    /// Fails when the engine rejects a resume.
    pub async fn resume_all(&self) -> anyhow::Result<usize> {
        let g = self.inner.lock().await;
        let Some(r) = g.as_ref() else { return Ok(0) };
        let mut n = 0;
        for s in r.seeds.iter().filter(|s| s.stopped.is_none()) {
            if s.handle.is_paused() {
                r.engine.resume(&s.handle).await?;
                n += 1;
            }
        }
        Ok(n)
    }

    /// Apply new speed caps, schedule, and stop rules from `cfg` to a
    /// running seeder. Other torrent options apply on the next start.
    ///
    /// # Errors
    ///
    /// Fails when the new rules do not parse.
    pub async fn reload(&self, cfg: &Config) -> anyhow::Result<bool> {
        let mut g = self.inner.lock().await;
        let Some(r) = g.as_mut() else {
            return Ok(false);
        };
        r.limits = cfg.torrent.seed_limits()?;
        let plan = cfg.torrent.speed_plan()?;
        // Drop the old schedule first so it cannot overwrite the new caps.
        r.speed = None;
        r.speed = Some(apply_plan(r.engine.rate_handle(), plan));
        Ok(true)
    }

    /// Stop the engine (app exit). Files are never touched.
    pub async fn stop(&self) {
        let r = self.inner.lock().await.take();
        if let Some(r) = r {
            drop(r.seeds);
            drop(r.speed);
            match Arc::try_unwrap(r.engine) {
                Ok(e) => e.stop().await,
                Err(_) => tracing::warn!("the engine is still in use; leaving it to exit"),
            }
        }
    }

    /// True when `dir` is being seeded (or paused by the user).
    pub async fn is_seeding(&self, dir: &Path) -> bool {
        let g = self.inner.lock().await;
        g.as_ref()
            .is_some_and(|r| r.seeds.iter().any(|s| s.dir == dir && s.stopped.is_none()))
    }

    /// True when anything is seeding, so closing the window should hide
    /// it instead of quitting.
    pub async fn active(&self) -> bool {
        let g = self.inner.lock().await;
        g.as_ref()
            .is_some_and(|r| r.seeds.iter().any(|s| s.stopped.is_none()))
    }

    /// [`Self::active`] without waiting: when the state is busy, assume
    /// something is seeding (hiding the window is the safe choice).
    pub fn active_hint(&self) -> bool {
        match self.inner.try_lock() {
            Ok(g) => g
                .as_ref()
                .is_some_and(|r| r.seeds.iter().any(|s| s.stopped.is_none())),
            Err(_) => true,
        }
    }

    /// Current status.
    pub async fn status(&self) -> SeedStatus {
        let g = self.inner.lock().await;
        let Some(r) = g.as_ref() else {
            return SeedStatus::default();
        };
        let st = r.engine.stats();
        let mut out = SeedStatus {
            running: true,
            listen: r.engine.listen_addr().map(|a| a.to_string()),
            upload_mibps: st.upload_mibps,
            download_mibps: st.download_mibps,
            peers: st.peers_live,
            uploaded: st.uploaded,
            ..SeedStatus::default()
        };
        for s in &r.seeds {
            let (state, detail, uploaded) = if let Some(hit) = &s.stopped {
                out.stopped += 1;
                ("stopped", Some(hit.clone()), s.uploaded)
            } else {
                let p = s.handle.progress();
                if let Some(e) = p.error {
                    ("error", Some(e), p.uploaded)
                } else if s.handle.is_paused() {
                    out.paused += 1;
                    ("paused", None, p.uploaded)
                } else {
                    out.seeding += 1;
                    ("seeding", None, p.uploaded)
                }
            };
            out.torrents.push(SeedView {
                name: s.name.clone(),
                dir: s.dir.display().to_string(),
                bytes: s.size,
                uploaded,
                peers: if s.stopped.is_some() {
                    0
                } else {
                    s.handle.peers()
                },
                state,
                detail,
            });
        }
        out
    }
}
