// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! End to end tests of `tensorseed serve` on loopback: the daemon syncs a
//! signed file catalog, downloads and seeds the selected revision, serves it
//! over HTTP with ranges and mirrors the catalog, reports health, feeds a
//! second `pull --swarm`, drops a takedown, stops on SIGTERM, and resumes
//! from its state file.

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Child, Output};
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tensorseed_catalog::file::{FileCatalog, FileCatalogConfig};
use tensorseed_catalog::minisign::SecretKey;
use tensorseed_catalog::sink::DirSink;
use tensorseed_catalog::{CatalogBackend, CatalogId, CatalogOp};
use tensorseed_core::keys::{RepoRef, RepoType, catalog_info_key};
use tensorseed_core::manifest::*;
use tensorseed_torrent::{BuildParams, InputFile, build_torrent};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const C: &str = "0123456789abcdef0123456789abcdef01234567";
const NAME: &str = "org__a@0123456789ab";

fn key() -> SecretKey {
    SecretKey::from_seed(&[21; 32])
}

fn canon<T: serde::Serialize>(v: &T) -> Vec<u8> {
    tensorseed_core::canonical::to_canonical_vec(v).unwrap()
}

fn files() -> Vec<(String, Vec<u8>)> {
    let big: Vec<u8> = (0..300_000u32).map(|i| (i * 13 % 251) as u8).collect();
    vec![
        ("config.json".into(), br#"{"a":1}"#.to_vec()),
        ("model.safetensors".into(), big),
    ]
}

/// Write the publisher tree (`t/`, torrent) and return the manifest.
fn build_revision(root: &Path, webseed: &str) -> Manifest {
    let content = root.join("t").join(NAME);
    std::fs::create_dir_all(&content).unwrap();
    for (p, b) in files() {
        std::fs::write(content.join(&p), b).unwrap();
    }
    let inputs: Vec<InputFile> = files()
        .iter()
        .map(|(p, b)| InputFile {
            path: p.clone(),
            length: b.len() as u64,
        })
        .collect();
    let t = build_torrent(
        &content,
        &inputs,
        &BuildParams {
            name: NAME.into(),
            piece_length: 16384,
            hybrid: true,
            trackers: vec![],
            webseeds: vec![webseed.into()],
        },
        0,
    )
    .unwrap();
    std::fs::write(root.join("t").join(format!("{NAME}.torrent")), &t.bytes).unwrap();
    let fes: Vec<FileEntry> = files()
        .iter()
        .map(|(p, b)| FileEntry {
            path: p.clone(),
            size: b.len() as u64,
            sha256: hex::encode(Sha256::digest(b)),
            hf_lfs_oid: None,
            bt_v2_pieces_root: t.file(p).unwrap().pieces_root.map(hex::encode),
            format: None,
            pickle_risk: false,
        })
        .collect();
    Manifest {
        schema: SCHEMA_MANIFEST.into(),
        repo: RepoInfo {
            repo_type: RepoType::Model,
            id: "org/a".into(),
            hf_url: "https://huggingface.co/org/a".into(),
        },
        hf_commit: C.into(),
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
            files: fes.len() as u64,
            bytes: fes.iter().map(|f| f.size).sum(),
        },
        files: fes,
        file_pages: 0,
        torrent: Some(t.torrent_info(vec![format!("{webseed}{NAME}.torrent")])),
        webseeds: vec![webseed.into()],
        backups: vec![],
        published_at: "2026-11-12T00:00:00Z".into(),
        publisher_tool: "test".into(),
    }
}

/// Publish one catalog revision into `out` (catalog `main`).
async fn publish(out: &Path, state: &Path, ops: Vec<CatalogOp>) {
    let mut cfg = FileCatalogConfig::new("main", "http://unused.invalid/", key().public_key());
    cfg.state_dir = Some(state.to_path_buf());
    let cat = FileCatalog::new(cfg)
        .unwrap()
        .with_signer(key())
        .unwrap()
        .with_sink(Arc::new(DirSink::new(out)));
    let id = CatalogId("main".into());
    cat.stage(&id, &ops).await.unwrap();
    cat.commit(&id).await.unwrap();
}

