// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! File catalog over HTTP: publish to a directory, serve it from a local
//! mock server, and sync subscribers with mirror fallback and persistence.

use std::path::Path;
use std::sync::Arc;

use tensorseed_catalog::file::{FileCatalog, FileCatalogConfig, HEAD_PATH};
use tensorseed_catalog::minisign::SecretKey;
use tensorseed_catalog::sink::DirSink;
use tensorseed_catalog::{CatalogBackend, CatalogError, CatalogId, CatalogOp};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const CAT: &str = "main";

fn key() -> SecretKey {
    SecretKey::from_seed(&[42; 32])
}

fn cfg(base: &str, state: Option<&Path>) -> FileCatalogConfig {
    let mut c = FileCatalogConfig::new(CAT, base, key().public_key());
    c.state_dir = state.map(Path::to_path_buf);
    c.snapshot_every = 2;
    c
}

fn publisher(out: &Path, state: &Path) -> FileCatalog {
    FileCatalog::new(cfg("http://unused.invalid/", Some(state)))
        .unwrap()
        .with_signer(key())
        .unwrap()
        .with_sink(Arc::new(DirSink::new(out)))
}

async fn publish(p: &FileCatalog, key: &str, doc: &str) {
    let id = CatalogId(CAT.into());
    p.stage(
        &id,
        &[CatalogOp::Upsert {
            key: key.into(),
            value: doc.as_bytes().to_vec(),
        }],
    )
    .await
    .unwrap();
    p.commit(&id).await.unwrap();
}

