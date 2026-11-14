// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Command line definition (spec section 9).
//!
//! The `///` comments on these types double as clap's `--help` text, so
//! they use the placeholder style of a terminal (`<dir>`, `org/name[@rev]`)
//! instead of Markdown. The rustdoc lints that would flag those
//! placeholders as HTML tags or links are allowed for this module only.

#![allow(rustdoc::invalid_html_tags, rustdoc::broken_intra_doc_links)]

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use tensorseed_core::commercial::CommercialServing;
use tensorseed_core::keys::RepoType;

/// Tensorseed: verifiable, chain anchored mirrors of open Hugging Face models.
#[derive(Debug, Parser)]
#[command(name = "tensorseed", version, about, long_about = None)]
pub struct Cli {
    /// Path to the config file (default: ~/.config/tensorseed/tensorseed.toml, or %APPDATA%\tensorseed\tensorseed.toml on Windows).
    #[arg(long, global = true, env = "TENSORSEED_CONFIG")]
    pub config: Option<PathBuf>,

    /// Log output format.
    #[arg(long, global = true, value_enum, default_value_t = LogFormat::Pretty)]
    pub log_format: LogFormat,

    /// Increase verbosity (-v debug, -vv trace).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Command to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Shells that `tensorseed completions` can generate a script for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CompletionShell {
    /// Bash.
    Bash,
    /// Zsh.
    Zsh,
    /// Fish.
    Fish,
    /// PowerShell.
    PowerShell,
}

impl From<CompletionShell> for clap_complete::Shell {
    fn from(shell: CompletionShell) -> Self {
        match shell {
            CompletionShell::Bash => Self::Bash,
            CompletionShell::Zsh => Self::Zsh,
            CompletionShell::Fish => Self::Fish,
            CompletionShell::PowerShell => Self::PowerShell,
        }
    }
}

/// Log formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LogFormat {
    /// Human readable.
    Pretty,
    /// One JSON object per line.
    Json,
}

/// Repo type argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RepoTypeArg {
    /// Model repository.
    Model,
    /// Dataset repository.
    Dataset,
}

impl From<RepoTypeArg> for RepoType {
    fn from(v: RepoTypeArg) -> Self {
        match v {
            RepoTypeArg::Model => RepoType::Model,
            RepoTypeArg::Dataset => RepoType::Dataset,
        }
    }
}

/// Commercial serving mark argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CommercialServingArg {
    /// Permissive license.
    Allowed,
    /// Commercial use allowed with conditions.
    Restricted,
    /// Non-commercial or research only.
    Forbidden,
    /// Missing, custom, or unrecognized license.
    Unknown,
}

impl From<CommercialServingArg> for CommercialServing {
    fn from(v: CommercialServingArg) -> Self {
        match v {
            CommercialServingArg::Allowed => Self::Allowed,
            CommercialServingArg::Restricted => Self::Restricted,
            CommercialServingArg::Forbidden => Self::Forbidden,
            CommercialServingArg::Unknown => Self::Unknown,
        }
    }
}

/// Runtime argument for `serve-inference`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RuntimeArg {
    /// llama.cpp for GGUF files, vLLM for safetensors.
    Auto,
    /// llama.cpp `llama-server`.
    LlamaCpp,
    /// vLLM `vllm serve`.
    Vllm,
}

