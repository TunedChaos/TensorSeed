// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

use super::*;
use tensorseed_core::hash::git_blob_sha1_bytes;
use wiremock::matchers::{header, header_exists, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn data(n: usize, seed: u8) -> Vec<u8> {
    (0..n)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn job(dir: &Path) -> JobSpec {
    JobSpec {
        id: job_id("model o/n@c", dir),
        label: "model o/n@c".into(),
        out_dir: dir.to_path_buf(),
    }
}

fn src(server: &MockServer, p: &str) -> Source {
    Source {
        url: format!("{}{p}", server.uri()),
        kind: SourceKind::Webseed,
        bearer: None,
    }
}

fn fj(path: &str, body: &[u8], sources: Vec<Source>) -> FileJob {
    FileJob {
        path: path.into(),
        size: body.len() as u64,
        digest: ExpectedDigest::Sha256(sha256_bytes(body)),
        sources,
    }
}

fn fast() -> FetchOptions {
    FetchOptions {
        concurrency: 2,
        attempts_per_source: 2,
        retry_delay: Duration::from_millis(1),
        dead_mirror_cooldown: Duration::from_secs(3600),
        ..Default::default()
    }
}

async fn serve(server: &MockServer, p: &str, body: &[u8]) {
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.to_vec()))
        .mount(server)
        .await;
}

#[tokio::test]
async fn downloads_verifies_and_renames() {
    let server = MockServer::start().await;
    let a = data(300_000, 1);
    let b = b"{\"x\":1}".to_vec();
    serve(&server, "/ws/a.safetensors", &a).await;
    serve(&server, "/ws/sub/b.json", &b).await;
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(StateDb::open_in_memory().unwrap());
    let f = Fetcher::new(Some(db.clone())).unwrap();
    let j = job(dir.path());
    let report = f
        .run(
            &j,
            vec![
                fj("a.safetensors", &a, vec![src(&server, "/ws/a.safetensors")]),
                fj("sub/b.json", &b, vec![src(&server, "/ws/sub/b.json")]),
            ],
            fast(),
        )
        .await
        .unwrap();
    assert!(report.ok(), "{report:?}");
    assert_eq!(report.counts(), (2, 0, 0));
    assert_eq!(std::fs::read(dir.path().join("a.safetensors")).unwrap(), a);
    assert_eq!(std::fs::read(dir.path().join("sub/b.json")).unwrap(), b);
    assert!(
        !dir.path().join(".tensorseed/partial").exists(),
        "partial dir cleaned up"
    );
    let jobs = db.jobs().unwrap();
    assert_eq!(jobs[0].status, state::status::DONE);
    assert_eq!(jobs[0].files_done, 2);

    // Second run: nothing is requested again (mtime fast path).
    let before = server.received_requests().await.unwrap().len();
    let report = f
        .run(
            &j,
            vec![
                fj("a.safetensors", &a, vec![src(&server, "/ws/a.safetensors")]),
                fj("sub/b.json", &b, vec![src(&server, "/ws/sub/b.json")]),
            ],
            fast(),
        )
        .await
        .unwrap();
    assert_eq!(report.counts(), (0, 2, 0));
    assert_eq!(server.received_requests().await.unwrap().len(), before);
}

