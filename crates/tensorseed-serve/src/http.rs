// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! HTTP file serving for `tensorseed serve` (spec sections 7.2 and 8.3).
//!
//! Paths follow the BEP 19 webseed layout Tensorseed uses everywhere (spec
//! 10.1), so the daemon's base URL plus `t/` works as a webseed or a
//! `mirrors.toml` entry:
//!
//! - `GET /t/<content name>.torrent`: the revision's torrent.
//! - `GET /t/<content name>/<relative path>`: one file of a revision.
//! - `GET /<catalog>/<artifact>`: the mirrored signed catalog files
//!   (`head.json`, `deltas/*.json`, `snapshots/*.json`, `mirrors.toml`, each
//!   with its `.minisig`), only with `--serve-catalog`.
//!
//! Only files listed in a revision's torrent are served, and only while the
//! daemon seeds that revision, so nothing else under the data directory
//! (for example `.tensorseed/` job state) is reachable. `Range` requests
//! (one range; RFC 9110 section 14) get `206 Partial Content`, `HEAD` works,
//! and an upload rate limit applies across all connections.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::StreamExt;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::ratelimit::RateLimiter;

/// One revision available over HTTP.
#[derive(Debug, Clone)]
pub struct Published {
    /// Directory holding the files (`.../t/<content name>/`).
    pub dir: PathBuf,
    /// The `.torrent` bytes.
    pub torrent: Bytes,
    /// Servable files: relative path to expected size.
    pub files: HashMap<String, u64>,
}

/// Revisions currently served, by content name.
#[derive(Debug, Default)]
pub struct Library {
    map: RwLock<HashMap<String, Arc<Published>>>,
}

impl Library {
    /// An empty library.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add or replace a revision.
    pub fn insert(&self, name: &str, p: Published) {
        self.map
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(name.to_owned(), Arc::new(p));
    }

    /// Stop serving a revision. Returns whether it was there.
    pub fn remove(&self, name: &str) -> bool {
        self.map
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(name)
            .is_some()
    }

    /// Look up a revision.
    pub fn get(&self, name: &str) -> Option<Arc<Published>> {
        self.map
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
    }

    /// Served content names, sorted.
    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .map
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        v.sort();
        v
    }
}

/// A mirrored catalog directory served at `/<name>/`.
#[derive(Debug, Clone)]
pub struct CatalogDir {
    /// Catalog name (first path segment).
    pub name: String,
    /// Directory with `head.json`, `deltas/`, `snapshots/`, ...
    pub dir: PathBuf,
}

/// Counters for the health endpoint.
#[derive(Debug, Default)]
pub struct HttpStats {
    requests: AtomicU64,
    bytes_sent: AtomicU64,
    not_found: AtomicU64,
}

/// Snapshot of [`HttpStats`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct HttpStatsSnapshot {
    /// Requests answered.
    pub requests: u64,
    /// Body bytes sent.
    pub bytes_sent: u64,
    /// 404 answers.
    pub not_found: u64,
}

impl HttpStats {
    /// Current values.
    pub fn snapshot(&self) -> HttpStatsSnapshot {
        HttpStatsSnapshot {
            requests: self.requests.load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            not_found: self.not_found.load(Ordering::Relaxed),
        }
    }
}

/// What the server exposes.
#[derive(Debug, Clone, Default)]
pub struct HttpConfig {
    /// Revisions (webseed paths); `None` serves no `t/` paths.
    pub library: Option<Arc<Library>>,
    /// Catalog files; `None` serves no catalog.
    pub catalog: Option<CatalogDir>,
    /// Shared upload limit.
    pub limiter: Option<Arc<RateLimiter>>,
    /// Counters.
    pub stats: Arc<HttpStats>,
}

/// Build the router.
pub fn router(cfg: HttpConfig) -> Router {
    Router::new().fallback(handle).with_state(Arc::new(cfg))
}