/// Top level commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check config, catalog backend, credentials, ports, and disk space.
    ///
    /// Each check prints ok, warn, or fail. Exits 1 when any check fails.
    Doctor(DoctorArgs),
    /// Manage the catalog.
    #[command(subcommand)]
    Catalog(CatalogCmd),
    /// Publish one Hugging Face repo revision.
    Publish {
        /// Hugging Face repo id (org/name).
        repo_id: String,
        /// Repo type.
        #[arg(long = "type", value_enum, default_value_t = RepoTypeArg::Model)]
        repo_type: RepoTypeArg,
        /// Commit SHA to publish (default: current main).
        #[arg(long)]
        revision: Option<String>,
        /// Accept this license id for this run even though it is not on the
        /// allowlist (repeatable), e.g. `--allow-license other` for your own
        /// repo. Gated, private, and takedown rules still apply.
        #[arg(long = "allow-license", value_name = "ID")]
        allow_license: Vec<String>,
    },
    /// Publish every target in the config.
    Sync,
    /// Mirror repos from the Hugging Face Hub while it is available
    /// (watchlist in `[harvest]`).
    Harvest(HarvestArgs),
    /// Download and verify a repo revision.
    Pull(PullArgs),
    /// Seed published revisions (storage.local_dir and torrent targets) or
    /// pulled directories over BitTorrent.
    Seed {
        /// Seed these directories instead: a pulled directory (with
        /// `.tensorseed/torrent`) or a published tree (with `t/`).
        #[arg(long)]
        dir: Vec<PathBuf>,
        /// Stop after this long (30s, 10m, 2h, 1d; default: until Ctrl-C).
        #[arg(long = "for")]
        duration: Option<String>,
        /// Also subscribe to and serve the catalog (not implemented yet).
        #[arg(long)]
        serve_catalog: bool,
        /// Swarm limits for this run.
        #[command(flatten)]
        limits: SwarmArgs,
        /// Stop seeding a torrent once it uploaded this multiple of its size
        /// (overrides torrent.seed_ratio_limit; 0 for no limit).
        #[arg(long, value_name = "RATIO")]
        ratio_limit: Option<f64>,
        /// Stop seeding a torrent after it seeded this long (30m, 12h, 7d;
        /// overrides torrent.seed_time_limit; 0 for no limit).
        #[arg(long, value_name = "DURATION")]
        seed_time: Option<String>,
    },
    /// Run the long lived seeder: sync the catalog on a timer, seed the
    /// selected revisions, drop takedowns, optionally fetch new revisions,
    /// serve files over HTTP, and report health (spec 8.3).
    Serve(ServeArgs),
    /// Re-hash a local copy against a signed manifest or the Hub's hashes.
    Verify(VerifyArgs),
    /// Serve a verified local copy through an OpenAI compatible inference
    /// server (llama.cpp for GGUF, vLLM for safetensors).
    ///
    /// The copy is verified against the signed manifest first and is never
    /// served when verification fails. Licenses marked forbidden for
    /// commercial serving are refused unless `--i-have-rights` is passed.
    /// No runtime is bundled; configure binaries in `[inference]` or put
    /// them on PATH. See docs/SELL-INFERENCE.md.
    ServeInference(ServeInferenceArgs),
    /// Generate expected next-token logprobs for a verified copy
    /// (`ts.reference-logprobs/1`), for logprob challenges.
    ///
    /// The copy is verified against the signed manifest first. The runtime
    /// is started the same way serve-inference starts it (on a private
    /// loopback port), or an already running endpoint is used with
    /// `--endpoint`. The output file holds the challenge prompts and is
    /// secret until revealed; `--commitment` writes the public part
    /// (`set_hash`). See docs/plans/verified-serving.md section 5.2.
    ReferenceLogprobs(ReferenceLogprobsArgs),
    /// Send logprob challenges from a reference file to an OpenAI
    /// compatible endpoint and compare the answers within tolerances.
    ///
    /// Exit status: 0 pass, 1 fail, 2 inconclusive or unsupported (also
    /// for errors that prevent a verdict).
    CheckEndpoint(CheckEndpointArgs),
    /// Create or show the Ed25519 response signing key.
    ///
    /// The key lives in an owner-only file next to the config file
    /// (`response-key.json`). The secret is never printed or taken on the
    /// command line; only the public key is shown.
    #[command(subcommand)]
    ResponseKey(ResponseKeyCmd),
    /// Write the `ts.served-weights/1` claim for an engine you run
    /// yourself, or attach a wallet signature to an existing claim.
    ///
    /// The copy is verified against the signed manifest first. With
    /// `--wallet` the claim names your wallet and the command prints the
    /// exact message to sign with EIP-191 (personal_sign) in your own
    /// wallet; Tensorseed never holds wallet keys. Run again with `--sig`
    /// to check and attach the signature.
    ClaimServing(ClaimServingArgs),
    /// Run the signing proxy in front of an OpenAI compatible engine.
    ///
    /// Requests and headers pass through unchanged. JSON responses get
    /// `X-Tensorseed-*` signature headers; event streams end with a signed
    /// SSE comment. The claim is served at
    /// `/.well-known/tensorseed/served-weights.json`.
    SignResponses(SignResponsesArgs),
    /// Verify a signed response against a `ts.served-weights/1` claim.
    ///
    /// Exit status: 0 valid, 1 invalid or unsigned, 2 error.
    VerifyResponse(VerifyResponseArgs),
    /// List catalog entries.
    ///
    /// Each row shows a commercial serving mark derived from the license id
    /// (allowed, restricted, forbidden, or unknown). The mark is not legal
    /// advice; read the license before serving a model commercially.
    List {
        /// Filter by repo id substring.
        #[arg(long)]
        filter: Option<String>,
        /// Only entries whose license permits commercial use (marked
        /// allowed or restricted).
        #[arg(long)]
        commercial: bool,
        /// Only entries with this commercial serving mark (repeatable).
        #[arg(long = "commercial-serving", value_enum, value_name = "MARK")]
        commercial_serving: Vec<CommercialServingArg>,
        /// Print one JSON document (`ts.list/1`) instead of text.
        #[arg(long)]
        json: bool,
        /// Use the local catalog copy without syncing.
        #[arg(long)]
        no_wait: bool,
        /// List this subscribed catalog only.
        #[arg(long, value_name = "NAME")]
        catalog: Option<String>,
    },
    /// Print a manifest from the catalog.
    Show {
        /// Hugging Face repo id (org/name[@commit]).
        repo: String,
        /// Repo type.
        #[arg(long = "type", value_enum, default_value_t = RepoTypeArg::Model)]
        repo_type: RepoTypeArg,
        /// Commit SHA (default: latest).
        #[arg(long)]
        revision: Option<String>,
        /// Use the local catalog copy without syncing.
        #[arg(long)]
        no_wait: bool,
        /// Use this subscribed catalog (required when two catalogs disagree on hashes).
        #[arg(long, value_name = "NAME")]
        catalog: Option<String>,
    },
    /// Write a tombstone for a repo and optionally delete webseed objects.
    Takedown {
        /// Hugging Face repo id (org/name).
        repo_id: String,
        /// Reason for the takedown.
        #[arg(long)]
        reason: String,
        /// Specific revisions (default: all).
        #[arg(long, num_args = 1..)]
        revisions: Vec<String>,
        /// Also delete webseed objects.
        #[arg(long)]
        delete_webseed: bool,
    },
    /// Inspect and manage resumable jobs.
    Jobs {
        /// Show the files of this job.
        #[arg(long)]
        show: Option<String>,
        /// Delete the record of this job (files on disk are kept).
        #[arg(long)]
        clear: Option<String>,
    },
    /// Manage mirror lists (local mirrors.toml and the signed published list).
    #[command(subcommand)]
    Mirror(MirrorCmd),
    /// Inspect configuration.
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Hugging Face token: login, logout, and status.
    #[command(subcommand)]
    Hf(HfCmd),
    /// Print a shell completion script (bash, zsh, fish, or powershell).
    Completions {
        /// Shell to generate completions for.
        shell: CompletionShell,
    },
    /// Write shell completions and man pages into a directory (`just generate-docs`).
    #[command(hide = true)]
    GenerateDocs {
        /// Output directory (`completions/` and `man/` are created under it).
        dir: PathBuf,
    },
}

/// `hf` subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum HfCmd {
    /// Save a Hugging Face token for Tensorseed (0600 on Unix, user-only ACL on Windows).
    Login,
    /// Remove the token saved by `tensorseed hf login`.
    Logout,
    /// Show the account and token source without printing the token.
    Status,
}

/// `mirror` subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum MirrorCmd {
    /// Add or replace (by name or URL) a mirror in the local mirrors.toml.
    Add {
        /// Public base URL of the mirrored tree.
        url: Option<String>,
        /// Unique name.
        #[arg(long)]
        name: Option<String>,
        /// http | s3 | r2 | b2 | vultr | wasabi | minio | aws | torrent.
        #[arg(long, default_value = "http")]
        provider: String,
        /// Lower is tried first.
        #[arg(long, default_value_t = 100)]
        priority: i64,
        /// Magnet link (torrent mirrors).
        #[arg(long)]
        magnet: Option<String>,
        /// Free text label.
        #[arg(long)]
        label: Option<String>,
        /// Only use for catalogs signed by this minisign public key.
        #[arg(long)]
        pubkey: Option<String>,
        /// Keep the entry but do not use it.
        #[arg(long)]
        disabled: bool,
    },
    /// Remove a mirror by name or URL.
    Remove {
        /// Name or URL.
        name_or_url: String,
    },
    /// Show the merged mirror ranking.
    List {
        /// Do not fetch the published list; use the cached copy.
        #[arg(long)]
        offline: bool,
    },
    /// Sign a mirror list and publish it as <catalog>/mirrors.toml.
    Publish {
        /// List to publish (default: the local mirrors.toml).
        #[arg(long)]
        file: Option<std::path::PathBuf>,
    },
}

