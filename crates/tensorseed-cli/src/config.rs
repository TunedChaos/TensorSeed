// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Configuration (spec section 10), loaded with figment from TOML plus
//! `TENSORSEED_*` environment overrides (nested keys split on `__`, for
//! example `TENSORSEED_CHIA__FEE_MOJOS=1000`).
//!
//! The default config file is `~/.config/tensorseed/tensorseed.toml` on
//! Unix (`$XDG_CONFIG_HOME` if set) and `%APPDATA%\tensorseed\tensorseed.toml`
//! on Windows. Default data paths live under `~/.local/share/tensorseed`
//! on Unix and `%LOCALAPPDATA%\tensorseed` on Windows. See [`default_path`],
//! [`expand`], [`config_dir`], and [`data_local_dir`].
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
    /// HTTP fetch behaviour (`[fetch]`).
    pub fetch: Fetch,
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
    /// Long running seeder (`[serve]`, spec 8.3).
    pub serve: crate::serve::ServeConfig,
    /// Automatic harvest watchlist (`[harvest]`).
    pub harvest: Harvest,
    /// Extra catalogs to follow (`[[catalogs]]`). Empty keeps the top-level
    /// `[catalog]` / `[file]` / `[chia]` block as the sole subscription.
    pub catalogs: Vec<CatalogSub>,
}

/// `[harvest]`: what `tensorseed harvest` mirrors from the Hub.
///
/// Explicit `repos` and `datasets` skip the discovery rules (`tags`,
/// `min_downloads`) but every candidate still passes the license allowlist,
/// gated check, takedown list, size cap, and disk budget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Harvest {
    /// Model repos to mirror: plain ids or `{ id, allow_license }` tables.
    pub repos: Vec<WatchRepo>,
    /// Dataset repos to mirror (needs `include_datasets`), same forms.
    pub datasets: Vec<WatchRepo>,
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

/// One explicit watchlist entry: either `"org/name"` or
/// `{ id = "org/name", allow_license = ["other"] }`.
///
/// `allow_license` lets a publisher mirror a repo whose license is not on
/// the allowlist (typically their own `license: other` repo) without
/// loosening the policy for anything else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WatchRepo {
    /// Just the repo id.
    Id(String),
    /// Repo id with per-repo overrides.
    Entry {
        /// Repo id (org/name).
        id: String,
        /// License ids accepted for this repo only.
        #[serde(default)]
        allow_license: Vec<String>,
    },
}

impl WatchRepo {
    /// Repo id.
    pub fn id(&self) -> &str {
        match self {
            Self::Id(id) | Self::Entry { id, .. } => id,
        }
    }

    /// Per-repo license overrides.
    pub fn allow_license(&self) -> &[String] {
        match self {
            Self::Id(_) => &[],
            Self::Entry { allow_license, .. } => allow_license,
        }
    }
}

impl From<&str> for WatchRepo {
    fn from(s: &str) -> Self {
        Self::Id(s.to_string())
    }
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
    /// sqlite state database (default under `~/.local/share/tensorseed` on
    /// Unix, `%LOCALAPPDATA%\tensorseed` on Windows).
    pub state_db: String,
    /// Download staging directory (same default root as `state_db`).
    pub staging_dir: String,
    /// Long term seeding directory (same default root as `state_db`).
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
            state_db: default_data_file("state.sqlite"),
            staging_dir: default_data_file("staging"),
            seed_dir: default_data_file("seed"),
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
    /// Check the file catalog against the Chia store (spec 6.14):
    /// `strict`, `warn`, or `off`. Unset means `strict` when
    /// `chia.catalog_store_id` is set and `off` otherwise.
    pub chain_check: Option<String>,
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
            chain_check: None,
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
    /// Local verified state and trust anchor (default under
    /// `~/.local/share/tensorseed` on Unix, `%LOCALAPPDATA%\tensorseed` on
    /// Windows).
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
            state_dir: default_data_file("catalog"),
            snapshot_every: 64,
            head_ttl_secs: 7 * 24 * 3600,
            torrent: true,
        }
    }
}

/// One `[[catalogs]]` entry (spec 18.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct CatalogSub {
    /// Catalog name bound into signatures.
    pub name: String,
    /// Local display name (not signed).
    pub label: String,
    /// Publisher public key (minisign `RW...`).
    pub publisher_pubkey: String,
    /// This catalog's Chia DataLayer store id (64 hex), if any.
    pub store_id: String,
    /// File catalog HTTPS base.
    pub base_url: String,
    /// `strict`, `warn`, or `off`. Unset inherits `[catalog] chain_check`.
    pub chain_check: Option<String>,
    /// Lower is preferred when hashes agree. Unset is 0.
    pub priority: Option<i64>,
    /// Override the file-backend state directory for this catalog.
    pub state_dir: Option<String>,
    /// Extra file-catalog mirrors for this catalog.
    pub mirror_urls: Vec<String>,
    /// Coin API endpoints for this catalog's chain check.
    pub coin_api_urls: Vec<String>,
}

/// A resolved catalog subscription used at runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    /// Catalog name bound into signatures.
    pub name: String,
    /// Local display name (falls back to `name`).
    pub label: String,
    /// Publisher public key.
    pub publisher_pubkey: String,
    /// Chia store id, if any.
    pub store_id: String,
    /// File catalog base URL.
    pub base_url: String,
    /// Extra file-catalog mirrors.
    pub mirror_urls: Vec<String>,
    /// Effective chain check mode.
    pub chain_check: ChainCheck,
    /// Tie-break when hashes agree (lower first).
    pub priority: i64,
    /// File-backend state directory (FileCatalog still appends `name`).
    pub state_dir: String,
    /// Coin API endpoints.
    pub coin_api_urls: Vec<String>,
    /// Whether this is the default subscription for commands with no `--catalog`.
    pub is_default: bool,
}

impl Subscription {
    /// Label if set, otherwise the catalog name.
    #[must_use]
    pub fn display(&self) -> &str {
        if self.label.is_empty() {
            &self.name
        } else {
            &self.label
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
    /// Where the chain check gets the proof of the file anchor: `light`
    /// (the published `chain-proof.json`, no Chia software needed) or
    /// `datalayer` (your own DataLayer node at `rpc_url`).
    pub chain_source: String,
    /// Full node RPC compatible HTTP endpoints used read only to look up the
    /// store's singleton coin (for example a public coinset style API or
    /// your own node). Empty with `chain_source = "datalayer"` trusts the
    /// local node's confirmed root instead.
    pub coin_api_urls: Vec<String>,
    /// How many blocks after the store moved on a proof for the previous
    /// root is still accepted (caches update a little after the chain).
    pub chain_grace_blocks: u64,
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
            chain_source: "light".into(),
            coin_api_urls: vec![DEFAULT_COIN_API.into()],
            chain_grace_blocks: 192,
        }
    }
}