/// Serve every file under `dir` at the same relative path.
async fn mount_dir(server: &MockServer, dir: &Path, prefix: &str) {
    fn walk(root: &Path, d: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for e in std::fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, std::fs::read(&p).unwrap()));
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, dir, &mut files);
    for (rel, body) in files {
        Mock::given(method("GET"))
            .and(path(format!("{prefix}/{rel}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(server)
            .await;
    }
}

#[tokio::test]
async fn subscriber_syncs_over_http_and_persists_anchor() {
    let out = tempfile::tempdir().unwrap();
    let pstate = tempfile::tempdir().unwrap();
    let p = publisher(out.path(), pstate.path());
    for i in 1..=3 {
        publish(
            &p,
            &format!("ts/v1/index/model/org/m{i}"),
            &format!(r#"{{"n":{i}}}"#),
        )
        .await;
    }
    assert!(out.path().join("main/snapshots/0000000002.json").exists());
    assert!(
        out.path()
            .join("main/deltas/0000000003.json.minisig")
            .exists()
    );

    let server = MockServer::start().await;
    mount_dir(&server, out.path(), "").await;
    let sstate = tempfile::tempdir().unwrap();
    let base = format!("{}/", server.uri());
    let sub = FileCatalog::new(cfg(&base, Some(sstate.path()))).unwrap();
    let r = sub.sync().await.unwrap();
    assert_eq!((r.from, r.to), (0, 3));
    assert_eq!(sub.state(), p.state());
    let id = CatalogId(CAT.into());
    assert_eq!(
        sub.get(&id, "ts/v1/index/model/org/m2")
            .await
            .unwrap()
            .unwrap(),
        br#"{"n":2}"#
    );

    // A new process reloads and re-verifies the persisted state.
    let again = FileCatalog::new(cfg(&base, Some(sstate.path()))).unwrap();
    assert_eq!(again.state().revision, 3);
    let st = again.sync_status(&id).await.unwrap();
    assert!(st.is_synced());

    // Tampering with the state file is detected on load.
    let sp = sstate.path().join("main/state.json");
    let text = std::fs::read_to_string(&sp)
        .unwrap()
        .replace(r#"\"n\":2"#, r#"\"n\":9"#);
    std::fs::write(&sp, text).unwrap();
    assert!(FileCatalog::new(cfg(&base, Some(sstate.path()))).is_err());
}

#[tokio::test]
async fn bootstraps_from_snapshot_and_falls_back_to_mirror() {
    let out = tempfile::tempdir().unwrap();
    let pstate = tempfile::tempdir().unwrap();
    let p = publisher(out.path(), pstate.path());
    for i in 1..=4 {
        publish(&p, &format!("ts/v1/index/model/org/m{i}"), "{}").await;
    }
    // Primary is broken; the mirror serves only snapshot 4 and no deltas 1..=4,
    // so success proves the snapshot bootstrap path.
    let primary = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&primary)
        .await;
    let mirror = MockServer::start().await;
    for d in 1..=4 {
        std::fs::remove_file(out.path().join(format!("main/deltas/{d:010}.json"))).unwrap();
    }
    mount_dir(&mirror, out.path(), "/m").await;
    let mut c = cfg(&format!("{}/", primary.uri()), None);
    c.mirror_urls = vec![format!("{}/m", mirror.uri())];
    let sub = FileCatalog::new(c).unwrap();
    let r = sub.sync().await.unwrap();
    assert_eq!(r.to, 4);
    assert!(r.source.contains("/m/"));
    assert_eq!(r.skipped.len(), 1);
    assert_eq!(sub.state().root, p.state().root);
}

#[tokio::test]
async fn stale_mirror_is_skipped_and_tampered_head_rejected() {
    let out = tempfile::tempdir().unwrap();
    let pstate = tempfile::tempdir().unwrap();
    let p = publisher(out.path(), pstate.path());
    publish(&p, "ts/v1/catalog", "{}").await;
    let old = tempfile::tempdir().unwrap();
    copy_dir(out.path(), old.path());
    publish(&p, "ts/v1/index/model/org/x", "{}").await;

    // Subscriber already at revision 2.
    let fresh = MockServer::start().await;
    mount_dir(&fresh, out.path(), "").await;
    let sstate = tempfile::tempdir().unwrap();
    let sub = FileCatalog::new(cfg(&format!("{}/", fresh.uri()), Some(sstate.path()))).unwrap();
    sub.sync().await.unwrap();

    // Now the primary serves an old head (rollback) and a mirror serves a
    // tampered head; both are skipped and the error says why.
    let stale = MockServer::start().await;
    mount_dir(&stale, old.path(), "").await;
    let evil = MockServer::start().await;
    let mut head = std::fs::read(out.path().join("main/head.json")).unwrap();
    head[10] ^= 1;
    Mock::given(method("GET"))
        .and(path(format!("/main/{HEAD_PATH}")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(head))
        .mount(&evil)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/main/{HEAD_PATH}.minisig")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_bytes(std::fs::read(out.path().join("main/head.json.minisig")).unwrap()),
        )
        .mount(&evil)
        .await;
    let mut c = cfg(&format!("{}/", stale.uri()), Some(sstate.path()));
    c.mirror_urls = vec![format!("{}/", evil.uri())];
    let sub = FileCatalog::new(c).unwrap();
    let err = sub.sync().await.unwrap_err().to_string();
    assert!(err.contains("rollback"), "{err}");
    assert!(err.contains("signature"), "{err}");
    assert_eq!(sub.state().revision, 2);
}

#[tokio::test]
async fn empty_catalog_and_head_resign() {
    let server = MockServer::start().await;
    let sub = FileCatalog::new(cfg(&format!("{}/", server.uri()), None)).unwrap();
    assert!(matches!(sub.sync().await, Err(CatalogError::NotFound(_))));

    let out = tempfile::tempdir().unwrap();
    let pstate = tempfile::tempdir().unwrap();
    let p = FileCatalog::new(cfg(&format!("{}/", server.uri()), Some(pstate.path())))
        .unwrap()
        .with_signer(key())
        .unwrap()
        .with_sink(Arc::new(DirSink::new(out.path())));
    p.sync_for_publish().await.unwrap();
    assert!(p.resign_head().await.is_err(), "nothing to re-sign yet");
    publish(&p, "ts/v1/catalog", "{}").await;
    let h1 = p.resign_head_at(1_000).await.unwrap();
    assert_eq!(h1.expires_unix, 1_000 + 7 * 24 * 3600);
    assert_eq!(h1.revision, 1);
    assert!(h1.snapshot.is_some());
    let snap = p.snapshot_now().await.unwrap();
    assert_eq!(snap.revision, 1);
}

fn copy_dir(from: &Path, to: &Path) {
    for e in std::fs::read_dir(from).unwrap() {
        let p = e.unwrap().path();
        let dst = to.join(p.file_name().unwrap());
        if p.is_dir() {
            std::fs::create_dir_all(&dst).unwrap();
            copy_dir(&p, &dst);
        } else {
            std::fs::copy(&p, &dst).unwrap();
        }
    }
}