/// `catalog` subcommands.
#[derive(Debug, Subcommand)]
pub enum CatalogCmd {
    /// Create a new catalog through the configured backend.
    Create {
        /// Chia: return right after submitting the store instead of waiting
        /// for it to confirm on chain (imports and publishes need it confirmed).
        #[arg(long)]
        no_wait: bool,
    },
    /// Generate a publisher key pair (minisign format).
    Keygen {
        /// Directory for `<name>.key` and `<name>.pub` (default ~/.config/tensorseed, or %APPDATA%\tensorseed on Windows).
        #[arg(long)]
        out_dir: Option<PathBuf>,
        /// File name stem.
        #[arg(long, default_value = "publisher")]
        name: String,
        /// Store the secret key unencrypted (not recommended).
        #[arg(long)]
        no_password: bool,
        /// Overwrite existing files.
        #[arg(long)]
        force: bool,
    },
    /// Sync and show catalog revision, root, and sources.
    Info {
        /// Use the local catalog copy without syncing.
        #[arg(long)]
        no_wait: bool,
        /// Report this subscribed catalog only.
        #[arg(long, value_name = "NAME")]
        catalog: Option<String>,
    },
    /// Advertise a catalog mirror.
    AddMirror {
        /// Public mirror URL(s).
        #[arg(long, required = true, num_args = 1..)]
        url: Vec<String>,
        /// Mirror coin amount in mojos.
        #[arg(long)]
        amount: Option<u64>,
        /// Fee in mojos.
        #[arg(long)]
        fee: Option<u64>,
    },
    /// Subscribe the local DataLayer to the catalog store (Chia backend) so
    /// it downloads and keeps a copy. Mirrors advertised on chain are added
    /// automatically; `--url` adds extra ones.
    Subscribe {
        /// Extra DataLayer mirror URL(s) (default: chia.catalog_mirror_urls).
        #[arg(long, num_args = 1..)]
        url: Vec<String>,
    },
    /// Export every signed entry to a backend neutral bundle (ts.export/1).
    Export {
        /// Output file for the bundle.
        #[arg(long)]
        out: PathBuf,
        /// Backend to read from (default: catalog.backend).
        #[arg(long, value_enum)]
        backend: Option<BackendArg>,
        /// Use the local catalog copy without syncing (file backend).
        #[arg(long)]
        no_wait: bool,
    },
    /// Verify a bundle against the pinned publisher key and publish its
    /// entries, signatures unchanged, as one new generation.
    Import {
        /// Bundle written by `catalog export`.
        bundle: PathBuf,
        /// Backend to write to (default: catalog.backend).
        #[arg(long, value_enum)]
        backend: Option<BackendArg>,
        /// Verify only; do not write anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Re-sign head.json with a fresh expiry and upload it (file backend).
    ResignHead,
    /// Publisher: rewrite the signed catalog info (ts/v1/catalog) from the
    /// current config, for example after changing `publisher_contact` or
    /// `publisher_policy_url` (file backend). Keeps the original `created_at`.
    UpdateInfo {
        /// Print the current and proposed values without signing or uploading.
        #[arg(long)]
        dry_run: bool,
    },
    /// Write a full snapshot now and point head.json at it (file backend).
    Snapshot,
    /// Publisher: make the Chia store (chia.catalog_store_id) anchor the
    /// current file catalog revision, writing every entry and the
    /// ts/v1/file-head anchor in one generation if needed, then upload
    /// chain-proof.json. Use once to bootstrap a new store, or to recover
    /// after an interrupted publish.
    Anchor,
    /// Check the file catalog against the Chia store and print the result,
    /// whatever catalog.chain_check says. Exits non-zero on a mismatch.
    ChainStatus {
        /// Compare the local catalog copy without syncing it first.
        #[arg(long)]
        no_wait: bool,
        /// Check this subscribed catalog only.
        #[arg(long, value_name = "NAME")]
        catalog: Option<String>,
    },
    /// List subscribed catalogs (name, label, head, chain status).
    List,
    /// Add a `[[catalogs]]` subscription to the config file.
    Add {
        /// Catalog name bound into signatures.
        #[arg(long)]
        name: String,
        /// Publisher public key (minisign `RW...`).
        #[arg(long)]
        publisher_pubkey: String,
        /// File catalog HTTPS base.
        #[arg(long)]
        base_url: String,
        /// Chia DataLayer store id (64 hex).
        #[arg(long)]
        store_id: Option<String>,
        /// Local display name (not signed).
        #[arg(long)]
        label: Option<String>,
        /// `strict`, `warn`, or `off`.
        #[arg(long)]
        chain_check: Option<String>,
        /// Lower is preferred when hashes agree.
        #[arg(long)]
        priority: Option<i64>,
    },
    /// Drop a `[[catalogs]]` subscription. Verified file catalog state on disk is kept.
    Remove {
        /// Catalog name.
        name: String,
    },
}

/// Catalog backend argument (mirrors `catalog.backend`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BackendArg {
    /// Chia DataLayer.
    Chia,
    /// Signed static files.
    File,
}

impl BackendArg {
    /// Config name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chia => "chia",
            Self::File => "file",
        }
    }
}

/// `config` subcommands.
#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    /// Print the effective merged configuration as JSON.
    Show,
    /// Print the config file path that would be used.
    Path,
}