fn entry_ops(m: &Manifest, taken_down: bool) -> Vec<CatalogOp> {
    let repo = RepoRef::new(RepoType::Model, "org/a").unwrap();
    let up = |key: String, value: Vec<u8>| CatalogOp::Upsert { key, value };
    let mut ops = vec![
        up(catalog_info_key(), br#"{"schema":"ts.catalog/1"}"#.to_vec()),
        up(
            repo.manifest_key(C).unwrap(),
            m.to_canonical_json().unwrap(),
        ),
        up(
            repo.latest_key(),
            canon(&LatestPointer {
                schema: SCHEMA_LATEST.into(),
                commit: C.into(),
                manifest_key: repo.manifest_key(C).unwrap(),
                updated_at: "2026-11-12T00:00:00Z".into(),
            }),
        ),
        up(
            repo.index_key(),
            canon(&IndexEntry {
                schema: SCHEMA_INDEX.into(),
                latest_commit: C.into(),
                license_spdx: Some("apache-2.0".into()),
                total_bytes: m.totals.bytes,
                updated_at: "2026-11-12T00:00:00Z".into(),
                taken_down,
            }),
        ),
    ];
    if taken_down {
        ops.push(up(
            repo.tombstone_key(),
            canon(&Tombstone {
                schema: SCHEMA_TOMBSTONE.into(),
                reason: TombstoneReason::Legal,
                detail: "test takedown".into(),
                effective_at: "2026-11-12T00:00:00Z".into(),
                revisions: vec![],
            }),
        ));
    }
    ops
}

async fn mount_dir(server: &MockServer, dir: &Path) {
    fn walk(root: &Path, d: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for e in std::fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
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

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn write_config(dir: &Path, catalog: &str, extra: &str) -> PathBuf {
    let d = dir.display();
    let pk = key().public_key().to_base64();
    let text = format!(
        r#"[general]
state_db = "{d}/state.db"
download_dir = "{d}/dl"

[catalog]
backend = "file"
name = "main"
publisher_pubkey = "{pk}"

[file]
base_url = "{catalog}/"
state_dir = "{d}/catstate"

[huggingface]
endpoint = "http://127.0.0.1:9"
token_env = "TS_TEST_NO_SUCH_TOKEN"

[mirrors]
file = "{d}/mirrors.toml"
fetch_published = false

[torrent]
listen_addr = "127.0.0.1"
dht = false

[storage]
local_dir = "{d}/publish"
{extra}
"#
    );
    let p = dir.join("tensorseed.toml");
    std::fs::create_dir_all(dir).unwrap();
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

/// A running `tensorseed serve` with its stdout lines.
struct Daemon {
    child: Child,
    lines: Receiver<String>,
    seen: String,
}

impl Daemon {
    fn spawn(config: &Path, args: &[&str]) -> Self {
        let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_tensorseed"));
        c.arg("--config")
            .arg(config)
            .arg("serve")
            .args(args)
            .env_remove("HF_TOKEN")
            .env("RUST_LOG", "warn")
            .stdout(std::process::Stdio::piped());
        let mut child = c.spawn().unwrap();
        let out = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                eprintln!("[serve] {line}");
                let _ = tx.send(line);
            }
        });
        Self {
            child,
            lines: rx,
            seen: String::new(),
        }
    }

    /// Wait for a line containing `pat`; returns it.
    fn wait_for(&mut self, pat: &str, secs: u64) -> String {
        let end = Instant::now() + Duration::from_secs(secs);
        loop {
            let left = end.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(l) => {
                    self.seen.push_str(&l);
                    self.seen.push('\n');
                    if l.contains(pat) {
                        return l;
                    }
                }
                Err(_) => panic!("no {pat:?} from serve within {secs}s:\n{}", self.seen),
            }
        }
    }

    /// `ADDR:PORT` from a line like `health   http://127.0.0.1:1234/health`.
    fn addr(line: &str) -> String {
        let rest = line.split("http://").nth(1).unwrap();
        rest.split('/').next().unwrap().to_owned()
    }

    fn sigterm(&mut self) -> std::process::ExitStatus {
        let pid = self.child.id().to_string();
        let ok = std::process::Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .unwrap();
        assert!(ok.success());
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            if let Some(s) = self.child.try_wait().unwrap() {
                // Drain the rest of the output.
                while let Ok(l) = self.lines.recv_timeout(Duration::from_millis(500)) {
                    self.seen.push_str(&l);
                    self.seen.push('\n');
                }
                return s;
            }
            assert!(Instant::now() < end, "serve did not exit on SIGTERM");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn health(addr: &str) -> serde_json::Value {
    let r = reqwest::get(format!("http://{addr}/health")).await.unwrap();
    serde_json::from_str(&r.text().await.unwrap()).unwrap()
}

/// Poll the health endpoint until `f` holds.
async fn health_until(
    addr: &str,
    what: &str,
    f: impl Fn(&serde_json::Value) -> bool,
) -> serde_json::Value {
    let end = Instant::now() + Duration::from_secs(60);
    loop {
        let v = health(addr).await;
        if f(&v) {
            return v;
        }
        assert!(Instant::now() < end, "health never showed {what}: {v:#}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn serve_fetches_seeds_serves_and_drops_takedowns() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = MockServer::start().await;
    let pubdir = tmp.path().join("pub");
    let pubstate = tmp.path().join("pubstate");
    let m = build_revision(&pubdir, &format!("{}/t/", origin.uri()));
    publish(&pubdir, &pubstate, entry_ops(&m, false)).await;
    mount_dir(&origin, &pubdir).await;

    // The daemon: fetch over HTTP (no swarm yet), seed, serve HTTP and the catalog.
    let port = free_port();
    let srv = tmp.path().join("srv");
    let scfg = write_config(
        &srv,
        &origin.uri(),
        &format!(
            "\n[serve]\ndata_dir = \"{}/serve\"\nselect = [\"org/*\"]\nfetch = true\nfetch_swarm = false\nsync_interval_secs = 1\nhttp_listen = \"127.0.0.1:0\"\nhealth_listen = \"127.0.0.1:0\"\n",
            srv.display()
        ),
    );
    let mut text = std::fs::read_to_string(&scfg).unwrap();
    text = text.replace("dht = false", &format!("dht = false\nlisten_port = {port}"));
    std::fs::write(&scfg, text).unwrap();
    let mut d = Daemon::spawn(&scfg, &["--serve-catalog", "--for", "300s"]);
    let haddr = Daemon::addr(&d.wait_for("health ", 30));
    let web = Daemon::addr(&d.wait_for("http     http://", 30));
    d.wait_for(&format!("seeding  {NAME}"), 60);

    let v = health_until(&haddr, "one torrent", |v| {
        v["torrents"].as_array().is_some_and(|a| a.len() == 1)
    })
    .await;
    assert_eq!(v["schema"], "ts.serve.health/1");
    assert_eq!(v["status"], "ok");
    assert_eq!(v["catalog"]["revision"], 1);
    assert!(
        v["catalog"]["head_expires_unix"].as_u64().unwrap() > 0,
        "{v:#}"
    );
    assert_eq!(v["catalog"]["serving"], true);
    assert_eq!(v["torrents"][0]["name"], NAME);
    assert_eq!(v["torrents"][0]["owned"], true);
    assert_eq!(v["swarm"]["utp"], true);
    assert_eq!(v["port_mapping"]["enabled"], false);
    let seeded = srv.join(format!("serve/t/{NAME}/model.safetensors"));
    assert_eq!(std::fs::read(&seeded).unwrap(), files()[1].1);

    // HTTP: ranges on the webseed path, the torrent, the catalog mirror.
    let http = reqwest::Client::new();
    let url = format!("http://{web}/t/{NAME}/model.safetensors");
    let r = http
        .get(&url)
        .header("Range", "bytes=100-199")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 206);
    assert_eq!(r.headers()["content-range"], "bytes 100-199/300000");
    assert_eq!(r.bytes().await.unwrap().as_ref(), &files()[1].1[100..200]);
    let r = http
        .get(format!("http://{web}/t/{NAME}.torrent"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.bytes().await.unwrap().as_ref(),
        std::fs::read(pubdir.join(format!("t/{NAME}.torrent"))).unwrap()
    );
    let r = http
        .get(format!("http://{web}/t/{NAME}/.tensorseed/pull.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    let r = http
        .get(format!("http://{web}/main/head.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.bytes().await.unwrap().as_ref(),
        std::fs::read(pubdir.join("main/head.json")).unwrap()
    );

    // A subscriber syncs the catalog from the daemon's mirror and pulls over
    // the swarm from the daemon only.
    let sub = tmp.path().join("sub");
    let ccfg = write_config(&sub, &format!("http://{web}"), "");
    let peer = format!("127.0.0.1:{port}");
    let out = sub.join("swarm");
    let o = run(
        &ccfg,
        &[
            "pull",
            "org/a",
            "--swarm-only",
            "--peer",
            &peer,
            "--out",
            out.to_str().unwrap(),
        ],
        &[("TENSORSEED_TORRENT__LISTEN_PORT", "0")],
    )
    .await;
    assert_eq!(o.status.code(), Some(0));
    let s = String::from_utf8_lossy(&o.stdout);
    assert!(s.contains("ok     model.safetensors (swarm)"), "{s}");
    assert!(s.contains("downloaded over HTTP (0 bytes)"), "{s}");
    let o = run(&ccfg, &["verify", out.to_str().unwrap()], &[]).await;
    assert_eq!(o.status.code(), Some(0));
    let v = health_until(&haddr, "uploaded bytes", |v| {
        v["swarm"]["uploaded_bytes"].as_u64().unwrap_or(0) >= 300_000
    })
    .await;
    assert!(v["torrents"][0]["uploaded_bytes"].as_u64().unwrap() >= 300_000);

    // The daemon as a plain HTTP mirror: origin goes away, Hub fallback off.
    origin.reset().await;
    let o = run(
        &ccfg,
        &[
            "mirror",
            "add",
            &format!("http://{web}/"),
            "--name",
            "daemon",
            "--priority",
            "1",
        ],
        &[],
    )
    .await;
    assert_eq!(o.status.code(), Some(0));
    let out2 = sub.join("http");
    let o = run(
        &ccfg,
        &[
            "pull",
            "org/a",
            "--no-hf-fallback",
            "--out",
            out2.to_str().unwrap(),
        ],
        &[],
    )
    .await;
    assert_eq!(o.status.code(), Some(0), "pull from the daemon webseed");
    assert_eq!(
        std::fs::read(out2.join("model.safetensors")).unwrap(),
        files()[1].1
    );

    // Takedown: the publisher signs a tombstone; the daemon stops and prunes.
    publish(&pubdir, &pubstate, entry_ops(&m, true)).await;
    mount_dir(&origin, &pubdir).await;
    d.wait_for(&format!("stopped  {NAME}: taken down"), 30);
    let v = health_until(&haddr, "no torrents", |v| {
        v["torrents"].as_array().is_some_and(|a| a.is_empty())
    })
    .await;
    assert_eq!(v["catalog"]["revision"], 2);
    assert_eq!(v["taken_down"][0]["item"], NAME);
    let r = http.get(&url).send().await.unwrap();
    assert_eq!(r.status(), 404, "taken down revisions are not served");
    assert!(!srv.join(format!("serve/t/{NAME}")).exists(), "pruned");
    // The catalog mirror follows (revision 2 carries the tombstone).
    let r = http
        .get(format!("http://{web}/main/deltas/0000000002.json"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    let st = d.sigterm();
    assert!(st.success(), "{st:?}\n{}", d.seen);
    assert!(d.seen.contains("SIGTERM: shutting down"), "{}", d.seen);
    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(srv.join("serve/serve-state.json")).unwrap())
            .unwrap();
    assert_eq!(state["schema"], "ts.servestate/1");
    assert!(state["uploaded_bytes_total"].as_u64().unwrap() >= 300_000);
    assert_eq!(state["revisions"].as_array().unwrap().len(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn serve_seeds_published_tree_resumes_and_respects_selection() {
    let tmp = tempfile::tempdir().unwrap();
    let origin = MockServer::start().await;
    let host = tmp.path().join("host");
    // The publisher runs serve over its own tree (storage.local_dir).
    let pubdir = host.join("publish");
    let m = build_revision(&pubdir, &format!("{}/t/", origin.uri()));
    publish(&pubdir, &tmp.path().join("pubstate"), entry_ops(&m, false)).await;
    mount_dir(&origin, &pubdir).await;
    let cfg = write_config(
        &host,
        &origin.uri(),
        "\n[serve]\nsync_interval_secs = 1\nhealth_listen = \"127.0.0.1:0\"\n",
    );
    let mut text = std::fs::read_to_string(&cfg).unwrap();
    text = text.replace(
        "[serve]\n",
        &format!("[serve]\ndata_dir = \"{}/serve\"\n", host.display()),
    );
    text = text.replace("dht = false", "dht = false\nlisten_port = 0");
    std::fs::write(&cfg, &text).unwrap();

    let mut d = Daemon::spawn(&cfg, &[]);
    let haddr = Daemon::addr(&d.wait_for("health ", 30));
    d.wait_for(
        &format!(
            "seeding  {NAME} from {}",
            pubdir.join("t").join(NAME).display()
        ),
        60,
    );
    let v = health_until(&haddr, "one torrent", |v| {
        v["torrents"].as_array().is_some_and(|a| a.len() == 1)
    })
    .await;
    assert_eq!(v["torrents"][0]["owned"], false);
    assert!(d.sigterm().success());

    // Restart: seeding resumes from the state file before the first sync.
    let mut d = Daemon::spawn(&cfg, &["--sync-interval", "3600"]);
    d.wait_for("resumed 1 revision(s)", 60);
    assert!(d.sigterm().success());
    assert!(
        pubdir.join("t").join(NAME).exists(),
        "published trees are never deleted"
    );

    // Deselected by config: stopped on the first sync, files kept.
    std::fs::write(
        &cfg,
        text.replace("[serve]\n", "[serve]\nselect = [\"other/*\"]\n"),
    )
    .unwrap();
    let mut d = Daemon::spawn(&cfg, &[]);
    d.wait_for(&format!("stopped  {NAME}: no longer selected"), 60);
    assert!(d.sigterm().success());
    assert!(
        pubdir
            .join("t")
            .join(NAME)
            .join("model.safetensors")
            .exists()
    );

    // fetch = false and nothing on disk: reported, not downloaded.
    std::fs::write(
        &cfg,
        text.replace(
            &format!("local_dir = \"{}/publish\"", host.display()),
            &format!("local_dir = \"{}/empty\"", host.display()),
        ),
    )
    .unwrap();
    let mut d = Daemon::spawn(&cfg, &[]);
    let haddr = Daemon::addr(&d.wait_for("health ", 30));
    let v = health_until(&haddr, "a skipped note", |v| {
        v["skipped"].as_array().is_some_and(|a| {
            a.iter()
                .any(|n| n["detail"].as_str().unwrap_or("").contains("fetch is off"))
        })
    })
    .await;
    assert!(v["torrents"].as_array().unwrap().is_empty());
    assert!(d.sigterm().success());

    // --serve-catalog needs an HTTP address.
    let o = run(&cfg, &["serve", "--serve-catalog", "--for", "1s"], &[]).await;
    assert_eq!(o.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&o.stderr).contains("needs an HTTP address"));
}
