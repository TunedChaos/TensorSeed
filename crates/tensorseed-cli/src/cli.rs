// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Command line definition (spec section 9).

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use tensorseed_core::keys::RepoType;

/// Tensorseed: verifiable, chain anchored mirrors of open Hugging Face models.
#[derive(Debug, Parser)]
#[command(name = "tensorseed", version, about, long_about = None)]
pub struct Cli {
    /// Path to the config file (default: ~/.config/tensorseed/tensorseed.toml).
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

/// Top level commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Check config, catalog backend, credentials, ports, and disk space.
    Doctor,
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
    },
    /// Re-hash a local copy against a signed manifest or the Hub's hashes.
    Verify(VerifyArgs),
    /// List catalog entries.
    List {
        /// Filter by repo id substring.
        #[arg(long)]
        filter: Option<String>,
        /// Use the local catalog copy without syncing.
        #[arg(long)]
        no_wait: bool,
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
    Create,
    /// Generate a publisher key pair (minisign format).
    Keygen {
        /// Directory for `<name>.key` and `<name>.pub` (default ~/.config/tensorseed).
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
    /// Write a full snapshot now and point head.json at it (file backend).
    Snapshot,
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
    /// Files downloaded at the same time (default: general.max_concurrent_downloads).
    #[arg(long)]
    pub jobs: Option<usize>,
    /// Also download pickle based files (.bin, .pt, .ckpt, ...), which can run code when loaded.
    #[arg(long)]
    pub allow_pickle: bool,
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
    /// Download even if the repo is tombstoned.
    #[arg(long)]
    pub ignore_tombstone: bool,
    /// Download from the BitTorrent swarm first (up to
    /// torrent.swarm_timeout_secs), then fill gaps over HTTP. Every file is
    /// checked against the manifest either way.
    #[arg(long)]
    pub swarm: bool,
    /// Extra peer to dial (`host:port`), repeatable; implies nothing without
    /// --swarm.
    #[arg(long)]
    pub peer: Vec<String>,
    /// After a verified download, seed it for this long (30s, 10m, 2h, 1d,
    /// or forever).
    #[arg(long)]
    pub seed_after: Option<String>,
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
            Self::Doctor => "doctor",
            Self::Catalog(CatalogCmd::Create) => "catalog create",
            Self::Catalog(CatalogCmd::Keygen { .. }) => "catalog keygen",
            Self::Catalog(CatalogCmd::Info { .. }) => "catalog info",
            Self::Catalog(CatalogCmd::AddMirror { .. }) => "catalog add-mirror",
            Self::Catalog(CatalogCmd::Export { .. }) => "catalog export",
            Self::Catalog(CatalogCmd::Import { .. }) => "catalog import",
            Self::Catalog(CatalogCmd::ResignHead) => "catalog resign-head",
            Self::Catalog(CatalogCmd::Snapshot) => "catalog snapshot",
            Self::Publish { .. } => "publish",
            Self::Sync => "sync",
            Self::Harvest(_) => "harvest",
            Self::Pull(_) => "pull",
            Self::Seed { .. } => "seed",
            Self::Verify(_) => "verify",
            Self::List { .. } => "list",
            Self::Show { .. } => "show",
            Self::Takedown { .. } => "takedown",
            Self::Jobs { .. } => "jobs",
            Self::Mirror(_) => "mirror",
            Self::Config(ConfigCmd::Show) => "config show",
            Self::Config(ConfigCmd::Path) => "config path",
        }
    }
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
