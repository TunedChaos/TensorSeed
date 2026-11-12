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
    /// Where published objects go (`[storage]`).
    pub storage: Storage,
    /// Cold backups.
    pub backup: Backups,
    /// Publishing policy.
    pub policy: Policy,
    /// Publish targets.
    pub targets: Vec<Target>,
    /// Seeding daemon.
    pub seed: Seed,
    /// Automatic harvest watchlist (`[harvest]`).
    pub harvest: Harvest,
}

/// `[harvest]`: what `tensorseed harvest` mirrors from the Hub.
///
/// Explicit `repos` and `datasets` skip the discovery rules (`tags`,
/// `min_downloads`) but every candidate still passes the license allowlist,
/// gated check, takedown list, size cap, and disk budget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Harvest {
    /// Model repo ids to mirror.
    pub repos: Vec<String>,
    /// Dataset repo ids to mirror (needs `include_datasets`).
    pub datasets: Vec<String>,
    /// Users or orgs whose repos are discovered.
    pub authors: Vec<String>,
    /// Discovered repos must carry every one of these tags.
    pub tags: Vec<String>,
    /// Discovered repos need at least this many downloads.
    pub min_downloads: u64,
    /// Most repos considered per author and type, by downloads.
    pub max_per_author: usize,
    /// License allowlist; empty means `[policy] license_allowlist`.
    pub license_allowlist: Vec<String>,
    /// Skip revisions larger than this (bytes, 0 = no cap).
    pub max_repo_bytes: u64,
    /// Stop when harvested data would exceed this (bytes, 0 = no budget).
    pub disk_budget_bytes: u64,
    /// Also discover datasets from `authors` and mirror `datasets`.
    pub include_datasets: bool,
    /// Mirror gated repos (needs HF_TOKEN with accepted terms). Off by default.
    pub allow_gated: bool,
    /// Repo ids never to fetch.
    pub takedown: Vec<String>,
    /// File with more repo ids never to fetch (one per line, `#` comments).
    pub takedown_file: String,
    /// Seconds between watch cycles.
    pub poll_interval_secs: u64,
    /// Pause between Hub API calls (politeness).
    pub request_delay_ms: u64,
    /// Retries for HTTP 429 and 5xx from the Hub.
    pub max_retries: u32,
    /// First backoff step when the Hub sends no Retry-After.
    pub retry_base_ms: u64,
    /// Sign and commit catalog entries for each harvested revision.
    pub publish: bool,
    /// With `storage.backend = "s3"`, upload model files to every target.
    pub upload: bool,
}

impl Default for Harvest {
    fn default() -> Self {
        Self {
            repos: Vec::new(),
            datasets: Vec::new(),
            authors: Vec::new(),
            tags: Vec::new(),
            min_downloads: 0,
            max_per_author: 100,
            license_allowlist: Vec::new(),
            max_repo_bytes: 0,
            disk_budget_bytes: 0,
            include_datasets: false,
            allow_gated: false,
            takedown: Vec::new(),
            takedown_file: String::new(),
            poll_interval_secs: 6 * 3600,
            request_delay_ms: 500,
            max_retries: 5,
            retry_base_ms: 2000,
            publish: true,
            upload: true,
        }
    }
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
    /// Default parent directory for `pull` output (`<download_dir>/<org>/<name>`).
    pub download_dir: String,
}