/// Serve `router` on `listener` until `shutdown` resolves.
///
/// # Errors
///
/// Any I/O error from accepting connections.
pub async fn serve(
    listener: tokio::net::TcpListener,
    router: Router,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await
}

/// Decode a request path into safe segments: percent decoded UTF-8, no
/// empty, `.`, or `..` segments, no separators or NUL inside a segment.
pub fn decode_path(raw: &str) -> Option<Vec<String>> {
    let raw = raw.strip_prefix('/')?;
    let mut out = Vec::new();
    for seg in raw.split('/') {
        let s = percent_encoding::percent_decode_str(seg)
            .decode_utf8()
            .ok()?
            .into_owned();
        if s.is_empty() || s == "." || s == ".." || s.contains(['/', '\\', '\0']) {
            return None;
        }
        out.push(s);
    }
    Some(out)
}

/// A parsed `Range` header against a body of `len` bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeSpec {
    /// No usable range: send everything.
    Full,
    /// Inclusive byte range.
    Part(u64, u64),
    /// Cannot be satisfied (416).
    Unsatisfiable,
}

/// Parse a `Range` header value. Multiple ranges and malformed values are
/// ignored (full body), as RFC 9110 allows.
pub fn parse_range(v: &str, len: u64) -> RangeSpec {
    let Some(spec) = v.trim().strip_prefix("bytes=") else {
        return RangeSpec::Full;
    };
    if spec.contains(',') {
        return RangeSpec::Full;
    }
    let Some((a, b)) = spec.trim().split_once('-') else {
        return RangeSpec::Full;
    };
    let (a, b) = (a.trim(), b.trim());
    match (a.is_empty(), b.is_empty()) {
        (true, true) => RangeSpec::Full,
        (true, false) => {
            let Ok(n) = b.parse::<u64>() else {
                return RangeSpec::Full;
            };
            if n == 0 || len == 0 {
                return RangeSpec::Unsatisfiable;
            }
            RangeSpec::Part(len.saturating_sub(n), len - 1)
        }
        (false, _) => {
            let Ok(start) = a.parse::<u64>() else {
                return RangeSpec::Full;
            };
            let end = if b.is_empty() {
                len.saturating_sub(1)
            } else {
                match b.parse::<u64>() {
                    Ok(e) if e >= start => e.min(len.saturating_sub(1)),
                    _ => return RangeSpec::Full,
                }
            };
            if start >= len {
                return RangeSpec::Unsatisfiable;
            }
            RangeSpec::Part(start, end)
        }
    }
}

fn plain(status: StatusCode, msg: &'static str) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        msg,
    )
        .into_response()
}

fn catalog_artifact_ok(segs: &[String]) -> bool {
    let strip = |s: &str| s.strip_suffix(".minisig").unwrap_or(s).to_owned();
    match segs {
        [f] => {
            let f = strip(f);
            f == "head.json" || f == "mirrors.toml"
        }
        [d, f] if d == "deltas" || d == "snapshots" => {
            let f = strip(f);
            f.strip_suffix(".json")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        }
        _ => false,
    }
}

fn content_type(path: &str) -> &'static str {
    if path.ends_with(".json") {
        "application/json"
    } else if path.ends_with(".minisig") || path.ends_with(".toml") || path.ends_with(".txt") {
        "text/plain; charset=utf-8"
    } else if path.ends_with(".md") {
        "text/markdown; charset=utf-8"
    } else {
        "application/octet-stream"
    }
}

async fn handle(
    State(cfg): State<Arc<HttpConfig>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    cfg.stats.requests.fetch_add(1, Ordering::Relaxed);
    let head = method == Method::HEAD;
    if method != Method::GET && !head {
        let mut r = plain(StatusCode::METHOD_NOT_ALLOWED, "GET or HEAD only\n");
        r.headers_mut()
            .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
        return r;
    }
    let resp = route(&cfg, uri.path(), &headers, head).await;
    if resp.status() == StatusCode::NOT_FOUND {
        cfg.stats.not_found.fetch_add(1, Ordering::Relaxed);
    }
    resp
}

