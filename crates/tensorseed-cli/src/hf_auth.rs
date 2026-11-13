// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! `tensorseed hf login`, `logout`, and `status`.

use std::io::{IsTerminal, Read};
use std::process::ExitCode;

use anyhow::Context;

use crate::cli::HfCmd;
use crate::config::{Config, remove_login_token, write_login_token};

/// Run a `tensorseed hf` subcommand.
pub async fn run(cfg: &Config, cmd: &HfCmd) -> anyhow::Result<ExitCode> {
    match cmd {
        HfCmd::Login => login(),
        HfCmd::Logout => logout(),
        HfCmd::Status => status(cfg).await,
    }
}

fn login() -> anyhow::Result<ExitCode> {
    let token = read_token()?;
    let path = write_login_token(&token)?;
    println!("Saved the Hugging Face token to {}", path.display());
    println!("The file is readable only by your user. The token is never printed.");
    Ok(ExitCode::SUCCESS)
}

fn logout() -> anyhow::Result<ExitCode> {
    if remove_login_token()? {
        println!("Removed the token saved by tensorseed hf login.");
    } else {
        println!("No token file from tensorseed hf login was present.");
    }
    println!("Environment variables and the Hugging Face CLI token were left unchanged.");
    Ok(ExitCode::SUCCESS)
}

async fn status(cfg: &Config) -> anyhow::Result<ExitCode> {
    let Some((token, source)) = cfg.huggingface.resolve_token_source()? else {
        println!("No Hugging Face token is configured.");
        println!(
            "Set {}, run `tensorseed hf login`, or use `hf auth login`.",
            cfg.huggingface.token_env
        );
        return Ok(ExitCode::from(1));
    };
    let name = cfg
        .huggingface
        .client()
        .context("building the Hugging Face client")?
        .whoami()
        .await
        .context("calling Hugging Face whoami-v2")?;
    let _ = token; // never printed
    println!("Hugging Face: logged in as {name}");
    println!("Token source: {}", source.label());
    Ok(ExitCode::SUCCESS)
}

fn read_token() -> anyhow::Result<String> {
    let mut stdin = std::io::stdin();
    let raw = if stdin.is_terminal() {
        rpassword::prompt_password("Hugging Face token: ").context("reading the token")?
    } else {
        let mut buf = String::new();
        stdin
            .read_to_string(&mut buf)
            .context("reading the token from stdin")?;
        buf
    };
    let token = raw.trim();
    if token.is_empty() {
        anyhow::bail!("the Hugging Face token is empty");
    }
    Ok(token.to_string())
}
