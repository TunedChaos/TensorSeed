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

/// Why [`check`] did not accept a token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckError {
    /// The token is empty after trimming.
    Empty,
    /// The Hub answered 401 or 403 for this token.
    Rejected,
    /// The Hub could not be reached or answered something unexpected.
    Unreachable(String),
}

impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("the Hugging Face token is empty"),
            Self::Rejected => f.write_str("Hugging Face rejected this token"),
            Self::Unreachable(e) => write!(f, "could not check the token with Hugging Face: {e}"),
        }
    }
}

impl std::error::Error for CheckError {}

/// Check a pasted token against the Hub (`whoami-v2`) without saving it,
/// and return the user it belongs to. Error text never contains the token.
///
/// # Errors
///
/// [`CheckError`] for an empty or rejected token, or when the Hub cannot be
/// reached.
pub async fn check(cfg: &Config, raw: &str) -> Result<String, CheckError> {
    use tensorseed_hf::{HfClient, HfError};
    let token = raw.trim();
    if token.is_empty() {
        return Err(CheckError::Empty);
    }
    let client = HfClient::new(&cfg.huggingface.endpoint, Some(token.to_owned()))
        .map_err(|e| CheckError::Unreachable(e.to_string()))?
        .with_retry(1, std::time::Duration::from_millis(500));
    match client.whoami().await {
        Ok(user) => Ok(user),
        Err(
            HfError::NotAccessible {
                status: 401 | 403, ..
            }
            | HfError::Status {
                status: 401 | 403, ..
            },
        ) => Err(CheckError::Rejected),
        // reqwest errors carry the URL, never request headers.
        Err(e) => Err(CheckError::Unreachable(e.to_string())),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A one route Hub: whoami-v2 answers for `hf_good` and 401 otherwise.
    async fn hub() -> String {
        use axum::http::{HeaderMap, StatusCode};
        async fn whoami(h: HeaderMap) -> (StatusCode, String) {
            match h.get("authorization").and_then(|v| v.to_str().ok()) {
                Some("Bearer hf_good") => (StatusCode::OK, r#"{"name":"someone"}"#.into()),
                _ => (StatusCode::UNAUTHORIZED, r#"{"error":"bad"}"#.into()),
            }
        }
        let app = axum::Router::new().route("/api/whoami-v2", axum::routing::get(whoami));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn check_accepts_good_and_rejects_bad_tokens() {
        let mut cfg = Config::default();
        cfg.huggingface.endpoint = hub().await;
        assert_eq!(check(&cfg, "  hf_good\n").await.unwrap(), "someone");
        assert_eq!(check(&cfg, "hf_bad").await, Err(CheckError::Rejected));
        assert_eq!(check(&cfg, "  ").await, Err(CheckError::Empty));
        cfg.huggingface.endpoint = "http://127.0.0.1:9".into();
        let e = check(&cfg, "hf_secret_value").await.unwrap_err();
        assert!(matches!(e, CheckError::Unreachable(_)), "{e}");
        assert!(!e.to_string().contains("hf_secret_value"), "{e}");
    }
}