/// `pull` arguments.
#[derive(Debug, Args)]
pub struct PullArgs {
    /// Hugging Face repo id, optionally with a revision: `org/name[@rev]`.
    pub repo: String,
    /// Repo type.
    #[arg(long = "type", value_enum, default_value_t = RepoTypeArg::Model)]
    pub repo_type: RepoTypeArg,
    /// Revision (catalog: commit SHA; --from-hf: branch, tag, or commit).
    #[arg(long)]
    pub revision: Option<String>,
    /// Output directory (default: <general.download_dir>/<org>/<name>).
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Ignore the catalog and download straight from the Hub, verifying
    /// against the Hub's published SHA-256 and git blob ids (unsigned).
    #[arg(long, conflicts_with_all = ["manifest_file", "manifest_url"])]
    pub from_hf: bool,
    /// Use a local manifest file (signed entry or plain manifest) instead of the catalog.
    #[arg(long, conflicts_with = "manifest_url")]
    pub manifest_file: Option<PathBuf>,
    /// Use a manifest URL (signed entry or plain manifest) instead of the catalog.
    #[arg(long)]
    pub manifest_url: Option<String>,
    /// Do not fall back to Hugging Face when webseeds fail.
    #[arg(long)]
    pub no_hf_fallback: bool,
    /// Ignore cooldown marks and try every HTTP mirror this run. A
    /// successful download from a previously dead origin clears the mark.
    #[arg(long)]
    pub retry_dead_mirrors: bool,
    /// Files downloaded at the same time (default: general.max_concurrent_downloads).
    #[arg(long)]
    pub jobs: Option<usize>,
    /// Also download pickle based files (.bin, .pt, .ckpt, ...), which can run code when loaded.
    #[arg(long)]
    pub allow_pickle: bool,
    /// Only download files whose paths match this glob (repeatable). `*`
    /// matches any run of characters, including `/`, the same rules
    /// `serve` uses for repo selection. If any `--include` is given, only
    /// matching paths are fetched.
    #[arg(long, value_name = "GLOB")]
    pub include: Vec<String>,
    /// Skip files whose paths match this glob (repeatable). Wins over
    /// `--include` when both match.
    #[arg(long, value_name = "GLOB")]
    pub exclude: Vec<String>,
    /// Allow gated repos with --from-hf (needs HF_TOKEN and accepted terms).
    #[arg(long)]
    pub allow_gated: bool,
    /// Accept this license id without the "would not be published" warning
    /// (repeatable). Personal downloads are never refused for license.
    #[arg(long = "allow-license", value_name = "ID")]
    pub allow_license: Vec<String>,
    /// Use the current local catalog copy without syncing.
    #[arg(long)]
    pub no_wait: bool,
    /// Use this subscribed catalog (required when two catalogs disagree on hashes).
    #[arg(long, value_name = "NAME")]
    pub catalog: Option<String>,
    /// Download even if the repo is tombstoned.
    #[arg(long)]
    pub ignore_tombstone: bool,
    /// Use the BitTorrent swarm first (the default when the manifest has a
    /// torrent and torrent.pull_swarm is on). HTTP mirrors and the Hub fill
    /// gaps. Accepted as a no-op alias so older command lines still work;
    /// it also forces the swarm on if pull_swarm is off.
    #[arg(long, conflicts_with = "no_swarm")]
    pub swarm: bool,
    /// Skip the swarm and download over HTTP mirrors and the Hub only.
    #[arg(long, conflicts_with = "swarm_only")]
    pub no_swarm: bool,
    /// Use only the BitTorrent swarm: never download over HTTP or from the
    /// Hub. Files the swarm does not deliver fail (exit 3). Useful to test
    /// seeders. Waits up to torrent.swarm_timeout_secs and does not apply
    /// the no-peer or stall failover rules.
    #[arg(long, conflicts_with = "from_hf")]
    pub swarm_only: bool,
    /// Extra peer to dial (`host:port`), repeatable. Used when the swarm is
    /// used (the default on pull).
    #[arg(long)]
    pub peer: Vec<String>,
    /// After a verified download, seed it for this long (30s, 10m, 2h, 1d,
    /// or forever).
    #[arg(long)]
    pub seed_after: Option<String>,
    /// Swarm limits for this run (also used by `--seed-after`).
    #[command(flatten)]
    pub limits: SwarmArgs,
}

/// Swarm limits shared by `pull` and `seed`; each overrides a `[torrent]`
/// key for this run.
#[derive(Debug, Clone, Default, Args)]
pub struct SwarmArgs {
    /// Download cap in KiB/s, 0 for unlimited (torrent.download_limit_kbps).
    #[arg(long, value_name = "KIBPS")]
    pub download_limit: Option<u64>,
    /// Upload cap in KiB/s, 0 for unlimited (torrent.upload_limit_kbps).
    #[arg(long, value_name = "KIBPS")]
    pub upload_limit: Option<u64>,
    /// Peer connections per torrent, 0 for the default of 128
    /// (torrent.max_peers_per_torrent).
    #[arg(long, value_name = "N")]
    pub max_peers: Option<u32>,
}

impl SwarmArgs {
    /// Apply the overrides to `t`.
    pub fn apply(&self, t: &mut tensorseed_app::config::Torrent) {
        if let Some(k) = self.download_limit {
            t.download_limit_kbps = k;
        }
        if let Some(k) = self.upload_limit {
            t.upload_limit_kbps = k;
        }
        if let Some(n) = self.max_peers {
            t.max_peers_per_torrent = n;
        }
    }
}

/// `serve` arguments (each overrides the matching `[serve]` key).
#[derive(Debug, Clone, Default, Args)]
pub struct ServeArgs {
    /// Stop after this long (30s, 10m, 2h, 1d; default: until SIGTERM or Ctrl-C).
    #[arg(long = "for")]
    pub duration: Option<String>,
    /// Mirror the signed catalog files and serve them over HTTP (needs an
    /// HTTP address, `--http` or `serve.http_listen`).
    #[arg(long)]
    pub serve_catalog: bool,
    /// Serve seeded revisions over HTTP on this address (webseed layout).
    #[arg(long, value_name = "ADDR:PORT")]
    pub http: Option<String>,
    /// Health endpoint address (`off` to disable).
    #[arg(long, value_name = "ADDR:PORT")]
    pub health: Option<String>,
    /// Download selected revisions that are not on disk yet.
    #[arg(long, conflicts_with = "no_fetch")]
    pub fetch: bool,
    /// Only seed what is already on disk.
    #[arg(long)]
    pub no_fetch: bool,
    /// Extra published tree (with `t/`) to seed from; repeatable.
    #[arg(long)]
    pub dir: Vec<PathBuf>,
    /// Seconds between catalog syncs.
    #[arg(long, value_name = "SECS")]
    pub sync_interval: Option<u64>,
}

/// `doctor` arguments.
#[derive(Debug, Clone, Args)]
pub struct DoctorArgs {
    /// Print the report as one JSON document (`ts.doctor/1`) on stdout.
    #[arg(long)]
    pub json: bool,
    /// Skip every network check (catalogs, mirrors, Hub).
    #[arg(long)]
    pub offline: bool,
    /// Seconds each network check may take.
    #[arg(long, value_name = "SECS", default_value_t = 15)]
    pub timeout: u64,
    /// Warn when the download directory has less than this many GiB free.
    #[arg(long, value_name = "GIB", default_value_t = 10)]
    pub min_free_gib: u64,
    /// Do not ask the Hub who the token belongs to.
    #[arg(long)]
    pub no_whoami: bool,
}

