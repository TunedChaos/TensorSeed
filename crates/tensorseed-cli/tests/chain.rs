// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! End to end test of Chia as the source of truth for the file catalog
//! (spec 6.14): the `tensorseed` binary publishes through a fake DataLayer
//! RPC (Chia first, then the file catalog and its chain proof), and clients
//! check the file catalog against a fake full node coin API in strict, warn,
//! and off modes. Everything runs against local mock servers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tensorseed_catalog::anchor::{coin_id, datalayer_leaf_hash, datalayer_puzzle_hash};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const STORE: &str = "3bc1ab7705c525eea55ba6f76db8baff25de0bc282a4b626c6f7b070de5c5d89";
const INNER: [u8; 32] = [0x11; 32];

fn h(parts: &[&[u8]]) -> [u8; 32] {
    let mut d = Sha256::new();
    for p in parts {
        d.update(p);
    }
    d.finalize().into()
}

fn pair(l: &[u8; 32], r: &[u8; 32]) -> [u8; 32] {
    h(&[&[2], l, r])
}

fn unhex(s: &str) -> Vec<u8> {
    hex::decode(s.strip_prefix("0x").unwrap_or(s)).unwrap()
}

struct Coin {
    id: [u8; 32],
    parent: [u8; 32],
    ph: [u8; 32],
    created: u64,
    spent: Option<u64>,
}

/// A small in-memory DataLayer store plus the chain view of its singleton.
#[derive(Default)]
struct Fake {
    kv: BTreeMap<Vec<u8>, Vec<u8>>,
    pending: Option<BTreeMap<Vec<u8>, Vec<u8>>>,
    roots: Vec<[u8; 32]>,
    coins: Vec<Coin>,
    height: u64,
    /// Extra blocks reported on top of the newest coin (staleness tests).
    peak_extra: u64,
    fail_submit: bool,
    submits: usize,
}

fn tree(leaves: &[[u8; 32]]) -> [u8; 32] {
    match leaves.len() {
        0 => [0; 32],
        1 => leaves[0],
        n => pair(&tree(&leaves[..n / 2]), &tree(&leaves[n / 2..])),
    }
}

/// Proof layers for leaf `i` in the tree shape of [`tree`].
fn proof(leaves: &[[u8; 32]], i: usize) -> Vec<(u8, [u8; 32], [u8; 32])> {
    if leaves.len() <= 1 {
        return Vec::new();
    }
    let mid = leaves.len() / 2;
    let (l, r) = leaves.split_at(mid);
    let (mut path, sibling, mine_left) = if i < mid {
        (proof(l, i), tree(r), true)
    } else {
        (proof(r, i - mid), tree(l), false)
    };
    let combined = pair(&tree(l), &tree(r));
    path.push((if mine_left { 1 } else { 0 }, sibling, combined));
    path
}

impl Fake {
    fn leaves(&self) -> Vec<[u8; 32]> {
        self.kv
            .iter()
            .map(|(k, v)| datalayer_leaf_hash(k, v))
            .collect()
    }

    fn root(&self) -> [u8; 32] {
        self.roots.last().copied().unwrap_or([0; 32])
    }

    fn new_generation(&mut self) {
        let root = tree(&self.leaves());
        self.height += 10;
        let parent = self.coins.last().map_or([7; 32], |c| c.id);
        if let Some(c) = self.coins.last_mut() {
            c.spent = Some(self.height);
        }
        let store: [u8; 32] = unhex(STORE).try_into().unwrap();
        let ph = datalayer_puzzle_hash(&store, &root, &INNER);
        self.coins.push(Coin {
            id: coin_id(&parent, &ph, 1),
            parent,
            ph,
            created: self.height,
            spent: None,
        });
        self.roots.push(root);
    }

