// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos LLC

//! `tensorseed` binary. See `docs/SPEC.md` and <https://tensorseed.dev>.

mod cli;
mod config;

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
    match &cli.command {
        Command::Config(ConfigCmd::Path) => {
            println!("{}", path.display());
            Ok(ExitCode::SUCCESS)
        }
        Command::Config(ConfigCmd::Show) => {
            let cfg = config::load(&path)?;
            println!("{}", serde_json::to_string_pretty(&cfg)?);
            Ok(ExitCode::SUCCESS)
        }
        other => {
            tracing::debug!(command = other.name(), ?other, "parsed command");
            eprintln!(
                "tensorseed {}: not implemented yet (pre-alpha, milestone M0). See docs/MILESTONES.md.",
                other.name()
            );
            Ok(ExitCode::from(EXIT_NOT_IMPLEMENTED))
        }
    }
}
