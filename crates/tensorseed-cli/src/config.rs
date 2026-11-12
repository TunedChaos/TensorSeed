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
    /// Mirror lists (`[mirrors]`).
    pub mirrors: Mirrors,
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
    /// Upload model files to every `[[storage.target]]` with `upload = true`.
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

/// `[storage]`: where published objects go.
///
/// The local directory is always written (it is what the seeder serves).
/// Remote copies are optional: zero or more named, repeatable
/// `[[storage.target]]` blocks, each with a `provider`. Publishing writes
/// every object to the local directory and to every target with
/// `enabled = true` and `upload = true`. Targets with a `public_base_url`
/// become webseeds (`<public_base_url>t/`) and catalog mirrors, ordered by
/// `priority`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Storage {
    /// Root directory: catalog files under `<catalog name>/`, webseed copies
    /// of model files under `t/`.
    pub local_dir: String,
    /// Public URL of `local_dir` if a static web server exposes it (enables
    /// it as a webseed and catalog base). Empty: no webseed.
    pub local_public_base_url: String,
    /// Priority of `local_public_base_url` among webseeds (lower first).
    pub local_priority: i64,
    /// Remote targets.
    pub target: Vec<StorageTarget>,
}

impl Default for Storage {
    fn default() -> Self {
        Self {
            local_dir: "~/.local/share/tensorseed/publish".into(),
            local_public_base_url: String::new(),
            local_priority: 100,
            target: Vec::new(),
        }
    }
}

/// Providers accepted in `[[storage.target]]`.
pub const STORAGE_PROVIDERS: &[&str] = &[
    "r2", "b2", "vultr", "wasabi", "minio", "aws", "s3", "http", "torrent", "local",
];

/// `[[storage.target]]`: one named destination. Several blocks may use the
/// same provider (different buckets, regions, or accounts).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StorageTarget {
    /// Unique name (logs, `mirror list`).
    pub name: String,
    /// `r2 | b2 | vultr | wasabi | minio | aws | s3 | http | torrent | local`.
    pub provider: String,
    /// Disabled targets are ignored entirely.
    pub enabled: bool,
    /// Webseed and mirror order, lower first.
    pub priority: i64,
    /// Upload on publish (`false`: only advertise `public_base_url`, e.g. a
    /// bucket some other process fills).
    pub upload: bool,
    /// Public URL serving this target's root, used as webseed and catalog
    /// mirror. Empty: upload only.
    pub public_base_url: String,
    /// Bucket (S3 family).
    pub bucket: String,
    /// Key prefix inside the bucket.
    pub prefix: String,
    /// Region. b2: e.g. `us-west-004`; vultr: e.g. `ewr1`; wasabi: e.g.
    /// `us-east-1`; aws: required. Defaults per provider.
    pub region: String,
    /// r2: Cloudflare account id (derives the endpoint).
    pub account_id: String,
    /// r2: env var holding the account id instead of `account_id`.
    pub account_id_env: String,
    /// vultr: hostname override (e.g. `sjc1.vultrobjects.com`).
    pub hostname: String,
    /// Explicit endpoint; required for `s3` and `minio`, overrides the
    /// derived endpoint for other providers.
    pub endpoint: String,
    /// Path style addressing; default per provider (false only for aws).
    pub path_style: Option<bool>,
    /// Env var holding the access key id (never the key itself).
    pub access_key_env: String,
    /// Env var holding the secret access key.
    pub secret_key_env: String,
    /// local: directory (e.g. a second disk or NFS mount).
    pub dir: String,
}

impl Default for StorageTarget {
    fn default() -> Self {
        Self {
            name: String::new(),
            provider: String::new(),
            enabled: true,
            priority: 100,
            upload: true,
            public_base_url: String::new(),
            bucket: String::new(),
            prefix: String::new(),
            region: String::new(),
            account_id: String::new(),
            account_id_env: String::new(),
            hostname: String::new(),
            endpoint: String::new(),
            path_style: None,
            access_key_env: "AWS_ACCESS_KEY_ID".into(),
            secret_key_env: "AWS_SECRET_ACCESS_KEY".into(),
            dir: String::new(),
        }
    }
}

/// Resolved, non secret S3 settings for a target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedS3 {
    /// Endpoint URL (empty: AWS default endpoint for the region).
    pub endpoint: String,
    /// Signing region.
    pub region: String,
    /// Path style addressing.
    pub path_style: bool,
}