    fn dl(&mut self, method: &str, b: &Value) -> Value {
        let ok = |mut v: Value| {
            v["success"] = json!(true);
            v
        };
        let err = |m: &str| json!({ "success": false, "error": m });
        match method {
            "get_value" => {
                let k = unhex(b["key"].as_str().unwrap());
                match self.kv.get(&k) {
                    Some(v) => ok(json!({ "value": hex::encode(v) })),
                    None => err(&format!("Key not found: {}", hex::encode(&k))),
                }
            }
            "get_keys" => ok(json!({
                "keys": self.kv.keys().map(|k| format!("0x{}", hex::encode(k))).collect::<Vec<_>>(),
                "total_pages": 1
            })),
            "batch_update" => {
                let mut p = self.pending.take().unwrap_or_else(|| self.kv.clone());
                for c in b["changelist"].as_array().unwrap() {
                    let k = unhex(c["key"].as_str().unwrap());
                    match c["action"].as_str().unwrap() {
                        "upsert" => {
                            p.insert(k, unhex(c["value"].as_str().unwrap()));
                        }
                        _ => {
                            p.remove(&k);
                        }
                    }
                }
                self.pending = Some(p);
                ok(json!({}))
            }
            "submit_pending_root" => {
                if self.fail_submit {
                    return err("wallet is not synced (test)");
                }
                let Some(p) = self.pending.take() else {
                    return err("Latest root is already confirmed.");
                };
                self.kv = p;
                self.submits += 1;
                self.new_generation();
                ok(json!({ "tx_id": format!("0x{}", hex::encode([9u8; 32])) }))
            }
            "get_root" => {
                ok(json!({ "hash": format!("0x{}", hex::encode(self.root())), "confirmed": true }))
            }
            "get_root_history" => ok(json!({
                "root_history": self.roots.iter().map(|r| json!({ "root_hash": format!("0x{}", hex::encode(r)), "confirmed": true })).collect::<Vec<_>>()
            })),
            "get_sync_status" => {
                let g = self.roots.len() as u64;
                ok(json!({ "sync_status": { "generation": g, "target_generation": g } }))
            }
            "get_proof" => {
                let k = unhex(b["keys"][0].as_str().unwrap());
                let keys: Vec<&Vec<u8>> = self.kv.keys().collect();
                let i = keys.iter().position(|x| **x == k).unwrap();
                let leaves = self.leaves();
                let layers: Vec<Value> = proof(&leaves, i)
                    .into_iter()
                    .map(|(side, other, comb)| {
                        json!({
                            "other_hash_side": side,
                            "other_hash": format!("0x{}", hex::encode(other)),
                            "combined_hash": format!("0x{}", hex::encode(comb)),
                        })
                    })
                    .collect();
                ok(json!({ "proof": {
                    "coin_id": format!("0x{}", hex::encode(self.coins.last().unwrap().id)),
                    "inner_puzzle_hash": format!("0x{}", hex::encode(INNER)),
                    "store_proofs": { "store_id": format!("0x{STORE}"), "proofs": [{
                        "key_clvm_hash": "0x00", "value_clvm_hash": "0x00",
                        "node_hash": format!("0x{}", hex::encode(leaves[i])),
                        "layers": layers
                    }]}
                }}))
            }
            other => err(&format!("unknown method {other}")),
        }
    }

    fn node(&self, method: &str, b: &Value) -> Value {
        match method {
            "get_coin_record_by_name" => {
                let id = unhex(b["name"].as_str().unwrap());
                match self.coins.iter().find(|c| c.id.as_slice() == id.as_slice()) {
                    Some(c) => json!({ "success": true, "coin_record": {
                        "coin": { "parent_coin_info": format!("0x{}", hex::encode(c.parent)), "puzzle_hash": format!("0x{}", hex::encode(c.ph)), "amount": 1 },
                        "coinbase": false, "confirmed_block_index": c.created,
                        "spent": c.spent.is_some(), "spent_block_index": c.spent.unwrap_or(0), "timestamp": 0
                    }}),
                    None => {
                        json!({ "success": false, "error": format!("Coin record 0x{} not found", hex::encode(&id)) })
                    }
                }
            }
            "get_blockchain_state" => {
                json!({ "success": true, "blockchain_state": { "peak": { "height": self.height + self.peak_extra } } })
            }
            _ => json!({ "success": false, "error": "unknown" }),
        }
    }
}

struct DlResponder(Arc<Mutex<Fake>>, bool);

impl Respond for DlResponder {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let method = req.url.path().trim_start_matches('/').to_string();
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(json!({}));
        let mut f = self.0.lock().unwrap();
        let v = if self.1 {
            f.dl(&method, &body)
        } else {
            f.node(&method, &body)
        };
        ResponseTemplate::new(200).set_body_json(v)
    }
}

