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
use wiremock::matchers::{header, method, path, path_regex};
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

[mirrors]
file = "{d}/mirrors.toml"

[storage]
local_dir = "{d}/publish"
"#
    );
    let p = dir.join("tensorseed.toml");
    std::fs::write(&p, text).unwrap();
    p
}

async fn run(config: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    run_stdin(config, args, env, None).await
}

async fn run_stdin(
    config: &Path,
    args: &[&str],
    env: &[(&str, &str)],
    stdin: Option<&str>,
) -> Output {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_tensorseed"));
    cmd.arg("--config").arg(config).args(args);
    cmd.env_remove("HF_TOKEN")
        .env_remove("HF_TOKEN_PATH")
        .env_remove("HF_HOME")
        .env("RUST_LOG", "warn");
    // Do not pick up a developer login or Hugging Face CLI token.
    let home = config.parent().unwrap();
    if !env.iter().any(|(k, _)| *k == "HOME") {
        cmd.env("HOME", home);
    }
    if !env.iter().any(|(k, _)| *k == "USERPROFILE") {
        cmd.env("USERPROFILE", home);
    }
    if !env.iter().any(|(k, _)| *k == "XDG_CONFIG_HOME") {
        cmd.env("XDG_CONFIG_HOME", home);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    if stdin.is_some() {
        cmd.stdin(std::process::Stdio::piped());
    }
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let body = stdin.map(str::to_owned);
    let out = tokio::task::spawn_blocking(move || {
        if let Some(body) = body {
            let mut child = cmd.spawn().unwrap();
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(body.as_bytes())
                .unwrap();
            child.wait_with_output().unwrap()
        } else {
            cmd.output().unwrap()
        }
    })
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
async fn pull_include_exclude_then_verify_partial() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let fx = Fixture::new();
    let m = manifest(&fx, "org/tiny", C, &format!("{}/t/", server.uri()));
    let pubdir = tmp.path().join("srv");
    publish_catalog(&pubdir, &tmp.path().join("pstate"), &m, false).await;
    mount_dir(&server, &pubdir).await;
    let name = m.content_name();
    for p in ["config.json", "model.safetensors", "pytorch_model.bin"] {
        serve(&server, &format!("/t/{name}/{p}"), fx.get(p)).await;
    }
    let cfg = write_config(
        tmp.path(),
        &server.uri(),
        &key().public_key().to_base64(),
        "",
    );

    let o = run(
        &cfg,
        &["pull", "org/tiny", "--include", "*.safetensors"],
        &[],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    let s = stdout(&o);
    assert!(s.contains("skip   config.json (unselected)"), "{s}");
    assert!(s.contains("skip   pytorch_model.bin (unselected)"), "{s}");
    assert!(s.contains("ok     model.safetensors"), "{s}");
    let dir = tmp.path().join("dl/org/tiny");
    assert!(dir.join("model.safetensors").exists());
    assert!(!dir.join("config.json").exists());
    assert!(!dir.join("pytorch_model.bin").exists());
    let rec: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join(".tensorseed/pull.json")).unwrap()).unwrap();
    let skipped = rec["skipped"].as_array().unwrap();
    assert!(skipped.iter().any(|v| v == "config.json"), "{rec}");
    assert!(skipped.iter().any(|v| v == "pytorch_model.bin"), "{rec}");
    let unselected = rec["unselected"].as_array().unwrap();
    assert!(unselected.iter().any(|v| v == "config.json"), "{rec}");
    assert!(unselected.iter().any(|v| v == "pytorch_model.bin"), "{rec}");
    assert_eq!(rec["files"].as_array().unwrap().len(), 3);

    let o = run(&cfg, &["verify", dir.to_str().unwrap()], &[]).await;
    assert_eq!(code(&o), 0, "{}", stdout(&o));
    assert!(stdout(&o).contains("3 files checked, 3 passed, 0 failed"));

    let o = run(&cfg, &["verify", "--strict", dir.to_str().unwrap()], &[]).await;
    assert_eq!(code(&o), 3, "strict counts unselected files as missing");

    let other = tmp.path().join("partial-exclude");
    let o = run(
        &cfg,
        &[
            "pull",
            "org/tiny",
            "--exclude",
            "*.json",
            "--out",
            other.to_str().unwrap(),
        ],
        &[],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    let s = stdout(&o);
    assert!(s.contains("skip   config.json (unselected)"), "{s}");
    assert!(s.contains("skip   pytorch_model.bin (pickle"), "{s}");
    assert!(s.contains("ok     model.safetensors"), "{s}");
    assert!(other.join("model.safetensors").exists());
    assert!(!other.join("config.json").exists());

    let o = run(
        &cfg,
        &[
            "pull",
            "org/tiny",
            "--include",
            "*.gguf",
            "--out",
            tmp.path().join("none").to_str().unwrap(),
        ],
        &[],
    )
    .await;
    assert_eq!(code(&o), 1);
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("no files matched --include/--exclude"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
}

fn append_toml(path: &Path, extra: &str) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(f, "\n{extra}").unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn pull_source_order_hub_before_mirrors() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let fx = Fixture::new();
    let m = manifest(&fx, "org/tiny", C, &format!("{}/t/", server.uri()));
    let pubdir = tmp.path().join("srv");
    publish_catalog(&pubdir, &tmp.path().join("pstate"), &m, false).await;
    mount_dir(&server, &pubdir).await;
    let name = m.content_name();
    for pth in ["config.json", "model.safetensors"] {
        serve(&server, &format!("/t/{name}/{pth}"), fx.get(pth)).await;
        serve(
            &server,
            &format!("/org/tiny/resolve/{C}/{pth}"),
            fx.get(pth),
        )
        .await;
    }
    let pk = key().public_key().to_base64();
    let cfg = write_config(tmp.path(), &server.uri(), &pk, "");
    let o = run(&cfg, &["pull", "org/tiny", "--no-swarm"], &[]).await;
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    let s = stdout(&o);
    assert!(s.contains("ok     config.json (huggingface)"), "{s}");
    assert!(s.contains("ok     model.safetensors (huggingface)"), "{s}");

    let dir2 = tmp.path().join("mfirst");
    std::fs::create_dir_all(&dir2).unwrap();
    let cfg2 = write_config(&dir2, &server.uri(), &pk, "");
    append_toml(
        &cfg2,
        "[fetch]\nsource_order = [\"swarm\", \"mirrors\", \"hub\"]",
    );
    let o = run(&cfg2, &["pull", "org/tiny", "--no-swarm"], &[]).await;
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    let s = stdout(&o);
    assert!(s.contains("ok     config.json (webseed)"), "{s}");
    assert!(s.contains("ok     model.safetensors (webseed)"), "{s}");

    // A 401 from the Hub must fall through to the webseed quickly.
    let gated = MockServer::start().await;
    let m2 = manifest(&fx, "org/tiny", C, &format!("{}/t/", gated.uri()));
    let pub2 = tmp.path().join("srv2");
    publish_catalog(&pub2, &tmp.path().join("pstate2"), &m2, false).await;
    mount_dir(&gated, &pub2).await;
    let name2 = m2.content_name();
    serve(
        &gated,
        &format!("/t/{name2}/config.json"),
        fx.get("config.json"),
    )
    .await;
    serve(
        &gated,
        &format!("/t/{name2}/model.safetensors"),
        fx.get("model.safetensors"),
    )
    .await;
    Mock::given(method("GET"))
        .and(path_regex(format!("^/org/tiny/resolve/{C}/")))
        .respond_with(ResponseTemplate::new(401))
        .mount(&gated)
        .await;
    let dir3 = tmp.path().join("gated");
    std::fs::create_dir_all(&dir3).unwrap();
    let cfg3 = write_config(&dir3, &gated.uri(), &pk, "");
    let o = run(&cfg3, &["pull", "org/tiny", "--no-swarm"], &[]).await;
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    let s = stdout(&o);
    assert!(s.contains("ok     config.json (webseed)"), "{s}");
    assert!(s.contains("ok     model.safetensors (webseed)"), "{s}");
}

#[tokio::test(flavor = "multi_thread")]
async fn hf_login_status_logout_never_prints_the_token() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/whoami-v2"))
        .and(header("authorization", "Bearer hf_test_token_xyz"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "name": "demo-user"
        })))
        .mount(&server)
        .await;
    let cfg = write_config(tmp.path(), &server.uri(), "", "");
    let secret = "hf_test_token_xyz";
    let o = run_stdin(&cfg, &["hf", "login"], &[], Some(&format!("{secret}\n"))).await;
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    let combined = format!("{}{}", stdout(&o), String::from_utf8_lossy(&o.stderr));
    assert!(!combined.contains(secret), "{combined}");
    let token_path = tmp.path().join("tensorseed").join("hf_token");
    assert_eq!(std::fs::read_to_string(&token_path).unwrap().trim(), secret);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&token_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{mode:o}");
    }

    let o = run(&cfg, &["hf", "status"], &[]).await;
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    let s = stdout(&o);
    assert!(s.contains("logged in as demo-user"), "{s}");
    assert!(s.contains("tensorseed login file"), "{s}");
    let combined = format!("{s}{}", String::from_utf8_lossy(&o.stderr));
    assert!(!combined.contains(secret), "{combined}");

    let o = run(&cfg, &["hf", "logout"], &[]).await;
    assert_eq!(code(&o), 0, "{}", String::from_utf8_lossy(&o.stderr));
    assert!(!token_path.exists());
    let o = run(&cfg, &["hf", "status"], &[]).await;
    assert_eq!(code(&o), 1);
    assert!(stdout(&o).contains("No Hugging Face token is configured"));
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
    mount_hf_repo_lic(server, repo, gated, "apache-2.0", fx).await;
}

