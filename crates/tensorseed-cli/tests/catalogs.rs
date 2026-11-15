// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Multi-catalog subscription: agreement, hash conflicts, per-catalog
//! takedowns, and `catalog list` / `add` / `remove`.

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
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const C: &str = "0123456789abcdef0123456789abcdef01234567";

fn key(seed: u8) -> SecretKey {
    SecretKey::from_seed(&[seed; 32])
}

fn sha256(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

fn webseed_path(repo: &str, commit: &str, file: &str) -> String {
    format!("/t/{}/{file}", TorrentInfo::name_for(repo, commit))
}

fn manifest(repo: &str, commit: &str, files: &[(&str, &[u8])], webseed: &str) -> Manifest {
    let files: Vec<FileEntry> = files
        .iter()
        .map(|(p, b)| FileEntry {
            path: (*p).into(),
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
        published_at: "2026-11-13T00:00:00Z".into(),
        publisher_tool: "test".into(),
    }
}

async fn publish(
    name: &str,
    sk: &SecretKey,
    out: &Path,
    state: &Path,
    m: &Manifest,
    tombstone: bool,
) {
    let mut cfg = FileCatalogConfig::new(name, "http://unused.invalid/", sk.public_key());
    cfg.state_dir = Some(state.to_path_buf());
    let cat = FileCatalog::new(cfg)
        .unwrap()
        .with_signer(sk.clone())
        .unwrap()
        .with_sink(Arc::new(DirSink::new(out)));
    let repo = RepoRef::new(RepoType::Model, m.repo.id.clone()).unwrap();
    let canon = |v: &dyn erased::Ser| v.canon();
    let latest = LatestPointer {
        schema: SCHEMA_LATEST.into(),
        commit: m.hf_commit.clone(),
        manifest_key: repo.manifest_key(&m.hf_commit).unwrap(),
        updated_at: "2026-11-13T00:00:00Z".into(),
    };
    let index = IndexEntry {
        schema: SCHEMA_INDEX.into(),
        latest_commit: m.hf_commit.clone(),
        license_spdx: Some("apache-2.0".into()),
        total_bytes: m.totals.bytes,
        updated_at: "2026-11-13T00:00:00Z".into(),
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
            value: canon(&latest),
        },
        CatalogOp::Upsert {
            key: repo.index_key(),
            value: canon(&index),
        },
    ];
    if tombstone {
        let t = Tombstone {
            schema: SCHEMA_TOMBSTONE.into(),
            reason: TombstoneReason::Legal,
            detail: "test takedown".into(),
            effective_at: "2026-11-13T00:00:00Z".into(),
            revisions: vec![],
        };
        ops.push(CatalogOp::Upsert {
            key: repo.tombstone_key(),
            value: canon(&t),
        });
    }
    let id = CatalogId(name.into());
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

/// A path for a TOML basic string, with forward slashes: a Windows path
/// would have its backslashes read as escapes. The loader turns them back
/// into native separators.
fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn write_config(dir: &Path, server: &str, entries: &str) -> PathBuf {
    let d = slash(dir);
    let text = format!(
        r#"[general]
state_db = "{d}/state.db"
download_dir = "{d}/dl"

[catalog]
backend = "file"
chain_check = "off"

[file]
base_url = "{server}/"
state_dir = "{d}/catstate"

[huggingface]
endpoint = "{server}"
token_env = "TS_TEST_NO_SUCH_TOKEN"

[storage]
local_dir = "{d}/publish"

{entries}
"#
    );
    let p = dir.join("tensorseed.toml");
    std::fs::write(&p, text).unwrap();
    p
}

fn entry(name: &str, label: &str, pk: &str, priority: i64) -> String {
    format!(
        r#"
[[catalogs]]
name = "{name}"
label = "{label}"
publisher_pubkey = "{pk}"
base_url = ""
priority = {priority}
"#
    )
}

async fn run(config: &Path, args: &[&str]) -> Output {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_tensorseed"));
    cmd.arg("--config").arg(config).args(args);
    cmd.env_remove("HF_TOKEN")
        .env_remove("HF_TOKEN_PATH")
        .env_remove("HF_HOME")
        .env("RUST_LOG", "warn")
        .env("HOME", config.parent().unwrap())
        .env("USERPROFILE", config.parent().unwrap())
        .env("XDG_CONFIG_HOME", config.parent().unwrap())
        .env("APPDATA", config.parent().unwrap())
        .env("LOCALAPPDATA", config.parent().unwrap());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
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

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn catalogs_block(server: &str, a_pk: &str, b_pk: &str) -> String {
    // base_url is filled from [file] when empty in resolve? Wait, empty
    // base_url inherits file.base_url which we set to server/. Good.
    let _ = server;
    format!(
        "{}{}",
        entry("alpha", "Alpha", a_pk, 0),
        entry("beta", "Beta", b_pk, 10)
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn list_and_pull_when_catalogs_agree() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let a = key(11);
    let b = key(22);
    let files = [("config.json", br#"{}"# as &[u8])];
    let web = format!("{}/t/", server.uri());
    let m = manifest("org/tiny", C, &files, &web);
    let pubdir = tmp.path().join("srv");
    publish("alpha", &a, &pubdir, &tmp.path().join("st-a"), &m, false).await;
    publish("beta", &b, &pubdir, &tmp.path().join("st-b"), &m, false).await;
    mount_dir(&server, &pubdir).await;
    Mock::given(method("GET"))
        .and(path(webseed_path("org/tiny", C, "config.json")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(files[0].1.to_vec()))
        .mount(&server)
        .await;
    let cfg = write_config(
        tmp.path(),
        &server.uri(),
        &catalogs_block(
            &server.uri(),
            &a.public_key().to_base64(),
            &b.public_key().to_base64(),
        ),
    );
    let listed = run(&cfg, &["list"]).await;
    assert_eq!(code(&listed), 0, "{}", stderr(&listed));
    let out = stdout(&listed);
    assert!(out.contains("org/tiny"), "{out}");
    assert!(
        out.contains("2 of 2 catalogs agree"),
        "expected agreement line, got:\n{out}"
    );

    let pull = run(
        &cfg,
        &[
            "pull",
            "org/tiny",
            "--no-swarm",
            "--out",
            &tmp.path().join("dl/tiny").display().to_string(),
        ],
    )
    .await;
    assert_eq!(code(&pull), 0, "{}", stderr(&pull));
    let got = tmp.path().join("dl/tiny/config.json");
    assert_eq!(std::fs::read(&got).unwrap(), files[0].1);
}

#[tokio::test(flavor = "multi_thread")]
async fn conflict_is_a_hard_error_unless_catalog_flag() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let a = key(11);
    let b = key(22);
    let web = format!("{}/t/", server.uri());
    let ma = manifest("org/tiny", C, &[("config.json", br#"{"v":1}"#)], &web);
    let mb = manifest("org/tiny", C, &[("config.json", br#"{"v":2}"#)], &web);
    let pubdir = tmp.path().join("srv");
    publish("alpha", &a, &pubdir, &tmp.path().join("st-a"), &ma, false).await;
    publish("beta", &b, &pubdir, &tmp.path().join("st-b"), &mb, false).await;
    mount_dir(&server, &pubdir).await;
    let cfg = write_config(
        tmp.path(),
        &server.uri(),
        &catalogs_block(
            &server.uri(),
            &a.public_key().to_base64(),
            &b.public_key().to_base64(),
        ),
    );
    let _ = run(&cfg, &["list"]).await;
    let listed = run(&cfg, &["list"]).await;
    assert_eq!(code(&listed), 0, "{}", stderr(&listed));
    let text = format!("{}{}", stdout(&listed), stderr(&listed));
    assert!(text.contains("CONFLICT"), "{text}");

    let pull = run(&cfg, &["pull", "org/tiny", "--no-swarm"]).await;
    assert_ne!(code(&pull), 0);
    let err = format!("{}{}", stdout(&pull), stderr(&pull));
    assert!(err.contains("CONFLICT"), "{err}");
    assert!(err.contains("--catalog"), "{err}");

    Mock::given(method("GET"))
        .and(path(webseed_path("org/tiny", C, "config.json")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(br#"{"v":1}"#.to_vec()))
        .mount(&server)
        .await;
    let ok = run(
        &cfg,
        &[
            "pull",
            "org/tiny",
            "--catalog",
            "alpha",
            "--no-swarm",
            "--out",
            &tmp.path().join("dl/a").display().to_string(),
        ],
    )
    .await;
    assert_eq!(code(&ok), 0, "{}", stderr(&ok));
    assert_eq!(
        std::fs::read(tmp.path().join("dl/a/config.json")).unwrap(),
        br#"{"v":1}"#
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn takedown_in_one_catalog_does_not_block_the_other() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let a = key(11);
    let b = key(22);
    let files = [("config.json", br#"{}"# as &[u8])];
    let web = format!("{}/t/", server.uri());
    let m = manifest("org/tiny", C, &files, &web);
    let pubdir = tmp.path().join("srv");
    publish("alpha", &a, &pubdir, &tmp.path().join("st-a"), &m, true).await;
    publish("beta", &b, &pubdir, &tmp.path().join("st-b"), &m, false).await;
    mount_dir(&server, &pubdir).await;
    Mock::given(method("GET"))
        .and(path(webseed_path("org/tiny", C, "config.json")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(files[0].1.to_vec()))
        .mount(&server)
        .await;
    let cfg = write_config(
        tmp.path(),
        &server.uri(),
        &catalogs_block(
            &server.uri(),
            &a.public_key().to_base64(),
            &b.public_key().to_base64(),
        ),
    );
    let _ = run(&cfg, &["list"]).await;
    let pull = run(
        &cfg,
        &[
            "pull",
            "org/tiny",
            "--no-swarm",
            "--out",
            &tmp.path().join("dl/tiny").display().to_string(),
        ],
    )
    .await;
    assert_eq!(code(&pull), 0, "{}", stderr(&pull));
    let text = format!("{}{}", stdout(&pull), stderr(&pull));
    assert!(
        text.contains("taken down in catalog") || text.contains("Alpha"),
        "expected a takedown notice, got:\n{text}"
    );
    assert_eq!(
        std::fs::read(tmp.path().join("dl/tiny/config.json")).unwrap(),
        files[0].1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn catalog_list_add_and_remove() {
    let tmp = tempfile::tempdir().unwrap();
    let a = key(11);
    let b = key(22);
    let cfg = write_config(
        tmp.path(),
        "https://catalog.example.org",
        &entry("alpha", "Alpha", &a.public_key().to_base64(), 0),
    );
    let listed = run(&cfg, &["catalog", "list"]).await;
    // May error if no local state, but should still print the name.
    let text = format!("{}{}", stdout(&listed), stderr(&listed));
    assert!(text.contains("alpha"), "{text}");

    let add = run(
        &cfg,
        &[
            "catalog",
            "add",
            "--name",
            "beta",
            "--publisher-pubkey",
            &b.public_key().to_base64(),
            "--base-url",
            "https://beta.example.org/",
            "--label",
            "Beta",
            "--priority",
            "10",
        ],
    )
    .await;
    assert_eq!(code(&add), 0, "{}", stderr(&add));
    let file = std::fs::read_to_string(&cfg).unwrap();
    assert!(file.contains("name = \"beta\""), "{file}");
    assert!(file.contains("https://beta.example.org/"), "{file}");

    let rm = run(&cfg, &["catalog", "remove", "beta"]).await;
    assert_eq!(code(&rm), 0, "{}", stderr(&rm));
    let file = std::fs::read_to_string(&cfg).unwrap();
    assert!(!file.contains("name = \"beta\""), "{file}");
    assert!(file.contains("name = \"alpha\""), "{file}");
}

/// A sealed, unsigned `ts.served-weights/1` claim for `m` in catalog
/// `name` signed by `pk_id`.
fn claim_for(m: &Manifest, name: &str, pk_id: &str) -> Vec<u8> {
    let repo = RepoRef::new(RepoType::Model, m.repo.id.clone()).unwrap();
    let files: Vec<serde_json::Value> = m
        .files
        .iter()
        .map(|f| serde_json::json!({"path": f.path, "size": f.size, "sha256": f.sha256}))
        .collect();
    let v = serde_json::json!({
        "schema": "ts.served-weights/1",
        "signed": false,
        "catalog": {
            "name": name,
            "publisher_key_id": pk_id,
            "manifest_key": repo.manifest_key(&m.hf_commit).unwrap(),
            "manifest_sha256": sha256(&m.to_canonical_json().unwrap()),
        },
        "type": "model",
        "repo": m.repo.id,
        "revision": m.hf_commit,
        "license": {"id": "apache-2.0", "class": "open", "commercial_serving": "allowed", "rights_asserted": false},
        "runtime": "vllm",
        "engine_version": "vllm 0.0.0-test",
        "entrypoint": {"kind": "directory"},
        "served_model_id": "tiny",
        "base_url": "http://127.0.0.1:8090/v1",
        "public_base_url": "https://llm.example.com/v1",
        "files": files,
        // RFC 8032 test vector 1 public key.
        "response_key": {"alg": "ed25519", "pubkey": "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"},
        "logprobs": "required",
        "started_at": "2026-11-14T00:00:00Z",
        "created_at": "2026-11-14T00:00:00Z",
        "claim_sha256": "",
    });
    let mut c: tensorseed_app::signing::ServedClaim = serde_json::from_value(v).unwrap();
    c.seal().unwrap();
    serde_json::to_vec_pretty(&c).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn check_claim_uses_catalogs_added_with_catalog_add() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let a = key(11);
    let b = key(22);
    let web = format!("{}/t/", server.uri());
    let m = manifest("org/tiny", C, &[("config.json", br#"{}"#)], &web);
    let pubdir = tmp.path().join("srv");
    publish("alpha", &a, &pubdir, &tmp.path().join("st-a"), &m, false).await;
    publish("beta", &b, &pubdir, &tmp.path().join("st-b"), &m, false).await;
    mount_dir(&server, &pubdir).await;
    let cfg = write_config(
        tmp.path(),
        &server.uri(),
        &entry("alpha", "Alpha", &a.public_key().to_base64(), 0),
    );
    let b_id = b.public_key().key_id_hex();
    let claim = tmp.path().join("claim.json");
    std::fs::write(&claim, claim_for(&m, "beta", &b_id)).unwrap();
    let c = claim.to_str().unwrap();
    let args = [
        "check-claim",
        c,
        "--expect-url",
        "https://llm.example.com/v1",
        "--json",
    ];

    // beta is not trusted yet.
    let o = run(&cfg, &args).await;
    assert_eq!(code(&o), 1, "{}", stderr(&o));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert!(v["checks"].to_string().contains("not one you trust"), "{v}");

    // After `catalog add`, the claim matches beta's signed manifest.
    let add = run(
        &cfg,
        &[
            "catalog",
            "add",
            "--name",
            "beta",
            "--publisher-pubkey",
            &b.public_key().to_base64(),
            "--base-url",
            &format!("{}/", server.uri()),
        ],
    )
    .await;
    assert_eq!(code(&add), 0, "{}", stderr(&add));
    let o = run(&cfg, &args).await;
    assert_eq!(code(&o), 0, "{}", stdout(&o));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["verdict"], "pass");
    assert_eq!(v["catalog"], "beta");
    assert_eq!(v["freshness"]["synced"], true);
    assert_eq!(v["license"]["commercial_serving"], "allowed");

    // The beta claim with alpha's key id: the publisher does not match.
    std::fs::write(&claim, claim_for(&m, "beta", &a.public_key().key_id_hex())).unwrap();
    let o = run(&cfg, &args).await;
    assert_eq!(code(&o), 1);
    assert!(stdout(&o).contains("is pinned to"), "{}", stdout(&o));
    // An alpha claim checks against alpha.
    std::fs::write(&claim, claim_for(&m, "alpha", &a.public_key().key_id_hex())).unwrap();
    let o = run(&cfg, &args).await;
    assert_eq!(code(&o), 0, "{}", stdout(&o));
}

#[tokio::test(flavor = "multi_thread")]
async fn catalog_export_works_for_added_catalogs() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let a = key(11);
    let b = key(22);
    let web = format!("{}/t/", server.uri());
    let m = manifest("org/tiny", C, &[("config.json", br#"{}"#)], &web);
    let pubdir = tmp.path().join("srv");
    publish("alpha", &a, &pubdir, &tmp.path().join("st-a"), &m, false).await;
    publish("beta", &b, &pubdir, &tmp.path().join("st-b"), &m, false).await;
    mount_dir(&server, &pubdir).await;
    let cfg = write_config(
        tmp.path(),
        &server.uri(),
        &entry("alpha", "Alpha", &a.public_key().to_base64(), 0),
    );
    let read = |p: &Path| -> tensorseed_catalog::export::ExportBundle {
        serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
    };

    // The default subscription exports without --catalog.
    let out_a = tmp.path().join("alpha.json");
    let o = run(
        &cfg,
        &["catalog", "export", "--out", out_a.to_str().unwrap()],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let bundle = read(&out_a);
    assert_eq!(bundle.catalog, "alpha");
    bundle.verify(&a.public_key()).unwrap();

    // Not followed yet: a clear error.
    let out_b = tmp.path().join("beta.json");
    let args = [
        "catalog",
        "export",
        "--catalog",
        "beta",
        "--out",
        out_b.to_str().unwrap(),
    ];
    let o = run(&cfg, &args).await;
    assert_ne!(code(&o), 0);
    assert!(
        stderr(&o).contains("no subscribed catalog named"),
        "{}",
        stderr(&o)
    );

    // After `catalog add`, its signed entries export and verify against
    // beta's key (not alpha's), and the label works too.
    let add = run(
        &cfg,
        &[
            "catalog",
            "add",
            "--name",
            "beta",
            "--label",
            "Beta",
            "--publisher-pubkey",
            &b.public_key().to_base64(),
            "--base-url",
            &format!("{}/", server.uri()),
        ],
    )
    .await;
    assert_eq!(code(&add), 0, "{}", stderr(&add));
    let o = run(&cfg, &args).await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(stdout(&o).contains("exported"), "{}", stdout(&o));
    let bundle = read(&out_b);
    assert_eq!(bundle.catalog, "beta");
    assert_eq!(bundle.publisher_pubkey, b.public_key().to_base64());
    assert!(!bundle.entries.is_empty());
    bundle.verify(&b.public_key()).unwrap();
    assert!(bundle.verify(&a.public_key()).is_err());
    let out_l = tmp.path().join("label.json");
    let o = run(
        &cfg,
        &[
            "catalog",
            "export",
            "--catalog",
            "Beta",
            "--no-wait",
            "--out",
            out_l.to_str().unwrap(),
        ],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert_eq!(read(&out_l).entries.len(), bundle.entries.len());
}

#[tokio::test(flavor = "multi_thread")]
async fn follow_lists_create_sign_follow_and_refuse_rollbacks() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let a = key(11);
    let b = key(22);
    let web = format!("{}/t/", server.uri());
    let m = manifest("org/tiny", C, &[("config.json", br#"{}"#)], &web);
    let pubdir = tmp.path().join("srv");
    publish("alpha", &a, &pubdir, &tmp.path().join("st-a"), &m, false).await;
    publish("beta", &b, &pubdir, &tmp.path().join("st-b"), &m, false).await;
    let apk = a.public_key().to_base64();
    let bpk = b.public_key().to_base64();

    // The list publisher follows alpha and beta and has its own key.
    let pdir = tmp.path().join("lister");
    std::fs::create_dir_all(&pdir).unwrap();
    let pcfg = write_config(
        &pdir,
        &server.uri(),
        &format!(
            "{}{}",
            entry("alpha", "Alpha", &apk, 0),
            entry("beta", "Beta", &bpk, 1)
        ),
    );
    let kg = run(
        &pcfg,
        &[
            "catalog",
            "keygen",
            "--out-dir",
            pdir.to_str().unwrap(),
            "--name",
            "lister",
            "--no-password",
        ],
    )
    .await;
    assert_eq!(code(&kg), 0, "{}", stderr(&kg));
    let seckey = pdir.join("lister.key");
    let list_pk = std::fs::read_to_string(pdir.join("lister.pub"))
        .unwrap()
        .lines()
        .nth(1)
        .unwrap()
        .to_string();
    let list = pubdir.join("trusted.json");
    let l = list.to_str().unwrap();
    let o = run(
        &pcfg,
        &[
            "catalog",
            "follow-list",
            "create",
            "--name",
            "trusted",
            "--out",
            l,
        ],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let sign = |rev: Option<&str>| {
        let mut args = vec![
            "catalog",
            "follow-list",
            "sign",
            l,
            "--key",
            seckey.to_str().unwrap(),
        ];
        if let Some(r) = rev {
            args.extend(["--rev", r]);
        }
        let pcfg = pcfg.clone();
        let args: Vec<String> = args.into_iter().map(String::from).collect();
        async move {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            run(&pcfg, &args).await
        }
    };
    let o = sign(None).await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(
        stdout(&o).contains("signed trusted rev 1"),
        "{}",
        stdout(&o)
    );
    assert!(stdout(&o).contains(&list_pk), "{}", stdout(&o));
    let rev1 = (
        std::fs::read(&list).unwrap(),
        std::fs::read(format!("{l}.minisig")).unwrap(),
    );
    mount_dir(&server, &pubdir).await;

    // Anyone can verify it, from the URL, with the list key only.
    let url = format!("{}/trusted.json", server.uri());
    let o = run(
        &pcfg,
        &[
            "catalog",
            "follow-list",
            "verify",
            &url,
            "--pubkey",
            &list_pk,
            "--json",
        ],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["schema"], "ts.follow-list/1");
    assert_eq!(v["rev"], 1);
    assert_eq!(v["catalogs"][1]["publisher_pubkey"], bpk.as_str());
    let o = run(
        &pcfg,
        &["catalog", "follow-list", "verify", &url, "--pubkey", &apk],
    )
    .await;
    assert_ne!(code(&o), 0);

    // A follower that trusts alpha follows the list from its URL.
    let fdir = tmp.path().join("follower");
    std::fs::create_dir_all(&fdir).unwrap();
    let fcfg = write_config(&fdir, &server.uri(), &entry("alpha", "Alpha", &apk, 0));
    let before = std::fs::read_to_string(&fcfg).unwrap();
    let o = run(&fcfg, &["catalog", "follow-list", "follow", &url]).await;
    assert_ne!(code(&o), 0);
    assert!(stderr(&o).contains("--pubkey"), "{}", stderr(&o));
    let o = run(
        &fcfg,
        &[
            "catalog",
            "follow-list",
            "follow",
            &url,
            "--pubkey",
            &list_pk,
        ],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let out = stdout(&o);
    assert!(out.contains("add       beta"), "{out}");
    assert!(out.contains("keep      alpha"), "{out}");
    assert!(out.contains("nothing written"), "{out}");
    assert_eq!(std::fs::read_to_string(&fcfg).unwrap(), before);
    let o = run(
        &fcfg,
        &[
            "catalog",
            "follow-list",
            "follow",
            &url,
            "--pubkey",
            &list_pk,
            "--yes",
        ],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let text = std::fs::read_to_string(&fcfg).unwrap();
    assert!(
        text.contains("[[follow_lists]]") && text.contains("follow_list = \"trusted\""),
        "{text}"
    );
    // The followed catalog works like one added by hand.
    let out_b = tmp.path().join("beta.json");
    let o = run(
        &fcfg,
        &[
            "catalog",
            "export",
            "--catalog",
            "beta",
            "--out",
            out_b.to_str().unwrap(),
        ],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));

    // Rev 2 drops beta. The follower refreshes by list name (pinned key),
    // and only the list's own entry goes.
    let mut v: serde_json::Value = serde_json::from_slice(&rev1.0).unwrap();
    v["catalogs"]
        .as_array_mut()
        .unwrap()
        .retain(|c| c["name"] != "beta");
    std::fs::write(&list, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    let o = sign(Some("1")).await;
    assert_ne!(code(&o), 0, "reusing rev 1 must fail");
    let o = sign(None).await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(stdout(&o).contains("rev 2"), "{}", stdout(&o));
    let rev2 = (
        std::fs::read(&list).unwrap(),
        std::fs::read(format!("{l}.minisig")).unwrap(),
    );
    server.reset().await;
    mount_dir(&server, &pubdir).await;
    let o = run(
        &fcfg,
        &["catalog", "follow-list", "follow", "trusted", "--yes"],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(stdout(&o).contains("remove    beta"), "{}", stdout(&o));
    let text = std::fs::read_to_string(&fcfg).unwrap();
    assert!(!text.contains("name = \"beta\""), "{text}");
    assert!(text.contains("rev = 2"), "{text}");
    assert!(text.contains("name = \"alpha\""), "{text}");

    // Serving rev 1 again is a rollback; edited bytes fail the signature.
    std::fs::write(&list, &rev1.0).unwrap();
    std::fs::write(format!("{l}.minisig"), &rev1.1).unwrap();
    server.reset().await;
    mount_dir(&server, &pubdir).await;
    let o = run(
        &fcfg,
        &["catalog", "follow-list", "follow", "trusted", "--yes"],
    )
    .await;
    assert_ne!(code(&o), 0);
    assert!(stderr(&o).contains("rollback"), "{}", stderr(&o));
    let tampered = String::from_utf8(rev2.0.clone())
        .unwrap()
        .replace("\"rev\": 2", "\"rev\": 3");
    std::fs::write(&list, tampered).unwrap();
    std::fs::write(format!("{l}.minisig"), &rev2.1).unwrap();
    server.reset().await;
    mount_dir(&server, &pubdir).await;
    let o = run(&fcfg, &["catalog", "follow-list", "follow", "trusted"]).await;
    assert_ne!(code(&o), 0);
    assert!(stderr(&o).contains("verifying"), "{}", stderr(&o));

    // Unfollow drops the list entry; alpha (added by hand) stays.
    let o = run(&fcfg, &["catalog", "follow-list", "unfollow", "trusted"]).await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let text = std::fs::read_to_string(&fcfg).unwrap();
    assert!(!text.contains("[[follow_lists]]"), "{text}");
    assert!(text.contains("name = \"alpha\""), "{text}");
    let o = run(&fcfg, &["config", "show"]).await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
}
