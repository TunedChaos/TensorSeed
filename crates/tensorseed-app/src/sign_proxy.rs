// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! The seller-side signing proxy (docs/plans/verified-serving.md, 5.1).
//!
//! It sits on loopback between the TLS reverse proxy and an OpenAI
//! compatible runtime. Requests and buyer headers are forwarded unchanged
//! (hop-by-hop headers excepted). Every non-streaming JSON response is
//! signed with the response key and gets the `X-Tensorseed-*` headers.
//! A streaming (SSE) response is passed through as it arrives and closed
//! with a signed SSE comment line over the hash of every streamed byte;
//! its headers say `X-Tensorseed-Signature-Mode: sse-trailer`. Other
//! responses (HTML, plain text) are passed through unsigned, with the
//! request id echoed. `Accept-Encoding` is not forwarded, so the signed
//! bytes are the identity-encoded body. The claim is served at
//! [`WELL_KNOWN_CLAIM`](crate::signing::WELL_KNOWN_CLAIM).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use sha2::{Digest, Sha256};

use crate::signing::{
    H_CLAIM, H_MODEL, H_REQUEST_ID, H_SIGNATURE_MODE, MODE_SSE_TRAILER, ResponseKey,
    WELL_KNOWN_CLAIM, claim_from_bytes, load_claim, mint_request_id, sse_trailer, valid_request_id,
};

/// Largest request or non-streaming response body (bytes).
pub const MAX_BODY: usize = 64 * 1024 * 1024;

const HOP_BY_HOP: [&str; 10] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "host",
    "content-length",
];

struct ProxyState {
    http: reqwest::Client,
    upstream: String,
    key: ResponseKey,
    claim_sha256: String,
    served_model_id: String,
    claim_path: PathBuf,
}

/// A bound signing proxy.
pub struct SignProxy {
    listener: tokio::net::TcpListener,
    state: Arc<ProxyState>,
}

/// Check that `key` is the response key named in the claim at `claim_path`.
///
/// # Errors
///
/// Fails when the claim does not verify or names another key.
pub fn check_claim_key(claim_path: &Path, key: &ResponseKey) -> anyhow::Result<(String, String)> {
    let (claim, _) = load_claim(claim_path)?;
    if claim.response_key.pubkey != key.pubkey_hex() {
        bail!(
            "the claim at {} names response key {}, but the loaded key is {}; write a new claim for this key",
            claim_path.display(),
            claim.response_key.pubkey,
            key.pubkey_hex()
        );
    }
    Ok((claim.claim_sha256, claim.served_model_id))
}

impl SignProxy {
    /// Bind `listen`, forwarding to `upstream` (a root URL such as
    /// `http://127.0.0.1:8080`, without `/v1`).
    ///
    /// # Errors
    ///
    /// Fails when the claim does not match the key, the upstream URL is not
    /// http(s), or the address cannot be bound.
    pub async fn bind(
        listen: SocketAddr,
        upstream: &str,
        claim_path: &Path,
        key: ResponseKey,
    ) -> anyhow::Result<Self> {
        let (claim_sha256, served_model_id) = check_claim_key(claim_path, &key)?;
        let up = reqwest::Url::parse(upstream).with_context(|| format!("upstream {upstream:?}"))?;
        if !matches!(up.scheme(), "http" | "https") {
            bail!("upstream must be an http or https URL");
        }
        let upstream = upstream.trim_end_matches('/').to_string();
        if upstream.ends_with("/v1") {
            bail!("give the upstream root (for example http://127.0.0.1:8080), not the /v1 base");
        }
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(600))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let listener = tokio::net::TcpListener::bind(listen)
            .await
            .with_context(|| format!("cannot listen on {listen}"))?;
        Ok(Self {
            listener,
            state: Arc::new(ProxyState {
                http,
                upstream,
                key,
                claim_sha256,
                served_model_id,
                claim_path: claim_path.to_path_buf(),
            }),
        })
    }

    /// The bound address.
    ///
    /// # Errors
    ///
    /// Fails if the socket has no local address.
    pub fn local_addr(&self) -> anyhow::Result<SocketAddr> {
        Ok(self.listener.local_addr()?)
    }

    /// `claim_sha256` the proxy signs with.
    #[must_use]
    pub fn claim_sha256(&self) -> &str {
        &self.state.claim_sha256
    }

    /// Serve until `shutdown` completes.
    ///
    /// # Errors
    ///
    /// Fails when the server stops with an I/O error.
    pub async fn run(
        self,
        shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> anyhow::Result<()> {
        let app = axum::Router::new()
            .route(WELL_KNOWN_CLAIM, axum::routing::get(claim_handler))
            .fallback(proxy_handler)
            .with_state(self.state);
        axum::serve(self.listener, app)
            .with_graceful_shutdown(shutdown)
            .await?;
        Ok(())
    }
}