async fn mount_hf_repo_lic(
    server: &MockServer,
    repo: &str,
    gated: serde_json::Value,
    license: &str,
    fx: &Fixture,
) {
    let info = serde_json::json!({
        "id": repo, "sha": C2, "gated": gated, "private": false,
        "cardData": {"license": license}, "tags": [format!("license:{license}")]
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
    // Publish fans out to every enabled target; disabled ones are skipped.
    let targets = format!(
        "{seck}\n[[storage.target]]\nname = \"nas1\"\nprovider = \"local\"\ndir = \"{0}/nas1\"\n\
         [[storage.target]]\nname = \"nas2\"\nprovider = \"local\"\ndir = \"{0}/nas2\"\n\
         [[storage.target]]\nname = \"off\"\nprovider = \"local\"\nenabled = false\ndir = \"{0}/off\"\n",
        a.display()
    );
    let cfg = write_config(&a, &server.uri(), &pk, &targets);
    let o = run(&cfg, &["catalog", "create"], &pw).await;
    assert_eq!(code(&o), 0);
    assert!(stdout(&o).contains("nas1 (local), nas2 (local)"));
    assert!(a.join("publish/main/head.json").exists());
    assert!(a.join("nas1/main/head.json").exists());
    assert!(a.join("nas2/main/head.json").exists());
    assert!(!a.join("off").exists());
    let o = run(&cfg, &["catalog", "create"], &pw).await;
    assert_ne!(code(&o), 0, "second create refuses");

    // Mirror list: edit locally, then sign and publish next to the catalog.
    let o = run(
        &cfg,
        &[
            "mirror",
            "add",
            "https://m1.example.org/x",
            "--name",
            "m1",
            "--priority",
            "7",
        ],
        &[],
    )
    .await;
    assert_eq!(code(&o), 0);
    let o = run(&cfg, &["mirror", "add", "ftp://bad", "--name", "bad"], &[]).await;
    assert_ne!(code(&o), 0, "non http mirror rejected");
    let o = run(&cfg, &["mirror", "publish"], &pw).await;
    assert_eq!(code(&o), 0);
    assert!(stdout(&o).contains("published 1 mirrors"));
    assert!(a.join("publish/main/mirrors.toml.minisig").exists());

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
    // The subscriber fetched and verified the published mirror list.
    let o = run(&cfg_b, &["mirror", "list", "--offline"], &[]).await;
    assert_eq!(code(&o), 0);
    let s = stdout(&o);
    assert!(
        s.contains("published") && s.contains("https://m1.example.org/x/"),
        "{s}"
    );
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

#[tokio::test(flavor = "multi_thread")]
async fn harvest_discovers_mirrors_publishes_and_is_idempotent() {
    use wiremock::matchers::query_param;
    let tmp = tempfile::tempdir().unwrap();
    let hub = MockServer::start().await;
    let mirror = MockServer::start().await; // later serves the published tree
    let fx = Fixture::new();

    // Discovery: one 429 first (backoff), then the listing.
    Mock::given(method("GET"))
        .and(path("/api/models"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .mount(&hub)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/models"))
        .and(query_param("author", "org"))
        .and(query_param("filter", "safetensors"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            {"id": "org/a", "downloads": 500, "tags": ["safetensors"]},
            {"id": "org/locked", "downloads": 400, "tags": ["safetensors"], "gated": "manual"},
            {"id": "org/custom", "downloads": 300, "tags": ["safetensors"]},
            {"id": "org/banned", "downloads": 200, "tags": ["safetensors"]},
            {"id": "org/low", "downloads": 3, "tags": ["safetensors"]}
        ])))
        .mount(&hub)
        .await;
    mount_hf_repo(&hub, "org/a", serde_json::json!(false), &fx).await;
    mount_hf_repo(&hub, "org/explicit", serde_json::json!(false), &fx).await;
    mount_hf_repo(&hub, "org/locked", serde_json::json!("manual"), &fx).await;
    mount_hf_repo_lic(&hub, "org/custom", serde_json::json!(false), "llama3", &fx).await;

    let sk = key();
    let keyfile = tmp.path().join("pub.key");
    std::fs::write(
        &keyfile,
        sk.to_minisign_file(None, tensorseed_catalog::minisign::KdfLimits::INSECURE_TEST)
            .unwrap(),
    )
    .unwrap();
    let pk = sk.public_key().to_base64();
    let extra = format!(
        "publisher_seckey_file = \"{}\"\n\n[harvest]\nrepos = [\"org/explicit\"]\nauthors = [\"org\"]\ntags = [\"safetensors\"]\nmin_downloads = 10\ntakedown = [\"org/banned\"]\nrequest_delay_ms = 0\nretry_base_ms = 1\n",
        keyfile.display()
    );
    let cfg = write_config(tmp.path(), &hub.uri(), &pk, &extra);
    // Expose the publish dir as a webseed via the mirror server.
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text.push_str(&format!("local_public_base_url = \"{}/\"\n", mirror.uri()));
    std::fs::write(&cfg, text).unwrap();

    let o = run(&cfg, &["harvest", "--dry-run"], &[]).await;
    assert_eq!(code(&o), 0);
    let s = stdout(&o);
    assert!(
        s.contains("plan     model org/a@89abcdef0123: 2 files"),
        "{s}"
    );
    assert!(s.contains("plan     model org/explicit@"), "{s}");
    assert!(s.contains("org/locked@89abcdef0123: gated"), "{s}");
    assert!(s.contains("skip     model org/custom@"), "{s}");
    assert!(s.contains("org/banned: on the takedown list"), "{s}");
    assert!(!s.contains("org/low"), "{s}");
    assert!(!tmp.path().join("publish/t").exists());

    let o = run(&cfg, &["harvest"], &[]).await;
    assert_eq!(code(&o), 0);
    let s = stdout(&o);
    assert!(
        s.contains("2 harvested, 0 up to date, 3 skipped, 0 failed"),
        "{s}"
    );
    let content = tmp.path().join("publish/t/org__a@89abcdef0123");
    assert_eq!(
        std::fs::read(content.join("model.safetensors")).unwrap(),
        fx.get("model.safetensors")
    );
    assert!(
        !content.join("pytorch_model.bin").exists(),
        "pickle excluded"
    );
    assert!(!content.join(".tensorseed").exists(), "scratch dir removed");
    let mf = tmp
        .path()
        .join(format!("publish/manifests/model/org/a/{C2}.json"));
    let entry =
        tensorseed_catalog::signed::SignedEntry::from_bytes(&std::fs::read(&mf).unwrap()).unwrap();
    entry.verify(&sk.public_key(), "main").unwrap();
    let m = Manifest::from_json(entry.value.as_bytes()).unwrap();
    assert_eq!(m.webseeds, vec![format!("{}/t/", mirror.uri())]);
    assert_eq!(m.files.len(), 2);
    assert!(tmp.path().join("publish/main/head.json").exists());

    // Rerun: nothing new.
    let o = run(&cfg, &["harvest"], &[]).await;
    assert_eq!(code(&o), 0);
    assert!(
        stdout(&o).contains("0 harvested, 2 up to date"),
        "{}",
        stdout(&o)
    );
    let o = run(&cfg, &["harvest", "--status"], &[]).await;
    assert!(stdout(&o).contains("done    model org/a@89abcdef0123"));
    // Daemon mode: repeated passes stay idempotent.
    let o = run(
        &cfg,
        &["harvest", "--watch", "--max-cycles", "2"],
        &[("TENSORSEED_HARVEST__POLL_INTERVAL_SECS", "1")],
    )
    .await;
    assert_eq!(code(&o), 0);
    assert!(stdout(&o).contains("harvest pass 2: 0 harvested, 2 up to date"));

    // A subscriber pulls org/a from the published tree, webseed only.
    mount_dir(&mirror, &tmp.path().join("publish")).await;
    let sub = tmp.path().join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let scfg = write_config(&sub, &mirror.uri(), &pk, "");
    let o = run(
        &scfg,
        &["pull", "org/a", "--no-hf-fallback", "--no-swarm"],
        &[],
    )
    .await;
    assert_eq!(code(&o), 0);
    let s = stdout(&o);
    assert!(!s.contains("swarm  "), "{s}");
    assert!(s.contains("ok     model.safetensors (webseed)"));
    let o = run(&scfg, &["verify", "org/a"], &[]).await;
    assert_eq!(code(&o), 0);

    // Default pull tries the swarm first. With no seeders and a short
    // no-peer window it falls back to HTTP quickly.
    let fb = sub.join("fallback");
    let fb_s = fb.to_str().unwrap();
    let o = run(
        &scfg,
        &["pull", "org/a", "--no-hf-fallback", "--out", fb_s],
        &[
            ("TENSORSEED_TORRENT__DHT", "false"),
            ("TENSORSEED_TORRENT__SWARM_NO_PEER_SECS", "2"),
            ("TENSORSEED_TORRENT__SWARM_STALL_SECS", "30"),
        ],
    )
    .await;
    assert_eq!(code(&o), 0);
    let s = stdout(&o);
    assert!(
        s.contains("swarm  no peers after 2s; using HTTP sources"),
        "{s}"
    );
    assert!(s.contains("ok     model.safetensors (webseed)"), "{s}");
    let o = run(&scfg, &["verify", fb_s], &[]).await;
    assert_eq!(code(&o), 0);

    // Harvest also built a hybrid torrent with the webseed in it.
    let t = m.torrent.as_ref().expect("manifest has a torrent");
    assert!(t.infohash_v1.is_some());
    assert!(t.magnet.contains("xt=urn:btmh:1220"));
    assert_eq!(
        t.torrent_file_urls,
        vec![format!("{}/t/org__a@89abcdef0123.torrent", mirror.uri())]
    );
    assert!(m.files.iter().all(|f| f.bt_v2_pieces_root.is_some()));
    let tbytes = std::fs::read(tmp.path().join("publish/t/org__a@89abcdef0123.torrent")).unwrap();
    let meta = tensorseed_torrent::Metainfo::parse(&tbytes).unwrap();
    meta.check_manifest(&m, &m.files).unwrap();
    assert_eq!(meta.webseeds, m.webseeds);

    // The publisher seeds its tree on loopback; a subscriber pulls over the
    // swarm, and the HTTP pass then finds every file already present.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let port_s = port.to_string();
    let net = [
        ("TENSORSEED_TORRENT__LISTEN_ADDR", "127.0.0.1"),
        ("TENSORSEED_TORRENT__LISTEN_PORT", port_s.as_str()),
        ("TENSORSEED_TORRENT__DHT", "false"),
    ];
    let mut seeder = std::process::Command::new(env!("CARGO_BIN_EXE_tensorseed"));
    seeder
        .arg("--config")
        .arg(&cfg)
        .args(["seed", "--for", "120s"])
        .env("RUST_LOG", "warn")
        .stdout(std::process::Stdio::piped());
    for (k, v) in net {
        seeder.env(k, v);
    }
    let mut child = seeder.spawn().unwrap();
    let out = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    let mut seen = String::new();
    while !seen.contains("listening on") {
        let line = rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .unwrap_or_else(|_| panic!("seeder did not start: {seen}"));
        seen.push_str(&line);
        seen.push('\n');
    }
    assert!(seen.contains("seeding org__a@89abcdef0123"), "{seen}");
    assert!(
        seen.contains("seeding org__explicit@89abcdef0123"),
        "{seen}"
    );

    let peer = format!("127.0.0.1:{port}");
    let swarm_out = sub.join("swarm");
    let swarm_out_s = swarm_out.to_str().unwrap();
    let o = run(
        &scfg,
        &[
            "pull",
            "org/a",
            "--no-hf-fallback",
            "--swarm",
            "--peer",
            &peer,
            "--out",
            swarm_out_s,
            "--seed-after",
            "1s",
        ],
        &[
            ("TENSORSEED_TORRENT__DHT", "false"),
            ("TENSORSEED_TORRENT__LISTEN_ADDR", "127.0.0.1"),
            ("TENSORSEED_TORRENT__LISTEN_PORT", "0"),
        ],
    )
    .await;
    child.kill().ok();
    child.wait().ok();
    assert_eq!(code(&o), 0);
    let s = stdout(&o);
    assert!(s.contains("swarm  "), "{s}");
    assert!(!s.contains("HTTP sources fill the rest"), "{s}");
    assert!(s.contains("ok     model.safetensors (swarm)"), "{s}");
    assert!(s.contains("downloaded over HTTP (0 bytes)"), "{s}");
    assert!(s.contains("seeding org__a@89abcdef0123"), "{s}");
    assert_eq!(
        std::fs::read(swarm_out.join("model.safetensors")).unwrap(),
        fx.get("model.safetensors")
    );
    assert!(swarm_out.join(".tensorseed/torrent").exists());
    let o = run(&scfg, &["verify", swarm_out_s], &[]).await;
    assert_eq!(code(&o), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn pull_tries_mirrors_by_priority_with_fallback() {
    use tensorseed_catalog::mirrors::{Mirror, MirrorList, sign_list};
    let tmp = tempfile::tempdir().unwrap();
    let origin = MockServer::start().await; // catalog; manifest webseed is empty
    let dead = MockServer::start().await; // priority 5: everything fails
    let good = MockServer::start().await; // priority 20: has the files
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&dead)
        .await;
    let fx = Fixture::new();
    let m = manifest(&fx, "org/tiny", C, &format!("{}/t/", origin.uri()));
    let pubdir = tmp.path().join("srv");
    publish_catalog(&pubdir, &tmp.path().join("pstate"), &m, false).await;
    // Publisher signed list naming the good mirror (and a disabled one).
    let mut good_m = Mirror::http("good", &good.uri());
    good_m.priority = 20;
    let mut off = Mirror::http("off", "https://off.invalid/");
    off.enabled = false;
    off.priority = 1;
    let list = MirrorList {
        mirrors: vec![good_m, off],
        ..Default::default()
    };
    let (body, sig) = sign_list(&key(), "main", &list).unwrap();
    std::fs::write(pubdir.join("main/mirrors.toml"), &body).unwrap();
    std::fs::write(pubdir.join("main/mirrors.toml.minisig"), &sig).unwrap();
    mount_dir(&origin, &pubdir).await;
    let name = m.content_name();
    for (f, b) in &fx.files {
        serve(&good, &format!("/t/{name}/{f}"), b).await;
    }

    let cfg = write_config(
        tmp.path(),
        &origin.uri(),
        &key().public_key().to_base64(),
        "",
    );
    // The user's own list adds the dead mirror ahead of everything.
    std::fs::write(
        tmp.path().join("mirrors.toml"),
        format!(
            "[[mirror]]\nname = \"dead\"\nurl = \"{}\"\npriority = 5\n",
            dead.uri()
        ),
    )
    .unwrap();

    let o = run(&cfg, &["mirror", "list"], &[]).await;
    assert_eq!(code(&o), 0);
    let s = stdout(&o);
    let (d, g) = (s.find(&dead.uri()).unwrap(), s.find(&good.uri()).unwrap());
    assert!(d < g, "dead (5) listed before good (20): {s}");
    assert!(!s.contains("off.invalid"), "disabled mirror hidden: {s}");

    let o = run(&cfg, &["pull", "org/tiny", "--no-hf-fallback"], &[]).await;
    assert_eq!(code(&o), 0);
    let s = stdout(&o);
    assert!(s.contains("ok     model.safetensors (webseed)"), "{s}");
    let dir = tmp.path().join("dl/org/tiny");
    assert_eq!(
        std::fs::read(dir.join("model.safetensors")).unwrap(),
        fx.get("model.safetensors")
    );
    // The dead mirror was tried first for files, then the good one served them.
    let dead_t = dead
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().starts_with("/t/"))
        .count();
    assert!(dead_t > 0, "dead mirror tried for files");
    let good_t = good.received_requests().await.unwrap().len();
    assert!(good_t >= 2, "good mirror served the files");
    let origin_t = origin
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().starts_with("/t/"))
        .count();
    assert_eq!(origin_t, 0, "priority 100 manifest webseed not needed");

    // A tampered published list is ignored; the cached verified copy stays.
    std::fs::write(
        pubdir.join("main/mirrors.toml"),
        b"[[mirror]]\nurl = \"https://evil/\"\n",
    )
    .unwrap();
    origin.reset().await;
    mount_dir(&origin, &pubdir).await;
    let o = run(&cfg, &["mirror", "list"], &[]).await;
    let s = stdout(&o);
    assert!(s.contains(&good.uri()) && !s.contains("evil"), "{s}");
}

#[tokio::test(flavor = "multi_thread")]
async fn other_license_needs_explicit_override() {
    let tmp = tempfile::tempdir().unwrap();
    let hub = MockServer::start().await;
    let fx = Fixture::new();
    mount_hf_repo_lic(&hub, "me/mine", serde_json::json!(false), "other", &fx).await;
    mount_hf_repo_lic(&hub, "org/else", serde_json::json!(false), "other", &fx).await;
    let sk = key();
    let keyfile = tmp.path().join("pub.key");
    std::fs::write(
        &keyfile,
        sk.to_minisign_file(None, tensorseed_catalog::minisign::KdfLimits::INSECURE_TEST)
            .unwrap(),
    )
    .unwrap();
    let pk = sk.public_key().to_base64();
    let base = format!(
        "publisher_seckey_file = \"{}\"\n\n[harvest]\nrequest_delay_ms = 0\nretry_base_ms = 1\n",
        keyfile.display()
    );

    // Plain watchlist entry: the strict default allowlist skips `other`.
    let cfg = write_config(
        tmp.path(),
        &hub.uri(),
        &pk,
        &format!("{base}repos = [\"me/mine\", \"org/else\"]\n"),
    );
    let s = stdout(&run(&cfg, &["harvest", "--dry-run"], &[]).await);
    assert!(s.contains("skip     model me/mine@"), "{s}");
    assert!(s.contains("license other"), "{s}");
    // A run-wide flag accepts it.
    let s = stdout(
        &run(
            &cfg,
            &["harvest", "--dry-run", "--allow-license", "other"],
            &[],
        )
        .await,
    );
    assert!(s.contains("plan     model me/mine@"), "{s}");

    // A per-repo override accepts only that repo.
    let cfg = write_config(
        tmp.path(),
        &hub.uri(),
        &pk,
        &format!(
            "{base}repos = [{{ id = \"me/mine\", allow_license = [\"other\"] }}, \"org/else\"]\n"
        ),
    );
    let s = stdout(&run(&cfg, &["harvest", "--dry-run"], &[]).await);
    assert!(s.contains("plan     model me/mine@"), "{s}");
    assert!(s.contains("skip     model org/else@"), "{s}");

    // `publish` without the override refuses; with it, publishes and keeps
    // the honest license class.
    let o = run(&cfg, &["publish", "org/else"], &[]).await;
    assert_ne!(code(&o), 0, "{}", stdout(&o));
    let o = run(
        &cfg,
        &["publish", "org/else", "--allow-license", "other"],
        &[],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stdout(&o));
    assert!(stdout(&o).contains("allowed by explicit override"));
    let m = std::fs::read_to_string(
        tmp.path()
            .join(format!("publish/manifests/model/org/else/{C2}.json")),
    )
    .unwrap();
    let entry: serde_json::Value = serde_json::from_str(&m).unwrap();
    let man: serde_json::Value = serde_json::from_str(entry["value"].as_str().unwrap()).unwrap();
    assert_eq!(man["license"]["class"], "unknown", "{m}");
    assert_eq!(man["license"]["hf_tag"], "other", "{m}");
}