/// Default read only chain endpoint (a public full node RPC compatible API
/// for Chia mainnet).
pub const DEFAULT_COIN_API: &str = "https://api.coinset.org";

/// Effective chain check mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainCheck {
    /// Refuse a file head the chain does not anchor, and fail when the chain
    /// cannot be reached.
    Strict,
    /// Log mismatches and unreachable chain endpoints, keep going.
    Warn,
    /// No chain check.
    Off,
}

impl Config {
    /// The effective `catalog.chain_check` (default: strict when a store id
    /// is configured, off otherwise).
    pub fn chain_check(&self) -> ChainCheck {
        parse_chain_check(
            self.catalog.chain_check.as_deref(),
            !self.chia.catalog_store_id.trim().is_empty(),
        )
    }

    /// Every catalog this process follows (spec 18.1).
    ///
    /// If `[[catalogs]]` is missing or empty, the top-level `[catalog]`,
    /// `[file]`, and `[chia]` block is the sole subscription. If the array
    /// is present, those top-level fields are also included when they name
    /// a publisher key, unless the same name and key already appear in the
    /// array.
    ///
    /// # Errors
    ///
    /// Returns an error if two entries share a name but not a publisher key,
    /// if a name or key is invalid, or if a store id is malformed.
    pub fn subscriptions(&self) -> anyhow::Result<Vec<Subscription>> {
        resolve_subscriptions(self)
    }

    /// The default subscription, or the one named by `name`.
    ///
    /// # Errors
    ///
    /// Returns an error if subscriptions cannot be resolved, none are
    /// configured, or `name` does not match a subscription.
    pub fn subscription(&self, name: Option<&str>) -> anyhow::Result<Subscription> {
        let subs = self.subscriptions()?;
        if subs.is_empty() {
            anyhow::bail!(
                "no catalog is configured; set catalog.publisher_pubkey or add a [[catalogs]] entry"
            );
        }
        match name {
            None => subs
                .into_iter()
                .find(|s| s.is_default)
                .ok_or_else(|| anyhow::anyhow!("internal error: no default catalog")),
            Some(n) => subs
                .into_iter()
                .find(|s| s.name == n || s.label == n)
                .ok_or_else(|| anyhow::anyhow!("no subscribed catalog named {n:?}")),
        }
    }

    /// A config view whose top-level `[catalog]` / `[file]` / `[chia]`
    /// fields describe `sub`, so existing single-catalog helpers can run
    /// against one subscription.
    #[must_use]
    pub fn for_subscription(&self, sub: &Subscription) -> Config {
        let mut c = self.clone();
        c.catalog.name = sub.name.clone();
        c.catalog.publisher_pubkey = sub.publisher_pubkey.clone();
        c.catalog.chain_check = Some(chain_check_name(sub.chain_check).into());
        c.file.base_url = sub.base_url.clone();
        if !sub.mirror_urls.is_empty() {
            c.file.mirror_urls = sub.mirror_urls.clone();
        }
        c.file.state_dir = sub.state_dir.clone();
        c.chia.catalog_store_id = sub.store_id.clone();
        if !sub.coin_api_urls.is_empty() {
            c.chia.coin_api_urls = sub.coin_api_urls.clone();
        }
        c
    }
}

fn parse_chain_check(raw: Option<&str>, has_store: bool) -> ChainCheck {
    match raw {
        Some("strict") => ChainCheck::Strict,
        Some("warn") => ChainCheck::Warn,
        Some("off") => ChainCheck::Off,
        _ if has_store => ChainCheck::Strict,
        _ => ChainCheck::Off,
    }
}

fn chain_check_name(m: ChainCheck) -> &'static str {
    match m {
        ChainCheck::Strict => "strict",
        ChainCheck::Warn => "warn",
        ChainCheck::Off => "off",
    }
}

fn resolve_subscriptions(cfg: &Config) -> anyhow::Result<Vec<Subscription>> {
    let mut raw: Vec<CatalogSub> = cfg.catalogs.clone();
    let top_has_key = !cfg.catalog.publisher_pubkey.trim().is_empty();
    if raw.is_empty() {
        return Ok(if top_has_key || !cfg.catalog.name.is_empty() {
            vec![subscription_from_top(cfg, true)?]
        } else {
            Vec::new()
        });
    }
    if top_has_key {
        let already = raw.iter().any(|e| {
            e.name == cfg.catalog.name && e.publisher_pubkey == cfg.catalog.publisher_pubkey
        });
        if !already {
            raw.insert(
                0,
                CatalogSub {
                    name: cfg.catalog.name.clone(),
                    label: String::new(),
                    publisher_pubkey: cfg.catalog.publisher_pubkey.clone(),
                    store_id: cfg.chia.catalog_store_id.clone(),
                    base_url: cfg.file.base_url.clone(),
                    chain_check: cfg.catalog.chain_check.clone(),
                    priority: None,
                    state_dir: None,
                    mirror_urls: cfg.file.mirror_urls.clone(),
                    coin_api_urls: cfg.chia.coin_api_urls.clone(),
                },
            );
        }
    }
    // Same name + same key collapse; same name + different key is an error.
    let mut by_name: Vec<CatalogSub> = Vec::new();
    for e in raw {
        if e.name.is_empty() {
            anyhow::bail!("[[catalogs]] entry is missing name");
        }
        tensorseed_catalog::signed::check_catalog_name(&e.name)?;
        if e.publisher_pubkey.trim().is_empty() {
            anyhow::bail!(
                "[[catalogs]] entry {:?} is missing publisher_pubkey",
                e.name
            );
        }
        tensorseed_catalog::minisign::PublicKey::parse(&e.publisher_pubkey)
            .map_err(|err| anyhow::anyhow!("[[catalogs]] entry {:?}: {err}", e.name))?;
        validate_store_id(&e.store_id)?;
        if let Some(m) = &e.chain_check
            && !["strict", "warn", "off"].contains(&m.as_str())
        {
            anyhow::bail!(
                "[[catalogs]] entry {:?}: chain_check must be \"strict\", \"warn\", or \"off\", not {m:?}",
                e.name
            );
        }
        if let Some(existing) = by_name.iter_mut().find(|x| x.name == e.name) {
            if existing.publisher_pubkey != e.publisher_pubkey {
                anyhow::bail!(
                    "catalog {:?} is listed more than once with different publisher keys",
                    e.name
                );
            }
            // Same subscription: keep the first, fill in blank fields from the later one.
            if existing.base_url.is_empty() {
                existing.base_url = e.base_url;
            }
            if existing.store_id.is_empty() {
                existing.store_id = e.store_id;
            }
            if existing.label.is_empty() {
                existing.label = e.label;
            }
            continue;
        }
        by_name.push(e);
    }
    let mut subs = Vec::with_capacity(by_name.len());
    for e in by_name {
        let store = e.store_id.trim().to_string();
        let base = if e.base_url.is_empty() {
            cfg.file.base_url.clone()
        } else {
            e.base_url
        };
        if !(base.starts_with("https://") || base.starts_with("http://")) {
            anyhow::bail!(
                "[[catalogs]] entry {:?}: base_url {base:?} is not an http(s) URL",
                e.name
            );
        }
        let state_dir = e
            .state_dir
            .clone()
            .unwrap_or_else(|| cfg.file.state_dir.clone());
        let inherit = parse_chain_check(cfg.catalog.chain_check.as_deref(), !store.is_empty());
        let chain_check = match e.chain_check.as_deref() {
            Some("strict") => ChainCheck::Strict,
            Some("warn") => ChainCheck::Warn,
            Some("off") => ChainCheck::Off,
            _ => inherit,
        };
        subs.push(Subscription {
            name: e.name,
            label: e.label,
            publisher_pubkey: e.publisher_pubkey,
            store_id: store,
            base_url: base,
            mirror_urls: e.mirror_urls,
            chain_check,
            priority: e.priority.unwrap_or(0),
            state_dir,
            coin_api_urls: e.coin_api_urls,
            is_default: false,
        });
    }
    mark_default(&mut subs);
    Ok(subs)
}

