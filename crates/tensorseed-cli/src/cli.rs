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
    },
    /// Publish every target in the config.
    Sync,
    /// Download and verify a mirrored revision.
    Pull(PullArgs),
    /// Run the seeding daemon.
    Seed {
        /// Also subscribe to and serve the catalog.
        #[arg(long)]
        serve_catalog: bool,
    },
    /// Re-hash a local copy against its manifest.
    Verify {
        /// Directory holding the local copy.
        dir: PathBuf,
        /// Catalog id.
        #[arg(long)]
        catalog: String,
        /// Hugging Face repo id (org/name).
        repo_id: String,
        /// Commit SHA (default: latest).
        #[arg(long)]
        revision: Option<String>,
    },
    /// List catalog entries.
    List {
        /// Catalog id (default: from config).
        #[arg(long)]
        catalog: Option<String>,
        /// Filter by repo id substring or glob.
        #[arg(long)]
        filter: Option<String>,
    },
    /// Print a manifest.
    Show {
        /// Hugging Face repo id (org/name).
        repo_id: String,
        /// Commit SHA (default: latest).
        #[arg(long)]
        revision: Option<String>,
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
        /// Retry the job with this id.
        #[arg(long)]
        retry: Option<String>,
        /// Clear the job with this id.
        #[arg(long)]
        clear: Option<String>,
    },
    /// Inspect configuration.
    #[command(subcommand)]
    Config(ConfigCmd),
}

/// `catalog` subcommands.
#[derive(Debug, Subcommand)]
pub enum CatalogCmd {
    /// Create a new catalog through the configured backend.
    Create,
    /// Show catalog info, generation, root, and mirrors.
    Info {
        /// Catalog id (default: from config).
        #[arg(long)]
        store: Option<String>,
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
    /// Hugging Face repo id (org/name).
    pub repo_id: String,
    /// Catalog id to read from.
    #[arg(long)]
    pub catalog: Option<String>,
    /// Commit SHA (default: latest).
    #[arg(long)]
    pub revision: Option<String>,
    /// Output directory.
    #[arg(long)]
    pub out: Option<PathBuf>,
    /// Keep seeding for this long after download (for example 2h).
    #[arg(long)]
    pub seed_after: Option<String>,
    /// Use a local manifest file instead of the catalog (no chain verification).
    #[arg(long, conflicts_with = "manifest_url")]
    pub manifest_file: Option<PathBuf>,
    /// Use a manifest URL instead of the catalog (no chain verification).
    #[arg(long)]
    pub manifest_url: Option<String>,
    /// Use the current local catalog copy without waiting for sync.
    #[arg(long)]
    pub no_wait: bool,
    /// Download even if the repo is tombstoned.
    #[arg(long)]
    pub ignore_tombstone: bool,
}

impl Command {
    /// Name used in "not implemented yet" messages.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Doctor => "doctor",
            Self::Catalog(CatalogCmd::Create) => "catalog create",
            Self::Catalog(CatalogCmd::Info { .. }) => "catalog info",
            Self::Catalog(CatalogCmd::AddMirror { .. }) => "catalog add-mirror",
            Self::Publish { .. } => "publish",
            Self::Sync => "sync",
            Self::Pull(_) => "pull",
            Self::Seed { .. } => "seed",
            Self::Verify { .. } => "verify",
            Self::List { .. } => "list",
            Self::Show { .. } => "show",
            Self::Takedown { .. } => "takedown",
            Self::Jobs { .. } => "jobs",
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
            "org/name",
            "--catalog",
            "abc",
            "--out",
            "/tmp/x",
        ])
        .unwrap();
        assert_eq!(cli.command.name(), "pull");
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
