// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Hugging Face login state: save, remove, and check the token used for Hub
//! reads. The token itself is never returned to callers.

use std::path::PathBuf;

use anyhow::Context;

use crate::config::{Config, TokenSource, remove_login_token, write_login_token};

/// Who the configured token belongs to, and where it came from.
#[derive(Debug, Clone)]
pub struct HfStatus {
    /// Hub user name from `whoami-v2`.
    pub user: String,
    /// Where the token was found (env, token file, login file, or the
    /// Hugging Face CLI token).
    pub source: TokenSource,
}

/// Save a pasted token for later Hub reads (the `hf login` file).
///
/// # Errors
///
/// Fails when the token is empty after trimming or the file cannot be
/// written with user only permissions.
pub fn login(raw: &str) -> anyhow::Result<PathBuf> {
    let token = raw.trim();
    if token.is_empty() {
        anyhow::bail!("the Hugging Face token is empty");
    }
    write_login_token(token)
}

/// Remove the saved login token. `false` when there was none. Environment
/// variables and the Hugging Face CLI token are left unchanged.
///
/// # Errors
///
/// Fails when the file exists but cannot be removed.
pub fn logout() -> anyhow::Result<bool> {
    remove_login_token()
}

/// Check the configured token against the Hub. `None` when no token is
/// configured.
///
/// # Errors
///
/// Fails when a token file is unreadable or unsafe, the client cannot be
/// built, or the Hub rejects the token.
pub async fn status(cfg: &Config) -> anyhow::Result<Option<HfStatus>> {
    let Some((_token, source)) = cfg.huggingface.resolve_token_source()? else {
        return Ok(None);
    };
    let user = cfg
        .huggingface
        .client()
        .context("building the Hugging Face client")?
        .whoami()
        .await
        .context("calling Hugging Face whoami-v2")?;
    Ok(Some(HfStatus { user, source }))
}
