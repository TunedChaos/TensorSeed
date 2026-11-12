// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Configuration (spec section 10), loaded with figment from TOML plus
//! `TENSORSEED_*` environment overrides (nested keys split on `__`, for
//! example `TENSORSEED_CHIA__FEE_MOJOS=1000`).
//!
//! Secrets are never stored here. Fields ending in `_env` name the
//! environment variable that holds the secret.

use std::path::{Path, PathBuf};

use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
use serde::{Deserialize, Serialize};

/// Full configuration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// General paths and limits.
    pub general: General,
    /// Catalog backend selection.
    pub catalog: Catalog,
    /// Chia DataLayer backend settings.
    pub chia: Chia,
    /// Signed static-file backend settings.
    pub file: FileBackend,
    /// Hugging Face settings.
    pub huggingface: HuggingFace,
    /// Torrent settings.
    pub torrent: Torrent,
    /// Webseeds.
    pub webseed: Webseeds,
    /// Cold backups.
    pub backup: Backups,
    /// Publishing policy.
    pub policy: Policy,
    /// Publish targets.
    pub targets: Vec<Target>,
    /// Seeding daemon.
    pub seed: Seed,
}

/// `[general]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct General {
    /// sqlite state database.
    pub state_db: String,
    /// Download staging directory.
    pub staging_dir: String,
    /// Long term seeding directory.
    pub seed_dir: String,
    /// Keep files locally after upload for seeding.
    pub keep_local_seed: bool,
    /// Concurrent HF downloads.
    pub max_concurrent_downloads: u32,
}

impl Default for General {
    fn default() -> Self {
        Self {
            state_db: "~/.local/share/tensorseed/state.sqlite".into(),
            staging_dir: "~/.local/share/tensorseed/staging".into(),
            seed_dir: "~/.local/share/tensorseed/seed".into(),
            keep_local_seed: true,
            max_concurrent_downloads: 2,
        }
    }
}

/// `[catalog]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Catalog {
    /// Backend name: `chia` or `file`; see `tensorseed_catalog::KNOWN_BACKENDS`.
    pub backend: String,
    /// Logical catalog name bound into every entry signature (same in every backend).
    pub name: String,
    /// Pinned publisher public key (minisign base64, `RW...`). Every entry in
    /// every backend must verify against it.
    pub publisher_pubkey: String,
    /// Publisher secret key file (minisign format). Only needed to publish.
    pub publisher_seckey_file: Option<String>,
}

impl Default for Catalog {
    fn default() -> Self {
        Self {
            backend: "chia".into(),
            name: "tensorseed".into(),
            publisher_pubkey: String::new(),
            publisher_seckey_file: None,
        }
    }
}

/// `[file]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileBackend {
    /// HTTPS base for head, deltas, and snapshots.
    pub base_url: String,
    /// Extra read mirrors (any static host) tried after `base_url`.
    pub mirror_urls: Vec<String>,
    /// Local verified state and trust anchor.
    pub state_dir: String,
    /// Write a full snapshot every N revisions.
    pub snapshot_every: u64,
    /// Head validity; publishers re-sign before it lapses.
    pub head_ttl_secs: u64,
    /// Also publish each snapshot as a torrent.
    pub torrent: bool,
}

impl Default for FileBackend {
    fn default() -> Self {
        Self {
            base_url: "https://catalog.tensorseed.dev/".into(),
            mirror_urls: Vec::new(),
            state_dir: "~/.local/share/tensorseed/catalog".into(),
            snapshot_every: 64,
            head_ttl_secs: 7 * 24 * 3600,
            torrent: true,
        }
    }
}

/// `[chia]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Chia {
    /// DataLayer RPC URL.
    pub rpc_url: String,
    /// mTLS client certificate.
    pub ssl_cert: String,
    /// mTLS client key.
    pub ssl_key: String,
    /// Optional CA certificate for the RPC server.
    pub ca_cert: Option<String>,
    /// Catalog store id, set by `catalog create`.
    pub catalog_store_id: String,
    /// Fee per update in mojos.
    pub fee_mojos: u64,
    /// Confirmation poll interval.
    pub confirm_poll_secs: u64,
    /// Confirmation timeout.
    pub confirm_timeout_secs: u64,
    /// Public DataLayer HTTP mirror URLs.
    pub catalog_mirror_urls: Vec<String>,
    /// Mirror coin amount in mojos.
    pub mirror_amount_mojos: u64,
}

