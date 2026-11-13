// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! `[serve]` configuration for the long running seeder (spec section 8.3).

use std::net::SocketAddr;

use anyhow::bail;
use serde::{Deserialize, Serialize};
use tensorseed_serve::select::Rules;

use crate::config::Config;

/// `[serve]`: the long running seeder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServeConfig {
    /// Downloaded revisions (`t/`), the catalog mirror, and the state file
    /// (default: `~/.local/share/tensorseed/serve` on Unix,
    /// `%LOCALAPPDATA%\tensorseed\serve` on Windows).
    pub data_dir: String,
    /// Extra published trees (with `t/`) to seed from, next to
    /// `storage.local_dir` and torrent storage targets.
    pub dirs: Vec<String>,
    /// Seconds between catalog syncs (takedowns apply within one period).
    pub sync_interval_secs: u64,
    /// Repo id globs to seed (`*`, `org/*`, `org/name`); empty seeds nothing.
    pub select: Vec<String>,
    /// Repo id globs never seeded.
    pub exclude: Vec<String>,
    /// Also select the `[harvest]` watchlist (`repos` and `authors`).
    pub select_watchlist: bool,
    /// Repo types to seed (`model`, `dataset`); empty means both.
    pub types: Vec<String>,
    /// License classes to seed (`open`, `attribution`, `restricted`,
    /// `unknown`); empty means any class the publisher published.
    pub license_classes: Vec<String>,
    /// Seed only each repo's latest revision.
    pub latest_only: bool,
    /// Disk budget: total bytes of all seeded revisions (0 = unlimited).
    pub max_bytes: u64,
    /// Skip revisions larger than this (0 = no cap).
    pub max_revision_bytes: u64,
    /// Download selected revisions that are not on disk yet.
    pub fetch: bool,
    /// Downloads try the swarm first (then webseeds, mirrors, the Hub).
    pub fetch_swarm: bool,
    /// Delete downloaded copies (under `data_dir`) when a revision is taken
    /// down or no longer selected. Published trees are never deleted.
    pub prune: bool,
    /// Upload cap in KiB/s (0 = `torrent.upload_limit_kbps`).
    pub upload_limit_kbps: u64,
    /// Swarm download cap in KiB/s (0 = unlimited).
    pub download_limit_kbps: u64,
    /// Accept and dial uTP next to TCP (UDP on the torrent port).
    pub utp: bool,
    /// Ask the router to forward the torrent port (NAT-PMP, then UPnP IGD).
    /// Off by default: it opens a port on the router.
    pub port_mapping: bool,
    /// Port mapping lease, renewed at half time.
    pub port_mapping_lease_secs: u32,
    /// Serve seeded revisions over HTTP here (`0.0.0.0:8080`); empty = off.
    pub http_listen: String,
    /// HTTP upload cap in KiB/s over all connections (0 = unlimited).
    pub http_upload_limit_kbps: u64,
    /// Mirror the signed catalog files and serve them over HTTP.
    pub serve_catalog: bool,
    /// Health endpoint (JSON); empty = off. Keep it on localhost.
    pub health_listen: String,
    /// Longest time to check one revision's pieces before seeding it.
    pub check_timeout_secs: u64,
    /// Per revision share ratio limit; unset uses `torrent.seed_ratio_limit`,
    /// 0 means no limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed_ratio_limit: Option<f64>,
    /// Per revision seeding time limit (`7d`); unset uses
    /// `torrent.seed_time_limit`, empty means no limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed_time_limit: Option<String>,
    /// `pause` or `remove` a revision that hit a limit; unset uses
    /// `torrent.seed_limit_action`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed_limit_action: Option<crate::swarm::SeedLimitAction>,
    /// Apply the `[torrent.alt_speed]` schedule to serve's swarm caps.
    pub alt_speed: bool,
}

impl Default for ServeConfig {
    fn default() -> Self {
        Self {
            data_dir: crate::config::default_data_file("serve"),
            dirs: Vec::new(),
            sync_interval_secs: 300,
            select: vec!["*".into()],
            exclude: Vec::new(),
            select_watchlist: false,
            types: Vec::new(),
            license_classes: Vec::new(),
            latest_only: true,
            max_bytes: 0,
            max_revision_bytes: 0,
            fetch: false,
            fetch_swarm: true,
            prune: true,
            upload_limit_kbps: 0,
            download_limit_kbps: 0,
            utp: true,
            port_mapping: false,
            port_mapping_lease_secs: 3600,
            http_listen: String::new(),
            http_upload_limit_kbps: 0,
            serve_catalog: false,
            health_listen: "127.0.0.1:6880".into(),
            check_timeout_secs: 3600,
            seed_ratio_limit: None,
            seed_time_limit: None,
            seed_limit_action: None,
            alt_speed: true,
        }
    }
}