/// Map a request path onto the webseed library (`t/...`) or the mirrored
/// catalog files. Paths are percent decoded and checked component by
/// component, so nothing outside the served directories is reachable.
async fn route(cfg: &HttpConfig, path: &str, headers: &HeaderMap, head: bool) -> Response {
    let not_found = || plain(StatusCode::NOT_FOUND, "not found\n");
    if path == "/" {
        return plain(
            StatusCode::OK,
            "Tensorseed mirror (https://tensorseed.dev)\n",
        );
    }
    let Some(segs) = decode_path(path) else {
        return not_found();
    };
    if let (Some(lib), Some(first)) = (&cfg.library, segs.first())
        && first == "t"
        && segs.len() >= 2
    {
        if segs.len() == 2
            && let Some(name) = segs[1].strip_suffix(".torrent")
        {
            let Some(p) = lib.get(name) else {
                return not_found();
            };
            return bytes_response(cfg, p.torrent.clone(), "application/x-bittorrent", head);
        }
        let Some(p) = lib.get(&segs[1]) else {
            return not_found();
        };
        let rel = segs[2..].join("/");
        let Some(&size) = p.files.get(&rel) else {
            return not_found();
        };
        return file_response(
            cfg,
            &p.dir.join(&rel),
            Some(size),
            content_type(&rel),
            "public, max-age=31536000, immutable",
            headers,
            head,
        )
        .await;
    }
    if let Some(cat) = &cfg.catalog
        && segs.first() == Some(&cat.name)
        && catalog_artifact_ok(&segs[1..])
    {
        let rel = segs[1..].join("/");
        let cache = if segs[1].starts_with("head.json") || segs[1].starts_with("mirrors.toml") {
            "no-cache"
        } else {
            "public, max-age=31536000, immutable"
        };
        return file_response(
            cfg,
            &cat.dir.join(&rel),
            None,
            content_type(&rel),
            cache,
            headers,
            head,
        )
        .await;
    }
    not_found()
}

fn bytes_response(cfg: &HttpConfig, b: Bytes, ctype: &'static str, head: bool) -> Response {
    let len = b.len();
    let body = if head {
        Body::empty()
    } else {
        cfg.stats
            .bytes_sent
            .fetch_add(len as u64, Ordering::Relaxed);
        Body::from(b)
    };
    let mut r = Response::new(body);
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(ctype));
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    // t/<content name>.torrent may be rewritten when webseeds change.
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=60, must-revalidate"),
    );
    r
}