/// `serve-inference` arguments.
#[derive(Debug, Args)]
pub struct ServeInferenceArgs {
    /// A directory written by `pull`, or a repo id `org/name[@rev]` (served
    /// from the pull default directory).
    pub target: String,
    /// Directory to serve when the target is a repo id.
    #[arg(long)]
    pub dir: Option<PathBuf>,
    /// Inference runtime.
    #[arg(long, value_enum, default_value_t = RuntimeArg::Auto)]
    pub runtime: RuntimeArg,
    /// GGUF file to load, relative to the directory (needed when the repo
    /// has more than one quantization).
    #[arg(long, value_name = "PATH")]
    pub file: Option<String>,
    /// Model id the server reports on /v1/models and that you list on a
    /// marketplace (default: the repo id).
    #[arg(long, value_name = "ID")]
    pub model_id: Option<String>,
    /// Listen address (overrides inference.host).
    #[arg(long)]
    pub host: Option<String>,
    /// Listen port (overrides inference.port).
    #[arg(long)]
    pub port: Option<u16>,
    /// Public base URL buyers reach, usually your TLS reverse proxy (for
    /// example `https://llm.example.com/v1`); recorded in served-weights.json.
    #[arg(long, value_name = "URL")]
    pub public_url: Option<String>,
    /// Serve a model whose license is marked forbidden for commercial
    /// serving, because you hold a separate license or will not charge.
    #[arg(long = "i-have-rights")]
    pub i_have_rights: bool,
    /// Print the runtime command and exit without starting it.
    #[arg(long)]
    pub print_command: bool,
    /// Seconds to wait for /v1/models to answer (overrides
    /// inference.ready_timeout_secs).
    #[arg(long, value_name = "SECS")]
    pub ready_timeout: Option<u64>,
    /// Use the local catalog copy without syncing.
    #[arg(long)]
    pub no_wait: bool,
    /// Use this subscribed catalog for the manifest.
    #[arg(long, value_name = "NAME")]
    pub catalog: Option<String>,
    /// Hashing threads for verification.
    #[arg(long, default_value_t = 4)]
    pub threads: usize,
    /// Put the signing proxy in front of the runtime: the runtime moves to
    /// a private loopback port, the proxy listens on host:port, and a
    /// signed-format claim (`ts.served-weights/1`) is written.
    #[arg(long)]
    pub sign: bool,
    /// Response key file (default: response-key.json next to the config).
    #[arg(long, value_name = "PATH", requires = "sign")]
    pub response_key: Option<PathBuf>,
    /// Seller wallet named in the claim (plain data; sign the printed
    /// message in your wallet and attach it with `claim-serving --sig`).
    #[arg(long, value_name = "0x...", requires = "sign")]
    pub wallet: Option<String>,
    /// Logprob tier recorded in the claim.
    #[arg(long, value_enum, default_value_t = LogprobsArg::Required, requires = "sign")]
    pub logprobs: LogprobsArg,
    /// Extra arguments passed to the runtime after the configured ones
    /// (after `--`).
    #[arg(last = true, value_name = "ARGS")]
    pub extra: Vec<String>,
}

/// Logprob tier of an offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LogprobsArg {
    /// Responses carry logprobs (the logprob-verified tier).
    Required,
    /// The engine cannot return logprobs.
    None,
}

/// `response-key` subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum ResponseKeyCmd {
    /// Create a new key (refuses to replace one without --force).
    Create {
        /// Key file (default: response-key.json next to the config).
        #[arg(long, value_name = "PATH")]
        out: Option<PathBuf>,
        /// Replace an existing key. Claims and buyers pinned to the old
        /// public key stop verifying.
        #[arg(long)]
        force: bool,
    },
    /// Print the public key and the key file path.
    Show {
        /// Key file (default: response-key.json next to the config).
        #[arg(long, value_name = "PATH")]
        key: Option<PathBuf>,
    },
}

/// `claim-serving` arguments.
#[derive(Debug, Args)]
pub struct ClaimServingArgs {
    /// A directory written by `pull`, or a repo id `org/name[@rev]`. With
    /// `--sig`, also a claim file.
    pub target: String,
    /// Directory when the target is a repo id.
    #[arg(long)]
    pub dir: Option<PathBuf>,
    /// Inference runtime the engine is.
    #[arg(long, value_enum, default_value_t = RuntimeArg::Auto)]
    pub runtime: RuntimeArg,
    /// GGUF file the engine loads, relative to the directory.
    #[arg(long, value_name = "PATH")]
    pub file: Option<String>,
    /// Model id the engine serves (default: the repo id).
    #[arg(long, value_name = "ID")]
    pub model_id: Option<String>,
    /// Base URL of the signing proxy in front of the engine.
    #[arg(long, value_name = "URL", default_value = "http://127.0.0.1:8090/v1")]
    pub base_url: String,
    /// Public base URL buyers reach (https).
    #[arg(long, value_name = "URL")]
    pub public_url: Option<String>,
    /// Seller wallet named in the claim (plain data).
    #[arg(long, value_name = "0x...")]
    pub wallet: Option<String>,
    /// Response key file (default: response-key.json next to the config).
    #[arg(long, value_name = "PATH")]
    pub response_key: Option<PathBuf>,
    /// Logprob tier.
    #[arg(long, value_enum, default_value_t = LogprobsArg::Required)]
    pub logprobs: LogprobsArg,
    /// Engine version string (default: the runtime's `--version`).
    #[arg(long, value_name = "TEXT")]
    pub engine_version: Option<String>,
    /// Serve a model whose license is marked forbidden for commercial
    /// serving, because you hold a separate license or will not charge.
    #[arg(long = "i-have-rights")]
    pub i_have_rights: bool,
    /// Use the local catalog copy without syncing.
    #[arg(long)]
    pub no_wait: bool,
    /// Use this subscribed catalog for the manifest.
    #[arg(long, value_name = "NAME")]
    pub catalog: Option<String>,
    /// Hashing threads for verification.
    #[arg(long, default_value_t = 4)]
    pub threads: usize,
    /// Claim file to write (default: `.tensorseed/served-weights.json` in
    /// the directory).
    #[arg(long, short = 'o', value_name = "PATH")]
    pub out: Option<PathBuf>,
    /// Attach this EIP-191 wallet signature (0x and 130 hex) to the
    /// existing claim after checking it recovers to the claim's wallet.
    #[arg(long, value_name = "0x...")]
    pub sig: Option<String>,
}

/// `sign-responses` arguments.
#[derive(Debug, Args)]
pub struct SignResponsesArgs {
    /// Engine root URL, without `/v1` (for example `http://127.0.0.1:8081`).
    #[arg(long, value_name = "URL")]
    pub upstream: String,
    /// The `ts.served-weights/1` claim to sign under.
    #[arg(long, value_name = "PATH")]
    pub claim: PathBuf,
    /// Listen address.
    #[arg(long, default_value = "127.0.0.1:8090")]
    pub listen: String,
    /// Response key file (default: response-key.json next to the config).
    #[arg(long, value_name = "PATH")]
    pub response_key: Option<PathBuf>,
}

/// `verify-response` arguments.
#[derive(Debug, Args)]
pub struct VerifyResponseArgs {
    /// The claim: a file, or an http(s) URL (a base URL fetches the
    /// well-known path).
    #[arg(long, value_name = "PATH|URL")]
    pub claim: String,
    /// Response headers (as written by `curl -D`).
    #[arg(long, value_name = "PATH")]
    pub headers: PathBuf,
    /// Response body exactly as received (`curl -o`; for streams, with
    /// `--no-buffer`).
    #[arg(long, value_name = "PATH")]
    pub body: PathBuf,
    /// The request id you sent; the response must echo it.
    #[arg(long, value_name = "ID")]
    pub request_id: Option<String>,
    /// The model id you requested.
    #[arg(long, value_name = "ID")]
    pub model_id: Option<String>,
    /// Allowed timestamp skew in seconds.
    #[arg(long, default_value_t = 300)]
    pub max_skew: u64,
    /// Print a JSON result.
    #[arg(long)]
    pub json: bool,
}