fn subscription_from_top(cfg: &Config, is_default: bool) -> anyhow::Result<Subscription> {
    tensorseed_catalog::signed::check_catalog_name(&cfg.catalog.name)?;
    if !cfg.catalog.publisher_pubkey.is_empty() {
        tensorseed_catalog::minisign::PublicKey::parse(&cfg.catalog.publisher_pubkey)?;
    }
    validate_store_id(&cfg.chia.catalog_store_id)?;
    Ok(Subscription {
        name: cfg.catalog.name.clone(),
        label: String::new(),
        publisher_pubkey: cfg.catalog.publisher_pubkey.clone(),
        store_id: cfg.chia.catalog_store_id.clone(),
        base_url: cfg.file.base_url.clone(),
        mirror_urls: cfg.file.mirror_urls.clone(),
        chain_check: cfg.chain_check(),
        priority: 0,
        state_dir: cfg.file.state_dir.clone(),
        coin_api_urls: cfg.chia.coin_api_urls.clone(),
        is_default,
    })
}

fn mark_default(subs: &mut [Subscription]) {
    if subs.is_empty() {
        return;
    }
    // Official name if present, else lowest priority, else the first entry.
    let idx = subs
        .iter()
        .position(|s| s.name == "tensorseed")
        .unwrap_or_else(|| {
            subs.iter()
                .enumerate()
                .min_by_key(|(i, s)| (s.priority, *i))
                .map(|(i, _)| i)
                .unwrap_or(0)
        });
    subs[idx].is_default = true;
}

fn validate_store_id(sid: &str) -> anyhow::Result<()> {
    let sid = sid.trim();
    if sid.is_empty() {
        return Ok(());
    }
    let h = sid.strip_prefix("0x").unwrap_or(sid);
    if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        anyhow::bail!("store id must be 64 hex characters, not {sid:?}");
    }
    Ok(())
}

/// `[huggingface]`
///
/// The token is never written in this file. It comes from the environment
/// variable named by `token_env`, or else from `token_file`, a file only
/// your account can read (mode 0600 or 0400).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HuggingFace {
    /// Hub endpoint.
    pub endpoint: String,
    /// Env var holding the optional HF token (checked first).
    pub token_env: String,
    /// File holding the token, used when `token_env` is unset or empty.
    /// Must not be readable by group or others.
    pub token_file: String,
    /// Never set. Present only so an inline `token = "hf_..."` gets a clear
    /// error instead of being silently accepted or vaguely rejected.
    #[serde(skip_serializing)]
    pub token: Option<serde_json::Value>,
}

impl Default for HuggingFace {
    fn default() -> Self {
        Self {
            endpoint: "https://huggingface.co".into(),
            token_env: "HF_TOKEN".into(),
            token_file: String::new(),
            token: None,
        }
    }
}

impl HuggingFace {
    fn validate(&self) -> anyhow::Result<()> {
        if self.token.is_some() {
            anyhow::bail!(
                "[huggingface] token is not allowed: never put the Hugging Face token in the \
                 config file. Set token_env (default HF_TOKEN) to the name of an environment \
                 variable that holds it, or token_file to a file only you can read (chmod 600)"
            );
        }
        if self.token_env.is_empty()
            || !self
                .token_env
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            anyhow::bail!(
                "huggingface.token_env must be an environment variable name like HF_TOKEN, \
                 not the token itself"
            );
        }
        Ok(())
    }

    /// Resolve the token: `token_env` first, then `token_file`, then the
    /// file written by `tensorseed hf login`, then the Hugging Face CLI
    /// stored token. A token file readable by group or others is refused.
    pub fn resolve_token(&self) -> anyhow::Result<Option<String>> {
        Ok(self.resolve_token_source()?.map(|(t, _)| t))
    }

    /// Like [`Self::resolve_token`], with a label for the source that never
    /// includes the token itself.
    pub fn resolve_token_source(&self) -> anyhow::Result<Option<(String, TokenSource)>> {
        if let Ok(t) = std::env::var(&self.token_env)
            && !t.trim().is_empty()
        {
            return Ok(Some((
                t.trim().to_string(),
                TokenSource::Env(self.token_env.clone()),
            )));
        }
        if !self.token_file.is_empty() {
            let p = expand(&self.token_file);
            let t = read_private_token(&p, "huggingface.token_file")?.ok_or_else(|| {
                anyhow::anyhow!("huggingface.token_file {} is empty", p.display())
            })?;
            return Ok(Some((t, TokenSource::TokenFile(p))));
        }
        let login = login_token_path();
        if login.is_file() {
            if let Some(t) = read_private_token(&login, "tensorseed login file")? {
                return Ok(Some((t, TokenSource::LoginFile(login))));
            }
        }
        if let Some(p) = hf_cli_token_path()
            && p.is_file()
        {
            if let Some(t) = read_private_token(&p, "Hugging Face CLI token file")? {
                return Ok(Some((t, TokenSource::HfCli(p))));
            }
        }
        Ok(None)
    }

    /// Hub client with the resolved token.
    pub fn client(&self) -> anyhow::Result<tensorseed_hf::HfClient> {
        Ok(tensorseed_hf::HfClient::new(
            &self.endpoint,
            self.resolve_token()?,
        )?)
    }
}