fn json_error(status: StatusCode, msg: &str) -> Response {
    let body = serde_json::json!({"error": {"message": msg, "type": "tensorseed_proxy"}});
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

async fn claim_handler(State(st): State<Arc<ProxyState>>) -> Response {
    let Ok(raw) = tokio::fs::read(&st.claim_path).await else {
        return json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "claim file is not readable",
        );
    };
    match claim_from_bytes(&raw) {
        Ok((c, _)) if c.claim_sha256 == st.claim_sha256 => {
            ([(header::CONTENT_TYPE, "application/json")], raw).into_response()
        }
        Ok(_) => json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "the claim file changed since the proxy started; restart it",
        ),
        Err(_) => json_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "claim file does not verify",
        ),
    }
}

fn copy_headers(from: &HeaderMap, to: &mut HeaderMap) {
    for (k, v) in from {
        if !HOP_BY_HOP.contains(&k.as_str()) {
            to.append(k.clone(), v.clone());
        }
    }
}

fn set(h: &mut HeaderMap, name: &'static str, value: &str) {
    if let Ok(v) = HeaderValue::from_str(value) {
        h.insert(HeaderName::from_static(name), v);
    }
}

async fn proxy_handler(State(st): State<Arc<ProxyState>>, req: Request) -> Response {
    let request_id = match req.headers().get(H_REQUEST_ID) {
        Some(v) => match v.to_str() {
            Ok(s) if valid_request_id(s) => s.to_string(),
            _ => {
                return json_error(
                    StatusCode::BAD_REQUEST,
                    "X-Tensorseed-Request-Id must be 1 to 128 of A-Z a-z 0-9 . _ ~ : -",
                );
            }
        },
        None => match mint_request_id() {
            Ok(id) => id,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "random source failed"),
        },
    };
    let (parts, body) = req.into_parts();
    let path = parts
        .uri
        .path_and_query()
        .map_or_else(|| "/".to_string(), |p| p.as_str().to_string());
    let body = match axum::body::to_bytes(body, MAX_BODY).await {
        Ok(b) => b,
        Err(_) => return json_error(StatusCode::PAYLOAD_TOO_LARGE, "request body is too large"),
    };
    let mut fwd = HeaderMap::new();
    copy_headers(&parts.headers, &mut fwd);
    // The signature covers the identity-encoded body; ask for it.
    fwd.remove(header::ACCEPT_ENCODING);
    fwd.insert(
        HeaderName::from_static(H_REQUEST_ID),
        HeaderValue::from_str(&request_id).unwrap_or_else(|_| HeaderValue::from_static("invalid")),
    );
    let url = format!("{}{path}", st.upstream);
    let up = st
        .http
        .request(parts.method.clone(), &url)
        .headers(fwd)
        .body(body)
        .send()
        .await;
    let up = match up {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(request_id = %request_id, "upstream request failed: {e}");
            return json_error(StatusCode::BAD_GATEWAY, "upstream is unavailable");
        }
    };
    let status = up.status();
    let mut headers = HeaderMap::new();
    copy_headers(up.headers(), &mut headers);
    let ctype = up
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    tracing::debug!(request_id = %request_id, %status, path = %parts.uri.path(), "proxied");
    set(&mut headers, H_REQUEST_ID, &request_id);

    if ctype.starts_with("text/event-stream") {
        set(&mut headers, H_CLAIM, &st.claim_sha256);
        set(&mut headers, H_MODEL, &st.served_model_id);
        set(&mut headers, H_SIGNATURE_MODE, MODE_SSE_TRAILER);
        let stream = futures_util::stream::unfold(
            Some((up, Sha256::new(), st.clone(), request_id)),
            |s| async move {
                let (mut up, mut h, st, rid) = s?;
                match up.chunk().await {
                    Ok(Some(c)) => {
                        h.update(&c);
                        Some((Ok::<Bytes, std::io::Error>(c), Some((up, h, st, rid))))
                    }
                    Ok(None) => {
                        let digest = hex::encode(h.finalize());
                        let f = st.key.sign_digest(
                            &st.claim_sha256,
                            &rid,
                            &st.served_model_id,
                            &tensorseed_core::time::now_rfc3339(),
                            &digest,
                        );
                        Some((Ok(Bytes::from(sse_trailer(&f))), None))
                    }
                    Err(e) => Some((Err(std::io::Error::other(e)), None)),
                }
            },
        );
        let mut resp = Response::new(Body::from_stream(stream));
        *resp.status_mut() = status;
        *resp.headers_mut() = headers;
        return resp;
    }

    let body = match read_capped(up, MAX_BODY).await {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(request_id = %request_id, "reading upstream body: {e:#}");
            return json_error(StatusCode::BAD_GATEWAY, "upstream body could not be read");
        }
    };
    if ctype.contains("json") {
        let f = st.key.sign_response(
            &st.claim_sha256,
            &request_id,
            &st.served_model_id,
            &tensorseed_core::time::now_rfc3339(),
            &body,
        );
        for (n, v) in f.headers() {
            set(&mut headers, n, v);
        }
    }
    let mut resp = Response::new(Body::from(body));
    *resp.status_mut() = status;
    *resp.headers_mut() = headers;
    resp
}

