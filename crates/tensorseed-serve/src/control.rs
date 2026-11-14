// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! The local control endpoint of `tensorseed serve`: Desktop on the same
//! machine attaches to a running daemon to show its status and pause or
//! resume seeding. It listens on a loopback address only, and every
//! request needs the bearer token the daemon wrote to its owner only
//! `seeder.json`.
//!
//! * `GET /control/status`: the health document ([`crate::health`]).
//! * `POST /control/action` with `{"action": "pause" | "resume", "name":
//!   "<revision>" | null}`: pause or resume one revision or all of them;
//!   answers `{"changed": N}`.

use std::future::Future;
use std::net::SocketAddr;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot};

use crate::health::{SharedStatus, snapshot};

/// What a control request asks the daemon to do.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Stop uploading (one revision or all).
    Pause,
    /// Seed again.
    Resume,
}

#[derive(Debug, Deserialize)]
struct ActionBody {
    action: Action,
    #[serde(default)]
    name: Option<String>,
}

/// A control request for the daemon loop, answered on `reply` with how
/// many revisions changed or why it failed.
#[derive(Debug)]
pub struct Command {
    /// The action.
    pub action: Action,
    /// One revision, or all when `None`.
    pub name: Option<String>,
    /// The answer.
    pub reply: oneshot::Sender<Result<usize, String>>,
}

#[derive(Clone)]
struct Ctx {
    status: SharedStatus,
    token: std::sync::Arc<str>,
    tx: mpsc::Sender<Command>,
}

/// Compare a presented token with the expected one in constant time.
pub fn token_matches(expected: &str, presented: &str) -> bool {
    let (a, b) = (expected.as_bytes(), presented.as_bytes());
    if a.len() != b.len() || a.is_empty() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn json(code: StatusCode, v: &impl serde::Serialize) -> Response {
    let body = serde_json::to_vec(v).unwrap_or_else(|_| b"{}".to_vec());
    (code, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}

fn error(code: StatusCode, msg: &str) -> Response {
    json(code, &serde_json::json!({ "error": msg }))
}

async fn auth(State(c): State<Ctx>, req: Request, next: Next) -> Response {
    let presented = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    if !token_matches(&c.token, presented) {
        return error(StatusCode::UNAUTHORIZED, "missing or wrong control token");
    }
    next.run(req).await
}

async fn status(State(c): State<Ctx>) -> Response {
    let (_, st) = snapshot(&c.status);
    json(StatusCode::OK, &st)
}

async fn action(State(c): State<Ctx>, body: bytes::Bytes) -> Response {
    let body: ActionBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => return error(StatusCode::BAD_REQUEST, &format!("bad request: {e}")),
    };
    let (reply, rx) = oneshot::channel();
    let cmd = Command {
        action: body.action,
        name: body.name,
        reply,
    };
    if c.tx.send(cmd).await.is_err() {
        return error(StatusCode::SERVICE_UNAVAILABLE, "the daemon is stopping");
    }
    match rx.await {
        Ok(Ok(n)) => json(StatusCode::OK, &serde_json::json!({ "changed": n })),
        Ok(Err(e)) => error(StatusCode::BAD_REQUEST, &e),
        Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "the daemon is stopping"),
    }
}

/// The control routes, all behind the token check.
pub fn router(status: SharedStatus, token: &str, tx: mpsc::Sender<Command>) -> Router {
    let c = Ctx {
        status,
        token: token.into(),
        tx,
    };
    Router::new()
        .route("/control/status", get(self::status))
        .route("/control/action", post(self::action))
        .layer(middleware::from_fn_with_state(c.clone(), auth))
        .with_state(c)
}

/// Bind the control endpoint on `127.0.0.1` (a free port).
///
/// # Errors
///
/// Fails when the loopback address cannot be bound.
pub async fn bind() -> std::io::Result<(tokio::net::TcpListener, SocketAddr)> {
    let l = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
    let a = l.local_addr()?;
    Ok((l, a))
}

/// Serve the control routes until `shutdown` resolves.
///
/// # Errors
///
/// Fails when the server fails.
pub async fn serve(
    listener: tokio::net::TcpListener,
    router: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::health::Status;
    use std::sync::{Arc, RwLock};

    #[test]
    fn tokens_compare() {
        assert!(token_matches("abc", "abc"));
        assert!(!token_matches("abc", "abd"));
        assert!(!token_matches("abc", "ab"));
        assert!(!token_matches("", ""));
    }

    #[tokio::test]
    async fn needs_the_token_and_forwards_actions() {
        let st: SharedStatus = Arc::new(RwLock::new(Status::new("0.1.0", 1)));
        st.write().unwrap().catalog.revision = 7;
        let (tx, mut rx) = mpsc::channel::<Command>(4);
        tokio::spawn(async move {
            while let Some(c) = rx.recv().await {
                let r = match (&c.action, c.name.as_deref()) {
                    (Action::Pause, None) => Ok(3),
                    (Action::Resume, Some("a")) => Ok(1),
                    _ => Err("no such revision".to_owned()),
                };
                let _ = c.reply.send(r);
            }
        });
        let (l, addr) = bind().await.unwrap();
        assert!(addr.ip().is_loopback());
        let (stop, stopped) = oneshot::channel::<()>();
        tokio::spawn(serve(l, router(st, "secret", tx), async {
            let _ = stopped.await;
        }));
        let http = reqwest::Client::new();
        let base = format!("http://{addr}/control");

        let r = http.get(format!("{base}/status")).send().await.unwrap();
        assert_eq!(r.status(), 401);
        let r = http
            .get(format!("{base}/status"))
            .bearer_auth("wrong")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401);
        let r = http
            .post(format!("{base}/action"))
            .json(&serde_json::json!({"action": "pause"}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401, "actions need the token too");

        let v: serde_json::Value = http
            .get(format!("{base}/status"))
            .bearer_auth("secret")
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(v["catalog"]["revision"], 7);

        let v: serde_json::Value = http
            .post(format!("{base}/action"))
            .bearer_auth("secret")
            .json(&serde_json::json!({"action": "pause", "name": null}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(v["changed"], 3);
        let r = http
            .post(format!("{base}/action"))
            .bearer_auth("secret")
            .json(&serde_json::json!({"action": "resume", "name": "b"}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 400);
        let v: serde_json::Value = r.json().await.unwrap();
        assert_eq!(v["error"], "no such revision");
        let _ = stop.send(());
    }
}