impl StorageTarget {
    /// True for the S3 API providers.
    pub fn is_s3(&self) -> bool {
        matches!(
            self.provider.as_str(),
            "r2" | "b2" | "vultr" | "wasabi" | "minio" | "aws" | "s3"
        )
    }

    /// True if publishing writes to this target.
    pub fn uploads(&self) -> bool {
        self.enabled && self.upload && (self.is_s3() || self.provider == "local")
    }

    /// Endpoint, region and addressing derived from provider fields.
    pub fn resolve_s3(&self) -> anyhow::Result<ResolvedS3> {
        let n = &self.name;
        let need = |v: &str, what: &str| -> anyhow::Result<String> {
            if v.is_empty() {
                anyhow::bail!("storage target {n:?} ({}) needs {what}", self.provider)
            }
            Ok(v.to_string())
        };
        let region_or = |d: &str| {
            if self.region.is_empty() {
                d.to_string()
            } else {
                self.region.clone()
            }
        };
        let (endpoint, region, path_style) = match self.provider.as_str() {
            "r2" => {
                let acct = if !self.account_id.is_empty() {
                    self.account_id.clone()
                } else if !self.account_id_env.is_empty() {
                    std::env::var(&self.account_id_env).map_err(|_| {
                        anyhow::anyhow!(
                            "storage target {n:?}: env var {} (account_id_env) is not set",
                            self.account_id_env
                        )
                    })?
                } else if self.endpoint.is_empty() {
                    anyhow::bail!("storage target {n:?} (r2) needs account_id or account_id_env")
                } else {
                    String::new()
                };
                (
                    format!("https://{acct}.r2.cloudflarestorage.com"),
                    region_or("auto"),
                    true,
                )
            }
            "b2" => {
                let r = need(&self.region, "region (e.g. us-west-004)")?;
                (format!("https://s3.{r}.backblazeb2.com"), r, true)
            }
            "vultr" => {
                let host = if !self.hostname.is_empty() {
                    self.hostname.clone()
                } else {
                    format!("{}.vultrobjects.com", region_or("ewr1"))
                };
                let host = host.trim_start_matches("https://").to_string();
                (format!("https://{host}"), region_or("ewr1"), true)
            }
            "wasabi" => {
                let r = region_or("us-east-1");
                (format!("https://s3.{r}.wasabisys.com"), r, true)
            }
            "minio" => (
                if self.endpoint.is_empty() {
                    "http://localhost:9000".into()
                } else {
                    self.endpoint.clone()
                },
                region_or("us-east-1"),
                true,
            ),
            "aws" => (String::new(), need(&self.region, "region")?, false),
            "s3" => (
                need(&self.endpoint, "endpoint")?,
                region_or("us-east-1"),
                true,
            ),
            other => anyhow::bail!("storage target {n:?}: provider {other:?} is not S3"),
        };
        Ok(ResolvedS3 {
            endpoint: if self.endpoint.is_empty() {
                endpoint
            } else {
                self.endpoint.clone()
            },
            region,
            path_style: self.path_style.unwrap_or(path_style),
        })
    }

    fn validate(&self) -> anyhow::Result<()> {
        let n = &self.name;
        if n.is_empty() {
            anyhow::bail!("every [[storage.target]] needs a name");
        }
        if !STORAGE_PROVIDERS.contains(&self.provider.as_str()) {
            anyhow::bail!(
                "storage target {n:?}: provider {:?} is not one of {STORAGE_PROVIDERS:?}",
                self.provider
            );
        }
        if !self.public_base_url.is_empty()
            && !(self.public_base_url.starts_with("https://")
                || self.public_base_url.starts_with("http://"))
        {
            anyhow::bail!("storage target {n:?}: public_base_url must be http(s)");
        }
        for (k, v) in [
            ("access_key_env", &self.access_key_env),
            ("secret_key_env", &self.secret_key_env),
        ] {
            // Catch the classic mistake of pasting the secret itself.
            if !v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                anyhow::bail!(
                    "storage target {n:?}: {k} must name an environment variable, not hold a secret"
                );
            }
        }
        match self.provider.as_str() {
            "local" => {
                if self.dir.is_empty() {
                    anyhow::bail!("storage target {n:?} (local) needs dir");
                }
            }
            "http" => {
                if self.public_base_url.is_empty() {
                    anyhow::bail!("storage target {n:?} (http) needs public_base_url");
                }
            }
            "torrent" => {
                anyhow::bail!(
                    "storage target {n:?}: torrent targets are not implemented yet (seeding is stubbed); list torrents as [[mirror]] in mirrors.toml"
                )
            }
            _ => {
                if self.bucket.is_empty() {
                    anyhow::bail!("storage target {n:?} has no bucket");
                }
                if self.enabled {
                    self.resolve_s3()?;
                }
            }
        }
        Ok(())
    }
}