/// Stream a file, honouring a single `Range` header. `expected` is the size
/// the manifest promises; a file with any other size (for example a
/// partial download) is not served.
async fn file_response(
    cfg: &HttpConfig,
    path: &Path,
    expected: Option<u64>,
    ctype: &'static str,
    cache: &'static str,
    headers: &HeaderMap,
    head: bool,
) -> Response {
    let Ok(mut f) = tokio::fs::File::open(path).await else {
        return plain(StatusCode::NOT_FOUND, "not found\n");
    };
    let len = match f.metadata().await {
        Ok(m) if m.is_file() => m.len(),
        _ => return plain(StatusCode::NOT_FOUND, "not found\n"),
    };
    if expected.is_some_and(|e| e != len) {
        tracing::warn!(path = %path.display(), "file size changed on disk; not serving it");
        return plain(StatusCode::NOT_FOUND, "not found\n");
    }
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .map(|v| parse_range(v, len))
        .unwrap_or(RangeSpec::Full);
    let (status, start, count) = match range {
        RangeSpec::Full => (StatusCode::OK, 0, len),
        RangeSpec::Part(a, b) => (StatusCode::PARTIAL_CONTENT, a, b - a + 1),
        RangeSpec::Unsatisfiable => {
            let mut r = plain(StatusCode::RANGE_NOT_SATISFIABLE, "range not satisfiable\n");
            if let Ok(v) = HeaderValue::from_str(&format!("bytes */{len}")) {
                r.headers_mut().insert(header::CONTENT_RANGE, v);
            }
            return r;
        }
    };
    let body = if head || count == 0 {
        Body::empty()
    } else {
        if start > 0 && f.seek(std::io::SeekFrom::Start(start)).await.is_err() {
            return plain(StatusCode::INTERNAL_SERVER_ERROR, "read error\n");
        }
        let limiter = cfg.limiter.clone();
        let stats = cfg.stats.clone();
        let stream = tokio_util::io::ReaderStream::with_capacity(f.take(count), 64 * 1024).then(
            move |chunk| {
                let limiter = limiter.clone();
                let stats = stats.clone();
                async move {
                    if let Ok(c) = &chunk {
                        if let Some(l) = &limiter {
                            l.acquire(c.len()).await;
                        }
                        stats
                            .bytes_sent
                            .fetch_add(c.len() as u64, Ordering::Relaxed);
                    }
                    chunk
                }
            },
        );
        Body::from_stream(stream)
    };
    let mut r = Response::new(body);
    *r.status_mut() = status;
    let h = r.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(ctype));
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(count));
    if status == StatusCode::PARTIAL_CONTENT
        && let Ok(v) = HeaderValue::from_str(&format!("bytes {start}-{}/{len}", start + count - 1))
    {
        h.insert(header::CONTENT_RANGE, v);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        use RangeSpec::*;
        assert_eq!(parse_range("bytes=0-9", 100), Part(0, 9));
        assert_eq!(parse_range("bytes=90-", 100), Part(90, 99));
        assert_eq!(parse_range("bytes=-10", 100), Part(90, 99));
        assert_eq!(parse_range("bytes=-1000", 100), Part(0, 99));
        assert_eq!(parse_range("bytes=95-1000", 100), Part(95, 99));
        assert_eq!(parse_range("bytes=100-", 100), Unsatisfiable);
        assert_eq!(parse_range("bytes=-0", 100), Unsatisfiable);
        assert_eq!(parse_range("bytes=5-1", 100), Full);
        assert_eq!(parse_range("bytes=0-1,5-6", 100), Full);
        assert_eq!(parse_range("items=0-1", 100), Full);
        assert_eq!(parse_range("bytes=x-1", 100), Full);
        assert_eq!(parse_range("bytes=0-", 0), Unsatisfiable);
    }

    #[test]
    fn paths() {
        assert_eq!(
            decode_path("/t/org__n%40abc/a%20b/c.json"),
            Some(vec![
                "t".into(),
                "org__n@abc".into(),
                "a b".into(),
                "c.json".into()
            ])
        );
        assert_eq!(decode_path("/t/../etc/passwd"), None);
        assert_eq!(decode_path("/t/%2e%2e/x"), None);
        assert_eq!(decode_path("/t/a%2fb"), None);
        assert_eq!(decode_path("/t//x"), None);
        assert_eq!(decode_path("no-slash"), None);
        assert!(catalog_artifact_ok(&["head.json".into()]));
        assert!(catalog_artifact_ok(&["head.json.minisig".into()]));
        assert!(catalog_artifact_ok(&[
            "deltas".into(),
            "0000000003.json".into()
        ]));
        assert!(catalog_artifact_ok(&[
            "snapshots".into(),
            "0000000064.json.minisig".into()
        ]));
        assert!(!catalog_artifact_ok(&["state.json".into()]));
        assert!(!catalog_artifact_ok(&["deltas".into(), "x.json".into()]));
    }

    async fn start(cfg: HttpConfig) -> (String, tokio::sync::oneshot::Sender<()>) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(serve(l, router(cfg), async {
            let _ = rx.await;
        }));
        (format!("http://{addr}"), tx)
    }

    #[tokio::test]
    async fn serves_revisions_and_catalog_with_ranges() {
        let d = tempfile::tempdir().unwrap();
        let content = d.path().join("t/org__n@0123456789ab");
        std::fs::create_dir_all(content.join("sub")).unwrap();
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(content.join("sub/model.bin"), &data).unwrap();
        std::fs::create_dir_all(content.join(".tensorseed")).unwrap();
        std::fs::write(content.join(".tensorseed/pull.json"), b"{}").unwrap();
        let cat = d.path().join("catalog/main");
        std::fs::create_dir_all(cat.join("deltas")).unwrap();
        std::fs::write(cat.join("head.json"), b"{\"h\":1}").unwrap();
        std::fs::write(cat.join("deltas/0000000001.json"), b"{\"d\":1}").unwrap();
        std::fs::write(cat.join("state.json"), b"secret").unwrap();

        let lib = Arc::new(Library::new());
        lib.insert(
            "org__n@0123456789ab",
            Published {
                dir: content.clone(),
                torrent: Bytes::from_static(b"d4:infoe"),
                files: HashMap::from([("sub/model.bin".to_string(), data.len() as u64)]),
            },
        );
        let stats = Arc::new(HttpStats::default());
        let (base, stop) = start(HttpConfig {
            library: Some(lib.clone()),
            catalog: Some(CatalogDir {
                name: "main".into(),
                dir: cat.clone(),
            }),
            limiter: Some(Arc::new(RateLimiter::new(0))),
            stats: stats.clone(),
        })
        .await;
        let http = reqwest::Client::new();
        let url = format!("{base}/t/org__n%400123456789ab/sub/model.bin");

        let r = http.get(&url).send().await.unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()["accept-ranges"], "bytes");
        assert_eq!(r.bytes().await.unwrap().as_ref(), &data[..]);

        let r = http
            .get(&url)
            .header("Range", "bytes=1000-1999")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 206);
        assert_eq!(r.headers()["content-range"], "bytes 1000-1999/200000");
        assert_eq!(r.bytes().await.unwrap().as_ref(), &data[1000..2000]);

        let r = http
            .get(&url)
            .header("Range", "bytes=-5")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 206);
        assert_eq!(r.bytes().await.unwrap().as_ref(), &data[data.len() - 5..]);

        let r = http
            .get(&url)
            .header("Range", "bytes=999999-")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 416);
        assert_eq!(r.headers()["content-range"], "bytes */200000");

        let r = http.head(&url).send().await.unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()["content-length"], "200000");

        let r = http
            .get(format!("{base}/t/org__n@0123456789ab.torrent"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(
            r.headers()["cache-control"],
            "public, max-age=60, must-revalidate"
        );
        assert_eq!(r.bytes().await.unwrap().as_ref(), b"d4:infoe");

        for p in [
            "/t/org__n@0123456789ab/.tensorseed/pull.json",
            "/t/other@0123456789ab/sub/model.bin",
            "/main/state.json",
            "/other/head.json",
        ] {
            let r = http.get(format!("{base}{p}")).send().await.unwrap();
            assert_eq!(r.status(), 404, "{p}");
        }
        let r = http
            .get(format!("{base}/main/head.json"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()["cache-control"], "no-cache");
        assert_eq!(r.bytes().await.unwrap().as_ref(), b"{\"h\":1}");
        let r = http
            .get(format!("{base}/main/deltas/0000000001.json"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let r = http.post(&url).send().await.unwrap();
        assert_eq!(r.status(), 405);

        // A file that changed size on disk is no longer served.
        std::fs::write(content.join("sub/model.bin"), b"short").unwrap();
        let r = http.get(&url).send().await.unwrap();
        assert_eq!(r.status(), 404);

        // Removed revisions disappear at once.
        assert!(lib.remove("org__n@0123456789ab"));
        let r = http
            .get(format!("{base}/t/org__n@0123456789ab.torrent"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 404);
        let s = stats.snapshot();
        assert!(s.requests >= 13 && s.not_found >= 6, "{s:?}");
        assert!(s.bytes_sent >= 200_000 + 1000 + 5, "{s:?}");
        let _ = stop.send(());
    }
}
