// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Test vectors for the `@tensorseed/verify` npm package
//! (`npm/verify/test/fixtures/vectors.json`): a small signed catalog, a
//! claim that matches it, a signed response, and a revocation.
//!
//! Every key here is a throwaway test value (fixed seeds and the
//! well-known Hardhat development account); none is a real key. The
//! vectors are deterministic: this test fails when the committed file is
//! out of date, and `TS_WRITE_NPM_FIXTURES=1 cargo test -p tensorseed-wasm
//! --test npm_fixtures` rewrites it.

use ed25519_dalek::Signer as _;
use sha3::Digest as _;
use tensorseed_core::commercial::CommercialServing;
use tensorseed_core::hash::sha256_bytes;
use tensorseed_core::keys::{RepoRef, RepoType};
use tensorseed_core::manifest::LicenseClass;
use tensorseed_verify::claim::{
    CLAIM_SCHEMA, Entrypoint, LogprobsMode, ResponseKeyRef, Runtime, ServedCatalog, ServedClaim,
    ServedLicense, VerifiedFile,
};
use tensorseed_verify::file::{ChainState, DeltaOp};
use tensorseed_verify::minisign::SecretKey;
use tensorseed_verify::response::signing_message;
use tensorseed_verify::revocation::{Revocation, Revocations, revocation_message};
use tensorseed_verify::signed::SignedEntry;

const CATALOG: &str = "main";
const PUBLISHER_SEED: [u8; 32] = [9; 32];
const RESPONSE_SEED: [u8; 32] = [7; 32];
// Hardhat and Anvil development account 0 (public test key).
const DEV_WALLET_KEY: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
const DEV_WALLET: &str = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266";
const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
const FILE_SHA: &str = "5b2d6ec5a5f3a7e3c3b39f5fbd4b9c8b0d6b0a2a9e1c4f7d3e2a1b0c9d8e7f60";

fn personal_sign(msg: &[u8]) -> String {
    let sk = k256::ecdsa::SigningKey::from_slice(&hex::decode(DEV_WALLET_KEY).unwrap()).unwrap();
    let mut p = format!("\x19Ethereum Signed Message:\n{}", msg.len()).into_bytes();
    p.extend_from_slice(msg);
    let h: [u8; 32] = sha3::Keccak256::digest(&p).into();
    let (sig, rid) = sk.sign_prehash_recoverable(&h).unwrap();
    let mut b = sig.to_bytes().to_vec();
    b.push(27 + rid.to_byte());
    format!("0x{}", hex::encode(b))
}