impl Storage {
    /// Public bases (with trailing `/`) and priorities, lowest first:
    /// `local_public_base_url` plus every enabled target's
    /// `public_base_url`.
    pub fn public_bases(&self) -> Vec<(i64, String)> {
        let mut v: Vec<(i64, String)> = Vec::new();
        if !self.local_public_base_url.is_empty() {
            v.push((self.local_priority, self.local_public_base_url.clone()));
        }
        for t in self.target.iter().filter(|t| t.enabled) {
            if !t.public_base_url.is_empty() {
                v.push((t.priority, t.public_base_url.clone()));
            }
        }
        v.sort_by_key(|(p, _)| *p);
        v.into_iter()
            .map(|(p, u)| (p, format!("{}/", u.trim_end_matches('/'))))
            .collect()
    }

    /// Webseed bases (`.../t/`) for manifests, in priority order.
    pub fn webseed_bases(&self) -> Vec<String> {
        self.public_bases()
            .into_iter()
            .map(|(_, u)| format!("{u}t/"))
            .collect()
    }

    /// Targets publishing writes to.
    pub fn upload_targets(&self) -> impl Iterator<Item = &StorageTarget> {
        self.target.iter().filter(|t| t.uploads())
    }

    fn validate(&self) -> anyhow::Result<()> {
        let mut names = std::collections::BTreeSet::new();
        for t in &self.target {
            t.validate()?;
            if !names.insert(t.name.as_str()) {
                anyhow::bail!("duplicate storage target name {:?}", t.name);
            }
        }
        Ok(())
    }
}

/// `[mirrors]`: mirror list handling (spec 6.12).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Mirrors {
    /// Local mirror list (`[[mirror]]` blocks), edited by `tensorseed mirror`.
    pub file: String,
    /// Fetch the publisher signed `<catalog>/mirrors.toml` on sync and pull.
    pub fetch_published: bool,
}