/// `[torrent]`: torrent creation and the built-in seeder (spec section 7.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Torrent {
    /// Build a torrent for every harvested or published revision.
    pub create: bool,
    /// Build hybrid v1+v2 torrents. The built-in engine (librqbit) only
    /// seeds and downloads hybrids; v2 only torrents are for other clients.
    pub hybrid: bool,
    /// Piece length in bytes (power of two, at least 16 KiB), or 0 for auto:
    /// 4 MiB, doubled until a revision has at most 16384 pieces, capped at
    /// 64 MiB.
    pub piece_length: u64,
    /// Tracker announce URLs written into new torrents and announced to.
    pub trackers: Vec<String>,
    /// Listen address for `seed` and `--seed-after`.
    pub listen_addr: String,
    /// Listen port.
    pub listen_port: u16,
    /// Upload cap in KiB/s, 0 means unlimited.
    pub upload_limit_kbps: u64,
    /// Enable DHT.
    pub dht: bool,
    /// Local service discovery (BEP 14) on the LAN.
    pub lsd: bool,
    /// Peers (`host:port`) to dial for every torrent, for example your own
    /// seed boxes.
    pub peers: Vec<String>,
    /// Use the BitTorrent swarm on `pull` when the manifest has a torrent.
    /// HTTP mirrors and the Hub fill gaps (or take over if no peers appear).
    /// `--no-swarm` turns this off; `--swarm` turns it on even when this is
    /// false. `--from-hf` never uses the swarm.
    pub pull_swarm: bool,
    /// Give up on the swarm if no peer connects within this many seconds
    /// after the torrent is live, then fill remaining files over HTTP.
    /// 0 disables the rule. Ignored for `--swarm-only`.
    pub swarm_no_peer_secs: u64,
    /// Give up on the swarm if verified bytes do not increase for this many
    /// seconds, then fill remaining files over HTTP. 0 disables the rule.
    /// Ignored for `--swarm-only`.
    pub swarm_stall_secs: u64,
    /// Longest the swarm pass waits overall before HTTP takes over.
    pub swarm_timeout_secs: u64,
}

impl Default for Torrent {
    fn default() -> Self {
        Self {
            create: true,
            hybrid: true,
            piece_length: 0,
            trackers: Vec::new(),
            listen_addr: "0.0.0.0".into(),
            listen_port: 6881,
            upload_limit_kbps: 0,
            dht: true,
            lsd: false,
            peers: Vec::new(),
            pull_swarm: true,
            swarm_no_peer_secs: 30,
            swarm_stall_secs: 60,
            swarm_timeout_secs: 600,
        }
    }
}

impl Torrent {
    /// Effective piece length for a revision of `total` bytes.
    pub fn piece_length_for(&self, total: u64) -> u64 {
        if self.piece_length != 0 {
            return self.piece_length;
        }
        let mut pl: u64 = 4 << 20;
        while pl < (64 << 20) && total.div_ceil(pl) > 16384 {
            pl *= 2;
        }
        pl
    }

    /// Parsed `peers`.
    pub fn peer_addrs(&self) -> anyhow::Result<Vec<std::net::SocketAddr>> {
        use std::net::ToSocketAddrs;
        let mut out = Vec::new();
        for p in &self.peers {
            out.extend(
                p.to_socket_addrs()
                    .map_err(|e| anyhow::anyhow!("torrent.peers: {p:?}: {e}"))?,
            );
        }
        Ok(out)
    }

    fn validate(&self) -> anyhow::Result<()> {
        let pl = self.piece_length;
        if pl != 0 && (pl < 16 * 1024 || !pl.is_power_of_two() || pl > 1 << 30) {
            anyhow::bail!(
                "torrent.piece_length must be 0 (auto) or a power of two from 16 KiB to 1 GiB"
            );
        }
        if self.listen_addr.parse::<std::net::IpAddr>().is_err() {
            anyhow::bail!("torrent.listen_addr must be an IP address");
        }
        for t in &self.trackers {
            if !["http://", "https://", "udp://"]
                .iter()
                .any(|s| t.starts_with(s))
            {
                anyhow::bail!("torrent.trackers: unsupported tracker URL {t:?}");
            }
        }
        Ok(())
    }
}

/// One entry in [`Fetch::source_order`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchSource {
    /// BitTorrent swarm.
    Swarm,
    /// Hugging Face Hub.
    Hub,
    /// HTTP webseeds and configured mirrors.
    Mirrors,
}

impl FetchSource {
    /// Parse a config token (`swarm`, `hub`, or `mirrors`).
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        match s {
            "swarm" => Ok(Self::Swarm),
            "hub" => Ok(Self::Hub),
            "mirrors" => Ok(Self::Mirrors),
            other => anyhow::bail!(
                "fetch.source_order entry {other:?} is not one of swarm, hub, or mirrors"
            ),
        }
    }

    /// Config token.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Swarm => "swarm",
            Self::Hub => "hub",
            Self::Mirrors => "mirrors",
        }
    }
}

/// Default `[fetch] source_order`: swarm, then the Hub, then HTTP mirrors.
pub fn default_source_order() -> Vec<String> {
    vec![
        FetchSource::Swarm.as_str().into(),
        FetchSource::Hub.as_str().into(),
        FetchSource::Mirrors.as_str().into(),
    ]
}

/// `[fetch]`: HTTP download behaviour (spec section 8.2).
///
/// Dead-mirror marks are stored in `general.state_db`. A connect failure
/// or repeated transient failures (attempts exhausted on one file, or
/// 5xx/network errors on two files) marks the origin dead. Later files
/// and later runs skip it until this cooldown expires, a download from
/// that origin succeeds, or `pull --retry-dead-mirrors` is used. A 404,
/// 401, or 403 is not a dead mirror: it only drops that origin for the
/// one file (gated or missing Hub files fall through to mirrors).
/// Hugging Face is tracked the same way. Torrent downloads are not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Fetch {
    /// Seconds a dead-mirror mark is honored (0 disables persistence;
    /// in-run marks still apply). Default 3600.
    pub dead_mirror_cooldown_secs: u64,
    /// Byte sources, in order. Each entry is `swarm`, `hub`, or
    /// `mirrors`. No duplicates. Default: swarm, hub, mirrors.
    pub source_order: Vec<String>,
}

impl Default for Fetch {
    fn default() -> Self {
        Self {
            dead_mirror_cooldown_secs: 3600,
            source_order: default_source_order(),
        }
    }
}

impl Fetch {
    fn validate(&self) -> anyhow::Result<()> {
        if self.source_order.is_empty() {
            anyhow::bail!("fetch.source_order must not be empty");
        }
        let mut seen = std::collections::BTreeSet::new();
        for s in &self.source_order {
            FetchSource::parse(s)?;
            if !seen.insert(s.as_str()) {
                anyhow::bail!("fetch.source_order has a duplicate {s:?}");
            }
        }
        Ok(())
    }

    /// Parsed [`source_order`](Self::source_order), in order.
    pub fn sources(&self) -> anyhow::Result<Vec<FetchSource>> {
        self.source_order
            .iter()
            .map(|s| FetchSource::parse(s))
            .collect()
    }