fn vectors() -> serde_json::Value {
    let sk = SecretKey::from_seed(&PUBLISHER_SEED);
    let pk = sk.public_key();
    let repo = RepoRef::new(RepoType::Model, "org/tiny").unwrap();
    let manifest = serde_json::json!({
        "schema": "ts.manifest/1",
        "repo": {"type": "model", "id": "org/tiny", "hf_url": "https://huggingface.co/org/tiny"},
        "hf_commit": REVISION,
        "hf_last_modified": null,
        "license": {"spdx": "apache-2.0", "hf_tag": "apache-2.0", "class": "open",
            "license_files": [], "attribution": null, "gated_upstream": false},
        "totals": {"files": 1, "bytes": 10},
        "files": [{"path": "tiny-q4.gguf", "size": 10, "sha256": FILE_SHA,
            "hf_lfs_oid": null, "bt_v2_pieces_root": null, "format": null, "pickle_risk": false}],
        "file_pages": 0,
        "torrent": null,
        "webseeds": [],
        "backups": [],
        "published_at": "2026-11-13T00:00:00Z",
        "publisher_tool": "test"
    });
    let value = tensorseed_core::canonical::to_canonical_vec(&manifest).unwrap();
    let key = repo.manifest_key(REVISION).unwrap();
    let entry = SignedEntry::sign(&sk, CATALOG, &key, &value).unwrap();
    let mut st = ChainState::empty(CATALOG).unwrap();
    let delta = st
        .build_delta(
            &sk,
            vec![DeltaOp::Upsert {
                entry: entry.clone(),
            }],
        )
        .unwrap();
    st.apply_delta(&pk, &delta).unwrap();
    let snapshot = st.build_snapshot(&sk).unwrap();
    // Expires 2026-12-31T00:00:00Z. The vectors check at 2026-11-20T00:00:00Z
    // (1795132800).
    let head = st.build_head(&sk, None, 1_798_675_200).unwrap();

    let response_key = ed25519_dalek::SigningKey::from_bytes(&RESPONSE_SEED);
    let mut claim = ServedClaim {
        schema: CLAIM_SCHEMA.into(),
        signed: false,
        catalog: ServedCatalog {
            name: CATALOG.into(),
            publisher_key_id: Some(pk.key_id_hex()),
            manifest_key: key,
            manifest_sha256: sha256_bytes(entry.value.as_bytes()),
        },
        repo_type: RepoType::Model,
        repo: "org/tiny".into(),
        revision: REVISION.into(),
        license: ServedLicense {
            id: Some("apache-2.0".into()),
            class: LicenseClass::Open,
            commercial_serving: CommercialServing::Allowed,
            rights_asserted: false,
        },
        runtime: Runtime::LlamaCpp,
        engine_version: "llama.cpp b1-test".into(),
        entrypoint: Entrypoint::Gguf("tiny-q4.gguf".into()),
        served_model_id: "org/tiny".into(),
        base_url: "http://127.0.0.1:8090/v1".into(),
        public_base_url: Some("https://llm.example.com/v1".into()),
        files: vec![VerifiedFile {
            path: "tiny-q4.gguf".into(),
            size: 10,
            sha256: FILE_SHA.into(),
        }],
        seller_wallet: Some(DEV_WALLET.into()),
        response_key: ResponseKeyRef {
            alg: "ed25519".into(),
            pubkey: hex::encode(response_key.verifying_key().to_bytes()),
        },
        logprobs: LogprobsMode::Required,
        started_at: "2026-11-13T00:00:00Z".into(),
        created_at: "2026-11-13T00:00:00Z".into(),
        expires_at: Some("2026-12-13T00:00:00Z".into()),
        claim_sha256: String::new(),
        sig: None,
    };
    claim.seal().unwrap();
    claim
        .attach_signature(&personal_sign(claim.message().as_bytes()))
        .unwrap();
    claim.verify().unwrap();

    let body = br#"{"id":"chatcmpl-1","object":"chat.completion","choices":[]}"#;
    let ts = "2026-11-20T00:00:00Z";
    let body_sha = sha256_bytes(body);
    let msg = signing_message(&claim.claim_sha256, "req-1", "org/tiny", ts, &body_sha);
    let sig = hex::encode(response_key.sign(msg.as_bytes()).to_bytes());

    let at = "2026-11-25T00:00:00Z";
    let rmsg = revocation_message(&claim.claim_sha256, at, "superseded");
    let mut revocations = Revocations::default();
    revocations.upsert(Revocation {
        claim_sha256: claim.claim_sha256.clone(),
        revoked_at: at.into(),
        reason: "superseded".into(),
        response_sig: Some(hex::encode(response_key.sign(rmsg.as_bytes()).to_bytes())),
        wallet_sig: None,
    });

    serde_json::json!({
        "note": "Test vectors for @tensorseed/verify. Every key is a throwaway test value. Regenerate with TS_WRITE_NPM_FIXTURES=1 cargo test -p tensorseed-wasm --test npm_fixtures.",
        "catalog": {
            "name": CATALOG,
            "pubkey": pk.to_base64(),
            "snapshot": {"body": String::from_utf8(snapshot.body).unwrap(), "sig": snapshot.sig},
            "head": {"body": String::from_utf8(head.body).unwrap(), "sig": head.sig},
            "now_unix": 1_795_132_800u64,
        },
        "claim": serde_json::to_string_pretty(&claim).unwrap(),
        "claim_url": "https://llm.example.com/.well-known/tensorseed/served-weights.json",
        "response": {
            "headers": [
                ["Content-Type", "application/json"],
                ["X-Tensorseed-Claim-SHA256", claim.claim_sha256],
                ["X-Tensorseed-Request-Id", "req-1"],
                ["X-Tensorseed-Served-Model", "org/tiny"],
                ["X-Tensorseed-Timestamp", ts],
                ["X-Tensorseed-Body-SHA256", body_sha],
                ["X-Tensorseed-Signature", sig],
            ],
            "body": String::from_utf8(body.to_vec()).unwrap(),
            "request_id": "req-1",
            "now_unix": 1_795_132_800u64,
        },
        "revocations": serde_json::to_string_pretty(&revocations).unwrap(),
        "seller_wallet": DEV_WALLET,
    })
}

#[test]
fn npm_fixtures_are_current() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../npm/verify/test/fixtures/vectors.json");
    let mut want = serde_json::to_string_pretty(&vectors()).unwrap();
    want.push('\n');
    // Deterministic: building twice gives the same bytes.
    let mut again = serde_json::to_string_pretty(&vectors()).unwrap();
    again.push('\n');
    assert_eq!(want, again);
    if std::env::var_os("TS_WRITE_NPM_FIXTURES").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &want).unwrap();
        return;
    }
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        have == want,
        "{} is out of date; run TS_WRITE_NPM_FIXTURES=1 cargo test -p tensorseed-wasm --test npm_fixtures",
        path.display()
    );
}
