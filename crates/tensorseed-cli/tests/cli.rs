// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! End to end tests of the `tensorseed` binary against local mock servers
//! (no real network): pull from a signed file catalog with webseed and Hub
//! fallback, verify and tamper detection, pull and verify with Hub hashes,
//! gated and tombstone refusals, and the publisher key and catalog commands.

use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;

use sha2::{Digest, Sha256};
use tensorseed_catalog::file::{FileCatalog, FileCatalogConfig};
use tensorseed_catalog::minisign::SecretKey;
use tensorseed_catalog::sink::DirSink;
use tensorseed_catalog::{CatalogBackend, CatalogId, CatalogOp};
use tensorseed_core::keys::{RepoRef, RepoType, catalog_info_key};
use tensorseed_core::manifest::*;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

const C: &str = "0123456789abcdef0123456789abcdef01234567";
const C2: &str = "89abcdef0123456789abcdef0123456789abcdef";

fn key() -> SecretKey {
    SecretKey::from_seed(&[11; 32])
}

fn sha256(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

fn git_sha1(b: &[u8]) -> String {
    tensorseed_core::hash::git_blob_sha1_bytes(b)
}

struct Fixture {
    files: Vec<(String, Vec<u8>)>,
}

impl Fixture {
    fn new() -> Self {
        let big: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
        Self {
            files: vec![
                ("config.json".into(), br#"{"a":1}"#.to_vec()),
                ("model.safetensors".into(), big),
                ("pytorch_model.bin".into(), b"pickled".to_vec()),
            ],
        }
    }

    fn get(&self, p: &str) -> &[u8] {
        &self.files.iter().find(|(n, _)| n == p).unwrap().1
    }
}

fn manifest(fx: &Fixture, repo: &str, commit: &str, webseed: &str) -> Manifest {
    let files: Vec<FileEntry> = fx
        .files
        .iter()
        .map(|(p, b)| FileEntry {
            path: p.clone(),
            size: b.len() as u64,
            sha256: sha256(b),
            hf_lfs_oid: None,
            bt_v2_pieces_root: None,
            format: None,
            pickle_risk: FileEntry::path_has_pickle_risk(p),
        })
        .collect();
    Manifest {
        schema: SCHEMA_MANIFEST.into(),
        repo: RepoInfo {
            repo_type: RepoType::Model,
            id: repo.into(),
            hf_url: format!("https://huggingface.co/{repo}"),
        },
        hf_commit: commit.into(),
        hf_last_modified: None,
        license: LicenseInfo {
            spdx: Some("apache-2.0".into()),
            hf_tag: Some("apache-2.0".into()),
            class: LicenseClass::Open,
            license_files: vec![],
            attribution: None,
            gated_upstream: false,
        },
        totals: Totals {
            files: files.len() as u64,
            bytes: files.iter().map(|f| f.size).sum(),
        },
        files,
        file_pages: 0,
        torrent: None,
        webseeds: vec![webseed.into()],
        backups: vec![],
        published_at: "2026-11-12T00:00:00Z".into(),
        publisher_tool: "test".into(),
    }
}

/// Publish a catalog with one repo into `out` (served at `<server>/main/...`).
async fn publish_catalog(out: &Path, state: &Path, m: &Manifest, tombstone: bool) {
    let mut cfg = FileCatalogConfig::new("main", "http://unused.invalid/", key().public_key());
    cfg.state_dir = Some(state.to_path_buf());
    let cat = FileCatalog::new(cfg)
        .unwrap()
        .with_signer(key())
        .unwrap()
        .with_sink(Arc::new(DirSink::new(out)));
    let repo = RepoRef::new(RepoType::Model, m.repo.id.clone()).unwrap();
    let j = |v: &dyn erased::Ser| v.canon();
    let latest = LatestPointer {
        schema: SCHEMA_LATEST.into(),
        commit: m.hf_commit.clone(),
        manifest_key: repo.manifest_key(&m.hf_commit).unwrap(),
        updated_at: "2026-11-12T00:00:00Z".into(),
    };
    let index = IndexEntry {
        schema: SCHEMA_INDEX.into(),
        latest_commit: m.hf_commit.clone(),
        license_spdx: Some("apache-2.0".into()),
        total_bytes: m.totals.bytes,
        updated_at: "2026-11-12T00:00:00Z".into(),
        taken_down: tombstone,
    };
    let mut ops = vec![
        CatalogOp::Upsert {
            key: catalog_info_key(),
            value: br#"{"schema":"ts.catalog/1"}"#.to_vec(),
        },
        CatalogOp::Upsert {
            key: repo.manifest_key(&m.hf_commit).unwrap(),
            value: m.to_canonical_json().unwrap(),
        },
        CatalogOp::Upsert {
            key: repo.latest_key(),
            value: j(&latest),
        },
        CatalogOp::Upsert {
            key: repo.index_key(),
            value: j(&index),
        },
    ];
    if tombstone {
        let t = Tombstone {
            schema: SCHEMA_TOMBSTONE.into(),
            reason: TombstoneReason::Legal,
            detail: "test takedown".into(),
            effective_at: "2026-11-12T00:00:00Z".into(),
            revisions: vec![],
        };
        ops.push(CatalogOp::Upsert {
            key: repo.tombstone_key(),
            value: j(&t),
        });
    }
    let id = CatalogId("main".into());
    cat.stage(&id, &ops).await.unwrap();
    cat.commit(&id).await.unwrap();
}

mod erased {
    pub trait Ser {
        fn canon(&self) -> Vec<u8>;
    }
    impl<T: serde::Serialize> Ser for T {
        fn canon(&self) -> Vec<u8> {
            tensorseed_core::canonical::to_canonical_vec(self).unwrap()
        }
    }
}

async fn mount_dir(server: &MockServer, dir: &Path) {
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
            .and(path(format!("/{rel}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(server)
            .await;
    }
}

async fn serve(server: &MockServer, p: &str, body: &[u8]) {
    Mock::given(method("GET"))
        .and(path(p.to_string()))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.to_vec()))
        .mount(server)
        .await;
}

fn write_config(dir: &Path, server: &str, pubkey: &str, extra: &str) -> PathBuf {
    let d = dir.display();
    let text = format!(
        r#"[general]
state_db = "{d}/state.db"
download_dir = "{d}/dl"

[catalog]
backend = "file"
name = "main"
publisher_pubkey = "{pubkey}"
{extra}

[file]
base_url = "{server}/"
state_dir = "{d}/catstate"

[huggingface]
endpoint = "{server}"
token_env = "TS_TEST_NO_SUCH_TOKEN"

[storage]
local_dir = "{d}/publish"
"#
    );
    let p = dir.join("tensorseed.toml");
    std::fs::write(&p, text).unwrap();
    p
}

async fn run(config: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_tensorseed"));
    cmd.arg("--config").arg(config).args(args);
    cmd.env_remove("HF_TOKEN").env("RUST_LOG", "warn");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap();
    eprintln!(
        "$ tensorseed {}\n--- stdout\n{}--- stderr\n{}",
        args.join(" "),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

#[tokio::test(flavor = "multi_thread")]
async fn pull_from_signed_catalog_then_verify_and_detect_tampering() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let fx = Fixture::new();
    let m = manifest(&fx, "org/tiny", C, &format!("{}/t/", server.uri()));
    let pubdir = tmp.path().join("srv");
    publish_catalog(&pubdir, &tmp.path().join("pstate"), &m, false).await;
    mount_dir(&server, &pubdir).await;
    let name = m.content_name();
    // Webseed has config.json only; model.safetensors falls back to the Hub.
    serve(
        &server,
        &format!("/t/{name}/config.json"),
        fx.get("config.json"),
    )
    .await;
    serve(
        &server,
        &format!("/org/tiny/resolve/{C}/model.safetensors"),
        fx.get("model.safetensors"),
    )
    .await;

    let cfg = write_config(
        tmp.path(),
        &server.uri(),
        &key().public_key().to_base64(),
        "",
    );
    let o = run(&cfg, &["pull", "org/tiny"], &[]).await;
    assert_eq!(code(&o), 0);
    let s = stdout(&o);
    assert!(s.contains("skip   pytorch_model.bin"), "{s}");
    assert!(s.contains("ok     config.json (webseed)"), "{s}");
    assert!(s.contains("ok     model.safetensors (huggingface)"), "{s}");
    let dir = tmp.path().join("dl/org/tiny");
    assert_eq!(
        std::fs::read(dir.join("model.safetensors")).unwrap(),
        fx.get("model.safetensors")
    );
    assert!(!dir.join("pytorch_model.bin").exists());
    assert!(dir.join(".tensorseed/manifest.json").exists());
    let dir_s = dir.to_str().unwrap();

    let o = run(&cfg, &["verify", dir_s], &[]).await;
    assert_eq!(code(&o), 0);
    assert!(stdout(&o).contains("3 files checked, 3 passed, 0 failed"));

    let o = run(&cfg, &["verify", "org/tiny"], &[]).await;
    assert_eq!(code(&o), 0, "repo mode verifies against the synced catalog");
    assert!(stdout(&o).contains("signed catalog"));

    let o = run(&cfg, &["verify", "--strict", dir_s], &[]).await;
    assert_eq!(code(&o), 3, "strict counts the skipped pickle file");

    let o = run(&cfg, &["list"], &[]).await;
    assert_eq!(code(&o), 0);
    assert!(stdout(&o).contains("model/org/tiny"));

    // Tamper with one byte: verify fails with exit 3 and names the file.
    let p = dir.join("model.safetensors");
    let mut b = std::fs::read(&p).unwrap();
    b[1000] ^= 0xff;
    std::fs::write(&p, &b).unwrap();
    let o = run(&cfg, &["verify", dir_s], &[]).await;
    assert_eq!(code(&o), 3);
    assert!(
        stdout(&o).contains("FAIL  HASH MISMATCH  model.safetensors"),
        "{}",
        stdout(&o)
    );

    // Pulling again repairs it.
    let o = run(&cfg, &["pull", "org/tiny", "--no-wait"], &[]).await;
    assert_eq!(code(&o), 0);
    let o = run(&cfg, &["verify", dir_s], &[]).await;
    assert_eq!(code(&o), 0);

    // A forged manifest file is rejected.
    let forged = tensorseed_catalog::signed::SignedEntry::sign(
        &SecretKey::from_seed(&[99; 32]),
        "main",
        &RepoRef::new(RepoType::Model, "org/tiny")
            .unwrap()
            .manifest_key(C)
            .unwrap(),
        &m.to_canonical_json().unwrap(),
    )
    .unwrap();
    let fp = tmp.path().join("forged.json");
    std::fs::write(&fp, forged.to_bytes().unwrap()).unwrap();
    let o = run(
        &cfg,
        &["verify", dir_s, "--manifest-file", fp.to_str().unwrap()],
        &[],
    )
    .await;
    assert_eq!(code(&o), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn tombstoned_repo_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let fx = Fixture::new();
    let m = manifest(&fx, "org/gone", C, &format!("{}/t/", server.uri()));
    let pubdir = tmp.path().join("srv");
    publish_catalog(&pubdir, &tmp.path().join("pstate"), &m, true).await;
    mount_dir(&server, &pubdir).await;
    let cfg = write_config(
        tmp.path(),
        &server.uri(),
        &key().public_key().to_base64(),
        "",
    );
    let o = run(&cfg, &["pull", "org/gone"], &[]).await;
    assert_eq!(code(&o), 1);
    assert!(String::from_utf8_lossy(&o.stderr).contains("taken down"));
}

async fn mount_hf_repo(server: &MockServer, repo: &str, gated: serde_json::Value, fx: &Fixture) {
    let info = serde_json::json!({
        "id": repo, "sha": C2, "gated": gated, "private": false,
        "cardData": {"license": "apache-2.0"}, "tags": ["license:apache-2.0"]
    });
    Mock::given(method("GET"))
        .and(path_regex(format!("^/api/models/{repo}(/revision/.*)?$")))
        .respond_with(ResponseTemplate::new(200).set_body_json(info))
        .mount(server)
        .await;
    let tree: Vec<serde_json::Value> = fx
        .files
        .iter()
        .map(|(p, b)| {
            if p.ends_with(".json") {
                serde_json::json!({"type": "file", "path": p, "size": b.len(), "oid": git_sha1(b)})
            } else {
                serde_json::json!({"type": "file", "path": p, "size": 134, "oid": "0".repeat(40),
                    "lfs": {"oid": sha256(b), "size": b.len(), "pointerSize": 134}})
            }
        })
        .collect();
    Mock::given(method("GET"))
        .and(path(format!("/api/models/{repo}/tree/{C2}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(tree))
        .mount(server)
        .await;
    for (p, b) in &fx.files {
        serve(server, &format!("/{repo}/resolve/{C2}/{p}"), b).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pull_and_verify_with_hub_hashes() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let fx = Fixture::new();
    mount_hf_repo(&server, "org/hf", serde_json::json!(false), &fx).await;
    mount_hf_repo(&server, "org/locked", serde_json::json!("manual"), &fx).await;
    let cfg = write_config(tmp.path(), &server.uri(), "", "");

    let o = run(
        &cfg,
        &["pull", "--from-hf", "org/hf", "--allow-pickle"],
        &[],
    )
    .await;
    assert_eq!(code(&o), 0);
    let dir = tmp.path().join("dl/org/hf");
    assert_eq!(
        std::fs::read(dir.join("pytorch_model.bin")).unwrap(),
        b"pickled"
    );
    assert!(dir.join(".tensorseed/NOTICE").exists());

    let o = run(&cfg, &["verify", "--hf", "org/hf"], &[]).await;
    assert_eq!(code(&o), 0);
    assert!(stdout(&o).contains("Hugging Face hashes"));
    let o = run(&cfg, &["verify", dir.to_str().unwrap()], &[]).await;
    assert_eq!(code(&o), 0, "pull.json records the Hub hashes");

    std::fs::write(dir.join("config.json"), b"{\"a\":2}").unwrap();
    let o = run(&cfg, &["verify", "--hf", dir.to_str().unwrap()], &[]).await;
    assert_eq!(code(&o), 3);
    assert!(stdout(&o).contains("HASH MISMATCH  config.json"));

    let o = run(&cfg, &["pull", "--from-hf", "org/locked"], &[]).await;
    assert_eq!(code(&o), 1);
    assert!(String::from_utf8_lossy(&o.stderr).contains("gated"));
    assert!(!tmp.path().join("dl/org/locked").exists());

    let o = run(&cfg, &["jobs"], &[]).await;
    assert_eq!(code(&o), 0);
    assert!(stdout(&o).contains("model org/hf@"));
}

#[tokio::test(flavor = "multi_thread")]
async fn keygen_create_export_import_on_local_storage() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await; // empty: no catalog published yet
    let pw = [("TENSORSEED_KEY_PASSWORD", "correct horse")];
    let cfg0 = write_config(tmp.path(), &server.uri(), "", "");
    let keys = tmp.path().join("keys");
    let o = run(
        &cfg0,
        &[
            "catalog",
            "keygen",
            "--out-dir",
            keys.to_str().unwrap(),
            "--name",
            "pub",
        ],
        &pw,
    )
    .await;
    assert_eq!(code(&o), 0);
    let pubfile = std::fs::read_to_string(keys.join("pub.pub")).unwrap();
    let pk = pubfile.lines().nth(1).unwrap().trim().to_string();
    assert!(
        std::fs::read_to_string(keys.join("pub.key"))
            .unwrap()
            .contains("encrypted")
    );

    let seck = format!(
        "publisher_seckey_file = \"{}\"",
        keys.join("pub.key").display()
    );
    let a = tmp.path().join("a");
    std::fs::create_dir_all(&a).unwrap();
    let cfg = write_config(&a, &server.uri(), &pk, &seck);
    let o = run(&cfg, &["catalog", "create"], &pw).await;
    assert_eq!(code(&o), 0);
    assert!(a.join("publish/main/head.json").exists());
    let o = run(&cfg, &["catalog", "create"], &pw).await;
    assert_ne!(code(&o), 0, "second create refuses");

    // Wrong password fails cleanly.
    let o = run(
        &cfg,
        &["catalog", "resign-head"],
        &[("TENSORSEED_KEY_PASSWORD", "nope")],
    )
    .await;
    assert_eq!(code(&o), 1);

    // Serve the published directory and read it back as a subscriber.
    let server2 = MockServer::start().await;
    mount_dir(&server2, &a.join("publish")).await;
    let b = tmp.path().join("b");
    std::fs::create_dir_all(&b).unwrap();
    let cfg_b = write_config(&b, &server2.uri(), &pk, &seck);
    let o = run(&cfg_b, &["catalog", "info"], &[]).await;
    assert_eq!(code(&o), 0);
    assert!(stdout(&o).contains("revision:  1"));
    let bundle = b.join("bundle.json");
    let o = run(
        &cfg_b,
        &[
            "catalog",
            "export",
            "--out",
            bundle.to_str().unwrap(),
            "--no-wait",
        ],
        &[],
    )
    .await;
    assert_eq!(code(&o), 0);

    // Import into a fresh, unpublished catalog location.
    let c = tmp.path().join("c");
    std::fs::create_dir_all(&c).unwrap();
    let cfg_c = write_config(&c, &server.uri(), &pk, &seck);
    let o = run(
        &cfg_c,
        &["catalog", "import", bundle.to_str().unwrap(), "--dry-run"],
        &[],
    )
    .await;
    assert_eq!(code(&o), 0);
    assert!(!c.join("publish").exists());
    let o = run(
        &cfg_c,
        &["catalog", "import", bundle.to_str().unwrap()],
        &pw,
    )
    .await;
    assert_eq!(code(&o), 0);
    assert!(stdout(&o).contains("revision 1"));
    assert!(c.join("publish/main/deltas").exists());
}