#[tokio::test]
async fn bad_source_is_dropped_and_next_source_used() {
    let server = MockServer::start().await;
    let good = data(5000, 2);
    let mut evil = good.clone();
    evil[100] ^= 0xff;
    serve(&server, "/evil/x.gguf", &evil).await;
    Mock::given(method("GET"))
        .and(path("/gone/x.gguf"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    serve(&server, "/hf/x.gguf", &good).await;
    let dir = tempfile::tempdir().unwrap();
    let f = Fetcher::new(None).unwrap();
    let mut hf = src(&server, "/hf/x.gguf");
    hf.kind = SourceKind::HuggingFace;
    let report = f
        .run(
            &job(dir.path()),
            vec![fj(
                "x.gguf",
                &good,
                vec![
                    src(&server, "/evil/x.gguf"),
                    src(&server, "/gone/x.gguf"),
                    hf,
                ],
            )],
            fast(),
        )
        .await
        .unwrap();
    let r = &report.files[0];
    assert!(
        matches!(
            &r.outcome,
            Outcome::Downloaded {
                kind: SourceKind::HuggingFace,
                ..
            }
        ),
        "{r:?}"
    );
    assert_eq!(r.errors.len(), 2);
    assert!(r.errors[0].contains("hash mismatch"), "{:?}", r.errors);
    assert!(r.errors[1].contains("404"), "{:?}", r.errors);
    assert_eq!(std::fs::read(dir.path().join("x.gguf")).unwrap(), good);
}

#[tokio::test]
async fn all_sources_bad_leaves_no_file() {
    let server = MockServer::start().await;
    let good = data(1000, 3);
    serve(&server, "/evil/x.bin", &data(1000, 4)).await;
    Mock::given(method("GET"))
        .and(path("/flaky/x.bin"))
        .respond_with(ResponseTemplate::new(503))
        .expect(2)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(StateDb::open_in_memory().unwrap());
    let f = Fetcher::new(Some(db.clone())).unwrap();
    let j = job(dir.path());
    let report = f
        .run(
            &j,
            vec![fj(
                "x.bin",
                &good,
                vec![src(&server, "/evil/x.bin"), src(&server, "/flaky/x.bin")],
            )],
            fast(),
        )
        .await
        .unwrap();
    assert!(!report.ok());
    assert!(!dir.path().join("x.bin").exists());
    assert_eq!(db.jobs().unwrap()[0].status, state::status::FAILED);
    let rec = db.file(&j.id, "x.bin").unwrap().unwrap();
    assert_eq!(rec.status, state::status::FAILED);
    assert!(rec.error.unwrap().contains("503"));
}

#[tokio::test]
async fn resumes_partial_with_range_request() {
    let server = MockServer::start().await;
    let body = data(10_000, 5);
    let have = 4_000usize;
    Mock::given(method("GET"))
        .and(path("/ws/big.safetensors"))
        .and(header("range", format!("bytes={have}-").as_str()))
        .respond_with(
            ResponseTemplate::new(206)
                .insert_header("content-range", format!("bytes {have}-9999/10000").as_str())
                .set_body_bytes(body[have..].to_vec()),
        )
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let file = fj(
        "big.safetensors",
        &body,
        vec![src(&server, "/ws/big.safetensors")],
    );
    let part_dir = dir.path().join(".tensorseed/partial");
    std::fs::create_dir_all(&part_dir).unwrap();
    let part = part_dir.join(format!(
        "{}-{}.part",
        file.digest.hex(),
        &sha256_bytes(b"big.safetensors")[..16]
    ));
    std::fs::write(&part, &body[..have]).unwrap();
    let report = Fetcher::new(None)
        .unwrap()
        .run(&job(dir.path()), vec![file], fast())
        .await
        .unwrap();
    assert!(
        matches!(
            report.files[0].outcome,
            Outcome::Downloaded {
                resumed_from: 4000,
                ..
            }
        ),
        "{report:?}"
    );
    assert_eq!(
        std::fs::read(dir.path().join("big.safetensors")).unwrap(),
        body
    );
}

#[tokio::test]
async fn server_ignoring_range_restarts_cleanly() {
    let server = MockServer::start().await;
    let body = data(3_000, 6);
    serve(&server, "/ws/f.bin", &body).await;
    let dir = tempfile::tempdir().unwrap();
    let file = fj("f.bin", &body, vec![src(&server, "/ws/f.bin")]);
    let part_dir = dir.path().join(".tensorseed/partial");
    std::fs::create_dir_all(&part_dir).unwrap();
    let part = part_dir.join(format!(
        "{}-{}.part",
        file.digest.hex(),
        &sha256_bytes(b"f.bin")[..16]
    ));
    std::fs::write(&part, &body[..1000]).unwrap();
    let report = Fetcher::new(None)
        .unwrap()
        .run(&job(dir.path()), vec![file], fast())
        .await
        .unwrap();
    assert!(report.ok(), "{report:?}");
    assert_eq!(std::fs::read(dir.path().join("f.bin")).unwrap(), body);
}

#[tokio::test]
async fn existing_correct_file_is_not_downloaded() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("c.json"), b"{}").unwrap();
    let report = Fetcher::new(None)
        .unwrap()
        .run(
            &job(dir.path()),
            vec![fj("c.json", b"{}", vec![src(&server, "/c.json")])],
            fast(),
        )
        .await
        .unwrap();
    assert_eq!(report.files[0].outcome, Outcome::AlreadyPresent);
}