    /// Whether `source` appears in [`source_order`](Self::source_order).
    pub fn wants(&self, source: FetchSource) -> bool {
        self.source_order.iter().any(|s| s == source.as_str())
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
    /// of model files under `t/` (default under `~/.local/share/tensorseed`
    /// on Unix, `%LOCALAPPDATA%\tensorseed` on Windows).
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
            local_dir: default_data_file("publish"),
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
                // A seeding root: `tensorseed seed` serves every revision
                // under `<dir>/t/` (default: storage.local_dir).
                if !self.public_base_url.is_empty() || !self.bucket.is_empty() {
                    anyhow::bail!(
                        "storage target {n:?} (torrent) takes only dir, enabled, and priority"
                    );
                }
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

    /// Seeding roots: `local_dir` plus every enabled `torrent` target's
    /// `dir`, each holding `t/<content name>.torrent` and `t/<content name>/`.
    pub fn seed_roots(&self) -> Vec<String> {
        let mut v = vec![self.local_dir.clone()];
        for t in self
            .target
            .iter()
            .filter(|t| t.enabled && t.provider == "torrent")
        {
            if !t.dir.is_empty() && !v.contains(&t.dir) {
                v.push(t.dir.clone());
            }
        }
        v
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
    /// Local mirror list (`[[mirror]]` blocks), edited by `tensorseed mirror`
    /// (default: `~/.config/tensorseed/mirrors.toml` on Unix,
    /// `%APPDATA%\tensorseed\mirrors.toml` on Windows).
    pub file: String,
    /// Fetch the publisher signed `<catalog>/mirrors.toml` on sync and pull.
    pub fetch_published: bool,
}

impl Default for Mirrors {
    fn default() -> Self {
        Self {
            file: default_config_file("mirrors.toml"),
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

/// User home directory: `$HOME`, or `%USERPROFILE%` if `$HOME` is unset.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Per-user config directory.
///
/// On Unix this is `$XDG_CONFIG_HOME`, or `$HOME/.config` when that is
/// unset. On Windows it is `%APPDATA%` (the roaming profile), falling back
/// to `%USERPROFILE%\AppData\Roaming`.
pub fn config_dir() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|h| h.join("AppData").join("Roaming")))
            .unwrap_or_else(|| PathBuf::from("."))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|h| h.join(".config")))
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

/// Where the token came from. Display labels never include the token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenSource {
    /// Environment variable named by `huggingface.token_env`.
    Env(String),
    /// `huggingface.token_file`.
    TokenFile(PathBuf),
    /// File written by `tensorseed hf login`.
    LoginFile(PathBuf),
    /// Token stored by the Hugging Face CLI (`hf auth login`).
    HfCli(PathBuf),
}

impl TokenSource {
    /// Human readable origin, never the token value.
    pub fn label(&self) -> String {
        match self {
            Self::Env(name) => format!("environment variable {name}"),
            Self::TokenFile(p) => format!("huggingface.token_file {}", p.display()),
            Self::LoginFile(p) => format!("tensorseed login file {}", p.display()),
            Self::HfCli(p) => format!("Hugging Face CLI token file {}", p.display()),
        }
    }
}

/// Path of the token file written by `tensorseed hf login`.
///
/// `~/.config/tensorseed/hf_token` on Unix (`$XDG_CONFIG_HOME` if set),
/// `%APPDATA%\tensorseed\hf_token` on Windows.
pub fn login_token_path() -> PathBuf {
    config_dir().join("tensorseed").join("hf_token")
}

/// Path of the token stored by the Hugging Face CLI, if one can be resolved.
///
/// Order: `HF_TOKEN_PATH`, else `$HF_HOME/token`, else
/// `~/.cache/huggingface/token` (or `%USERPROFILE%\.cache\huggingface\token`
/// on Windows).
pub fn hf_cli_token_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("HF_TOKEN_PATH")
        && !p.is_empty()
    {
        return Some(PathBuf::from(p));
    }
    if let Some(p) = std::env::var_os("HF_HOME")
        && !p.is_empty()
    {
        return Some(PathBuf::from(p).join("token"));
    }
    home_dir().map(|h| h.join(".cache").join("huggingface").join("token"))
}

/// Write `token` to [`login_token_path`] with mode 0600 on Unix and a
/// user-only ACL on Windows.
pub fn write_login_token(token: &str) -> anyhow::Result<PathBuf> {
    let token = token.trim();
    if token.is_empty() {
        anyhow::bail!("the Hugging Face token is empty");
    }
    let path = login_token_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| anyhow::anyhow!("creating {}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension("tmp");
    write_private_file(&tmp, format!("{token}\n").as_bytes())?;
    std::fs::rename(&tmp, &path).map_err(|e| anyhow::anyhow!("writing {}: {e}", path.display()))?;
    set_user_only_permissions(&path)?;
    Ok(path)
}

/// Remove the file written by [`write_login_token`], if it exists.
pub fn remove_login_token() -> anyhow::Result<bool> {
    let path = login_token_path();
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(anyhow::anyhow!("removing {}: {e}", path.display())),
    }
}

fn write_private_file(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| anyhow::anyhow!("writing {}: {e}", path.display()))?;
        f.write_all(bytes)
            .map_err(|e| anyhow::anyhow!("writing {}: {e}", path.display()))?;
        f.sync_all()
            .map_err(|e| anyhow::anyhow!("writing {}: {e}", path.display()))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
            .map_err(|e| anyhow::anyhow!("writing {}: {e}", path.display()))?;
        set_user_only_permissions(path)?;
        Ok(())
    }
}

/// Set mode 0600 on Unix, or a user-only ACL on Windows, on `path`.
pub fn set_user_only_permissions(path: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| anyhow::anyhow!("chmod 600 {}: {e}", path.display()))?;
        Ok(())
    }
    #[cfg(windows)]
    {
        let user = std::env::var("USERNAME").unwrap_or_else(|_| String::from("%USERNAME%"));
        let grant = format!("{user}:(R,W)");
        let out = std::process::Command::new("icacls")
            .arg(path)
            .arg("/inheritance:r")
            .arg("/grant:r")
            .arg(&grant)
            .output()
            .map_err(|e| anyhow::anyhow!("running icacls on {}: {e}", path.display()))?;
        if !out.status.success() {
            anyhow::bail!(
                "icacls failed to set a user-only ACL on {}: {}",
                path.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Ok(())
    }
}

fn read_private_token(path: &Path, label: &str) -> anyhow::Result<Option<String>> {
    let meta =
        std::fs::metadata(path).map_err(|e| anyhow::anyhow!("{label} {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            anyhow::bail!(
                "{label} {} has mode {mode:o}; it must not be readable by group or others. \
                 Run: chmod 600 {}",
                path.display(),
                path.display()
            );
        }
    }
    #[cfg(not(unix))]
    let _ = meta;
    let t = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("reading {label} {}: {e}", path.display()))?;
    let t = t.trim();
    Ok((!t.is_empty()).then(|| t.to_string()))
}