const LICENSE_CLASSES: [&str; 4] = ["open", "attribution", "restricted", "unknown"];

impl ServeConfig {
    /// Check values that would otherwise fail later.
    ///
    /// # Errors
    ///
    /// Fails on the first invalid value, naming the key.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.sync_interval_secs == 0 {
            bail!("serve.sync_interval_secs must be at least 1");
        }
        for (k, v) in [
            ("serve.http_listen", &self.http_listen),
            ("serve.health_listen", &self.health_listen),
        ] {
            if !v.is_empty() && v.parse::<SocketAddr>().is_err() {
                bail!("{k} must be ADDR:PORT (for example 127.0.0.1:6880), got {v:?}");
            }
        }
        if let Some(t) = self
            .types
            .iter()
            .find(|t| !["model", "dataset"].contains(&t.as_str()))
        {
            bail!("serve.types: unknown repo type {t:?} (use model or dataset)");
        }
        if let Some(c) = self
            .license_classes
            .iter()
            .find(|c| !LICENSE_CLASSES.contains(&c.as_str()))
        {
            bail!("serve.license_classes: unknown class {c:?} (use {LICENSE_CLASSES:?})");
        }
        if self.port_mapping && self.port_mapping_lease_secs < 120 {
            bail!("serve.port_mapping_lease_secs must be at least 120");
        }
        let t = crate::config::Torrent::default();
        self.seed_limits(&t)?;
        Ok(())
    }

    /// Per revision seeding stop rules: these `[serve]` overrides on top of
    /// `[torrent]`.
    ///
    /// # Errors
    ///
    /// Fails on a negative ratio or a malformed time limit.
    pub fn seed_limits(
        &self,
        torrent: &crate::config::Torrent,
    ) -> anyhow::Result<crate::swarm::SeedLimits> {
        crate::swarm::SeedLimits::new(
            self.seed_ratio_limit.unwrap_or(torrent.seed_ratio_limit),
            self.seed_time_limit
                .as_deref()
                .unwrap_or(&torrent.seed_time_limit),
            self.seed_limit_action.unwrap_or(torrent.seed_limit_action),
        )
        .map_err(|e| anyhow::anyhow!("serve.{e:#}"))
    }

    /// Serve's swarm caps: `upload_limit_kbps` (falling back to
    /// `torrent.upload_limit_kbps`) and `download_limit_kbps`, with the
    /// `[torrent.alt_speed]` schedule when `alt_speed` is on.
    ///
    /// # Errors
    ///
    /// Fails when an `alt_speed` window is invalid.
    pub fn speed_plan(
        &self,
        torrent: &crate::config::Torrent,
    ) -> anyhow::Result<crate::swarm::SpeedPlan> {
        let up = if self.upload_limit_kbps > 0 {
            self.upload_limit_kbps
        } else {
            torrent.upload_limit_kbps
        };
        let off = crate::swarm::AltSpeed::default();
        crate::swarm::SpeedPlan::new(
            crate::swarm::RateLimits::from_kbps(up, self.download_limit_kbps),
            if self.alt_speed {
                &torrent.alt_speed
            } else {
                &off
            },
        )
    }

    /// The `tensorseed_serve` selection rules for this config, including the
    /// `[harvest]` watchlist when `select_watchlist` is set.
    #[must_use]
    pub fn rules(&self, cfg: &Config) -> Rules {
        let mut select = self.select.clone();
        if self.select_watchlist {
            for r in cfg.harvest.repos.iter().chain(cfg.harvest.datasets.iter()) {
                select.push(r.id().to_owned());
            }
            for a in &cfg.harvest.authors {
                select.push(format!("{a}/*"));
            }
        }
        Rules {
            select,
            exclude: self.exclude.clone(),
            types: self.types.clone(),
            license_classes: self.license_classes.clone(),
            latest_only: self.latest_only,
            max_revision_bytes: self.max_revision_bytes,
            max_bytes: self.max_bytes,
        }
    }
}