/// `reference-logprobs` arguments.
#[derive(Debug, Args)]
pub struct ReferenceLogprobsArgs {
    /// A directory written by `pull`, or a repo id `org/name[@rev]` (read
    /// from the pull default directory).
    pub target: String,
    /// Directory when the target is a repo id.
    #[arg(long)]
    pub dir: Option<PathBuf>,
    /// Inference runtime.
    #[arg(long, value_enum, default_value_t = RuntimeArg::Auto)]
    pub runtime: RuntimeArg,
    /// GGUF file (quantization) to load, relative to the directory. Each
    /// file gets its own reference set.
    #[arg(long, value_name = "PATH")]
    pub file: Option<String>,
    /// Write the reference file (secret until revealed) here.
    #[arg(long, short = 'o', value_name = "PATH")]
    pub out: PathBuf,
    /// Also write the public commitment (`set_hash` and bindings, no
    /// prompts) here.
    #[arg(long, value_name = "PATH")]
    pub commitment: Option<PathBuf>,
    /// Challenge prompts: a JSON array, an object with `prompts`, or JSON
    /// Lines; each prompt has `messages` (chat) or `prompt` (completions).
    #[arg(long, value_name = "PATH", conflicts_with_all = ["count", "gen_seed"])]
    pub prompts: Option<PathBuf>,
    /// Number of prompts to generate (ts.challenge-gen/1).
    #[arg(long, default_value_t = 32)]
    pub count: u32,
    /// Generator seed, 64 hex digits (default: random, kept in the
    /// secret file so the set can be regenerated after reveal).
    #[arg(long, value_name = "HEX")]
    pub gen_seed: Option<String>,
    /// Alternatives per position (1 to 20).
    #[arg(long, default_value_t = 5)]
    pub top_logprobs: u32,
    /// Tokens per challenge (1 to 64).
    #[arg(long, default_value_t = 1)]
    pub max_tokens: u32,
    /// Request seed sent with every challenge.
    #[arg(long, default_value_t = 1)]
    pub sampling_seed: u64,
    /// Use this already running OpenAI compatible endpoint (for example
    /// `http://127.0.0.1:8090/v1`) instead of starting the runtime. It
    /// must be serving this verified copy and entrypoint.
    #[arg(long, value_name = "URL")]
    pub endpoint: Option<String>,
    /// Model id to request (default: the repo id).
    #[arg(long, value_name = "ID")]
    pub model_id: Option<String>,
    /// Environment variable holding the endpoint's API key (with
    /// `--endpoint`). The key is never taken on the command line.
    #[arg(long, value_name = "VAR")]
    pub api_key_env: Option<String>,
    /// Engine version to record (default: the runtime's `--version`, or
    /// the endpoint's `/props` build info, or `unknown`).
    #[arg(long, value_name = "TEXT")]
    pub engine_version: Option<String>,
    /// Seconds per request.
    #[arg(long, value_name = "SECS", default_value_t = 120)]
    pub timeout: u64,
    /// Seconds to wait for /v1/models after starting the runtime
    /// (overrides inference.ready_timeout_secs).
    #[arg(long, value_name = "SECS")]
    pub ready_timeout: Option<u64>,
    /// Use the local catalog copy without syncing.
    #[arg(long)]
    pub no_wait: bool,
    /// Use this subscribed catalog for the manifest.
    #[arg(long, value_name = "NAME")]
    pub catalog: Option<String>,
    /// Hashing threads for verification.
    #[arg(long, default_value_t = 4)]
    pub threads: usize,
    /// Extra arguments passed to the runtime after the configured ones
    /// (after `--`).
    #[arg(last = true, value_name = "ARGS")]
    pub extra: Vec<String>,
}

/// `check-endpoint` arguments.
#[derive(Debug, Args)]
pub struct CheckEndpointArgs {
    /// Endpoint base URL, usually ending in `/v1`.
    pub base_url: String,
    /// Reference file (`ts.reference-logprobs/1`).
    #[arg(long, short = 'r', value_name = "PATH")]
    pub reference: PathBuf,
    /// Require the reference file to have this published set_hash.
    #[arg(long, value_name = "HEX")]
    pub expect_set_hash: Option<String>,
    /// Challenges to send, sampled from the set (0 for all).
    #[arg(long, default_value_t = 8)]
    pub sample: usize,
    /// Sampling seed, 64 hex digits (default: random; printed so a run
    /// can be repeated).
    #[arg(long, value_name = "HEX")]
    pub sample_seed: Option<String>,
    /// Model id to request (default: the reference file's repo id).
    #[arg(long, value_name = "ID")]
    pub model_id: Option<String>,
    /// Environment variable holding the endpoint's API key. The key is
    /// never taken on the command line and never printed.
    #[arg(long, value_name = "VAR")]
    pub api_key_env: Option<String>,
    /// Seconds per request.
    #[arg(long, value_name = "SECS", default_value_t = 60)]
    pub timeout: u64,
    /// Challenges in flight at once.
    #[arg(long, default_value_t = 2)]
    pub concurrency: usize,
    /// Attempts for a challenge outside tolerance; it fails only when
    /// every attempt does.
    #[arg(long, default_value_t = 3)]
    pub repeats: u32,
    /// Largest mean absolute logprob deviation per challenge (nats).
    #[arg(long, value_name = "NATS", default_value_t = 0.5)]
    pub max_mean_abs: f64,
    /// Largest deviation of any one token (nats).
    #[arg(long, value_name = "NATS", default_value_t = 2.0)]
    pub max_token_abs: f64,
    /// Send llama.cpp's `cache_prompt: false` with every challenge, for
    /// self-checks and calibration against your own llama.cpp server. A
    /// marketplace should not send it, so challenges look like ordinary
    /// requests.
    #[arg(long)]
    pub no_prompt_cache: bool,
    /// Check response signatures against this `ts.served-weights/1` claim
    /// (a file or an http(s) URL). Without it, the claim is fetched from
    /// the endpoint's well-known path when one is served there. With a
    /// claim, an unsigned or badly signed response fails its challenge.
    #[arg(long, value_name = "PATH|URL")]
    pub claim: Option<String>,
    /// Fail when the endpoint serves no claim (no signed responses).
    #[arg(long)]
    pub require_signatures: bool,
    /// Allowed signature timestamp skew in seconds.
    #[arg(long, default_value_t = 300)]
    pub max_skew: u64,
    /// Print one JSON document (`ts.check-endpoint/1`) on stdout.
    #[arg(long)]
    pub json: bool,
}

