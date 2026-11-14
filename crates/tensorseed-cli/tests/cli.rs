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

/// A path for a TOML basic string, with forward slashes: a Windows path
/// would have its backslashes read as escapes. The loader turns them back
/// into native separators.
fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn write_config(dir: &Path, server: &str, pubkey: &str, extra: &str) -> PathBuf {
    let d = slash(dir);
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
    // The Windows config and data dirs.
    for k in ["APPDATA", "LOCALAPPDATA"] {
        if !env.iter().any(|(e, _)| *e == k) {
            cmd.env(k, home);
        }
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
        slash(&keys.join("pub.key"))
    );
    let a = tmp.path().join("a");
    std::fs::create_dir_all(&a).unwrap();
    // Publish fans out to every enabled target; disabled ones are skipped.
    let targets = format!(
        "{seck}\n[[storage.target]]\nname = \"nas1\"\nprovider = \"local\"\ndir = \"{0}/nas1\"\n\
         [[storage.target]]\nname = \"nas2\"\nprovider = \"local\"\ndir = \"{0}/nas2\"\n\
         [[storage.target]]\nname = \"off\"\nprovider = \"local\"\nenabled = false\ndir = \"{0}/off\"\n",
        slash(&a)
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
        slash(&keyfile)
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
        slash(&keyfile)
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

/// A GGUF repo published in a signed catalog with `license`, then pulled.
async fn pulled_gguf(
    tmp: &Path,
    server: &MockServer,
    license: &str,
) -> (PathBuf, PathBuf, Vec<u8>) {
    let gguf: Vec<u8> = b"GGUF"
        .iter()
        .copied()
        .chain((0..4096u32).map(|i| (i % 251) as u8))
        .collect();
    let fx = Fixture {
        files: vec![
            ("README.md".into(), b"# tiny\n".to_vec()),
            ("tiny-q4.gguf".into(), gguf.clone()),
        ],
    };
    let mut m = manifest(&fx, "org/tiny", C, &format!("{}/t/", server.uri()));
    m.license.spdx = Some(license.into());
    m.license.hf_tag = Some(license.into());
    let pubdir = tmp.join("srv");
    publish_catalog(&pubdir, &tmp.join("pstate"), &m, false).await;
    mount_dir(server, &pubdir).await;
    let name = m.content_name();
    for (p, b) in &fx.files {
        serve(server, &format!("/t/{name}/{p}"), b).await;
    }
    let cfg = write_config(tmp, &server.uri(), &key().public_key().to_base64(), "");
    let o = run(&cfg, &["pull", "org/tiny", "--no-swarm"], &[]).await;
    assert_eq!(code(&o), 0);
    // Native separators, as the CLI prints it.
    (
        cfg,
        tensorseed_core::paths::join_slash(tmp, "dl/org/tiny"),
        gguf,
    )
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn serve_inference_verifies_before_serving() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let (cfg, dir, _) = pulled_gguf(tmp.path(), &server, "apache-2.0").await;
    let dir_s = dir.to_str().unwrap();

    // Commercial serving marks in list (text and JSON) and show.
    let o = run(&cfg, &["list", "--no-wait"], &[]).await;
    assert!(stdout(&o).contains("serving:allowed"), "{}", stdout(&o));
    assert!(stderr(&o).contains("not legal advice"));
    let o = run(&cfg, &["list", "--no-wait", "--json"], &[]).await;
    assert_eq!(code(&o), 0);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["schema"], "ts.list/1");
    assert_eq!(v["entries"][0]["repo"], "org/tiny");
    assert_eq!(v["entries"][0]["commercial_serving"], "allowed");
    assert!(
        v["disclaimer"]
            .as_str()
            .unwrap()
            .contains("not legal advice")
    );
    let o = run(
        &cfg,
        &["list", "--no-wait", "--commercial-serving", "forbidden"],
        &[],
    )
    .await;
    assert!(!stdout(&o).contains("org/tiny"));
    assert!(stderr(&o).contains("0 entries"));
    let o = run(&cfg, &["list", "--no-wait", "--commercial"], &[]).await;
    assert!(stdout(&o).contains("org/tiny"));
    let o = run(&cfg, &["show", "org/tiny", "--no-wait"], &[]).await;
    assert!(stderr(&o).contains("commercial serving: allowed"));

    // --print-command: verified, then the exact llama.cpp command.
    let o = run(
        &cfg,
        &[
            "serve-inference",
            dir_s,
            "--no-wait",
            "--print-command",
            "--model-id",
            "tiny",
            "--",
            "-ngl",
            "0",
        ],
        &[("PATH", "")],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let s = stdout(&o);
    assert!(s.contains("verified 2 files of org/tiny@"), "{s}");
    assert!(s.contains("commercial serving allowed"), "{s}");
    let model = tensorseed_core::paths::join_slash(&dir, "tiny-q4.gguf");
    let quoted = tensorseed_app::inference::quote(&model.to_string_lossy());
    assert!(s.contains(&format!("--model {quoted}")), "{s}");
    assert!(
        s.contains("--alias tiny --host 127.0.0.1 --port 8090 -ngl 0"),
        "{s}"
    );
    assert!(
        stderr(&o).contains("llama.cpp is not installed: `llama-server` was not found on PATH"),
        "{}",
        stderr(&o)
    );

    // Without --print-command and without a runtime: a clear error.
    let o = run(
        &cfg,
        &["serve-inference", "org/tiny", "--no-wait"],
        &[("PATH", "")],
    )
    .await;
    assert_eq!(code(&o), 1);
    let e = stderr(&o);
    assert!(e.contains("was not found"), "{e}");
    assert!(e.contains("does not bundle inference engines"), "{e}");
    assert!(e.contains("winget install llama.cpp"), "{e}");
    assert!(
        e.contains(tensorseed_app::inference::ENGINE_DOCS_URL),
        "{e}"
    );

    // The engine can be named for one run through the environment.
    let o = run(
        &cfg,
        &[
            "serve-inference",
            "org/tiny",
            "--no-wait",
            "--print-command",
        ],
        &[
            ("PATH", ""),
            (
                "TENSORSEED_INFERENCE__LLAMA_SERVER",
                "/opt/llama/llama-server",
            ),
        ],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(
        // Config paths get native separators.
        stdout(&o).contains(&format!(
            "{} --model",
            tensorseed_core::paths::native_string("/opt/llama/llama-server")
        )),
        "{}",
        stdout(&o)
    );

    // vLLM needs a safetensors checkpoint.
    let o = run(
        &cfg,
        &[
            "serve-inference",
            dir_s,
            "--no-wait",
            "--runtime",
            "vllm",
            "--print-command",
        ],
        &[],
    )
    .await;
    assert_eq!(code(&o), 1);
    assert!(stderr(&o).contains("safetensors"));

    // A changed byte refuses to serve, and no record is written.
    let p = dir.join("tiny-q4.gguf");
    let mut b = std::fs::read(&p).unwrap();
    b[100] ^= 0xff;
    std::fs::write(&p, &b).unwrap();
    let o = run(
        &cfg,
        &["serve-inference", dir_s, "--no-wait", "--print-command"],
        &[],
    )
    .await;
    assert_eq!(code(&o), 1);
    assert!(stderr(&o).contains("refusing to serve"), "{}", stderr(&o));
    assert!(stderr(&o).contains("tiny-q4.gguf"));
    assert!(!dir.join(".tensorseed/served-weights.json").exists());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn serve_inference_reports_a_runtime_that_exits() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let (cfg, dir, _) = pulled_gguf(tmp.path(), &server, "mit").await;
    // A fake llama-server that records its arguments and exits.
    let bin = tmp.path().join("fake-llama-server");
    let args_file = tmp.path().join("args.txt");
    std::fs::write(
        &bin,
        format!("#!/bin/sh\necho \"$@\" > '{}'\necho \"key=${{LLAMA_API_KEY:-none}}\" >> '{}'\nexit 3\n", args_file.display(), args_file.display()),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    append_toml(
        &cfg,
        &format!(
            "\n[inference]\nllama_server = \"{}\"\nport = {port}\napi_key_env = \"TS_TEST_UPSTREAM\"\nready_timeout_secs = 20\n",
            bin.display()
        ),
    );
    // The key variable must be set.
    let o = run(
        &cfg,
        &["serve-inference", dir.to_str().unwrap(), "--no-wait"],
        &[],
    )
    .await;
    assert_eq!(code(&o), 1);
    assert!(stderr(&o).contains("TS_TEST_UPSTREAM"), "{}", stderr(&o));

    let o = run(
        &cfg,
        &["serve-inference", dir.to_str().unwrap(), "--no-wait"],
        &[("TS_TEST_UPSTREAM", "s3cret-test-value")],
    )
    .await;
    assert_eq!(code(&o), 1);
    assert!(
        stderr(&o).contains("exited before it was ready"),
        "{}",
        stderr(&o)
    );
    // The key reaches the runtime through its environment, never argv or output.
    let recorded = std::fs::read_to_string(&args_file).unwrap();
    assert!(recorded.contains("--alias org/tiny"), "{recorded}");
    assert!(recorded.contains("key=s3cret-test-value"));
    assert!(!recorded.lines().next().unwrap().contains("s3cret"));
    assert!(!stdout(&o).contains("s3cret") && !stderr(&o).contains("s3cret"));
    assert!(!dir.join(".tensorseed/served-weights.json").exists());

    // doctor checks the [inference] section.
    let o = run(&cfg, &["doctor", "--offline", "--json"], &[]).await;
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let checks = v["checks"].as_array().unwrap();
    let by = |id: &str| checks.iter().find(|c| c["id"] == id).cloned().unwrap();
    assert_eq!(by("inference.llama_server")["status"], "ok");
    assert_eq!(by("port.inference")["status"], "ok");
    assert_eq!(by("inference.api_key")["status"], "fail");
}

#[tokio::test(flavor = "multi_thread")]
async fn serve_inference_refuses_non_commercial_licenses() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let (cfg, dir, _) = pulled_gguf(tmp.path(), &server, "cc-by-nc-4.0").await;
    let dir_s = dir.to_str().unwrap();
    let o = run(
        &cfg,
        &["serve-inference", dir_s, "--no-wait", "--print-command"],
        &[],
    )
    .await;
    assert_eq!(code(&o), 1);
    let e = stderr(&o);
    assert!(
        e.contains("forbidden") && e.contains("--i-have-rights"),
        "{e}"
    );
    assert!(!stdout(&o).contains("--model"));
    let o = run(
        &cfg,
        &[
            "serve-inference",
            dir_s,
            "--no-wait",
            "--print-command",
            "--i-have-rights",
        ],
        &[],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(stderr(&o).contains("--i-have-rights was passed"));
    assert!(stdout(&o).contains("--model"));
}

#[tokio::test(flavor = "multi_thread")]
async fn serve_inference_refuses_an_unsigned_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    // A signed catalog exists (with org/tiny), but org/hf comes from the Hub.
    let (cfg, _, _) = pulled_gguf(tmp.path(), &server, "mit").await;
    let fx = Fixture::new();
    mount_hf_repo(&server, "org/hf", serde_json::json!(false), &fx).await;
    let o = run(&cfg, &["pull", "--from-hf", "org/hf"], &[]).await;
    assert_eq!(code(&o), 0);
    let dir = tmp.path().join("dl/org/hf");
    let o = run(
        &cfg,
        &[
            "serve-inference",
            dir.to_str().unwrap(),
            "--no-wait",
            "--print-command",
        ],
        &[],
    )
    .await;
    assert_eq!(code(&o), 1, "Hub hashes are not a signed manifest");
    assert!(
        stderr(&o).contains("no signed catalog manifest for org/hf"),
        "{}",
        stderr(&o)
    );
    assert!(!stdout(&o).contains("vllm"));
}

/// A chat completion body with logprobs at one position.
fn chat_logprobs(tok: &str, lp: f64, top: &[(&str, f64)]) -> serde_json::Value {
    let top: Vec<serde_json::Value> = top
        .iter()
        .map(|(t, l)| serde_json::json!({"token": t, "logprob": l, "bytes": []}))
        .collect();
    serde_json::json!({
        "object": "chat.completion",
        "model": "org/tiny",
        "choices": [{"index": 0, "finish_reason": "length",
            "message": {"role": "assistant", "content": tok},
            "logprobs": {"content": [{"token": tok, "logprob": lp, "bytes": [], "top_logprobs": top}]}}]
    })
}

/// A legacy completions body with logprobs at one position.
fn legacy_logprobs(tok: &str, lp: f64, top: &[(&str, f64)]) -> serde_json::Value {
    let top: serde_json::Map<String, serde_json::Value> = top
        .iter()
        .map(|(t, l)| ((*t).to_string(), serde_json::json!(l)))
        .collect();
    serde_json::json!({
        "object": "text_completion",
        "choices": [{"index": 0, "text": tok, "logprobs": {
            "tokens": [tok], "token_logprobs": [lp], "top_logprobs": [top], "text_offset": [0]}}]
    })
}

async fn mount_engine(server: &MockServer, chat: serde_json::Value, legacy: serde_json::Value) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(legacy))
        .mount(server)
        .await;
}

const REF_TOP: &[(&str, f64)] = &[("Hi", -0.2), ("Hello", -1.9), ("Hey", -2.6)];
const REF_LEGACY: &[(&str, f64)] = &[(" blue", -0.4), (" clear", -1.5)];

#[tokio::test(flavor = "multi_thread")]
async fn reference_logprobs_then_check_endpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let (cfg, dir, _) = pulled_gguf(tmp.path(), &server, "apache-2.0").await;
    let dir_s = dir.to_str().unwrap();
    mount_engine(
        &server,
        chat_logprobs("Hi", -0.2, REF_TOP),
        legacy_logprobs(" blue", -0.4, REF_LEGACY),
    )
    .await;
    Mock::given(method("GET"))
        .and(path("/props"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({"build_info": "b1-test"})),
        )
        .mount(&server)
        .await;
    let ep = format!("{}/v1", server.uri());
    let refp = tmp.path().join("ref.json");
    let commitp = tmp.path().join("commit.json");

    // Generated prompts from a fixed seed, against a running endpoint.
    let seed = "11".repeat(32);
    let o = run(
        &cfg,
        &[
            "reference-logprobs",
            dir_s,
            "--no-wait",
            "--endpoint",
            &ep,
            "--count",
            "6",
            "--gen-seed",
            &seed,
            "--out",
            refp.to_str().unwrap(),
            "--commitment",
            commitp.to_str().unwrap(),
        ],
        &[("PATH", "")],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let r: serde_json::Value = serde_json::from_slice(&std::fs::read(&refp).unwrap()).unwrap();
    assert_eq!(r["schema"], "ts.reference-logprobs/1");
    assert_eq!(r["repo"], "org/tiny");
    assert_eq!(r["revision"], C);
    assert_eq!(r["entrypoint"]["kind"], "gguf");
    assert_eq!(r["entrypoint"]["path"], "tiny-q4.gguf");
    assert_eq!(r["generator"]["engine_version"], "llama.cpp b1-test");
    assert_eq!(r["generator"]["runtime"], "llama-cpp");
    assert_eq!(r["generation"]["method"], "ts.challenge-gen/1");
    assert_eq!(r["generation"]["seed"], seed.as_str());
    assert_eq!(r["prompts"].as_array().unwrap().len(), 6);
    assert_eq!(r["prompts"][0]["expected"][0]["token"], "Hi");
    assert_eq!(
        r["prompts"][0]["expected"][0]["top_logprobs"][1]["token"],
        "Hello"
    );
    let set_hash = r["set_hash"].as_str().unwrap().to_string();
    assert!(stdout(&o).contains(&set_hash));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&refp).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "the secret set is private");
    }
    // The public commitment has the hash and bindings, not the prompts.
    let c = std::fs::read_to_string(&commitp).unwrap();
    let cv: serde_json::Value = serde_json::from_str(&c).unwrap();
    assert_eq!(cv["schema"], "ts.reference-logprobs-commitment/1");
    assert_eq!(cv["set_hash"], set_hash.as_str());
    assert_eq!(cv["manifest_sha256"], r["manifest_sha256"]);
    assert_eq!(cv["prompt_count"], 6);
    assert!(!c.contains("lp-0001") && !c.contains(&seed) && !c.contains("\"Hi\""));
    // Requests used fixed decoding and the repo id.
    let reqs = server.received_requests().await.unwrap();
    let chat: Vec<serde_json::Value> = reqs
        .iter()
        .filter(|q| q.url.path() == "/v1/chat/completions")
        .map(|q| serde_json::from_slice(&q.body).unwrap())
        .collect();
    // Five whole passes over the set by default; the expected values are
    // per-token medians over the samples.
    assert_eq!(chat.len(), 30);
    assert_eq!(r["generator"]["samples"], 5);
    for b in &chat {
        assert_eq!(b["model"], "org/tiny");
        assert_eq!(b["temperature"], 0.0);
        assert_eq!(b["logprobs"], true);
        assert_eq!(b["top_logprobs"], 5);
        assert_eq!(b["max_tokens"], 1);
        assert_eq!(
            b["cache_prompt"], false,
            "llama.cpp reference runs skip the prompt cache"
        );
    }
    assert_eq!(r["generator"]["request_extra"]["cache_prompt"], false);

    // The same seed regenerates the same prompts (auditable after reveal).
    let refp2 = tmp.path().join("ref2.json");
    let o = run(
        &cfg,
        &[
            "reference-logprobs",
            dir_s,
            "--no-wait",
            "--endpoint",
            &ep,
            "--count",
            "6",
            "--gen-seed",
            &seed,
            "--samples",
            "1",
            "--out",
            refp2.to_str().unwrap(),
        ],
        &[],
    )
    .await;
    assert_eq!(code(&o), 0);
    let r2: serde_json::Value = serde_json::from_slice(&std::fs::read(&refp2).unwrap()).unwrap();
    assert_eq!(r2["prompts"], r["prompts"]);
    assert!(
        r2["generator"].get("samples").is_none(),
        "one sample is the 0.3.0 file shape"
    );
    assert_ne!(r2["set_hash"], r["set_hash"], "a fresh salt each time");

    // A prompt file mixing chat and legacy completions prompts.
    let prompts = tmp.path().join("prompts.json");
    std::fs::write(
        &prompts,
        r#"[{"id":"chat-1","messages":[{"role":"user","content":"Say hi"}]},{"id":"cmp-1","prompt":"The sky is"}]"#,
    )
    .unwrap();
    let mixed = tmp.path().join("mixed.json");
    let o = run(
        &cfg,
        &[
            "reference-logprobs",
            dir_s,
            "--no-wait",
            "--endpoint",
            &ep,
            "--prompts",
            prompts.to_str().unwrap(),
            "--out",
            mixed.to_str().unwrap(),
            "--engine-version",
            "pinned-1",
        ],
        &[],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let m: serde_json::Value = serde_json::from_slice(&std::fs::read(&mixed).unwrap()).unwrap();
    assert_eq!(m["generation"]["method"], "file");
    assert_eq!(m["generator"]["engine_version"], "pinned-1");
    assert_eq!(m["prompts"][1]["id"], "cmp-1");
    assert_eq!(m["prompts"][1]["expected"][0]["token"], " blue");
    assert_eq!(
        m["prompts"][1]["expected"][0]["top_logprobs"][1]["token"],
        " clear"
    );

    // check-endpoint: same engine passes, with the key from the environment.
    let same = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer s3cr3t-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_logprobs(
            "Hi",
            -0.25,
            &[("Hi", -0.25), ("Hello", -1.8), ("Hey", -2.7)],
        )))
        .mount(&same)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/completions"))
        .and(header("authorization", "Bearer s3cr3t-key"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(legacy_logprobs(" blue", -0.4, REF_LEGACY)),
        )
        .mount(&same)
        .await;
    let same_url = format!("{}/v1", same.uri());
    let check = |url: String, reference: PathBuf, extra: Vec<String>| {
        let cfg = cfg.clone();
        async move {
            let mut a: Vec<String> = vec![
                "check-endpoint".into(),
                url,
                "--reference".into(),
                reference.to_str().unwrap().into(),
            ];
            a.extend(extra);
            let a: Vec<&str> = a.iter().map(String::as_str).collect();
            run(&cfg, &a, &[("TS_TEST_EP_KEY", "s3cr3t-key")]).await
        }
    };
    let o = check(
        same_url.clone(),
        refp.clone(),
        vec![
            "--sample".into(),
            "4".into(),
            "--api-key-env".into(),
            "TS_TEST_EP_KEY".into(),
            "--json".into(),
            "--concurrency".into(),
            "3".into(),
        ],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let rep: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(rep["schema"], "ts.check-endpoint/1");
    assert_eq!(rep["verdict"], "pass");
    assert_eq!(rep["model_id"], "org/tiny");
    assert_eq!(rep["counts"]["pass"], 4);
    assert_eq!(rep["sample"]["sent"], 4);
    assert_eq!(rep["reference"]["set_hash"], set_hash.as_str());
    assert_eq!(rep["challenges"][0]["verdict"], "pass");
    assert_eq!(rep["tolerance_source"], "default");
    assert_eq!(rep["tolerance"]["max_fail_fraction"], 0.25);
    assert_eq!(rep["tolerance"]["max_median_mean_abs"], 0.35);
    assert_eq!(rep["decision"]["rule"], "ts.set-decision/1");
    assert_eq!(rep["decision"]["scored"], 4);
    assert_eq!(rep["decision"]["failed"], 0);
    assert!(
        rep["challenges"][0]["comparison"]["mean_abs_dev"]
            .as_f64()
            .unwrap()
            > 0.0
    );
    let all = format!("{}{}", stdout(&o), stderr(&o));
    assert!(!all.contains("s3cr3t-key"), "the key is never printed");
    assert!(
        !all.contains("Continue this") && !all.contains("\"Hi\""),
        "no prompt text or expected values in the report"
    );
    // Both request shapes, text output.
    let o = check(
        same_url.clone(),
        mixed.clone(),
        vec![
            "--sample".into(),
            "0".into(),
            "--api-key-env".into(),
            "TS_TEST_EP_KEY".into(),
        ],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stdout(&o));
    assert!(
        stdout(&o).contains("verdict: pass (2 pass"),
        "{}",
        stdout(&o)
    );

    // calibrate-reference: repeated runs against the reference engine write
    // tolerances into the file, sealed by a new set_hash.
    let calp = tmp.path().join("ref.cal.json");
    let calc = tmp.path().join("ref.cal.commit.json");
    let o = run(
        &cfg,
        &[
            "calibrate-reference",
            &same_url,
            "-r",
            refp.to_str().unwrap(),
            "-o",
            calp.to_str().unwrap(),
            "--commitment",
            calc.to_str().unwrap(),
            "--runs",
            "2",
            "--api-key-env",
            "TS_TEST_EP_KEY",
        ],
        &[("TS_TEST_EP_KEY", "s3cr3t-key")],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(
        stdout(&o).contains("calibrated tolerance"),
        "{}",
        stdout(&o)
    );
    let cal: serde_json::Value = serde_json::from_slice(&std::fs::read(&calp).unwrap()).unwrap();
    assert_eq!(cal["calibration"]["method"], "ts.calibrate/1");
    assert_eq!(cal["calibration"]["runs"], 2);
    assert_eq!(cal["calibration"]["challenges"], 6);
    assert_eq!(cal["calibration"]["observed"]["scored"], 12);
    assert_eq!(cal["prompts"], r["prompts"]);
    assert_ne!(cal["set_hash"], set_hash.as_str());
    let cc: serde_json::Value = serde_json::from_slice(&std::fs::read(&calc).unwrap()).unwrap();
    assert_eq!(cc["set_hash"], cal["set_hash"]);
    assert_eq!(cc["tolerance"], cal["calibration"]["tolerance"]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&calp).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "the calibrated set stays private");
    }
    // check-endpoint takes the calibrated tolerances; flags override them.
    let key = |mut v: Vec<String>| {
        v.extend([
            "--api-key-env".into(),
            "TS_TEST_EP_KEY".into(),
            "--json".into(),
        ]);
        v
    };
    let o = check(same_url.clone(), calp.clone(), key(vec![])).await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let rep: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(rep["tolerance_source"], "calibration");
    assert_eq!(rep["tolerance"], cal["calibration"]["tolerance"]);
    assert_eq!(rep["reference"]["set_hash"], cal["set_hash"]);
    let o = check(
        same_url.clone(),
        calp.clone(),
        key(vec!["--max-mean-abs".into(), "0.01".into()]),
    )
    .await;
    assert_eq!(code(&o), 1, "{}", stderr(&o));
    let rep: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(rep["tolerance_source"], "calibration+flags");
    assert_eq!(rep["decision"]["failed"], 6);
    assert!(
        rep["decision"]["reason"]
            .as_str()
            .unwrap()
            .contains("6 of 6"),
        "{}",
        rep["decision"]["reason"]
    );
    let o = check(
        same_url.clone(),
        refp.clone(),
        key(vec!["--max-fail-fraction".into(), "1.5".into()]),
    )
    .await;
    assert_eq!(code(&o), 2);
    assert!(stderr(&o).contains("--max-fail-fraction"), "{}", stderr(&o));
    // A calibrated file edited after sealing gives no verdict.
    let mut t = cal.clone();
    t["calibration"]["tolerance"]["max_mean_abs"] = serde_json::json!(5.0);
    let loose = tmp.path().join("loose.json");
    std::fs::write(&loose, serde_json::to_vec(&t).unwrap()).unwrap();
    let o = check(same_url.clone(), loose, key(vec![])).await;
    assert_eq!(code(&o), 2);
    assert!(
        stderr(&o).contains("set_hash does not match"),
        "{}",
        stderr(&o)
    );

    // Without the key the endpoint rejects every request: inconclusive.
    let o = check(
        same_url.clone(),
        refp.clone(),
        vec!["--repeats".into(), "1".into()],
    )
    .await;
    assert_eq!(code(&o), 2);
    assert!(
        stdout(&o).contains("verdict: inconclusive"),
        "{}",
        stdout(&o)
    );
    // An unset key variable is an error before any request (exit 2).
    let o = check(
        same_url.clone(),
        refp.clone(),
        vec!["--api-key-env".into(), "TS_TEST_NO_SUCH_VAR".into()],
    )
    .await;
    assert_eq!(code(&o), 2);
    assert!(stderr(&o).contains("TS_TEST_NO_SUCH_VAR"));

    // A different model fails on every attempt.
    let other = MockServer::start().await;
    mount_engine(
        &other,
        chat_logprobs("Zz", -0.01, &[("Zz", -0.01), ("Q", -5.0)]),
        legacy_logprobs(" red", -0.1, &[(" red", -0.1)]),
    )
    .await;
    let o = check(
        format!("{}/v1", other.uri()),
        refp.clone(),
        vec![
            "--sample".into(),
            "2".into(),
            "--repeats".into(),
            "2".into(),
            "--json".into(),
        ],
    )
    .await;
    assert_eq!(code(&o), 1);
    let rep: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(rep["verdict"], "fail");
    assert_eq!(rep["challenges"][0]["attempts"], 2);
    assert_eq!(
        rep["challenges"][0]["comparison"]["first_token_match"],
        false
    );
    let posts = other
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|q| q.method.as_str() == "POST")
        .count();
    assert_eq!(posts, 4);

    // No logprobs: unsupported.
    let plain = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "Hi"}}]
        })))
        .mount(&plain)
        .await;
    let o = check(
        format!("{}/v1", plain.uri()),
        refp.clone(),
        vec!["--json".into()],
    )
    .await;
    assert_eq!(code(&o), 2);
    let rep: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(rep["verdict"], "unsupported");
    // reference-logprobs refuses an engine without logprobs.
    let o = run(
        &cfg,
        &[
            "reference-logprobs",
            dir_s,
            "--no-wait",
            "--endpoint",
            &format!("{}/v1", plain.uri()),
            "--count",
            "1",
            "--out",
            tmp.path().join("x.json").to_str().unwrap(),
        ],
        &[],
    )
    .await;
    assert_eq!(code(&o), 1);
    assert!(stderr(&o).contains("no logprobs"), "{}", stderr(&o));

    // Timeouts are inconclusive.
    let slow = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(chat_logprobs("Hi", -0.2, REF_TOP))
                .set_delay(std::time::Duration::from_secs(5)),
        )
        .mount(&slow)
        .await;
    let o = check(
        format!("{}/v1", slow.uri()),
        refp.clone(),
        vec![
            "--sample".into(),
            "1".into(),
            "--timeout".into(),
            "1".into(),
            "--repeats".into(),
            "1".into(),
        ],
    )
    .await;
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).contains("timed out"), "{}", stdout(&o));

    // A changed reference file, or the wrong published hash, gives no verdict.
    let o = check(
        same_url.clone(),
        refp.clone(),
        vec!["--expect-set-hash".into(), "0".repeat(64)],
    )
    .await;
    assert_eq!(code(&o), 2);
    assert!(stderr(&o).contains("not the expected"));
    let tampered = tmp.path().join("tampered.json");
    let mut t = r.clone();
    t["prompts"][0]["expected"][0]["logprob"] = serde_json::json!(-0.3);
    std::fs::write(&tampered, serde_json::to_vec(&t).unwrap()).unwrap();
    let o = check(same_url.clone(), tampered, vec![]).await;
    assert_eq!(code(&o), 2);
    assert!(
        stderr(&o).contains("set_hash does not match"),
        "{}",
        stderr(&o)
    );

    // A changed weight byte: no reference is produced.
    let p = dir.join("tiny-q4.gguf");
    let mut b = std::fs::read(&p).unwrap();
    b[10] ^= 0xff;
    std::fs::write(&p, &b).unwrap();
    let bad = tmp.path().join("bad.json");
    let o = run(
        &cfg,
        &[
            "reference-logprobs",
            dir_s,
            "--no-wait",
            "--endpoint",
            &ep,
            "--count",
            "1",
            "--out",
            bad.to_str().unwrap(),
        ],
        &[],
    )
    .await;
    assert_eq!(code(&o), 1);
    assert!(stderr(&o).contains("failed verification"), "{}", stderr(&o));
    assert!(!bad.exists());
}