impl Default for Chia {
    fn default() -> Self {
        Self {
            rpc_url: "https://localhost:8562".into(),
            ssl_cert: "~/.chia/mainnet/config/ssl/data_layer/private_data_layer.crt".into(),
            ssl_key: "~/.chia/mainnet/config/ssl/data_layer/private_data_layer.key".into(),
            ca_cert: None,
            catalog_store_id: String::new(),
            fee_mojos: 0,
            confirm_poll_secs: 20,
            confirm_timeout_secs: 1800,
            catalog_mirror_urls: Vec::new(),
            mirror_amount_mojos: 0,
        }
    }
}

/// `[huggingface]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HuggingFace {
    /// Hub endpoint.
    pub endpoint: String,
    /// Env var holding the optional HF token.
    pub token_env: String,
}

impl Default for HuggingFace {
    fn default() -> Self {
        Self {
            endpoint: "https://huggingface.co".into(),
            token_env: "HF_TOKEN".into(),
        }
    }
}

/// `[torrent]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Torrent {
    /// Build hybrid v1+v2 torrents.
    pub hybrid: bool,
    /// Piece length in bytes (tune in M0).
    pub piece_length: u64,
    /// Tracker announce URLs.
    pub trackers: Vec<String>,
    /// Listen port.
    pub listen_port: u16,
    /// Upload cap in KiB/s, 0 means unlimited.
    pub upload_limit_kbps: u64,
    /// Enable DHT.
    pub dht: bool,
}

impl Default for Torrent {
    fn default() -> Self {
        Self {
            hybrid: true,
            piece_length: 4 * 1024 * 1024,
            trackers: Vec::new(),
            listen_port: 6881,
            upload_limit_kbps: 0,
            dht: true,
        }
    }
}

/// `[webseed]`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Webseeds {
    /// `[webseed.r2]`
    pub r2: R2,
}

/// `[webseed.r2]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct R2 {
    /// Enable the R2 webseed.
    pub enabled: bool,
    /// Env var holding the Cloudflare account id.
    pub account_id_env: String,
    /// Env var holding the access key id.
    pub access_key_env: String,
    /// Env var holding the secret access key.
    pub secret_key_env: String,
    /// Bucket name.
    pub bucket: String,
    /// Public base URL (custom domain), ending in `/`.
    pub public_base_url: String,
}

impl Default for R2 {
    fn default() -> Self {
        Self {
            enabled: false,
            account_id_env: "R2_ACCOUNT_ID".into(),
            access_key_env: "R2_ACCESS_KEY_ID".into(),
            secret_key_env: "R2_SECRET_ACCESS_KEY".into(),
            bucket: "tensorseed".into(),
            public_base_url: "https://models.tensorseed.dev/t/".into(),
        }
    }
}

/// `[backup]`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Backups {
    /// `[backup.b2]`
    pub b2: B2,
    /// `[backup.sia]`
    pub sia: Sia,
}

/// `[backup.b2]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct B2 {
    /// Enable B2 backup.
    pub enabled: bool,
    /// S3 endpoint.
    pub endpoint: String,
    /// Bucket name.
    pub bucket: String,
    /// Env var holding the key id.
    pub key_id_env: String,
    /// Env var holding the application key.
    pub app_key_env: String,
}

impl Default for B2 {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: String::new(),
            bucket: "tensorseed-cold".into(),
            key_id_env: "B2_KEY_ID".into(),
            app_key_env: "B2_APP_KEY".into(),
        }
    }
}

/// `[backup.sia]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Sia {
    /// Enable Sia backup.
    pub enabled: bool,
    /// renterd URL.
    pub renterd_url: String,
    /// Env var holding the renterd password.
    pub password_env: String,
}

impl Default for Sia {
    fn default() -> Self {
        Self {
            enabled: false,
            renterd_url: "http://localhost:9980".into(),
            password_env: "RENTERD_PASSWORD".into(),
        }
    }
}

/// `[policy]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    /// Licenses published by default.
    pub license_allowlist: Vec<String>,
    /// Licenses allowed only with attribution handling.
    pub attribution_licenses: Vec<String>,
    /// Allow gated repos (open or attribution licenses only).
    pub allow_gated: bool,
    /// Allow pickle based formats.
    pub allow_pickle_formats: bool,
}

impl Default for Policy {
    fn default() -> Self {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect();
        Self {
            license_allowlist: s(&[
                "apache-2.0",
                "mit",
                "bsd-3-clause",
                "bsd-2-clause",
                "cc-by-4.0",
                "cc0-1.0",
                "openrail",
                "openrail++",
            ]),
            attribution_licenses: Vec::new(),
            allow_gated: false,
            allow_pickle_formats: false,
        }
    }
}