/// Per-user data directory for state, staging, publish trees, and caches.
///
/// On Unix this is `$HOME/.local/share` (the same location the default
/// `~/...` config strings expand to; `$XDG_DATA_HOME` is not consulted, so
/// existing configs stay stable). On Windows it is `%LOCALAPPDATA%`,
/// falling back to `%USERPROFILE%\AppData\Local`.
// Used by default_data_file on Windows; kept as a public helper on Unix too.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn data_local_dir() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|h| h.join("AppData").join("Local")))
            .unwrap_or_else(|| PathBuf::from("."))
    }
    #[cfg(not(windows))]
    {
        home_dir()
            .map(|h| h.join(".local").join("share"))
            .unwrap_or_else(|| PathBuf::from("."))
    }
}

/// Default path string under the per-user data directory.
///
/// On Unix this is a `~/.local/share/tensorseed/<rel>` form, expanded later
/// by [`expand`]. On Windows it is an absolute path under
/// `%LOCALAPPDATA%\tensorseed`.
pub(crate) fn default_data_file(rel: &str) -> String {
    #[cfg(windows)]
    {
        data_local_dir()
            .join("tensorseed")
            .join(rel)
            .to_string_lossy()
            .into_owned()
    }
    #[cfg(not(windows))]
    {
        format!("~/.local/share/tensorseed/{rel}")
    }
}

/// Default path string under the per-user config directory.
///
/// On Unix this is a `~/.config/tensorseed/<rel>` form. On Windows it is an
/// absolute path under `%APPDATA%\tensorseed`.
pub(crate) fn default_config_file(rel: &str) -> String {
    #[cfg(windows)]
    {
        config_dir()
            .join("tensorseed")
            .join(rel)
            .to_string_lossy()
            .into_owned()
    }
    #[cfg(not(windows))]
    {
        format!("~/.config/tensorseed/{rel}")
    }
}

/// Expand a leading `~/` (or `~\`) to the user home directory.
///
/// Home is `$HOME`, or `%USERPROFILE%` when `$HOME` is unset. Paths that do
/// not start with a tilde slash are returned unchanged.
pub fn expand(path: &str) -> PathBuf {
    let rest = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\"));
    match rest {
        Some(rest) => home_dir()
            .map(|h| h.join(rest))
            .unwrap_or_else(|| PathBuf::from(path)),
        None => PathBuf::from(path),
    }
}