impl Default for General {
    fn default() -> Self {
        Self {
            state_db: "~/.local/share/tensorseed/state.sqlite".into(),
            staging_dir: "~/.local/share/tensorseed/staging".into(),
            seed_dir: "~/.local/share/tensorseed/seed".into(),
            keep_local_seed: true,
            max_concurrent_downloads: 2,
            download_dir: ".".into(),
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
    /// Publisher name shown in `ts/v1/catalog` (written by `catalog create`).
    pub publisher_name: String,
    /// Publisher contact (email or URL) for takedown requests.
    pub publisher_contact: String,
    /// URL of the publisher's mirroring and takedown policy.
    pub publisher_policy_url: String,
}

impl Default for Catalog {
    fn default() -> Self {
        Self {
            backend: "chia".into(),
            name: "tensorseed".into(),
            publisher_pubkey: String::new(),
            publisher_seckey_file: None,
            publisher_name: String::new(),
            publisher_contact: String::new(),
            publisher_policy_url: String::new(),
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

/// `[storage]`: where published objects go. Local disk by default; S3
/// compatible object storage (any provider) is optional.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Storage {
    /// `local` (default) or `s3`.
    pub backend: String,
    /// Root directory for `local`: catalog files under `<catalog name>/`,
    /// webseed copies of model files under `t/`.
    pub local_dir: String,
    /// Public URL of `local_dir` if a static web server exposes it (enables
    /// it as a webseed and catalog base). Empty: local only, no webseed.
    pub local_public_base_url: String,
    /// Targets for `s3`; every object is written to each of them.
    pub targets: Vec<StorageTarget>,
}

impl Default for Storage {
    fn default() -> Self {
        Self {
            backend: "local".into(),
            local_dir: "~/.local/share/tensorseed/publish".into(),
            local_public_base_url: String::new(),
            targets: Vec::new(),
        }
    }
}

/// `[[storage.targets]]`: one S3 compatible bucket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageTarget {
    /// Label for logs.
    pub name: String,
    /// S3 endpoint URL; empty means AWS S3 itself.
    pub endpoint: String,
    /// Signing region (`auto` for R2).
    pub region: String,
    /// Bucket.
    pub bucket: String,
    /// Key prefix inside the bucket.
    pub prefix: String,
    /// Path style addressing (`endpoint/bucket/key`).
    pub path_style: bool,
    /// Public URL that serves this bucket (and prefix), ending in `/`. Used
    /// as a webseed base (`<public_base_url>t/`) and catalog mirror. Empty:
    /// upload only.
    pub public_base_url: String,
    /// Env var holding the access key id.
    pub access_key_env: String,
    /// Env var holding the secret access key.
    pub secret_key_env: String,
}

impl Default for StorageTarget {
    fn default() -> Self {
        Self {
            name: String::new(),
            endpoint: String::new(),
            region: "us-east-1".into(),
            bucket: String::new(),
            prefix: String::new(),
            path_style: true,
            public_base_url: String::new(),
            access_key_env: "AWS_ACCESS_KEY_ID".into(),
            secret_key_env: "AWS_SECRET_ACCESS_KEY".into(),
        }
    }
}

impl Storage {
    /// Webseed bases (`.../t/`) for manifests, from every public URL.
    pub fn webseed_bases(&self) -> Vec<String> {
        let publics: Vec<&str> = match self.backend.as_str() {
            "s3" => self
                .targets
                .iter()
                .map(|t| t.public_base_url.as_str())
                .collect(),
            _ => vec![self.local_public_base_url.as_str()],
        };
        publics
            .into_iter()
            .filter(|u| !u.is_empty())
            .map(|u| format!("{}/t/", u.trim_end_matches('/')))
            .collect()
    }
}

/// `[backup]`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Backups {
    /// `[backup.sia]`
    pub sia: Sia,
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

/// Expand a leading `~/` to `$HOME`.
pub fn expand(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(rest))
            .unwrap_or_else(|| PathBuf::from(path)),
        None => PathBuf::from(path),
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
        .merge(
            Env::prefixed("TENSORSEED_")
                .ignore(&["CONFIG", "KEY_PASSWORD"])
                .split("__"),
        )
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
    match cfg.storage.backend.as_str() {
        "local" => {}
        "s3" => {
            if cfg.storage.targets.is_empty() {
                anyhow::bail!("storage.backend = \"s3\" needs at least one [[storage.targets]]");
            }
            for t in &cfg.storage.targets {
                if t.bucket.is_empty() {
                    anyhow::bail!("storage target {:?} has no bucket", t.name);
                }
            }
        }
        other => anyhow::bail!("unknown storage.backend {other:?} (use local or s3)"),
    }
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
            assert_eq!(cfg.storage.backend, "local");
            assert!(cfg.storage.webseed_bases().is_empty());
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
            assert_eq!(cfg.storage.backend, "local");
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
    fn storage_targets() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "s.toml",
                "[storage]\nbackend = \"s3\"\n[[storage.targets]]\nname = \"a\"\nendpoint = \"https://s3.example.org\"\nbucket = \"x\"\npublic_base_url = \"https://a.example.org/\"\n[[storage.targets]]\nname = \"b\"\nbucket = \"y\"\n",
            )?;
            let cfg = load(Path::new("s.toml")).map_err(|e| e.to_string())?;
            assert_eq!(cfg.storage.targets.len(), 2);
            assert!(cfg.storage.targets[1].path_style);
            assert_eq!(
                cfg.storage.webseed_bases(),
                vec!["https://a.example.org/t/"]
            );
            jail.create_file("e.toml", "[storage]\nbackend = \"s3\"\n")?;
            assert!(load(Path::new("e.toml")).is_err());
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
