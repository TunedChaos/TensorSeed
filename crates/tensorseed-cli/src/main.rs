// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! `tensorseed` binary. See `docs/SPEC.md` and <https://tensorseed.dev>.

mod backend;
mod catalog_cmd;
mod cli;
mod config;
mod consumer;
mod harvest;
mod keys;

use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::EnvFilter;

use cli::{Cli, Command, ConfigCmd, LogFormat};

/// Exit code used for commands that are not implemented yet.
const EXIT_NOT_IMPLEMENTED: u8 = 2;

fn init_tracing(format: LogFormat, verbose: u8) {
    let default = match verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr);
    match format {
        LogFormat::Pretty => builder.init(),
        LogFormat::Json => builder.json().init(),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.log_format, cli.verbose);
    match run(cli).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    let path = cli.config.clone().unwrap_or_else(config::default_path);
    let load = || config::load(&path);
    match &cli.command {
        Command::Config(ConfigCmd::Path) => {
            println!("{}", path.display());
            Ok(ExitCode::SUCCESS)
        }
        Command::Config(ConfigCmd::Show) => {
            println!("{}", serde_json::to_string_pretty(&load()?)?);
            Ok(ExitCode::SUCCESS)
        }
        Command::Catalog(cmd) => catalog_cmd::run(cmd, load).await,
        Command::Harvest(args) => harvest::run(&load()?, args).await,
        Command::Pull(args) => consumer::pull(&load()?, args).await,
        Command::Verify(args) => consumer::verify(&load()?, args).await,
        Command::List { filter, no_wait } => {
            consumer::list(&load()?, filter.as_deref(), *no_wait).await
        }
        Command::Show {
            repo,
            repo_type,
            revision,
            no_wait,
        } => {
            consumer::show(
                &load()?,
                repo,
                (*repo_type).into(),
                revision.as_deref(),
                *no_wait,
            )
            .await
        }
        Command::Jobs { show, clear } => {
            consumer::jobs(&load()?, show.as_deref(), clear.as_deref())
        }
        other => {
            tracing::debug!(command = other.name(), ?other, "parsed command");
            eprintln!(
                "tensorseed {}: not implemented yet (pre-alpha). See docs/MILESTONES.md.",
                other.name()
            );
            Ok(ExitCode::from(EXIT_NOT_IMPLEMENTED))
        }
    }
}