/// Serves a directory straight from disk, so later publishes are visible.
struct DirResponder(PathBuf);

impl Respond for DirResponder {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let rel = req.url.path().trim_start_matches('/');
        match std::fs::read(self.0.join(rel)) {
            Ok(b) => ResponseTemplate::new(200).set_body_bytes(b),
            Err(_) => ResponseTemplate::new(404),
        }
    }
}

fn write_config(dir: &Path, base: &str, pubkey: &str, extra_catalog: &str, chia: &str) -> PathBuf {
    let d = dir.display();
    let text = format!(
        r#"[general]
state_db = "{d}/state.db"

[catalog]
backend = "file"
name = "main"
publisher_pubkey = "{pubkey}"
{extra_catalog}

[chia]
confirm_poll_secs = 1
confirm_timeout_secs = 30
{chia}

[file]
base_url = "{base}/"
state_dir = "{d}/catstate"

[mirrors]
file = "{d}/mirrors.toml"
fetch_published = false

[storage]
local_dir = "{d}/publish"
"#
    );
    let p = dir.join("tensorseed.toml");
    std::fs::write(&p, text).unwrap();
    p
}

async fn run(config: &Path, args: &[&str]) -> Output {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_tensorseed"));
    cmd.arg("--config").arg(config).args(args);
    cmd.env_remove("HF_TOKEN").env("RUST_LOG", "warn");
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

fn head_revision(publish: &Path) -> u64 {
    let v: Value =
        serde_json::from_slice(&std::fs::read(publish.join("main/head.json")).unwrap()).unwrap();
    v["revision"].as_u64().unwrap()
}

fn export_bundle(
    dir: &Path,
    key: &tensorseed_catalog::minisign::SecretKey,
    key_name: &str,
    doc: &str,
) -> PathBuf {
    let e = tensorseed_catalog::signed::SignedEntry::sign(key, "main", key_name, doc.as_bytes())
        .unwrap();
    let bundle = json!({
        "schema": "ts.export/1", "catalog": "main", "publisher_pubkey": key.public_key().to_base64(),
        "source_kind": "test", "source_id": "test", "entries": [e]
    });
    let p = dir.join(format!("{}.json", key_name.replace('/', "_")));
    std::fs::write(&p, serde_json::to_vec(&bundle).unwrap()).unwrap();
    p
}

#[tokio::test(flavor = "multi_thread")]
async fn chia_first_publish_and_strict_client_check() {
    let tmp = tempfile::tempdir().unwrap();
    let state = Arc::new(Mutex::new(Fake::default()));
    let dl = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(DlResponder(state.clone(), true))
        .mount(&dl)
        .await;
    let node = MockServer::start().await;
    Mock::given(wiremock::matchers::method("POST"))
        .respond_with(DlResponder(state.clone(), false))
        .mount(&node)
        .await;

    // Publisher: a test key, local storage served over HTTP from disk.
    let pubdir = tmp.path().join("pub");
    std::fs::create_dir_all(&pubdir).unwrap();
    let sk = tensorseed_catalog::minisign::SecretKey::from_seed(&[23; 32]);
    let keyfile = pubdir.join("pub.key");
    std::fs::write(
        &keyfile,
        sk.to_minisign_file(
            None,
            tensorseed_catalog::minisign::KdfLimits::MINISIGN_DEFAULT,
        )
        .unwrap(),
    )
    .unwrap();
    let pk = sk.public_key().to_base64();
    let files = MockServer::start().await;
    Mock::given(wiremock::matchers::method("GET"))
        .respond_with(DirResponder(pubdir.join("publish")))
        .mount(&files)
        .await;
    let chia_ok = format!(
        "rpc_url = \"{}\"\ncatalog_store_id = \"{STORE}\"\ncoin_api_urls = [\"{}\"]",
        dl.uri(),
        node.uri()
    );
    let seck = format!("publisher_seckey_file = \"{}\"", keyfile.display());
    let pcfg = write_config(&pubdir, &files.uri(), &pk, &seck, &chia_ok);

    // 1. Chia first: if the chain write fails, nothing reaches the file catalog.
    state.lock().unwrap().fail_submit = true;
    let o = run(&pcfg, &["catalog", "create"]).await;
    assert_ne!(o.status.code(), Some(0));
    assert!(
        stderr(&o).contains("wallet is not synced"),
        "chain error surfaces"
    );
    assert!(
        !pubdir.join("publish/main/head.json").exists(),
        "no file revision without the chain"
    );
    state.lock().unwrap().fail_submit = false;

    let o = run(&pcfg, &["catalog", "create"]).await;
    assert_eq!(o.status.code(), Some(0));
    assert!(stdout(&o).contains("chia     generation 1"));
    assert_eq!(head_revision(&pubdir.join("publish")), 1);
    assert!(pubdir.join("publish/main/chain-proof.json").exists());
    {
        let f = state.lock().unwrap();
        assert_eq!(f.submits, 1);
        assert!(f.kv.contains_key(b"ts/v1/file-head".as_slice()));
        assert!(f.kv.contains_key(b"ts/v1/catalog".as_slice()));
    }

    // 2. A strict client (default once a store id is set) accepts it.
    let cdir = tmp.path().join("client");
    std::fs::create_dir_all(&cdir).unwrap();
    let chia_client = format!(
        "catalog_store_id = \"{STORE}\"\ncoin_api_urls = [\"{}\"]",
        node.uri()
    );
    let ccfg = write_config(&cdir, &files.uri(), &pk, "", &chia_client);
    let o = run(&ccfg, &["catalog", "info"]).await;
    assert_eq!(o.status.code(), Some(0));
    assert!(stdout(&o).contains("revision:  1"));
    assert!(stdout(&o).contains("chain:     Strict"));
    assert!(stdout(&o).contains("anchored:  revision 1"));
    let o = run(&ccfg, &["catalog", "chain-status"]).await;
    assert_eq!(o.status.code(), Some(0));
    assert!(stdout(&o).contains("result:     OK"));

    // 3. Re-signing the head costs no chain transaction and stays valid.
    let o = run(&pcfg, &["catalog", "resign-head"]).await;
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(state.lock().unwrap().submits, 1);
    let o = run(&ccfg, &["catalog", "info"]).await;
    assert_eq!(o.status.code(), Some(0));

    // 4. A normal publish: one chain generation, then the file revision.
    let b1 = export_bundle(
        tmp.path(),
        &sk,
        "ts/v1/index/model/org/a",
        r#"{"n":1,"schema":"ts.test/1"}"#,
    );
    let o = run(&pcfg, &["catalog", "import", b1.to_str().unwrap()]).await;
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(head_revision(&pubdir.join("publish")), 2);
    assert_eq!(state.lock().unwrap().submits, 2);
    let o = run(&ccfg, &["catalog", "info"]).await;
    assert_eq!(o.status.code(), Some(0));
    assert!(stdout(&o).contains("revision:  2"));

    // 5. A file revision the chain never saw (published without the store
    //    id, as a compromised publishing host or a rogue cache could) is
    //    refused by strict clients, whose state does not move.
    let rogue = write_config(&pubdir, &files.uri(), &pk, &seck, "");
    let b2 = export_bundle(
        tmp.path(),
        &sk,
        "ts/v1/index/model/org/b",
        r#"{"n":2,"schema":"ts.test/1"}"#,
    );
    let o = run(&rogue, &["catalog", "import", b2.to_str().unwrap()]).await;
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(head_revision(&pubdir.join("publish")), 3);
    assert_eq!(state.lock().unwrap().submits, 2);
    // Back to the real publisher config (same directory and state).
    let pcfg = write_config(&pubdir, &files.uri(), &pk, &seck, &chia_ok);
    let o = run(&ccfg, &["catalog", "info"]).await;
    assert_ne!(o.status.code(), Some(0));
    assert!(
        stderr(&o).contains("chain"),
        "strict refusal explains itself"
    );
    let o = run(&ccfg, &["catalog", "info", "--no-wait"]).await;
    assert!(
        stdout(&o).contains("revision:  2"),
        "strict client stayed at the anchored revision"
    );
    let o = run(&ccfg, &["catalog", "chain-status"]).await;
    assert_ne!(o.status.code(), Some(0));
    assert!(stdout(&o).contains("MISMATCH"));

    // warn logs and goes on; off skips the check.
    let wdir = tmp.path().join("warn");
    std::fs::create_dir_all(&wdir).unwrap();
    let wcfg = write_config(
        &wdir,
        &files.uri(),
        &pk,
        "chain_check = \"warn\"",
        &chia_client,
    );
    let o = run(&wcfg, &["catalog", "info"]).await;
    assert_eq!(o.status.code(), Some(0));
    assert!(stdout(&o).contains("revision:  3"));
    assert!(stderr(&o).contains("WARN"));
    let odir = tmp.path().join("off");
    std::fs::create_dir_all(&odir).unwrap();
    let ocfg = write_config(
        &odir,
        &files.uri(),
        &pk,
        "chain_check = \"off\"",
        &chia_client,
    );
    let o = run(&ocfg, &["catalog", "info"]).await;
    assert_eq!(o.status.code(), Some(0));
    assert!(stdout(&o).contains("revision:  3"));

    // A normal publish now refuses to build on a revision the chain lacks.
    let b3 = export_bundle(
        tmp.path(),
        &sk,
        "ts/v1/index/model/org/c",
        r#"{"n":3,"schema":"ts.test/1"}"#,
    );
    let o = run(&pcfg, &["catalog", "import", b3.to_str().unwrap()]).await;
    assert_ne!(o.status.code(), Some(0));
    assert!(stderr(&o).contains("catalog anchor"));

    // 6. `catalog anchor` brings the chain up to the file catalog; strict
    //    clients follow.
    let o = run(&pcfg, &["catalog", "anchor"]).await;
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(state.lock().unwrap().submits, 3);
    let o = run(&pcfg, &["catalog", "anchor"]).await;
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(state.lock().unwrap().submits, 3, "anchoring again is free");
    let o = run(&ccfg, &["catalog", "info"]).await;
    assert_eq!(o.status.code(), Some(0));
    assert!(stdout(&o).contains("revision:  3"));

    // 7. The chain moves on (a newer generation the caches do not have):
    //    within the grace window the old proof is accepted with a warning,
    //    after it strict clients refuse.
    {
        let mut f = state.lock().unwrap();
        f.kv.insert(b"ts/v1/zz".to_vec(), b"x".to_vec());
        f.new_generation();
    }
    let o = run(&ccfg, &["catalog", "info"]).await;
    assert_eq!(o.status.code(), Some(0));
    assert!(stderr(&o).contains("moved past"));
    state.lock().unwrap().peak_extra = 10_000;
    let o = run(&ccfg, &["catalog", "info"]).await;
    assert_ne!(o.status.code(), Some(0));
    assert!(stderr(&o).contains("newer state"));

    // 8. Unreachable chain endpoints: strict fails, warn continues.
    let dead = "coin_api_urls = [\"http://127.0.0.1:9\"]";
    let s2 = tmp.path().join("strict2");
    std::fs::create_dir_all(&s2).unwrap();
    let c2 = write_config(
        &s2,
        &files.uri(),
        &pk,
        "",
        &format!("catalog_store_id = \"{STORE}\"\n{dead}"),
    );
    let o = run(&c2, &["catalog", "info"]).await;
    assert_ne!(o.status.code(), Some(0));
    assert!(stderr(&o).contains("unreachable"));
    let w2 = tmp.path().join("warn2");
    std::fs::create_dir_all(&w2).unwrap();
    let c3 = write_config(
        &w2,
        &files.uri(),
        &pk,
        "chain_check = \"warn\"",
        &format!("catalog_store_id = \"{STORE}\"\n{dead}"),
    );
    let o = run(&c3, &["catalog", "info"]).await;
    assert_eq!(o.status.code(), Some(0));

    // 9. No store id: off by default, with a notice; today's catalogs keep working.
    let n = tmp.path().join("nostore");
    std::fs::create_dir_all(&n).unwrap();
    let c4 = write_config(&n, &files.uri(), &pk, "", "");
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_tensorseed"));
    cmd.arg("--config")
        .arg(&c4)
        .args(["catalog", "info"])
        .env("RUST_LOG", "info");
    let o = tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert!(stdout(&o).contains("chain:     Off"));
    assert!(stderr(&o).contains("chain check off"));
}
