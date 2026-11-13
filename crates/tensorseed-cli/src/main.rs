// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! `tensorseed` binary. See `docs/SPEC.md` and <https://tensorseed.dev>.

mod catalog_cmd;
mod chain;
mod cli;
mod consumer;
mod harvest;
mod hf_auth;
mod keys;
mod mirrors;
mod serve;
mod torrents;

use std::process::ExitCode;

// Library modules, reachable as `crate::config` and so on from the CLI.
use tensorseed_app::{backend, config, follow};

use clap::Parser;
use tracing_subscriber::EnvFilter;

use cli::{Cli, Command, ConfigCmd, LogFormat, generate_completions, write_shell_docs};

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
        .with_writer(std::io::stderr)
        // No color codes in log files, journald, or pipes.
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()));
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
        Command::Completions { shell } => {
            generate_completions(*shell, &mut std::io::stdout())?;
            Ok(ExitCode::SUCCESS)
        }
        Command::GenerateDocs { dir } => {
            write_shell_docs(dir)?;
            Ok(ExitCode::SUCCESS)
        }
        Command::Config(ConfigCmd::Path) => {
            println!("{}", path.display());
            Ok(ExitCode::SUCCESS)
        }
        Command::Config(ConfigCmd::Show) => {
            println!("{}", serde_json::to_string_pretty(&load()?)?);
            Ok(ExitCode::SUCCESS)
        }
        Command::Hf(cmd) => hf_auth::run(&load()?, cmd).await,
        Command::Catalog(cmd) => catalog_cmd::run(cmd, load, &path).await,
        Command::Harvest(args) => harvest::run(&load()?, args).await,
        Command::Publish {
            repo_id,
            repo_type,
            revision,
            allow_license,
        } => {
            harvest::publish(
                &load()?,
                repo_id,
                (*repo_type).into(),
                revision.as_deref(),
                allow_license,
            )
            .await
        }
        Command::Pull(args) => consumer::pull(&load()?, args).await,
        Command::Verify(args) => consumer::verify(&load()?, args).await,
        Command::List {
            filter,
            no_wait,
            catalog,
        } => consumer::list(&load()?, filter.as_deref(), *no_wait, catalog.as_deref()).await,
        Command::Show {
            repo,
            repo_type,
            revision,
            no_wait,
            catalog,
        } => {
            consumer::show(
                &load()?,
                repo,
                (*repo_type).into(),
                revision.as_deref(),
                *no_wait,
                catalog.as_deref(),
            )
            .await
        }
        Command::Mirror(cmd) => mirrors::run(&load()?, cmd.clone()).await,
        Command::Seed {
            dir,
            duration,
            serve_catalog,
        } => torrents::seed(&load()?, dir, duration.as_deref(), *serve_catalog).await,
        Command::Serve(args) => serve::run(load()?, args).await,
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
