// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Chia as the source of truth for the file catalog (spec section 6.14).
//!
//! The publisher writes a reserved DataLayer key, [`FILE_HEAD_KEY`], in every
//! Chia generation. Its value is a signed [`FileAnchor`] naming the file
//! catalog revision and root that generation stands for. After the
//! generation confirms, the publisher asks its DataLayer for a proof of
//! inclusion of that key and publishes it next to `head.json` as
//! [`CHAIN_PROOF_PATH`] (a [`ChainProof`]).
//!
//! A client checks the file catalog against the chain without a wallet or a
//! full node:
//!
//! 1. [`ChainProof::verify`]: the anchor entry carries a valid publisher
//!    signature; hashing the stored key and value up the proof layers gives a
//!    DataLayer root; and the store singleton puzzle hash for
//!    `(store id, root, owner inner puzzle hash)` is computed locally. This
//!    step needs no network and trusts nobody.
//! 2. [`CoinApi::check`]: one or more full node RPC compatible endpoints
//!    (for example a public coinset style API, or your own node) are asked
//!    for the coin named in the proof. The coin id must hash from the
//!    returned parent, puzzle hash, and amount; the puzzle hash must equal
//!    the computed one; and the coin must be unspent (the store's current
//!    state) or spent only within a grace window. This step trusts the
//!    endpoint to report the chain honestly; it cannot forge a root, but it
//!    could hide a newer state or claim a coin does not exist.
//!
//! This is the same check the reference wallet's `verify_proof` performs,
//! with the coin lookup done over HTTP instead of a wallet peer connection.

use serde_json::{Value, json};

use crate::{CatalogError, Result};

pub use tensorseed_verify::anchor::{
    CHAIN_PROOF_PATH, CHAIN_PROOF_SCHEMA, ChainProof, CoinCheck, CoinRecord, CoinState,
    FILE_ANCHOR_SCHEMA, FILE_HEAD_KEY, FileAnchor, MAX_CHAIN_PROOF_BYTES, ProofLayer,
    VerifiedProof, chain_deletes, check_coin_record, coin_id, datalayer_leaf_hash,
    datalayer_puzzle_hash, fake_get_proof, fold_proof, h32, is_reserved_key, norm_hex, puzzles,
    worse_coin_state,
};

/// Read only client for full node RPC compatible HTTP endpoints, such as a
/// public coinset style API or a local full node exposed on localhost.
#[derive(Debug, Clone)]
pub struct CoinApi {
    http: reqwest::Client,
    urls: Vec<String>,
}

impl CoinApi {
    /// Client for `urls` (each a base such as `https://api.example.org`).
    pub fn new(urls: Vec<String>, http: reqwest::Client) -> Self {
        Self { http, urls }
    }

    /// Configured endpoints.
    pub fn urls(&self) -> &[String] {
        &self.urls
    }

    async fn call(&self, base: &str, method: &str, body: Value) -> Result<Value> {
        let url = format!("{}/{method}", base.trim_end_matches('/'));
        let resp = self.http.post(&url).json(&body).send().await?;
        let status = resp.status();
        let v: Value = resp.json().await?;
        if !status.is_success() && v.get("success").is_none() {
            return Err(CatalogError::Backend(format!("{url}: HTTP {status}")));
        }
        Ok(v)
    }