/// `verify` arguments.
#[derive(Debug, Args)]
pub struct VerifyArgs {
    /// A directory written by `pull`, or a repo id `org/name[@rev]`.
    pub target: String,
    /// Directory to check when the target is a repo id (default: the pull default).
    #[arg(long)]
    pub dir: Option<PathBuf>,
    /// Repo whose catalog manifest to check a directory against.
    #[arg(long)]
    pub repo: Option<String>,
    /// Repo type.
    #[arg(long = "type", value_enum, default_value_t = RepoTypeArg::Model)]
    pub repo_type: RepoTypeArg,
    /// Revision (commit SHA for the catalog; any revision with --hf).
    #[arg(long)]
    pub revision: Option<String>,
    /// Check against the Hugging Face Hub's own SHA-256 (LFS) and git blob ids.
    #[arg(long, conflicts_with = "manifest_file")]
    pub hf: bool,
    /// Check against a manifest file (signed entry or plain manifest).
    #[arg(long)]
    pub manifest_file: Option<PathBuf>,
    /// Use the local catalog copy without syncing.
    #[arg(long)]
    pub no_wait: bool,
    /// Use this subscribed catalog (required when two catalogs disagree on hashes).
    #[arg(long, value_name = "NAME")]
    pub catalog: Option<String>,
    /// Count missing pickle files as failures (by default they were skipped on pull).
    #[arg(long)]
    pub strict: bool,
    /// Hashing threads.
    #[arg(long, default_value_t = 4)]
    pub threads: usize,
}

/// `harvest` arguments.
#[derive(Debug, Args)]
pub struct HarvestArgs {
    /// Keep running: re-check the watchlist every `harvest.poll_interval_secs`.
    #[arg(long)]
    pub watch: bool,
    /// Only harvest these repo ids (default: everything in the watchlist).
    #[arg(long, num_args = 1..)]
    pub only: Vec<String>,
    /// Plan only: discover and print what would be harvested.
    #[arg(long)]
    pub dry_run: bool,
    /// Print what has been harvested so far and exit.
    #[arg(long, conflicts_with_all = ["watch", "dry_run"])]
    pub status: bool,
    /// Stop after this many watch cycles (0 = forever; for testing).
    #[arg(long, default_value_t = 0, hide = true)]
    pub max_cycles: u64,
    /// Accept this license id for every repo in this run even though it is
    /// not on the allowlist (repeatable). Prefer the per-repo
    /// `allow_license` in the watchlist for anything long lived.
    #[arg(long = "allow-license", value_name = "ID")]
    pub allow_license: Vec<String>,
}

impl Command {
    /// Name used in "not implemented yet" messages.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Doctor(_) => "doctor",
            Self::Catalog(CatalogCmd::Create { .. }) => "catalog create",
            Self::Catalog(CatalogCmd::Subscribe { .. }) => "catalog subscribe",
            Self::Catalog(CatalogCmd::Keygen { .. }) => "catalog keygen",
            Self::Catalog(CatalogCmd::Info { .. }) => "catalog info",
            Self::Catalog(CatalogCmd::AddMirror { .. }) => "catalog add-mirror",
            Self::Catalog(CatalogCmd::Export { .. }) => "catalog export",
            Self::Catalog(CatalogCmd::Import { .. }) => "catalog import",
            Self::Catalog(CatalogCmd::ResignHead) => "catalog resign-head",
            Self::Catalog(CatalogCmd::UpdateInfo { .. }) => "catalog update-info",
            Self::Catalog(CatalogCmd::Snapshot) => "catalog snapshot",
            Self::Catalog(CatalogCmd::Anchor) => "catalog anchor",
            Self::Catalog(CatalogCmd::ChainStatus { .. }) => "catalog chain-status",
            Self::Catalog(CatalogCmd::List) => "catalog list",
            Self::Catalog(CatalogCmd::Add { .. }) => "catalog add",
            Self::Catalog(CatalogCmd::Remove { .. }) => "catalog remove",
            Self::Publish { .. } => "publish",
            Self::Sync => "sync",
            Self::Harvest(_) => "harvest",
            Self::Pull(_) => "pull",
            Self::Seed { .. } => "seed",
            Self::Serve(_) => "serve",
            Self::Verify(_) => "verify",
            Self::ServeInference(_) => "serve-inference",
            Self::ReferenceLogprobs(_) => "reference-logprobs",
            Self::CheckEndpoint(_) => "check-endpoint",
            Self::ResponseKey(ResponseKeyCmd::Create { .. }) => "response-key create",
            Self::ResponseKey(ResponseKeyCmd::Show { .. }) => "response-key show",
            Self::ClaimServing(_) => "claim-serving",
            Self::SignResponses(_) => "sign-responses",
            Self::VerifyResponse(_) => "verify-response",
            Self::List { .. } => "list",
            Self::Show { .. } => "show",
            Self::Takedown { .. } => "takedown",
            Self::Jobs { .. } => "jobs",
            Self::Mirror(_) => "mirror",
            Self::Config(ConfigCmd::Show) => "config show",
            Self::Config(ConfigCmd::Path) => "config path",
            Self::Hf(HfCmd::Login) => "hf login",
            Self::Hf(HfCmd::Logout) => "hf logout",
            Self::Hf(HfCmd::Status) => "hf status",
            Self::Completions { .. } => "completions",
            Self::GenerateDocs { .. } => "generate-docs",
        }
    }
}

/// Write a completion script for `shell` to `out`.
///
/// # Errors
///
/// Returns an error if the script cannot be written, except for a broken
/// pipe (so `tensorseed completions bash | head` exits cleanly).
pub fn generate_completions(shell: CompletionShell, out: &mut dyn Write) -> anyhow::Result<()> {
    let mut cmd = Cli::command();
    let mut buf = Vec::new();
    clap_complete::generate::<clap_complete::Shell, _>(
        shell.into(),
        &mut cmd,
        "tensorseed",
        &mut buf,
    );
    match out.write_all(&buf) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(e).context("writing the completion script"),
    }
}