/// EIP-191 `personal_sign` with a well-known development key (Hardhat and
/// Anvil account 0), standing in for the seller's own wallet.
fn dev_wallet_sign(msg: &str) -> String {
    use sha3::Digest as _;
    let sk = k256::ecdsa::SigningKey::from_slice(
        &hex::decode("ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80").unwrap(),
    )
    .unwrap();
    let mut p = format!("\x19Ethereum Signed Message:\n{}", msg.len()).into_bytes();
    p.extend_from_slice(msg.as_bytes());
    let h = sha3::Keccak256::digest(&p);
    let (sig, rid) = sk.sign_prehash_recoverable(&h).unwrap();
    let mut b = sig.to_bytes().to_vec();
    b.push(27 + rid.to_byte());
    format!("0x{}", hex::encode(b))
}
const DEV_WALLET: &str = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266";

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Write a response the way `curl -D headers -o body` would.
async fn save_response(r: reqwest::Response, headers: &Path, body: &Path) {
    let mut h = format!("HTTP/1.1 {}\r\n", r.status());
    for (k, v) in r.headers() {
        h.push_str(&format!("{k}: {}\r\n", v.to_str().unwrap()));
    }
    h.push_str("\r\n");
    std::fs::write(headers, h).unwrap();
    std::fs::write(body, r.bytes().await.unwrap()).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn signed_serving_key_claim_proxy_verify_and_check() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let (cfg, dir, _) = pulled_gguf(tmp.path(), &server, "apache-2.0").await;
    let dir_s = dir.to_str().unwrap();
    mount_engine(
        &server,
        chat_logprobs("Hi", -0.2, REF_TOP),
        legacy_logprobs(" blue", -0.4, REF_LEGACY),
    )
    .await;

    // The response key: owner-only, secret never printed.
    let o = run(&cfg, &["response-key", "create"], &[]).await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let keyp = cfg.parent().unwrap().join("response-key.json");
    let kv: serde_json::Value = serde_json::from_slice(&std::fs::read(&keyp).unwrap()).unwrap();
    let secret = kv["secret"].as_str().unwrap().to_string();
    let pubkey = kv["pubkey"].as_str().unwrap().to_string();
    assert_eq!(kv["schema"], "ts.response-key/1");
    assert!(stdout(&o).contains(&pubkey));
    assert!(!stdout(&o).contains(&secret) && !stderr(&o).contains(&secret));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&keyp).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "the key file is private");
    }
    let o = run(&cfg, &["response-key", "create"], &[]).await;
    assert_ne!(code(&o), 0, "an existing key is not replaced");
    let o = run(&cfg, &["response-key", "show"], &[]).await;
    assert!(stdout(&o).contains(&pubkey) && !stdout(&o).contains(&secret));

    // The claim, naming a wallet; the exact EIP-191 message is printed.
    let port = free_port();
    let proxy = format!("http://127.0.0.1:{port}");
    let o = run(
        &cfg,
        &[
            "claim-serving",
            dir_s,
            "--no-wait",
            "--engine-version",
            "llama.cpp b1-test",
            "--base-url",
            &format!("{proxy}/v1"),
            "--public-url",
            "https://llm.example.com/v1",
            "--wallet",
            "0xF39Fd6e51aad88F6F4ce6aB8827279cffFb92266",
        ],
        &[("PATH", "")],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let claimp = dir.join(".tensorseed/served-weights.json");
    let c: serde_json::Value = serde_json::from_slice(&std::fs::read(&claimp).unwrap()).unwrap();
    assert_eq!(c["schema"], "ts.served-weights/1");
    assert_eq!(c["signed"], false);
    assert_eq!(c["seller_wallet"], DEV_WALLET);
    assert_eq!(c["response_key"]["pubkey"], pubkey.as_str());
    assert_eq!(c["entrypoint"]["path"], "tiny-q4.gguf");
    assert_eq!(c["engine_version"], "llama.cpp b1-test");
    let hash = c["claim_sha256"].as_str().unwrap().to_string();
    let message = format!("tensorseed served-weights/1:\n{hash}");
    assert!(stdout(&o).contains(&message), "{}", stdout(&o));
    assert!(!stdout(&o).contains(&secret));

    // A signature over another message is refused; the right one attaches.
    let o = run(
        &cfg,
        &[
            "claim-serving",
            claimp.to_str().unwrap(),
            "--sig",
            &dev_wallet_sign("other"),
        ],
        &[],
    )
    .await;
    assert_eq!(code(&o), 1);
    assert!(stderr(&o).contains("recovers to"), "{}", stderr(&o));
    let o = run(
        &cfg,
        &[
            "claim-serving",
            claimp.to_str().unwrap(),
            "--sig",
            &dev_wallet_sign(&message),
        ],
        &[],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let c: serde_json::Value = serde_json::from_slice(&std::fs::read(&claimp).unwrap()).unwrap();
    assert_eq!(c["signed"], true);
    assert_eq!(
        c["claim_sha256"],
        hash.as_str(),
        "the signature does not change the hash"
    );
    // Writing the same claim again keeps the signed one.
    let o = run(
        &cfg,
        &[
            "claim-serving",
            dir_s,
            "--no-wait",
            "--engine-version",
            "llama.cpp b1-test",
            "--base-url",
            &format!("{proxy}/v1"),
            "--public-url",
            "https://llm.example.com/v1",
            "--wallet",
            DEV_WALLET,
        ],
        &[],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(stdout(&o).contains("kept it"), "{}", stdout(&o));
    let c: serde_json::Value = serde_json::from_slice(&std::fs::read(&claimp).unwrap()).unwrap();
    assert_eq!(c["signed"], true);

    // The proxy in front of the mock engine.
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_tensorseed"))
        .arg("--config")
        .arg(&cfg)
        .args([
            "sign-responses",
            "--upstream",
            &server.uri(),
            "--claim",
            claimp.to_str().unwrap(),
            "--listen",
            &format!("127.0.0.1:{port}"),
        ])
        .env("RUST_LOG", "warn")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let http = reqwest::Client::new();
    let well_known = format!("{proxy}/.well-known/tensorseed/served-weights.json");
    let mut up = false;
    for _ in 0..100 {
        if let Ok(r) = http.get(&well_known).send().await
            && r.status().is_success()
        {
            up = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(up, "the proxy came up");

    // A signed response verifies; tampering, a wrong id, or a wrong model fail.
    let r = http
        .post(format!("{proxy}/v1/chat/completions"))
        .header("x-tensorseed-request-id", "buyer-1")
        .json(&serde_json::json!({"model": "org/tiny", "messages": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.headers()["x-tensorseed-request-id"], "buyer-1");
    assert_eq!(r.headers()["x-tensorseed-claim-sha256"], hash.as_str());
    let hp = tmp.path().join("h.txt");
    let bp = tmp.path().join("b.json");
    save_response(r, &hp, &bp).await;
    let verify = |claim: String, body: PathBuf, extra: Vec<String>| {
        let cfg = cfg.clone();
        let hp = hp.clone();
        async move {
            let mut a = vec![
                "verify-response".to_string(),
                "--claim".into(),
                claim,
                "--headers".into(),
                hp.to_str().unwrap().into(),
                "--body".into(),
                body.to_str().unwrap().into(),
            ];
            a.extend(extra);
            let a: Vec<&str> = a.iter().map(String::as_str).collect();
            run(&cfg, &a, &[]).await
        }
    };
    let o = verify(
        proxy.clone(),
        bp.clone(),
        vec!["--request-id".into(), "buyer-1".into()],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stdout(&o));
    assert!(stdout(&o).contains(DEV_WALLET), "{}", stdout(&o));
    let o = verify(
        claimp.to_str().unwrap().into(),
        bp.clone(),
        vec!["--json".into()],
    )
    .await;
    assert_eq!(code(&o), 0);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["valid"], true);
    assert_eq!(v["seller_wallet"], DEV_WALLET);
    let o = verify(
        proxy.clone(),
        bp.clone(),
        vec!["--request-id".into(), "other".into()],
    )
    .await;
    assert_eq!(code(&o), 1);
    let o = verify(
        proxy.clone(),
        bp.clone(),
        vec!["--model-id".into(), "org/else".into()],
    )
    .await;
    assert_eq!(code(&o), 1);
    let tampered = tmp.path().join("t.json");
    let mut b = std::fs::read(&bp).unwrap();
    let i = b.iter().position(|&c| c == b'H').unwrap();
    b[i] = b'J';
    std::fs::write(&tampered, &b).unwrap();
    let o = verify(proxy.clone(), tampered, vec!["--json".into()]).await;
    assert_eq!(code(&o), 1);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["error"], "body_hash_mismatch");

    // check-endpoint through the proxy finds the claim and verifies every
    // response; straight at the engine with the claim, it fails unsigned.
    let refp = tmp.path().join("ref.json");
    let o = run(
        &cfg,
        &[
            "reference-logprobs",
            dir_s,
            "--no-wait",
            "--endpoint",
            &format!("{}/v1", server.uri()),
            "--count",
            "4",
            "--engine-version",
            "llama.cpp b1-test",
            "--out",
            refp.to_str().unwrap(),
        ],
        &[],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let o = run(
        &cfg,
        &[
            "check-endpoint",
            &format!("{proxy}/v1"),
            "-r",
            refp.to_str().unwrap(),
            "--sample",
            "0",
            "--require-signatures",
            "--json",
        ],
        &[],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stdout(&o));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["verdict"], "pass");
    assert_eq!(v["signatures"]["verified"], 4);
    assert_eq!(v["signatures"]["invalid"], 0);
    assert_eq!(v["signatures"]["claim_sha256"], hash.as_str());
    assert_eq!(v["signatures"]["seller_wallet"], DEV_WALLET);
    assert_eq!(v["challenges"][0]["signature"]["status"], "verified");
    let o = run(
        &cfg,
        &[
            "check-endpoint",
            &format!("{}/v1", server.uri()),
            "-r",
            refp.to_str().unwrap(),
            "--claim",
            claimp.to_str().unwrap(),
        ],
        &[],
    )
    .await;
    assert_eq!(code(&o), 1, "{}", stdout(&o));
    assert!(stdout(&o).contains("signature absent"), "{}", stdout(&o));
    let o = run(
        &cfg,
        &[
            "check-endpoint",
            &format!("{}/v1", server.uri()),
            "-r",
            refp.to_str().unwrap(),
            "--require-signatures",
        ],
        &[],
    )
    .await;
    assert_eq!(code(&o), 2, "no claim served: an error, not a verdict");

    // No revocations yet: the proxy answers 404 and the claim passes.
    let revs_url = format!("{proxy}/.well-known/tensorseed/revocations.json");
    let r = http.get(&revs_url).send().await.unwrap();
    assert_eq!(r.status(), 404);
    let o = run(
        &cfg,
        &["check-claim", &format!("{proxy}/v1"), "--no-wait", "--json"],
        &[],
    )
    .await;
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let st = |v: &serde_json::Value, name: &str| {
        v["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .map(|c| c["status"].as_str().unwrap().to_string())
            .unwrap_or_else(|| panic!("no {name} in {v}"))
    };
    assert_eq!(st(&v, "revocation"), "pass", "{v}");
    assert_eq!(st(&v, "expiry"), "warn", "this claim has no expires_at");
    assert_eq!(v["revoked"], false);

    // The wallet can revoke too: --print-message shows what to sign.
    let at = "2026-01-01T00:00:00Z";
    let o = run(
        &cfg,
        &[
            "revoke-claim",
            claimp.to_str().unwrap(),
            "--at",
            at,
            "--print-message",
        ],
        &[],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let msg =
        format!("ts.claim-revocation/1\nclaim_sha256={hash}\nrevoked_at={at}\nreason=superseded\n");
    assert!(stdout(&o).contains(&msg), "{}", stdout(&o));
    assert!(!claimp.with_file_name("revocations.json").exists());
    // A bad reason or time is refused before anything is written.
    let o = run(
        &cfg,
        &[
            "revoke-claim",
            claimp.to_str().unwrap(),
            "--reason",
            "Bad Reason",
        ],
        &[],
    )
    .await;
    assert_ne!(code(&o), 0);
    // Revoke with the response key and the wallet, dated in the past.
    let o = run(
        &cfg,
        &[
            "revoke-claim",
            claimp.to_str().unwrap(),
            "--at",
            at,
            "--wallet-sig",
            &dev_wallet_sign(&msg),
        ],
        &[],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(
        stdout(&o).contains("the response key and the seller wallet"),
        "{}",
        stdout(&o)
    );
    let listp = claimp.with_file_name("revocations.json");
    let list: serde_json::Value = serde_json::from_slice(&std::fs::read(&listp).unwrap()).unwrap();
    assert_eq!(list["schema"], "ts.claim-revocations/1");
    assert_eq!(list["revocations"][0]["claim_sha256"], hash.as_str());
    assert_eq!(list["revocations"][0]["revoked_at"], at);
    // The running proxy serves the new list at once.
    let r = http.get(&revs_url).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.bytes().await.unwrap(), std::fs::read(&listp).unwrap());
    // check-claim finds it next to the claim and fails; from a file it
    // needs --revocations.
    let o = run(
        &cfg,
        &["check-claim", &format!("{proxy}/v1"), "--no-wait", "--json"],
        &[],
    )
    .await;
    assert_eq!(code(&o), 1, "{}", stdout(&o));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(st(&v, "revocation"), "fail");
    assert_eq!(v["revoked"], true);
    let o = run(
        &cfg,
        &[
            "check-claim",
            claimp.to_str().unwrap(),
            "--no-wait",
            "--json",
        ],
        &[],
    )
    .await;
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(st(&v, "revocation"), "skip");
    let o = run(
        &cfg,
        &[
            "check-claim",
            claimp.to_str().unwrap(),
            "--no-wait",
            "--json",
            "--revocations",
            listp.to_str().unwrap(),
        ],
        &[],
    )
    .await;
    assert_eq!(code(&o), 1);
    // A response signed after the revocation time no longer verifies.
    let o = verify(proxy.clone(), bp.clone(), vec!["--json".into()]).await;
    assert_eq!(code(&o), 1, "{}", stdout(&o));
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    assert_eq!(v["error"], "claim_revoked");
    let o = verify(
        claimp.to_str().unwrap().into(),
        bp.clone(),
        vec!["--revocations".into(), listp.to_str().unwrap().into()],
    )
    .await;
    assert_eq!(code(&o), 1, "{}", stdout(&o));
    // A revoked claim is never reused: writing again makes a new one.
    let o = run(
        &cfg,
        &[
            "claim-serving",
            dir_s,
            "--no-wait",
            "--engine-version",
            "llama.cpp b1-test",
            "--base-url",
            &format!("{proxy}/v1"),
            "--public-url",
            "https://llm.example.com/v1",
            "--wallet",
            DEV_WALLET,
            "--expires-in",
            "30d",
        ],
        &[],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(!stdout(&o).contains("kept it"), "{}", stdout(&o));
    let c: serde_json::Value = serde_json::from_slice(&std::fs::read(&claimp).unwrap()).unwrap();
    assert_ne!(c["claim_sha256"], hash.as_str());
    assert!(c["expires_at"].as_str().is_some());
    assert!(stdout(&o).contains("expires      "), "{}", stdout(&o));
    let o = run(
        &cfg,
        &["claim-serving", dir_s, "--no-wait", "--expires-in", "5m"],
        &[],
    )
    .await;
    assert_ne!(code(&o), 0, "under an hour is refused");

    let _ = child.kill().await;
    let out = child.wait_with_output().await.unwrap();
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!all.contains(&secret), "the proxy never prints the key");
}

#[tokio::test(flavor = "multi_thread")]
async fn serve_inference_sign_moves_the_runtime_behind_the_proxy() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let (cfg, dir, _) = pulled_gguf(tmp.path(), &server, "apache-2.0").await;
    let dir_s = dir.to_str().unwrap();
    let args = [
        "serve-inference",
        dir_s,
        "--no-wait",
        "--sign",
        "--print-command",
    ];
    let o = run(&cfg, &args, &[("PATH", "")]).await;
    assert_eq!(code(&o), 1);
    assert!(stderr(&o).contains("response-key create"), "{}", stderr(&o));
    let o = run(&cfg, &["response-key", "create"], &[]).await;
    assert_eq!(code(&o), 0);
    let o = run(&cfg, &args, &[("PATH", "")]).await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let s = stdout(&o);
    assert!(s.contains("--host 127.0.0.1 --port "), "{s}");
    assert!(!s.contains("--port 8090"), "the proxy keeps 8090: {s}");
    assert!(
        stderr(&o).contains("signing proxy listens on 127.0.0.1:8090"),
        "{}",
        stderr(&o)
    );
    let o = run(
        &cfg,
        &[
            "serve-inference",
            dir_s,
            "--no-wait",
            "--sign",
            "--wallet",
            "0x12",
            "--print-command",
        ],
        &[],
    )
    .await;
    assert_eq!(code(&o), 1, "a malformed wallet is refused");
}

/// `tensorseed gui` starts the Desktop binary it finds with the entry link
/// and the same config, and says where to get Desktop when there is none.
#[cfg(unix)]
#[test]
fn gui_starts_desktop_or_says_where_to_get_it() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let cfg = tmp.path().join("tensorseed.toml");
    std::fs::write(&cfg, "").unwrap();
    let run = |path: &Path| {
        std::process::Command::new(env!("CARGO_BIN_EXE_tensorseed"))
            .args(["--config", cfg.to_str().unwrap(), "gui", "org/tiny"])
            .env("PATH", path)
            .env("HOME", tmp.path())
            .env_remove("TENSORSEED_DESKTOP")
            .output()
            .unwrap()
    };

    let out = run(&bin);
    let err = String::from_utf8_lossy(&out.stderr);
    assert_eq!(code(&out), 1, "{err}");
    assert!(err.contains("Tensorseed Desktop is not installed"), "{err}");
    assert!(err.contains("https://tensorseed.dev"), "{err}");

    let log = tmp.path().join("started");
    let fake = bin.join("tensorseed-desktop");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\necho \"$1 $TENSORSEED_CONFIG\" > {}\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = run(&bin);
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout(&out).contains("opened Tensorseed Desktop (PATH)"));
    let mut seen = String::new();
    for _ in 0..50 {
        seen = std::fs::read_to_string(&log).unwrap_or_default();
        if !seen.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(
        seen.trim(),
        format!("tensorseed://entry/model/org/tiny {}", cfg.display())
    );
}

/// Edit a claim and seal it again (as a seller could), dropping any
/// wallet signature unless `keep_sig`.
fn reseal(
    c: &serde_json::Value,
    edit: impl FnOnce(&mut serde_json::Value),
    keep_sig: bool,
) -> Vec<u8> {
    let mut v = c.clone();
    edit(&mut v);
    if !keep_sig {
        v.as_object_mut().unwrap().remove("sig");
        v["signed"] = serde_json::json!(false);
    }
    let mut claim: tensorseed_app::signing::ServedClaim = serde_json::from_value(v).unwrap();
    claim.seal().unwrap();
    serde_json::to_vec_pretty(&claim).unwrap()
}

async fn serve_claim(body: &[u8]) -> MockServer {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/.well-known/tensorseed/served-weights.json"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.to_vec()))
        .mount(&s)
        .await;
    s
}

#[tokio::test(flavor = "multi_thread")]
async fn check_claim_against_the_catalog() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let (cfg, dir, _) = pulled_gguf(tmp.path(), &server, "apache-2.0").await;
    let dir_s = dir.to_str().unwrap();
    let o = run(&cfg, &["response-key", "create"], &[]).await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));

    // The seller's public endpoint serves its claim at the well-known path.
    // Its URL is known before the claim is written, so the claim names it.
    let public = MockServer::start().await;
    let public_url = format!("{}/v1", public.uri());
    let o = run(
        &cfg,
        &[
            "claim-serving",
            dir_s,
            "--no-wait",
            "--engine-version",
            "llama.cpp b1-test",
            "--base-url",
            "http://127.0.0.1:8090/v1",
            "--public-url",
            &public_url,
            "--wallet",
            DEV_WALLET,
        ],
        &[("PATH", "")],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let claimp = dir.join(".tensorseed/served-weights.json");
    let unsigned: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&claimp).unwrap()).unwrap();
    let hash = unsigned["claim_sha256"].as_str().unwrap().to_string();
    let o = run(
        &cfg,
        &[
            "claim-serving",
            claimp.to_str().unwrap(),
            "--sig",
            &dev_wallet_sign(&format!("tensorseed served-weights/1:\n{hash}")),
        ],
        &[],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let signed_raw = std::fs::read(&claimp).unwrap();
    let signed: serde_json::Value = serde_json::from_slice(&signed_raw).unwrap();
    Mock::given(method("GET"))
        .and(path("/.well-known/tensorseed/served-weights.json"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(signed_raw.clone()))
        .mount(&public)
        .await;

    let check = |src: String, extra: &[&str]| {
        let cfg = cfg.clone();
        let extra: Vec<String> = extra.iter().map(|s| (*s).to_string()).collect();
        async move {
            let mut a = vec!["check-claim".to_string(), src];
            a.extend(extra);
            let a: Vec<&str> = a.iter().map(String::as_str).collect();
            run(&cfg, &a, &[]).await
        }
    };
    let json = |o: &Output| -> serde_json::Value {
        serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", stdout(o)))
    };
    let status = |v: &serde_json::Value, name: &str| -> String {
        v["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == name)
            .map(|c| c["status"].as_str().unwrap().to_string())
            .unwrap_or_else(|| panic!("no {name} check in {v}"))
    };

    // A good claim, fetched from the endpoint, with the catalog synced.
    let o = check(public_url.clone(), &["--json"]).await;
    assert_eq!(code(&o), 0, "{}\n{}", stdout(&o), stderr(&o));
    let v = json(&o);
    assert_eq!(v["schema"], "ts.claim-check/1");
    assert_eq!(v["verdict"], "pass");
    assert_eq!(v["claim_sha256"], hash.as_str());
    assert_eq!(v["catalog"], "main");
    assert_eq!(v["seller_wallet"], DEV_WALLET);
    assert_eq!(v["wallet_verified"], true);
    assert_eq!(v["license"]["commercial_serving"], "allowed");
    assert_eq!(v["license"]["claimed"], "allowed");
    assert_eq!(v["freshness"]["synced"], true);
    assert_eq!(
        v["origin"]["location"],
        format!(
            "{}/.well-known/tensorseed/served-weights.json",
            public.uri()
        )
    );
    for name in [
        "claim_sha256",
        "claim_fields",
        "wallet_signature",
        "public_url",
        "catalog",
        "publisher_key",
        "manifest",
        "files",
        "takedown",
        "freshness",
        "license",
    ] {
        assert_eq!(status(&v, name), "pass", "{name}: {v}");
    }
    assert_eq!(status(&v, "https"), "warn", "a loopback test URL is http");
    // Text output.
    let o = check(public_url.clone(), &["--no-wait"]).await;
    assert_eq!(code(&o), 0, "{}", stdout(&o));
    let t = stdout(&o);
    assert!(t.contains("verdict: pass"), "{t}");
    assert!(t.contains(&format!("{DEV_WALLET} (verified)")), "{t}");
    assert!(t.contains("warn freshness"), "{t}");
    assert!(t.contains("apache-2.0 -> allowed"), "{t}");
    // Not synced is a failure only when asked.
    let o = check(
        public_url.clone(),
        &["--no-wait", "--require-fresh", "--json"],
    )
    .await;
    assert_eq!(code(&o), 1);
    assert_eq!(status(&json(&o), "freshness"), "fail");

    // From a file: the URL check is skipped unless an expected URL is given.
    let file = claimp.to_str().unwrap().to_string();
    let o = check(file.clone(), &["--no-wait", "--json"]).await;
    assert_eq!(code(&o), 0, "{}", stdout(&o));
    assert_eq!(status(&json(&o), "public_url"), "skip");
    let o = check(
        file.clone(),
        &["--no-wait", "--json", "--expect-url", &public_url],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stdout(&o));
    assert_eq!(status(&json(&o), "public_url"), "pass");
    let o = check(
        file.clone(),
        &[
            "--no-wait",
            "--json",
            "--expect-url",
            "https://other.example/v1",
        ],
    )
    .await;
    assert_eq!(code(&o), 1);
    assert_eq!(status(&json(&o), "public_url"), "fail");

    // The same valid claim served by another endpoint: copied.
    let copier = serve_claim(&signed_raw).await;
    let o = check(format!("{}/v1", copier.uri()), &["--no-wait", "--json"]).await;
    assert_eq!(code(&o), 1, "{}", stdout(&o));
    let v = json(&o);
    assert_eq!(status(&v, "public_url"), "fail");
    assert_eq!(status(&v, "wallet_signature"), "pass");

    // Edited without resealing: the hash no longer matches.
    let mut t = signed.clone();
    t["served_model_id"] = serde_json::json!("bigger-model");
    let p = tmp.path().join("edited.json");
    std::fs::write(&p, serde_json::to_vec(&t).unwrap()).unwrap();
    let o = check(p.to_str().unwrap().into(), &["--no-wait", "--json"]).await;
    assert_eq!(code(&o), 1);
    let v = json(&o);
    assert_eq!(status(&v, "claim_sha256"), "fail");
    assert_eq!(status(&v, "claim_fields"), "skip");
    assert_eq!(status(&v, "wallet_signature"), "skip");
    assert_eq!(v["wallet_verified"], false);

    let write = |name: &str, body: Vec<u8>| {
        let p = tmp.path().join(name);
        std::fs::write(&p, body).unwrap();
        p.to_str().unwrap().to_string()
    };
    // A better license mark, resealed by the seller: recomputed and refused.
    let p = write(
        "license.json",
        reseal(
            &signed,
            |v| v["license"]["commercial_serving"] = serde_json::json!("restricted"),
            false,
        ),
    );
    let o = check(p.clone(), &["--no-wait", "--json"]).await;
    assert_eq!(code(&o), 1);
    let v = json(&o);
    assert_eq!(status(&v, "claim_sha256"), "pass");
    assert_eq!(status(&v, "license"), "fail");
    assert_eq!(v["license"]["commercial_serving"], "allowed");
    assert_eq!(v["license"]["claimed"], "restricted");
    // Unsigned: a warning, or a failure with --require-wallet-sig.
    assert_eq!(status(&v, "wallet_signature"), "warn");
    assert_eq!(v["wallet_verified"], false);
    let o = check(p, &["--no-wait", "--json", "--require-wallet-sig"]).await;
    assert_eq!(status(&json(&o), "wallet_signature"), "fail");

    // Resealed with the old wallet signature kept: it no longer recovers.
    let p = write(
        "moved.json",
        reseal(
            &signed,
            |v| v["served_model_id"] = serde_json::json!("other-id"),
            true,
        ),
    );
    let o = check(p, &["--no-wait", "--json"]).await;
    assert_eq!(code(&o), 1);
    let v = json(&o);
    assert_eq!(status(&v, "claim_sha256"), "pass");
    assert_eq!(status(&v, "wallet_signature"), "fail");
    // The wallet a buyer expects.
    let o = check(
        file.clone(),
        &[
            "--no-wait",
            "--json",
            "--expect-wallet",
            "0x0000000000000000000000000000000000000001",
        ],
    )
    .await;
    assert_eq!(code(&o), 1);
    assert_eq!(status(&json(&o), "wallet_signature"), "fail");
    let o = check(
        file.clone(),
        &[
            "--no-wait",
            "--json",
            "--expect-wallet",
            &DEV_WALLET.to_uppercase().replace("0X", "0x"),
        ],
    )
    .await;
    assert_eq!(code(&o), 0, "{}", stdout(&o));

    // A catalog the buyer does not trust.
    let p = write(
        "catalog.json",
        reseal(
            &signed,
            |v| v["catalog"]["name"] = serde_json::json!("elsewhere"),
            false,
        ),
    );
    let o = check(p.clone(), &["--no-wait", "--json"]).await;
    assert_eq!(code(&o), 1);
    let v = json(&o);
    assert_eq!(status(&v, "catalog"), "fail");
    assert!(v["checks"].to_string().contains("not one you trust"), "{v}");
    // Forcing the trusted catalog still flags the name.
    let o = check(p, &["--no-wait", "--json", "--catalog", "main"]).await;
    assert_eq!(code(&o), 1);
    assert_eq!(status(&json(&o), "catalog"), "fail");

    // A manifest hash, publisher key, or file the catalog does not have.
    let p = write(
        "manifest.json",
        reseal(
            &signed,
            |v| v["catalog"]["manifest_sha256"] = serde_json::json!("ab".repeat(32)),
            false,
        ),
    );
    let o = check(p, &["--no-wait", "--json"]).await;
    assert_eq!(code(&o), 1);
    assert_eq!(status(&json(&o), "manifest"), "fail");
    let p = write(
        "key.json",
        reseal(
            &signed,
            |v| v["catalog"]["publisher_key_id"] = serde_json::json!("0123456789ABCDEF"),
            false,
        ),
    );
    let o = check(p, &["--no-wait", "--json"]).await;
    assert_eq!(code(&o), 1);
    assert_eq!(status(&json(&o), "publisher_key"), "fail");
    let p = write(
        "files.json",
        reseal(
            &signed,
            |v| v["files"][0]["sha256"] = serde_json::json!("cd".repeat(32)),
            false,
        ),
    );
    let o = check(p, &["--no-wait", "--json"]).await;
    assert_eq!(code(&o), 1);
    assert_eq!(status(&json(&o), "files"), "fail");
    // A revision that is not in the catalog.
    let p = write(
        "rev.json",
        reseal(
            &signed,
            |v| v["revision"] = serde_json::json!("ef".repeat(20)),
            false,
        ),
    );
    let o = check(p, &["--no-wait", "--json"]).await;
    assert_eq!(code(&o), 1);
    let v = json(&o);
    assert_eq!(status(&v, "manifest"), "fail");
    assert!(v["checks"].to_string().contains("not in catalog"), "{v}");

    // Nothing to check: exit 2.
    let empty = MockServer::start().await;
    let o = check(format!("{}/v1", empty.uri()), &["--no-wait"]).await;
    assert_eq!(code(&o), 2);
    assert!(stderr(&o).contains("HTTP 404"), "{}", stderr(&o));
    let o = check(
        tmp.path().join("missing.json").to_str().unwrap().into(),
        &[],
    )
    .await;
    assert_eq!(code(&o), 2);
    // Not a claim at all: a failed check, not an error.
    let p = write("v0.json", br#"{"schema":"ts.other/1"}"#.to_vec());
    let o = check(p, &["--no-wait", "--json"]).await;
    assert_eq!(code(&o), 1);
    assert_eq!(status(&json(&o), "claim"), "fail");
}