/// Default config file location.
///
/// On Unix: `$XDG_CONFIG_HOME/tensorseed/tensorseed.toml`, or
/// `$HOME/.config/tensorseed/tensorseed.toml`. On Windows:
/// `%APPDATA%\tensorseed\tensorseed.toml`.
pub fn default_path() -> PathBuf {
    config_dir().join("tensorseed").join("tensorseed.toml")
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
    cfg.huggingface.validate()?;
    cfg.fetch.validate()?;
    cfg.storage.validate()?;
    cfg.torrent.validate()?;
    cfg.serve.validate()?;
    if !cfg.catalog.publisher_pubkey.is_empty() {
        tensorseed_catalog::minisign::PublicKey::parse(&cfg.catalog.publisher_pubkey)?;
    }
    let _ = cfg.subscriptions()?;
    if let Some(m) = &cfg.catalog.chain_check
        && !["strict", "warn", "off"].contains(&m.as_str())
    {
        anyhow::bail!("catalog.chain_check must be \"strict\", \"warn\", or \"off\", not {m:?}");
    }
    if !["light", "datalayer"].contains(&cfg.chia.chain_source.as_str()) {
        anyhow::bail!(
            "chia.chain_source must be \"light\" or \"datalayer\", not {:?}",
            cfg.chia.chain_source
        );
    }
    let sid = cfg.chia.catalog_store_id.trim();
    if !sid.is_empty()
        && (sid.strip_prefix("0x").unwrap_or(sid).len() != 64
            || !sid
                .strip_prefix("0x")
                .unwrap_or(sid)
                .bytes()
                .all(|b| b.is_ascii_hexdigit()))
    {
        anyhow::bail!("chia.catalog_store_id must be 64 hex characters");
    }
    for u in &cfg.chia.coin_api_urls {
        if !(u.starts_with("https://") || u.starts_with("http://")) {
            anyhow::bail!("chia.coin_api_urls: {u:?} is not an http(s) URL");
        }
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
    fn chain_check_defaults_and_validation() {
        Jail::expect_with(|jail| {
            jail.create_file("a.toml", "[catalog]\nbackend = \"file\"\n")?;
            let cfg = load(Path::new("a.toml")).map_err(|e| e.to_string())?;
            assert_eq!(cfg.chain_check(), ChainCheck::Off, "no store id: off");
            assert_eq!(cfg.chia.chain_source, "light");
            assert_eq!(cfg.chia.coin_api_urls, vec![DEFAULT_COIN_API.to_string()]);
            let store = "ab".repeat(32);
            jail.create_file(
                "b.toml",
                &format!("[catalog]\nbackend = \"file\"\n[chia]\ncatalog_store_id = \"{store}\"\n"),
            )?;
            let cfg = load(Path::new("b.toml")).map_err(|e| e.to_string())?;
            assert_eq!(cfg.chain_check(), ChainCheck::Strict, "store id: strict");
            jail.create_file(
                "c.toml",
                &format!("[catalog]\nbackend = \"file\"\nchain_check = \"warn\"\n[chia]\ncatalog_store_id = \"{store}\"\n"),
            )?;
            let cfg = load(Path::new("c.toml")).map_err(|e| e.to_string())?;
            assert_eq!(cfg.chain_check(), ChainCheck::Warn);
            for bad in [
                "[catalog]\nchain_check = \"maybe\"\n",
                "[chia]\nchain_source = \"oracle\"\n",
                "[chia]\ncatalog_store_id = \"xyz\"\n",
                "[chia]\ncoin_api_urls = [\"ftp://x\"]\n",
            ] {
                jail.create_file("d.toml", bad)?;
                assert!(load(Path::new("d.toml")).is_err(), "{bad}");
            }
            Ok(())
        });
    }

    #[test]
    fn example_config_parses() {
        Jail::expect_with(|jail| {
            jail.create_file("tensorseed.toml", EXAMPLE)?;
            let cfg = load(Path::new("tensorseed.toml")).map_err(|e| e.to_string())?;
            assert_eq!(cfg.catalog.backend, "file");
            assert_eq!(cfg.file.base_url, "https://catalog.tensorseed.dev/");
            // Multi-provider example, all disabled: default stays local only.
            assert_eq!(cfg.storage.target.len(), 9);
            assert!(cfg.storage.target.iter().all(|t| !t.enabled));
            assert_eq!(cfg.storage.upload_targets().count(), 0);
            // [serve] in the example matches the built-in defaults. The file
            // documents the Unix tilde form of data_dir, so on Windows (where
            // the default is under %LOCALAPPDATA%) only that field differs.
            let mut expected = crate::serve::ServeConfig::default();
            if cfg!(windows) {
                expected.data_dir = "~/.local/share/tensorseed/serve".into();
            }
            assert_eq!(cfg.serve, expected);
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
            assert_eq!(cfg.fetch.source_order, default_source_order());
            Ok(())
        });
    }

    #[test]
    fn fetch_source_order_validates() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "ok.toml",
                "[fetch]\nsource_order = [\"hub\", \"mirrors\"]\n",
            )?;
            let cfg = load(Path::new("ok.toml")).map_err(|e| e.to_string())?;
            assert_eq!(cfg.fetch.source_order, vec!["hub", "mirrors"]);
            assert!(cfg.fetch.wants(FetchSource::Hub));
            assert!(!cfg.fetch.wants(FetchSource::Swarm));
            for bad in [
                "[fetch]\nsource_order = []\n",
                "[fetch]\nsource_order = [\"hub\", \"hub\"]\n",
                "[fetch]\nsource_order = [\"cdn\"]\n",
            ] {
                jail.create_file("bad.toml", bad)?;
                let err = load(Path::new("bad.toml")).unwrap_err().to_string();
                assert!(
                    err.contains("source_order"),
                    "expected source_order error for {bad}: {err}"
                );
            }
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
                    "torrent_with_bucket",
                    "[[storage.target]]\nname=\"a\"\nprovider = \"torrent\"\nbucket = \"b\"\n",
                ),
                ("bad_piece", "[torrent]\npiece_length = 1000\n"),
                ("bad_listen", "[torrent]\nlisten_addr = \"nope\"\n"),
                ("bad_tracker", "[torrent]\ntrackers = [\"ftp://x\"]\n"),
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
    fn inline_hf_token_rejected_with_clear_error() {
        Jail::expect_with(|jail| {
            jail.create_file("c.toml", "[huggingface]\ntoken = \"hf_abc\"\n")?;
            let e = load(Path::new("c.toml")).unwrap_err().to_string();
            assert!(e.contains("never put the Hugging Face token"), "{e}");
            jail.create_file("d.toml", "[huggingface]\ntoken_env = \"hf_abc-def\"\n")?;
            assert!(load(Path::new("d.toml")).is_err());
            Ok(())
        });
    }

    #[cfg(unix)]
    #[test]
    fn hf_token_env_then_private_file() {
        use std::os::unix::fs::PermissionsExt;
        Jail::expect_with(|jail| {
            let dir = jail.directory().to_path_buf();
            let tf = dir.join("hf_token");
            std::fs::write(&tf, "hf_from_file\n").unwrap();
            std::fs::set_permissions(&tf, std::fs::Permissions::from_mode(0o600)).unwrap();
            jail.create_file(
                "c.toml",
                &format!(
                    "[huggingface]\ntoken_env = \"TS_TEST_HF_TOKEN\"\ntoken_file = \"{}\"\n",
                    tf.display()
                ),
            )?;
            let cfg = load(Path::new("c.toml")).map_err(|e| e.to_string())?;
            assert_eq!(
                cfg.huggingface.resolve_token().unwrap().as_deref(),
                Some("hf_from_file")
            );
            jail.set_env("TS_TEST_HF_TOKEN", "hf_from_env");
            assert_eq!(
                cfg.huggingface.resolve_token().unwrap().as_deref(),
                Some("hf_from_env")
            );
            jail.set_env("TS_TEST_HF_TOKEN", "");
            std::fs::set_permissions(&tf, std::fs::Permissions::from_mode(0o644)).unwrap();
            let e = cfg.huggingface.resolve_token().unwrap_err().to_string();
            assert!(e.contains("chmod 600"), "{e}");
            // `config show` never prints a token field.
            let shown = serde_json::to_string(&cfg).unwrap();
            assert!(!shown.contains("\"token\""), "{shown}");
            Ok(())
        });
    }

    #[cfg(unix)]
    #[test]
    fn hf_token_login_file_then_cli_file() {
        use std::os::unix::fs::PermissionsExt;
        Jail::expect_with(|jail| {
            jail.clear_env();
            let root = jail.directory().to_path_buf();
            let xdg = root.join("xdg");
            let home = root.join("home");
            std::fs::create_dir_all(&xdg).unwrap();
            std::fs::create_dir_all(home.join(".cache").join("huggingface")).unwrap();
            jail.set_env("XDG_CONFIG_HOME", xdg.to_str().unwrap());
            jail.set_env("HOME", home.to_str().unwrap());
            jail.set_env("USERPROFILE", home.to_str().unwrap());
            jail.create_file(
                "c.toml",
                "[huggingface]\ntoken_env = \"TS_TEST_HF_TOKEN\"\n",
            )?;
            let cfg = load(Path::new("c.toml")).map_err(|e| e.to_string())?;
            assert!(cfg.huggingface.resolve_token().unwrap().is_none());

            let cli = home.join(".cache").join("huggingface").join("token");
            std::fs::write(&cli, "hf_from_cli\n").unwrap();
            std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o600)).unwrap();
            let (tok, src) = cfg.huggingface.resolve_token_source().unwrap().unwrap();
            assert_eq!(tok, "hf_from_cli");
            assert!(matches!(src, TokenSource::HfCli(_)), "{src:?}");
            assert!(!src.label().contains("hf_from_cli"), "{}", src.label());

            let path = write_login_token("hf_from_login").map_err(|e| e.to_string())?;
            assert_eq!(path, xdg.join("tensorseed").join("hf_token"));
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{mode:o}");
            let (tok, src) = cfg.huggingface.resolve_token_source().unwrap().unwrap();
            assert_eq!(tok, "hf_from_login");
            assert!(matches!(src, TokenSource::LoginFile(_)), "{src:?}");

            let pinned = root.join("explicit");
            std::fs::write(&pinned, "from_path\n").unwrap();
            jail.set_env("HF_TOKEN_PATH", pinned.to_str().unwrap());
            assert_eq!(
                cfg.huggingface.resolve_token().unwrap().as_deref(),
                Some("hf_from_login")
            );
            remove_login_token().map_err(|e| e.to_string())?;
            std::fs::set_permissions(&pinned, std::fs::Permissions::from_mode(0o600)).unwrap();
            let (tok, src) = cfg.huggingface.resolve_token_source().unwrap().unwrap();
            assert_eq!(tok, "from_path");
            assert!(matches!(src, TokenSource::HfCli(_)), "{src:?}");
            Ok(())
        });
    }

    #[test]
    fn watchlist_entries_take_ids_or_tables() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "c.toml",
                "[harvest]\nrepos = [\"a/b\", { id = \"me/mine\", allow_license = [\"other\"] }]\n",
            )?;
            let cfg = load(Path::new("c.toml")).map_err(|e| e.to_string())?;
            assert_eq!(cfg.harvest.repos.len(), 2);
            assert_eq!(cfg.harvest.repos[0], WatchRepo::from("a/b"));
            assert_eq!(cfg.harvest.repos[1].id(), "me/mine");
            assert_eq!(cfg.harvest.repos[1].allow_license(), ["other".to_string()]);
            assert!(cfg.harvest.repos[0].allow_license().is_empty());
            // The default allowlist stays strict.
            assert!(!cfg.policy.license_allowlist.iter().any(|l| l == "other"));
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

    #[test]
    fn expand_tilde_uses_home_then_userprofile() {
        Jail::expect_with(|jail| {
            jail.set_env("HOME", "/home/ts");
            jail.set_env("USERPROFILE", "C:/Users/ts");
            assert_eq!(
                expand("~/tensorseed/x"),
                PathBuf::from("/home/ts/tensorseed/x")
            );
            assert_eq!(expand("/abs/path"), PathBuf::from("/abs/path"));
            assert_eq!(expand("rel/path"), PathBuf::from("rel/path"));
            Ok(())
        });
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_default_paths_keep_tilde_forms() {
        Jail::expect_with(|jail| {
            jail.set_env("HOME", "/home/ts");
            jail.set_env("XDG_CONFIG_HOME", "/xdg/config");
            assert_eq!(
                default_path(),
                PathBuf::from("/xdg/config/tensorseed/tensorseed.toml")
            );
            assert_eq!(
                General::default().state_db,
                "~/.local/share/tensorseed/state.sqlite"
            );
            assert_eq!(
                General::default().staging_dir,
                "~/.local/share/tensorseed/staging"
            );
            assert_eq!(
                General::default().seed_dir,
                "~/.local/share/tensorseed/seed"
            );
            assert_eq!(
                FileBackend::default().state_dir,
                "~/.local/share/tensorseed/catalog"
            );
            assert_eq!(
                Storage::default().local_dir,
                "~/.local/share/tensorseed/publish"
            );
            assert_eq!(Mirrors::default().file, "~/.config/tensorseed/mirrors.toml");
            assert_eq!(
                crate::serve::ServeConfig::default().data_dir,
                "~/.local/share/tensorseed/serve"
            );
            Ok(())
        });
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_default_path_falls_back_to_home_config() {
        Jail::expect_with(|jail| {
            // GitHub Actions and some desktops set XDG_CONFIG_HOME. Clear
            // inherited env so this tests the $HOME/.config fallback.
            jail.clear_env();
            jail.set_env("HOME", "/home/ts");
            assert!(std::env::var_os("XDG_CONFIG_HOME").is_none());
            assert_eq!(
                default_path(),
                PathBuf::from("/home/ts/.config/tensorseed/tensorseed.toml")
            );
            Ok(())
        });
    }

    #[cfg(windows)]
    #[test]
    fn windows_default_path_uses_appdata() {
        Jail::expect_with(|jail| {
            jail.set_env("APPDATA", r"C:\Users\ts\AppData\Roaming");
            jail.set_env("LOCALAPPDATA", r"C:\Users\ts\AppData\Local");
            jail.set_env("USERPROFILE", r"C:\Users\ts");
            assert_eq!(
                default_path(),
                PathBuf::from(r"C:\Users\ts\AppData\Roaming\tensorseed\tensorseed.toml")
            );
            assert_eq!(
                General::default().state_db,
                r"C:\Users\ts\AppData\Local\tensorseed\state.sqlite"
            );
            assert_eq!(
                crate::serve::ServeConfig::default().data_dir,
                r"C:\Users\ts\AppData\Local\tensorseed\serve"
            );
            assert_eq!(
                Mirrors::default().file,
                r"C:\Users\ts\AppData\Roaming\tensorseed\mirrors.toml"
            );
            Ok(())
        });
    }

    const PK1: &str = "RWSSA0NfLXhfVAteHxL9PPhshHfwrMfYyxUF2VhGZIzNWARaJdwDR5TY";
    const PK2: &str = "RWS4/z1UPQ4ezmjrcpKqDYpqyn5h1WUS2oDs1B73Dh91kVzzNIZd03xd";

    #[test]
    fn single_catalog_config_is_the_sole_subscription() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "one.toml",
                &format!(
                    "[catalog]\nbackend = \"file\"\nname = \"main\"\npublisher_pubkey = \"{PK1}\"\n"
                ),
            )?;
            let cfg = load(Path::new("one.toml")).map_err(|e| e.to_string())?;
            let subs = cfg.subscriptions().map_err(|e| e.to_string())?;
            assert_eq!(subs.len(), 1);
            assert_eq!(subs[0].name, "main");
            assert!(subs[0].is_default);
            assert_eq!(subs[0].publisher_pubkey, PK1);
            assert_eq!(subs[0].state_dir, cfg.file.state_dir);
            Ok(())
        });
    }

    #[test]
    fn catalogs_array_and_conflict_and_default() {
        Jail::expect_with(|jail| {
            let store = "ab".repeat(32);
            jail.create_file(
                "multi.toml",
                &format!(
                    r#"
[catalog]
backend = "file"
chain_check = "off"

[[catalogs]]
name = "other"
label = "Mirror"
publisher_pubkey = "{PK1}"
base_url = "https://catalog.example.org/"
priority = 5

[[catalogs]]
name = "tensorseed"
label = "Official"
publisher_pubkey = "{PK2}"
store_id = "{store}"
base_url = "https://catalog.tensorseed.dev/"
chain_check = "strict"
priority = 20
"#
                ),
            )?;
            let cfg = load(Path::new("multi.toml")).map_err(|e| e.to_string())?;
            let subs = cfg.subscriptions().map_err(|e| e.to_string())?;
            assert_eq!(subs.len(), 2);
            let def = cfg.subscription(None).map_err(|e| e.to_string())?;
            assert_eq!(def.name, "tensorseed");
            assert!(def.is_default);
            assert_eq!(def.label, "Official");
            assert_eq!(def.store_id, store);
            let other = cfg
                .subscription(Some("Mirror"))
                .map_err(|e| e.to_string())?;
            assert_eq!(other.name, "other");
            assert_eq!(other.priority, 5);
            assert_eq!(other.chain_check, ChainCheck::Off, "inherits top-level off");
            // Same name, different key is an error.
            jail.create_file(
                "bad.toml",
                &format!(
                    r#"
[[catalogs]]
name = "dup"
publisher_pubkey = "{PK1}"
base_url = "https://a.example/"

[[catalogs]]
name = "dup"
publisher_pubkey = "{PK2}"
base_url = "https://b.example/"
"#
                ),
            )?;
            let err = load(Path::new("bad.toml")).unwrap_err().to_string();
            assert!(
                err.contains("different publisher keys"),
                "unexpected error: {err}"
            );
            // Same name and key collapse.
            jail.create_file(
                "ok.toml",
                &format!(
                    r#"
[[catalogs]]
name = "dup"
publisher_pubkey = "{PK1}"
base_url = "https://a.example/"

[[catalogs]]
name = "dup"
publisher_pubkey = "{PK1}"
base_url = "https://a.example/"
"#
                ),
            )?;
            let cfg = load(Path::new("ok.toml")).map_err(|e| e.to_string())?;
            assert_eq!(cfg.subscriptions().map_err(|e| e.to_string())?.len(), 1);
            Ok(())
        });
    }
}