/// Write bash, zsh, fish, and PowerShell completions plus man pages under `dir`.
///
/// Completions go in `dir/completions/`. Man pages go in `dir/man/`, one page
/// per visible command (`tensorseed.1`, `tensorseed-pull.1`, and so on).
///
/// # Errors
///
/// Returns an error if a directory cannot be created or a file cannot be written.
pub fn write_shell_docs(dir: &Path) -> anyhow::Result<()> {
    let completions = dir.join("completions");
    let man = dir.join("man");
    std::fs::create_dir_all(&completions)
        .with_context(|| format!("creating {}", completions.display()))?;
    std::fs::create_dir_all(&man).with_context(|| format!("creating {}", man.display()))?;
    for (shell, name) in [
        (CompletionShell::Bash, "tensorseed.bash"),
        (CompletionShell::Zsh, "_tensorseed"),
        (CompletionShell::Fish, "tensorseed.fish"),
        (CompletionShell::PowerShell, "tensorseed.ps1"),
    ] {
        let mut buf = Vec::new();
        generate_completions(shell, &mut buf)?;
        let path = completions.join(name);
        std::fs::write(&path, buf).with_context(|| format!("writing {}", path.display()))?;
    }
    write_man_pages(Cli::command(), &man)
}

fn write_man_pages(mut cmd: clap::Command, dir: &Path) -> anyhow::Result<()> {
    cmd = cmd.disable_help_subcommand(true);
    cmd.build();
    fn walk(cmd: clap::Command, dir: &Path) -> anyhow::Result<()> {
        let parent = cmd
            .get_display_name()
            .unwrap_or_else(|| cmd.get_name())
            .to_owned();
        for sub in cmd.get_subcommands().filter(|s| !s.is_hide_set()).cloned() {
            let display = format!("{parent}-{}", sub.get_name());
            walk(sub.display_name(display), dir)?;
        }
        clap_mangen::Man::new(cmd)
            .generate_to(dir)
            .with_context(|| format!("writing man pages in {}", dir.display()))?;
        Ok(())
    }
    walk(cmd, dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_pull() {
        let cli = Cli::try_parse_from([
            "tensorseed",
            "pull",
            "org/name@main",
            "--from-hf",
            "--out",
            "/tmp/x",
        ])
        .unwrap();
        assert_eq!(cli.command.name(), "pull");
        assert!(
            Cli::try_parse_from([
                "tensorseed",
                "pull",
                "o/n",
                "--from-hf",
                "--manifest-file",
                "m.json"
            ])
            .is_err()
        );
    }

    #[test]
    fn pull_swarm_flags() {
        let cli = Cli::try_parse_from(["tensorseed", "pull", "org/name", "--no-swarm"]).unwrap();
        match cli.command {
            Command::Pull(a) => assert!(a.no_swarm && !a.swarm && !a.swarm_only),
            _ => panic!("expected pull"),
        }
        let cli = Cli::try_parse_from(["tensorseed", "pull", "org/name", "--swarm"]).unwrap();
        match cli.command {
            Command::Pull(a) => assert!(a.swarm && !a.no_swarm),
            _ => panic!("expected pull"),
        }
        // --from-hf with the default swarm flag is accepted; pull skips the
        // swarm silently because there is no torrent.
        assert!(
            Cli::try_parse_from(["tensorseed", "pull", "org/name", "--from-hf", "--swarm"]).is_ok()
        );
        assert!(
            Cli::try_parse_from(["tensorseed", "pull", "org/name", "--no-swarm", "--swarm"])
                .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "tensorseed",
                "pull",
                "org/name",
                "--from-hf",
                "--swarm-only"
            ])
            .is_err()
        );
        let cli = Cli::try_parse_from([
            "tensorseed",
            "pull",
            "org/name",
            "--include",
            "*.gguf",
            "--include",
            "tokenizer.json",
            "--exclude",
            "*.md",
        ])
        .unwrap();
        match cli.command {
            Command::Pull(a) => {
                assert_eq!(a.include, vec!["*.gguf", "tokenizer.json"]);
                assert_eq!(a.exclude, vec!["*.md"]);
            }
            _ => panic!("expected pull"),
        }
    }

    #[test]
    fn parses_and_writes_completions() {
        let cli = Cli::try_parse_from(["tensorseed", "completions", "bash"]).unwrap();
        assert_eq!(cli.command.name(), "completions");
        assert!(Cli::try_parse_from(["tensorseed", "completions", "elvish"]).is_err());
        let dir = tempfile::tempdir().unwrap();
        write_shell_docs(dir.path()).unwrap();
        let bash = std::fs::read_to_string(dir.path().join("completions/tensorseed.bash")).unwrap();
        assert!(
            bash.contains("tensorseed"),
            "bash completions:
{bash}"
        );
        assert!(dir.path().join("completions/_tensorseed").is_file());
        assert!(dir.path().join("completions/tensorseed.fish").is_file());
        assert!(dir.path().join("completions/tensorseed.ps1").is_file());
        let man = std::fs::read_to_string(dir.path().join("man/tensorseed.1")).unwrap();
        assert!(
            man.contains("tensorseed") || man.contains("TENSORSEED"),
            "man page:
{man}"
        );
        assert!(dir.path().join("man/tensorseed-pull.1").is_file());
        assert!(dir.path().join("man/tensorseed-hf-login.1").is_file());
        assert!(!dir.path().join("man/tensorseed-generate-docs.1").exists());
        let hidden =
            Cli::try_parse_from(["tensorseed", "generate-docs", dir.path().to_str().unwrap()])
                .unwrap();
        assert_eq!(hidden.command.name(), "generate-docs");
    }

    #[test]
    fn parses_hf_subcommands() {
        for (args, name) in [
            (["tensorseed", "hf", "login"], "hf login"),
            (["tensorseed", "hf", "logout"], "hf logout"),
            (["tensorseed", "hf", "status"], "hf status"),
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert_eq!(cli.command.name(), name);
        }
    }

    #[test]
    fn parses_verify_modes() {
        let cli = Cli::try_parse_from(["tensorseed", "verify", "--hf", "org/name", "--dir", "d"])
            .unwrap();
        match cli.command {
            Command::Verify(v) => {
                assert!(v.hf);
                assert_eq!(v.target, "org/name");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn parses_catalog_export_import() {
        let cli = Cli::try_parse_from([
            "tensorseed",
            "catalog",
            "export",
            "--out",
            "bundle.json",
            "--backend",
            "chia",
        ])
        .unwrap();
        assert_eq!(cli.command.name(), "catalog export");
        let cli = Cli::try_parse_from([
            "tensorseed",
            "catalog",
            "import",
            "bundle.json",
            "--backend",
            "file",
            "--dry-run",
        ])
        .unwrap();
        match cli.command {
            Command::Catalog(CatalogCmd::Import {
                backend, dry_run, ..
            }) => {
                assert_eq!(backend, Some(BackendArg::File));
                assert!(dry_run);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn parses_publish_dataset() {
        let cli = Cli::try_parse_from([
            "tensorseed",
            "-v",
            "publish",
            "org/data",
            "--type",
            "dataset",
        ])
        .unwrap();
        match cli.command {
            Command::Publish { repo_type, .. } => assert_eq!(repo_type, RepoTypeArg::Dataset),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(cli.verbose, 1);
    }
}