/// `[[targets]]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    /// Repo id.
    pub repo: String,
    /// "model" or "dataset".
    #[serde(rename = "type", default = "default_target_type")]
    pub repo_type: String,
    /// Include globs.
    #[serde(default)]
    pub include: Vec<String>,
    /// Exclude globs.
    #[serde(default)]
    pub exclude: Vec<String>,
}

fn default_target_type() -> String {
    "model".into()
}

/// `[seed]`
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Seed {
    /// Poll interval.
    pub poll_interval_secs: u64,
    /// Disk budget in bytes.
    pub max_bytes: u64,
    /// Repo selection globs.
    pub select: Vec<String>,
    /// Seed only the latest revision of each repo.
    pub latest_only: bool,
    /// Delete data for removed or tombstoned items.
    pub prune: bool,
    /// Also serve the catalog.
    pub serve_catalog: bool,
}

impl Default for Seed {
    fn default() -> Self {
        Self {
            poll_interval_secs: 600,
            max_bytes: 2_000_000_000_000,
            select: Vec::new(),
            latest_only: true,
            prune: true,
            serve_catalog: false,
        }
    }
}

/// Default config file location.
pub fn default_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("tensorseed").join("tensorseed.toml")
}

/// Build the figment: defaults, then the TOML file (if present), then env.
pub fn figment(path: &Path) -> Figment {
    Figment::from(Serialized::defaults(Config::default()))
        .merge(Toml::file(path))
        .merge(Env::prefixed("TENSORSEED_").ignore(&["CONFIG"]).split("__"))
}

/// Load and validate configuration.
pub fn load(path: &Path) -> anyhow::Result<Config> {
    let cfg: Config = figment(path).extract()?;
    if !tensorseed_catalog::KNOWN_BACKENDS.contains(&cfg.catalog.backend.as_str()) {
        anyhow::bail!(
            "unknown catalog.backend {:?}; known backends: {:?}",
            cfg.catalog.backend,
            tensorseed_catalog::KNOWN_BACKENDS
        );
    }
    tensorseed_catalog::signed::check_catalog_name(&cfg.catalog.name)?;
    if !cfg.catalog.publisher_pubkey.is_empty() {
        tensorseed_catalog::minisign::PublicKey::parse(&cfg.catalog.publisher_pubkey)?;
    }
    Ok(cfg)
}

#[cfg(test)]
#[allow(clippy::result_large_err)] // figment::Error is large; fine in tests
mod tests {
    use super::*;
    use figment::Jail;

    const EXAMPLE: &str = include_str!("../../../config/tensorseed.example.toml");

    #[test]
    fn example_config_parses() {
        Jail::expect_with(|jail| {
            jail.create_file("tensorseed.toml", EXAMPLE)?;
            let cfg = load(Path::new("tensorseed.toml")).map_err(|e| e.to_string())?;
            assert_eq!(cfg.catalog.backend, "chia");
            assert_eq!(cfg.file.base_url, "https://catalog.tensorseed.dev/");
            assert_eq!(
                cfg.webseed.r2.public_base_url,
                "https://models.tensorseed.dev/t/"
            );
            assert!(!cfg.targets.is_empty());
            Ok(())
        });
    }

    #[test]
    fn missing_file_uses_defaults_and_env_overrides() {
        Jail::expect_with(|jail| {
            jail.set_env("TENSORSEED_CHIA__FEE_MOJOS", "1000");
            let cfg = load(Path::new("does-not-exist.toml")).map_err(|e| e.to_string())?;
            assert_eq!(cfg.chia.fee_mojos, 1000);
            assert_eq!(cfg.general.max_concurrent_downloads, 2);
            Ok(())
        });
    }

    #[test]
    fn file_backend_and_pinned_key() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "c.toml",
                "[catalog]\nbackend = \"file\"\nname = \"main\"\npublisher_pubkey = \"RWSSA0NfLXhfVAteHxL9PPhshHfwrMfYyxUF2VhGZIzNWARaJdwDR5TY\"\n",
            )?;
            let cfg = load(Path::new("c.toml")).map_err(|e| e.to_string())?;
            assert_eq!(cfg.catalog.backend, "file");
            jail.create_file("bad.toml", "[catalog]\npublisher_pubkey = \"nope\"\n")?;
            assert!(load(Path::new("bad.toml")).is_err());
            Ok(())
        });
    }

    #[test]
    fn unknown_backend_rejected() {
        Jail::expect_with(|jail| {
            jail.create_file("c.toml", "[catalog]\nbackend = \"nope\"\n")?;
            assert!(load(Path::new("c.toml")).is_err());
            Ok(())
        });
    }
}