async fn read_capped(mut r: reqwest::Response, max: usize) -> anyhow::Result<Bytes> {
    let mut buf = Vec::new();
    while let Some(c) = r.chunk().await? {
        if buf.len() + c.len() > max {
            bail!("response body is larger than {max} bytes");
        }
        buf.extend_from_slice(&c);
    }
    Ok(Bytes::from(buf))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signing::tests::{claim, key};
    use crate::signing::{
        DEFAULT_MAX_SKEW_SECS, Expect, H_BODY, H_SIGNATURE, SigError, extract_signed, verify_fields,
    };
    use axum::routing::{get, post};

    async fn upstream() -> SocketAddr {
        async fn chat() -> Response {
            (
                [(header::CONTENT_TYPE, "application/json")],
                r#"{"id":"c1","choices":[{"message":{"content":"hi"}}]}"#,
            )
                .into_response()
        }
        async fn echo(h: HeaderMap) -> Response {
            let m: serde_json::Map<String, serde_json::Value> = h
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").into()))
                .collect();
            (
                [(header::CONTENT_TYPE, "application/json")],
                serde_json::Value::Object(m).to_string(),
            )
                .into_response()
        }
        async fn stream() -> Response {
            (
                [(header::CONTENT_TYPE, "text/event-stream")],
                "data: {\"a\":1}\n\ndata: [DONE]\n\n",
            )
                .into_response()
        }
        async fn html() -> Response {
            ([(header::CONTENT_TYPE, "text/html")], "<p>ok</p>").into_response()
        }
        let app = axum::Router::new()
            .route("/v1/chat/completions", post(chat))
            .route("/echo", post(echo))
            .route("/stream", post(stream))
            .route("/", get(html));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        addr
    }

    struct Harness {
        base: String,
        claim_sha256: String,
        _dir: tempfile::TempDir,
        _stop: tokio::sync::oneshot::Sender<()>,
    }

    async fn proxy() -> Harness {
        let up = upstream().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("served-weights.json");
        let c = claim();
        c.write(&path).unwrap();
        let p = SignProxy::bind(
            "127.0.0.1:0".parse().unwrap(),
            &format!("http://{up}"),
            &path,
            key(),
        )
        .await
        .unwrap();
        let addr = p.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(p.run(async {
            let _ = rx.await;
        }));
        Harness {
            base: format!("http://{addr}"),
            claim_sha256: c.claim_sha256,
            _dir: dir,
            _stop: tx,
        }
    }

    fn pairs(h: &reqwest::header::HeaderMap) -> Vec<(String, String)> {
        h.iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_string()))
            .collect()
    }

    fn check(
        h: &Harness,
        headers: &[(String, String)],
        body: &[u8],
        rid: Option<&str>,
    ) -> Result<(), SigError> {
        let (f, signed) = extract_signed(headers, body)?;
        verify_fields(
            &f,
            signed,
            &Expect {
                pubkey: key().pubkey(),
                claim_sha256: &h.claim_sha256,
                served_model_id: "org/tiny",
                request_id: rid,
                requested_model: Some("org/tiny"),
                now_unix: i64::try_from(tensorseed_core::time::unix_now()).unwrap(),
                max_skew_secs: DEFAULT_MAX_SKEW_SECS,
            },
        )
    }

    #[tokio::test]
    async fn signs_json_and_echoes_request_id() {
        let h = proxy().await;
        let http = reqwest::Client::new();
        let r = http
            .post(format!("{}/v1/chat/completions", h.base))
            .header(H_REQUEST_ID, "buyer-42")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success());
        let headers = pairs(r.headers());
        let body = r.bytes().await.unwrap();
        check(&h, &headers, &body, Some("buyer-42")).unwrap();
        // A wrong expected request id, a changed body, and a changed
        // header each fail.
        assert_eq!(
            check(&h, &headers, &body, Some("other")),
            Err(SigError::RequestId)
        );
        let mut tampered = body.to_vec();
        tampered[10] ^= 1;
        assert_eq!(
            check(&h, &headers, &tampered, None),
            Err(SigError::BodyHash)
        );
        let mut hs = headers.clone();
        for (k, v) in &mut hs {
            if k == H_BODY {
                *v = crate::signing::tests::tests_sha("x");
            }
        }
        assert!(check(&h, &hs, b"x", None).is_err());
        let mut hs = headers.clone();
        hs.retain(|(k, _)| k != H_SIGNATURE);
        assert_eq!(
            check(&h, &hs, &body, None),
            Err(SigError::Missing(H_SIGNATURE))
        );
    }

    #[tokio::test]
    async fn mints_ids_forwards_headers_and_rejects_bad_ids() {
        let h = proxy().await;
        let http = reqwest::Client::new();
        let r = http
            .post(format!("{}/echo", h.base))
            .header("authorization", "Bearer buyer-token")
            .header("accept-encoding", "gzip")
            .header("x-buyer", "1")
            .send()
            .await
            .unwrap();
        let rid = r.headers()[H_REQUEST_ID].to_str().unwrap().to_string();
        assert_eq!(rid.len(), 64);
        let headers = pairs(r.headers());
        let body = r.bytes().await.unwrap();
        check(&h, &headers, &body, Some(&rid)).unwrap();
        let seen: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(seen["authorization"], "Bearer buyer-token");
        assert_eq!(seen["x-buyer"], "1");
        assert_eq!(seen[H_REQUEST_ID], rid.as_str());
        assert!(seen.get("accept-encoding").is_none());

        let r = http
            .post(format!("{}/echo", h.base))
            .header(H_REQUEST_ID, "has space")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn streams_end_with_a_signed_trailer() {
        let h = proxy().await;
        let r = reqwest::Client::new()
            .post(format!("{}/stream", h.base))
            .header(H_REQUEST_ID, "s-1")
            .send()
            .await
            .unwrap();
        assert_eq!(r.headers()[H_SIGNATURE_MODE], MODE_SSE_TRAILER);
        let headers = pairs(r.headers());
        let body = r.bytes().await.unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.starts_with("data: {\"a\":1}\n\ndata: [DONE]\n\n: ts.response-sig/1 "));
        check(&h, &headers, &body, Some("s-1")).unwrap();
        // Dropping an event breaks it; cutting the trailer leaves it unsigned.
        let cut = text.replacen("data: {\"a\":1}\n\n", "", 1);
        assert_eq!(
            check(&h, &headers, cut.as_bytes(), None),
            Err(SigError::BodyHash)
        );
        let head = text.split(": ts.response-sig/1").next().unwrap();
        assert!(check(&h, &headers, head.as_bytes(), None).is_err());
    }

    #[tokio::test]
    async fn serves_the_claim_and_passes_other_content_unsigned() {
        let h = proxy().await;
        let http = reqwest::Client::new();
        let raw = http
            .get(format!("{}{WELL_KNOWN_CLAIM}", h.base))
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        let (c, _) = claim_from_bytes(&raw).unwrap();
        assert_eq!(c.claim_sha256, h.claim_sha256);
        let r = http.get(format!("{}/", h.base)).send().await.unwrap();
        assert!(r.headers().get(H_SIGNATURE).is_none());
        assert!(r.headers().get(H_REQUEST_ID).is_some());
    }

    #[tokio::test]
    async fn refuses_a_claim_for_another_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.json");
        claim().write(&path).unwrap();
        let other = ResponseKey::from_seed(&[9; 32]);
        let e = SignProxy::bind(
            "127.0.0.1:0".parse().unwrap(),
            "http://127.0.0.1:1",
            &path,
            other,
        )
        .await
        .err()
        .unwrap();
        assert!(e.to_string().contains("names response key"));
        let e = SignProxy::bind(
            "127.0.0.1:0".parse().unwrap(),
            "http://127.0.0.1:1/v1",
            &path,
            key(),
        )
        .await
        .err()
        .unwrap();
        assert!(e.to_string().contains("upstream root"));
    }
}