impl Default for Mirrors {
    fn default() -> Self {
        Self {
            file: "~/.config/tensorseed/mirrors.toml".into(),
            fetch_published: true,
        }
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
    cfg.storage.validate()?;
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
            // Multi-provider example, all disabled: default stays local only.
            assert_eq!(cfg.storage.target.len(), 9);
            assert!(cfg.storage.target.iter().all(|t| !t.enabled));
            assert_eq!(cfg.storage.upload_targets().count(), 0);
            assert!(cfg.storage.webseed_bases().is_empty());
            let mut enabled = cfg.clone();
            for t in &mut enabled.storage.target {
                t.enabled = true;
            }
            assert_eq!(
                enabled.storage.target[1].resolve_s3().unwrap().endpoint,
                "https://0123456789abcdef0123456789abcdef.r2.cloudflarestorage.com"
            );
            assert_eq!(enabled.storage.upload_targets().count(), 8);
            assert_eq!(
                enabled.storage.webseed_bases()[0],
                "https://models.example.org/t/"
            );
            assert!(cfg.mirrors.fetch_published);
            assert!(!cfg.targets.is_empty());
            Ok(())
        });
    }

    #[test]
    fn mirrors_example_parses() {
        let l = tensorseed_catalog::mirrors::MirrorList::parse(include_str!(
            "../../../config/mirrors.example.toml"
        ))
        .unwrap();
        assert_eq!(l.mirrors.len(), 5);
    }

    #[test]
    fn missing_file_uses_defaults_and_env_overrides() {
        Jail::expect_with(|jail| {
            jail.set_env("TENSORSEED_CHIA__FEE_MOJOS", "1000");
            let cfg = load(Path::new("does-not-exist.toml")).map_err(|e| e.to_string())?;
            assert_eq!(cfg.chia.fee_mojos, 1000);
            assert_eq!(cfg.general.max_concurrent_downloads, 2);
            assert!(cfg.storage.target.is_empty());
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
            jail.set_env("TS_R2_ACCOUNT", "acct123");
            jail.create_file(
                "s.toml",
                r#"
[storage]
local_public_base_url = "https://seed.example.org/pub"
local_priority = 50

[[storage.target]]
name = "r2-main"
provider = "r2"
account_id_env = "TS_R2_ACCOUNT"
bucket = "x"
priority = 10
public_base_url = "https://r2.example.org/"
access_key_env = "TS_R2_KEY"
secret_key_env = "TS_R2_SECRET"

[[storage.target]]
name = "b2-us"
provider = "b2"
region = "us-west-004"
bucket = "y"

[[storage.target]]
name = "b2-eu"
provider = "b2"
region = "eu-central-003"
bucket = "z"
upload = false
public_base_url = "https://eu.example.org"

[[storage.target]]
name = "vultr"
provider = "vultr"
region = "sjc1"
bucket = "v"

[[storage.target]]
name = "wasabi"
provider = "wasabi"
bucket = "w"
enabled = false
public_base_url = "https://off.example.org/"

[[storage.target]]
name = "aws"
provider = "aws"
region = "eu-west-1"
bucket = "a"

[[storage.target]]
name = "garage"
provider = "s3"
endpoint = "https://garage.example.org"
path_style = false
bucket = "g"

[[storage.target]]
name = "nas"
provider = "local"
dir = "/mnt/nas/tensorseed"
"#,
            )?;
            let cfg = load(Path::new("s.toml")).map_err(|e| e.to_string())?;
            let t = &cfg.storage.target;
            assert_eq!(t.len(), 8);
            let r = |i: usize| t[i].resolve_s3().unwrap();
            assert_eq!(r(0).endpoint, "https://acct123.r2.cloudflarestorage.com");
            assert_eq!(r(0).region, "auto");
            assert_eq!(r(1).endpoint, "https://s3.us-west-004.backblazeb2.com");
            assert_eq!(r(2).endpoint, "https://s3.eu-central-003.backblazeb2.com");
            assert_eq!(r(3).endpoint, "https://sjc1.vultrobjects.com");
            assert_eq!(r(4).endpoint, "https://s3.us-east-1.wasabisys.com");
            assert_eq!(r(5).endpoint, "");
            assert!(!r(5).path_style);
            assert!(!r(6).path_style);
            assert!(r(1).path_style);
            let up: Vec<&str> = cfg
                .storage
                .upload_targets()
                .map(|t| t.name.as_str())
                .collect();
            assert_eq!(up, ["r2-main", "b2-us", "vultr", "aws", "garage", "nas"]);
            assert_eq!(
                cfg.storage.webseed_bases(),
                vec![
                    "https://r2.example.org/t/",
                    "https://seed.example.org/pub/t/",
                    "https://eu.example.org/t/",
                ]
            );
            for (name, body) in [
                (
                    "noname",
                    "[[storage.target]]\nprovider = \"b2\"\nregion=\"r\"\nbucket = \"y\"\n",
                ),
                (
                    "badprov",
                    "[[storage.target]]\nname=\"a\"\nprovider = \"ftp\"\n",
                ),
                (
                    "b2noregion",
                    "[[storage.target]]\nname=\"a\"\nprovider = \"b2\"\nbucket = \"y\"\n",
                ),
                (
                    "r2noacct",
                    "[[storage.target]]\nname=\"a\"\nprovider = \"r2\"\nbucket = \"y\"\n",
                ),
                (
                    "s3noendpoint",
                    "[[storage.target]]\nname=\"a\"\nprovider = \"s3\"\nbucket = \"y\"\n",
                ),
                (
                    "nobucket",
                    "[[storage.target]]\nname=\"a\"\nprovider = \"wasabi\"\n",
                ),
                (
                    "inline",
                    "[[storage.target]]\nname=\"a\"\nprovider = \"minio\"\nbucket=\"b\"\naccess_key_env = \"AKIA/abc+def\"\n",
                ),
                (
                    "dup",
                    "[[storage.target]]\nname=\"a\"\nprovider = \"minio\"\nbucket=\"b\"\n[[storage.target]]\nname=\"a\"\nprovider = \"minio\"\nbucket=\"c\"\n",
                ),
                (
                    "torrent",
                    "[[storage.target]]\nname=\"a\"\nprovider = \"torrent\"\n",
                ),
                ("oldbackend", "[storage]\nbackend = \"s3\"\n"),
            ] {
                let f = format!("{name}.toml");
                jail.create_file(&f, body)?;
                assert!(load(Path::new(&f)).is_err(), "{name} should be rejected");
            }
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