    /// The coin record, or `None` when the endpoint says it does not exist.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Http`] on transport failure and
    /// [`CatalogError::Backend`] when the endpoint answers with an error or an
    /// unexpected body.
    pub async fn coin_record(&self, base: &str, coin: &str) -> Result<Option<CoinRecord>> {
        let v = self
            .call(
                base,
                "get_coin_record_by_name",
                json!({ "name": format!("0x{}", norm_hex(coin)) }),
            )
            .await?;
        if v.get("success").and_then(Value::as_bool) != Some(true) {
            let err = v.get("error").and_then(Value::as_str).unwrap_or("");
            if err.to_ascii_lowercase().contains("not found") {
                return Ok(None);
            }
            return Err(CatalogError::Backend(format!(
                "{base}: get_coin_record_by_name failed: {err}"
            )));
        }
        let r = v
            .get("coin_record")
            .ok_or_else(|| CatalogError::Backend(format!("{base}: no coin_record")))?;
        if r.is_null() {
            return Ok(None);
        }
        let c = r
            .get("coin")
            .ok_or_else(|| CatalogError::Backend(format!("{base}: no coin")))?;
        let s = |v: &Value, k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .map(norm_hex)
                .ok_or_else(|| CatalogError::Backend(format!("{base}: no {k}")))
        };
        let n = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
        Ok(Some(CoinRecord {
            parent: s(c, "parent_coin_info")?,
            puzzle_hash: s(c, "puzzle_hash")?,
            amount: c
                .get("amount")
                .and_then(Value::as_u64)
                .ok_or_else(|| CatalogError::Backend(format!("{base}: no amount")))?,
            spent: r.get("spent").and_then(Value::as_bool).unwrap_or(false)
                || n(r, "spent_block_index") > 0,
            spent_block_index: n(r, "spent_block_index"),
            confirmed_block_index: n(r, "confirmed_block_index"),
        }))
    }

    /// Current peak height.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Http`] on transport failure and
    /// [`CatalogError::Backend`] when the endpoint answers with an error or an
    /// unexpected body.
    pub async fn peak_height(&self, base: &str) -> Result<u64> {
        let v = self.call(base, "get_blockchain_state", json!({})).await?;
        v.pointer("/blockchain_state/peak/height")
            .and_then(Value::as_u64)
            .ok_or_else(|| CatalogError::Backend(format!("{base}: no peak height")))
    }

    /// Look the proven coin up on every endpoint. Any endpoint that returns
    /// a coin that does not match the proof, or says the coin does not
    /// exist, fails the check (`Chain` error). Endpoints that cannot be
    /// reached are skipped; if none answers the result is
    /// [`CatalogError::ChainUnreachable`]. When endpoints disagree about
    /// whether the coin is spent, the most conservative answer wins.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Chain`] when any reachable endpoint contradicts the
    /// proof, and [`CatalogError::ChainUnreachable`] when no endpoint answers.
    pub async fn check(&self, proof: &VerifiedProof, grace_blocks: u64) -> Result<CoinCheck> {
        if self.urls.is_empty() {
            return Err(CatalogError::Config(
                "no chain endpoints configured (chia.coin_api_urls)".into(),
            ));
        }
        let mut out = CoinCheck {
            state: CoinState::Current,
            confirmed_block_index: 0,
            answered: Vec::new(),
            skipped: Vec::new(),
        };
        for base in &self.urls {
            let rec = match self.coin_record(base, &proof.coin_id).await {
                Ok(Some(r)) => r,
                Ok(None) => {
                    return Err(CatalogError::Chain(format!(
                        "{base}: store coin {} does not exist on chain",
                        proof.coin_id
                    )));
                }
                Err(e) => {
                    out.skipped.push(format!("{base}: {e}"));
                    continue;
                }
            };
            let peak = if rec.spent {
                match self.peak_height(base).await {
                    Ok(p) => Some(p),
                    Err(e) => {
                        out.skipped.push(format!("{base}: {e}"));
                        continue;
                    }
                }
            } else {
                None
            };
            let st = match check_coin_record(proof, &rec, peak, grace_blocks) {
                Ok(s) => s,
                Err(e) => return Err(e.into()),
            };
            out.confirmed_block_index = rec.confirmed_block_index;
            if rec.spent {
                out.state = worse_coin_state(&out.state, st);
            }
            out.answered.push(base.clone());
        }
        if out.answered.is_empty() {
            return Err(CatalogError::ChainUnreachable(out.skipped.join("; ")));
        }
        Ok(out)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crate::signed::SignedEntry;

    // A real testnet11 DataLayer store (created by the Tensorseed testnet
    // run, spec 6.13): generation 1 root, owner inner puzzle hash, and the
    // singleton coin as reported on chain.
    const STORE: &str = "3bc1ab7705c525eea55ba6f76db8baff25de0bc282a4b626c6f7b070de5c5d89";
    const ROOT1: &str = "dbf3a4f4eba03f77fd549a7df21512ee2990c5273699f762530df49ba436ea38";
    const INNER1: &str = "dde6ead54478217121c72f3be11f69bbb368fa7d471cb27622b9882e81300705";
    const PH1: &str = "7b6691b65b9e0fd350e815b1f3f912bfb18a21a17ba375f7603683cf52b5eead";
    const PARENT1: &str = "263611b1acb455e626cd48a2257a3c0b406cd7cdef14953bf500d65e6093d79b";
    const COIN1: &str = "ba3d4ca3f6c5c53e564d50f96f785a7588d318e372aed5bfbeb565f676f290f0";
    // Generation 0 (empty root) of the same store.
    const _INNER0: &str = "f177f865c13c8d83bd6b01005b801d24a59e4153d6ecb78e5db497d259e15ecb";
    const PH0: &str = "0806911388049ba83ee0c6fda4d7752c51921bb97eab29158818a68f5b6df4c1";

    fn b(s: &str) -> [u8; 32] {
        h32(s, "test").unwrap()
    }

    const REAL_PROOF: &str = include_str!("../tests/fixtures/testnet11-proof.json");

    fn real_inclusion() -> (Vec<u8>, Vec<u8>, String, Vec<ProofLayer>) {
        let v: Value = serde_json::from_str(REAL_PROOF).unwrap();
        let layers = serde_json::from_value(v["layers"].clone()).unwrap();
        (
            hex::decode(v["key"].as_str().unwrap()).unwrap(),
            hex::decode(v["value"].as_str().unwrap()).unwrap(),
            v["node_hash"].as_str().unwrap().to_string(),
            layers,
        )
    }

    fn key() -> crate::minisign::SecretKey {
        crate::minisign::SecretKey::from_seed(&[42; 32])
    }

    /// Live: look the real testnet11 store coin up on a public coinset style
    /// API. Run with `cargo test -p tensorseed-catalog -- --ignored live_`.
    #[tokio::test]
    #[ignore = "needs network"]
    async fn live_testnet11_store_coin_matches_the_proof() {
        let (key, value, _, layers) = real_inclusion();
        let root = fold_proof(datalayer_leaf_hash(&key, &value), &layers).unwrap();
        let ph = datalayer_puzzle_hash(&b(STORE), &root, &b(INNER1));
        let p = VerifiedProof {
            store_id: STORE.into(),
            root: hex::encode(root),
            coin_id: COIN1.into(),
            puzzle_hash: hex::encode(ph),
            anchor: FileAnchor::new("tensorseed", 1, ROOT1),
        };
        let url = std::env::var("TS_TESTNET_COIN_API")
            .unwrap_or_else(|_| "https://testnet11.api.coinset.org".into());
        let c = api(vec![url]).check(&p, 192).await.unwrap();
        eprintln!(
            "testnet11 store coin {COIN1}: {:?}, created at {}",
            c.state, c.confirmed_block_index
        );
        assert_eq!(c.confirmed_block_index, 4956663);
    }

    async fn coin_server(record: Value, peak: u64) -> wiremock::MockServer {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/get_coin_record_by_name"))
            .respond_with(ResponseTemplate::new(200).set_body_json(record))
            .mount(&s)
            .await;
        Mock::given(method("POST"))
            .and(path("/get_blockchain_state"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"success": true, "blockchain_state": {"peak": {"height": peak}}}),
            ))
            .mount(&s)
            .await;
        s
    }

    fn real_proof() -> VerifiedProof {
        VerifiedProof {
            store_id: STORE.into(),
            root: ROOT1.into(),
            coin_id: COIN1.into(),
            puzzle_hash: PH1.into(),
            anchor: FileAnchor::new("main", 1, &"ab".repeat(32)),
        }
    }

    fn record(ph: &str, spent_at: u64) -> Value {
        // Shape as returned by a public coinset style API on testnet11.
        json!({"coin_record": {
            "coin": {"parent_coin_info": format!("0x{PARENT1}"), "puzzle_hash": format!("0x{ph}"), "amount": 1},
            "coinbase": false, "confirmed_block_index": 4956663,
            "spent": spent_at > 0, "spent_block_index": spent_at, "timestamp": 1794526151
        }, "success": true})
    }

    fn api(urls: Vec<String>) -> CoinApi {
        CoinApi::new(urls, reqwest::Client::new())
    }

    #[tokio::test]
    async fn coin_check_states() {
        let p = real_proof();
        let live = coin_server(record(PH1, 0), 5_000_000).await;
        let c = api(vec![live.uri()]).check(&p, 100).await.unwrap();
        assert_eq!(c.state, CoinState::Current);
        assert_eq!(c.confirmed_block_index, 4956663);

        let recent = coin_server(record(PH1, 4_999_950), 5_000_000).await;
        let c = api(vec![recent.uri()]).check(&p, 100).await.unwrap();
        assert_eq!(c.state, CoinState::RecentlySpent { blocks_ago: 50 });
        let c = api(vec![live.uri(), recent.uri()])
            .check(&p, 10)
            .await
            .unwrap();
        assert_eq!(
            c.state,
            CoinState::Stale { blocks_ago: 50 },
            "most conservative answer wins"
        );
        assert_eq!(c.answered.len(), 2);
    }

    #[tokio::test]
    async fn coin_check_rejects_mismatches_and_reports_unreachable() {
        let p = real_proof();
        // A coin whose puzzle hash commits to another root: forged proof.
        let wrong = coin_server(record(PH0, 0), 10).await;
        let e = api(vec![wrong.uri()]).check(&p, 100).await.unwrap_err();
        assert!(matches!(e, CatalogError::Chain(_)), "{e}");
        // Not on chain at all.
        let none = coin_server(
            json!({"success": false, "error": "Coin record 0x00 not found"}),
            10,
        )
        .await;
        assert!(matches!(
            api(vec![none.uri()]).check(&p, 100).await,
            Err(CatalogError::Chain(_))
        ));
        // A record that does not hash to the requested coin id.
        let mut r = record(PH1, 0);
        r["coin_record"]["coin"]["amount"] = json!(3);
        let liar = coin_server(r, 10).await;
        assert!(matches!(
            api(vec![liar.uri()]).check(&p, 100).await,
            Err(CatalogError::Chain(_))
        ));
        // Nobody answers.
        let e = api(vec!["http://127.0.0.1:9".into()])
            .check(&p, 100)
            .await
            .unwrap_err();
        assert!(matches!(e, CatalogError::ChainUnreachable(_)), "{e}");
        // One dead endpoint is skipped when another answers.
        let live = coin_server(record(PH1, 0), 10).await;
        let c = api(vec!["http://127.0.0.1:9".into(), live.uri()])
            .check(&p, 100)
            .await
            .unwrap();
        assert_eq!(c.skipped.len(), 1);
        assert!(matches!(
            api(vec![]).check(&p, 100).await,
            Err(CatalogError::Config(_))
        ));
    }

    #[test]
    fn deletes_skip_reserved_and_present_keys() {
        let sk = key();
        let mut entries = BTreeMap::new();
        let e = SignedEntry::sign(&sk, "main", "ts/v1/a", b"{}").unwrap();
        entries.insert(e.key.clone(), e);
        let keys = vec![
            "ts/v1/a".to_string(),
            "ts/v1/b".to_string(),
            FILE_HEAD_KEY.to_string(),
        ];
        assert_eq!(chain_deletes(&keys, &entries), vec!["ts/v1/b".to_string()]);
    }
}