#[tokio::test]
async fn wrong_existing_file_is_replaced_atomically() {
    let server = MockServer::start().await;
    serve(&server, "/c.json", b"{\"ok\":true}").await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("c.json"), b"{\"ok\":nope}").unwrap();
    let report = Fetcher::new(None)
        .unwrap()
        .run(
            &job(dir.path()),
            vec![fj(
                "c.json",
                b"{\"ok\":true}",
                vec![src(&server, "/c.json")],
            )],
            fast(),
        )
        .await
        .unwrap();
    assert!(report.ok());
    assert_eq!(
        std::fs::read(dir.path().join("c.json")).unwrap(),
        b"{\"ok\":true}"
    );
}

#[tokio::test]
async fn git_blob_digest_and_bearer_token() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/hf/README.md"))
        .and(header("authorization", "Bearer tok"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"hello\n".to_vec()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/ws/README.md"))
        .and(header_exists("authorization"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let file = FileJob {
        path: "README.md".into(),
        size: 6,
        digest: ExpectedDigest::GitBlobSha1(git_blob_sha1_bytes(b"hello\n")),
        sources: vec![Source {
            url: format!("{}/hf/README.md", server.uri()),
            kind: SourceKind::HuggingFace,
            bearer: Some("tok".into()),
        }],
    };
    let report = Fetcher::new(None)
        .unwrap()
        .run(&job(dir.path()), vec![file], fast())
        .await
        .unwrap();
    assert!(report.ok(), "{report:?}");
}

#[tokio::test]
async fn oversized_body_is_rejected_early() {
    let server = MockServer::start().await;
    serve(&server, "/ws/x", &data(2000, 7)).await;
    let dir = tempfile::tempdir().unwrap();
    let small = data(1000, 7);
    let report = Fetcher::new(None)
        .unwrap()
        .run(
            &job(dir.path()),
            vec![fj("x", &small, vec![src(&server, "/ws/x")])],
            fast(),
        )
        .await
        .unwrap();
    assert!(!report.ok());
    assert!(
        report.files[0].errors[0].contains("more than"),
        "{report:?}"
    );
}

#[tokio::test]
async fn rejects_unsafe_paths() {
    let dir = tempfile::tempdir().unwrap();
    let err = Fetcher::new(None)
        .unwrap()
        .run(
            &job(dir.path()),
            vec![fj("../escape", b"x", vec![])],
            fast(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, FetchError::Invalid(_)));
}

#[test]
fn parses_content_range() {
    assert_eq!(content_range_start("bytes 100-199/200"), Some(100));
    assert_eq!(content_range_start("bytes */200"), None);
    assert_eq!(content_range_start("items 1-2/3"), None);
}

#[test]
fn source_origin_strips_path() {
    assert_eq!(
        source_origin("https://mirror.example/t/org/file.bin"),
        "https://mirror.example"
    );
    assert_eq!(
        source_origin("http://127.0.0.1:8080/a/b"),
        "http://127.0.0.1:8080"
    );
    assert_eq!(source_origin("not-a-url"), "not-a-url");
}

#[test]
fn dead_ago_label_units() {
    assert_eq!(dead_ago_label(100, 130), "just now");
    assert_eq!(dead_ago_label(100, 100 + 12 * 60), "12m ago");
    assert_eq!(dead_ago_label(100, 100 + 2 * 3600), "2h ago");
    assert_eq!(
        dead_skip_line("https://m", 100, 100 + 12 * 60),
        "skip   https://m (marked dead 12m ago; --retry-dead-mirrors to retry)"
    );
}

async fn status(server: &MockServer, p: &str, code: u16) {
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(ResponseTemplate::new(code))
        .mount(server)
        .await;
}

#[tokio::test]
async fn dead_host_is_skipped_for_later_files() {
    let dead = MockServer::start().await;
    let good = MockServer::start().await;
    let a = data(1000, 1);
    let b = data(1000, 2);
    status(&dead, "/ws/a.bin", 503).await;
    status(&dead, "/ws/b.bin", 503).await;
    serve(&good, "/ws/a.bin", &a).await;
    serve(&good, "/ws/b.bin", &b).await;
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(StateDb::open_in_memory().unwrap());
    let f = Fetcher::new(Some(db.clone())).unwrap();
    let j = job(dir.path());
    let mut opts = fast();
    opts.concurrency = 1; // first file marks dead before the second starts
    let report = f
        .run(
            &j,
            vec![
                fj(
                    "a.bin",
                    &a,
                    vec![src(&dead, "/ws/a.bin"), src(&good, "/ws/a.bin")],
                ),
                fj(
                    "b.bin",
                    &b,
                    vec![src(&dead, "/ws/b.bin"), src(&good, "/ws/b.bin")],
                ),
            ],
            opts,
        )
        .await
        .unwrap();
    assert!(report.ok(), "{report:?}");
    assert_eq!(report.skipped_dead.len(), 1);
    assert_eq!(report.skipped_dead[0].origin, dead.uri());
    assert!(
        report.skipped_dead[0].line.contains("--retry-dead-mirrors"),
        "{:?}",
        report.skipped_dead[0].line
    );
    // The dead origin was persisted.
    assert!(db.dead_mirror(&dead.uri()).unwrap().is_some());
    assert!(db.dead_mirror(&good.uri()).unwrap().is_none());
}

#[tokio::test]
async fn not_found_does_not_mark_origin_dead() {
    let a_src = MockServer::start().await;
    let b_src = MockServer::start().await;
    let a = data(800, 3);
    let b = data(800, 4);
    status(&a_src, "/ws/a.bin", 404).await;
    serve(&a_src, "/ws/b.bin", &b).await;
    serve(&b_src, "/ws/a.bin", &a).await;
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(StateDb::open_in_memory().unwrap());
    let f = Fetcher::new(Some(db.clone())).unwrap();
    let j = job(dir.path());
    let mut opts = fast();
    opts.concurrency = 1;
    let report = f
        .run(
            &j,
            vec![
                fj(
                    "a.bin",
                    &a,
                    vec![src(&a_src, "/ws/a.bin"), src(&b_src, "/ws/a.bin")],
                ),
                fj("b.bin", &b, vec![src(&a_src, "/ws/b.bin")]),
            ],
            opts,
        )
        .await
        .unwrap();
    assert!(report.ok(), "{report:?}");
    assert!(report.skipped_dead.is_empty(), "{report:?}");
    assert!(db.dead_mirror(&a_src.uri()).unwrap().is_none());
}

#[tokio::test]
async fn unauthorized_or_forbidden_does_not_mark_origin_dead() {
    let a_src = MockServer::start().await;
    let b_src = MockServer::start().await;
    let a = data(800, 6);
    let b = data(800, 7);
    status(&a_src, "/ws/a.bin", 401).await;
    status(&a_src, "/ws/b.bin", 403).await;
    serve(&b_src, "/ws/a.bin", &a).await;
    serve(&b_src, "/ws/b.bin", &b).await;
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(StateDb::open_in_memory().unwrap());
    let f = Fetcher::new(Some(db.clone())).unwrap();
    let j = job(dir.path());
    let mut opts = fast();
    opts.concurrency = 1;
    let report = f
        .run(
            &j,
            vec![
                fj(
                    "a.bin",
                    &a,
                    vec![src(&a_src, "/ws/a.bin"), src(&b_src, "/ws/a.bin")],
                ),
                fj(
                    "b.bin",
                    &b,
                    vec![src(&a_src, "/ws/b.bin"), src(&b_src, "/ws/b.bin")],
                ),
            ],
            opts,
        )
        .await
        .unwrap();
    assert!(report.ok(), "{report:?}");
    assert!(report.skipped_dead.is_empty(), "{report:?}");
    assert!(db.dead_mirror(&a_src.uri()).unwrap().is_none());
}

#[tokio::test]
async fn cooldown_expiry_retries_and_flag_overrides() {
    let dead = MockServer::start().await;
    let body = data(400, 5);
    serve(&dead, "/ws/a.bin", &body).await;
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(StateDb::open_in_memory().unwrap());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    db.mark_dead_at(&dead.uri(), now - 120, "connect refused")
        .unwrap();

    // Within cooldown: skip, safety net still tries (only source), succeeds, clears mark.
    let f = Fetcher::new(Some(db.clone())).unwrap();
    let mut opts = fast();
    opts.dead_mirror_cooldown = Duration::from_secs(3600);
    let j = job(dir.path());
    let report = f
        .run(
            &j,
            vec![fj("a.bin", &body, vec![src(&dead, "/ws/a.bin")])],
            opts,
        )
        .await
        .unwrap();
    assert!(report.ok(), "{report:?}");
    assert!(
        matches!(report.files[0].outcome, Outcome::Downloaded { .. }),
        "{report:?}"
    );
    // Safety net tried the only (dead) source and success cleared the mark.
    assert!(db.dead_mirror(&dead.uri()).unwrap().is_none());

    // Re-mark recently and skip a later file when a live alternative exists.
    db.mark_dead_at(&dead.uri(), now, "HTTP 503").unwrap();
    let good = MockServer::start().await;
    let other = data(400, 6);
    serve(&good, "/ws/b.bin", &other).await;
    serve(&dead, "/ws/b.bin", &other).await;
    let dir2 = tempfile::tempdir().unwrap();
    let j2 = job(dir2.path());
    let f = Fetcher::new(Some(db.clone())).unwrap();
    let report = f
        .run(
            &j2,
            vec![fj(
                "b.bin",
                &other,
                vec![src(&dead, "/ws/b.bin"), src(&good, "/ws/b.bin")],
            )],
            opts,
        )
        .await
        .unwrap();
    assert!(report.ok(), "{report:?}");
    assert_eq!(report.skipped_dead.len(), 1);
    // Succeeded from the good mirror; the dead mark remains (we never got a
    // success from the dead origin).
    assert!(db.dead_mirror(&dead.uri()).unwrap().is_some());

    // --retry-dead-mirrors tries the marked origin and clears it on success.
    let dir3 = tempfile::tempdir().unwrap();
    let j3 = job(dir3.path());
    let mut retry = opts;
    retry.retry_dead_mirrors = true;
    let f = Fetcher::new(Some(db.clone())).unwrap();
    let report = f
        .run(
            &j3,
            vec![fj("b.bin", &other, vec![src(&dead, "/ws/b.bin")])],
            retry,
        )
        .await
        .unwrap();
    assert!(report.ok(), "{report:?}");
    assert!(report.skipped_dead.is_empty(), "{report:?}");
    assert!(db.dead_mirror(&dead.uri()).unwrap().is_none());

    // An expired mark is retried automatically.
    db.mark_dead_at(&dead.uri(), now - 7200, "old").unwrap();
    let dir4 = tempfile::tempdir().unwrap();
    let j4 = job(dir4.path());
    let f = Fetcher::new(Some(db.clone())).unwrap();
    let report = f
        .run(
            &j4,
            vec![fj("a.bin", &body, vec![src(&dead, "/ws/a.bin")])],
            opts,
        )
        .await
        .unwrap();
    assert!(report.ok(), "{report:?}");
    assert!(report.skipped_dead.is_empty(), "{report:?}");
    assert!(db.dead_mirror(&dead.uri()).unwrap().is_none());
}

#[tokio::test]
async fn all_dead_safety_net_still_tries() {
    let server = MockServer::start().await;
    let body = data(200, 7);
    serve(&server, "/ws/a.bin", &body).await;
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(StateDb::open_in_memory().unwrap());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    db.mark_dead_at(&server.uri(), now, "connect refused")
        .unwrap();
    let f = Fetcher::new(Some(db.clone())).unwrap();
    let mut opts = fast();
    opts.dead_mirror_cooldown = Duration::from_secs(3600);
    let report = f
        .run(
            &job(dir.path()),
            vec![fj("a.bin", &body, vec![src(&server, "/ws/a.bin")])],
            opts,
        )
        .await
        .unwrap();
    assert!(report.ok(), "{report:?}");
    // Safety net does not emit a skip line; success clears the mark.
    assert!(report.skipped_dead.is_empty(), "{report:?}");
    assert!(db.dead_mirror(&server.uri()).unwrap().is_none());
}

#[tokio::test]
async fn connect_failure_marks_origin_dead() {
    // Bind and drop a port so connect is refused quickly.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let origin = format!("http://127.0.0.1:{port}");
    let body = data(100, 8);
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(StateDb::open_in_memory().unwrap());
    let f = Fetcher::with_timeouts(
        Some(db.clone()),
        Duration::from_millis(200),
        Duration::from_secs(1),
    )
    .unwrap();
    let good = MockServer::start().await;
    serve(&good, "/a.bin", &body).await;
    let mut opts = fast();
    opts.concurrency = 1;
    let bad = Source {
        url: format!("{origin}/a.bin"),
        kind: SourceKind::Webseed,
        bearer: None,
    };
    let report = f
        .run(
            &job(dir.path()),
            vec![
                fj("a.bin", &body, vec![bad.clone(), src(&good, "/a.bin")]),
                fj(
                    "b.bin",
                    &body,
                    vec![
                        Source {
                            url: format!("{origin}/b.bin"),
                            kind: SourceKind::Webseed,
                            bearer: None,
                        },
                        src(&good, "/a.bin"),
                    ],
                ),
            ],
            opts,
        )
        .await
        .unwrap();
    assert!(report.ok(), "{report:?}");
    assert_eq!(report.skipped_dead.len(), 1);
    assert_eq!(report.skipped_dead[0].origin, origin);
    assert!(db.dead_mirror(&origin).unwrap().is_some());
}
