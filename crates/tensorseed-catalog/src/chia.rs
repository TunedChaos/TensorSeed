// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Chia DataLayer backend (spec sections 6.1 to 6.7).
//!
//! Talks to the local `chia data` service RPC (default `https://localhost:8562`)
//! with mutual TLS using `private_data_layer.crt` / `.key`. The RPC port must
//! never be exposed publicly.
//!
//! Status: every call the catalog needs is implemented (`create_data_store`,
//! `get_value` with envelope unwrap and verify, paginated `get_keys`,
//! `batch_update` with `submit_on_chain=false`, `submit_pending_root`,
//! confirmation polling via `get_root` and `get_root_history`, `subscribe`,
//! `get_sync_status`, `add_mirror`) and tested against mocked responses
//! shaped after the DataLayer RPC reference. None of it has run against a
//! live node yet; the M0 testnet spike must confirm the response fields.

use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::minisign::{PublicKey, SecretKey};
use crate::signed::SignedEntry;
use crate::{
    CatalogBackend, CatalogError, CatalogId, CatalogOp, Generation, PendingCommit, Result,
    SyncStatus,
};

/// Default DataLayer RPC URL.
pub const DEFAULT_RPC_URL: &str = "https://localhost:8562";

/// Connection settings for the DataLayer RPC.
#[derive(Debug, Clone)]
pub struct ChiaConfig {
    /// RPC base URL.
    pub rpc_url: String,
    /// Client certificate (PEM).
    pub ssl_cert: PathBuf,
    /// Client private key (PEM).
    pub ssl_key: PathBuf,
    /// Optional CA certificate (PEM), usually `config/ssl/ca/private_ca.crt`.
    /// When unset, server certificate validation is disabled, which is only
    /// acceptable because the RPC is bound to localhost.
    pub ca_cert: Option<PathBuf>,
    /// Fee per on-chain update in mojos.
    pub fee_mojos: u64,
}

/// Chia DataLayer catalog backend.
#[derive(Debug)]
pub struct ChiaDataLayer {
    cfg: ChiaConfig,
    http: reqwest::Client,
    catalog: Option<(String, PublicKey)>,
    signer: Option<SecretKey>,
    poll: Duration,
    timeout: Duration,
}

fn read(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path)
        .map_err(|e| CatalogError::Config(format!("cannot read {}: {e}", path.display())))
}

impl ChiaDataLayer {
    /// Build the backend and its mTLS HTTP client.
    pub fn new(cfg: ChiaConfig) -> Result<Self> {
        let mut pem = read(&cfg.ssl_cert)?;
        pem.push(b'\n');
        pem.extend(read(&cfg.ssl_key)?);
        let identity = reqwest::Identity::from_pem(&pem)?;
        let mut builder = reqwest::Client::builder().identity(identity);
        builder = match &cfg.ca_cert {
            Some(ca) => builder
                .tls_certs_only([reqwest::Certificate::from_pem(&read(ca)?)?])
                .danger_accept_invalid_hostnames(true),
            None => builder.danger_accept_invalid_certs(true),
        };
        let http = builder.build()?;
        Ok(Self::with_client(cfg, http))
    }

    /// Build with a caller supplied HTTP client (tests use plain HTTP mocks).
    pub fn with_client(cfg: ChiaConfig, http: reqwest::Client) -> Self {
        Self {
            cfg,
            http,
            catalog: None,
            signer: None,
            poll: Duration::from_secs(20),
            timeout: Duration::from_secs(1800),
        }
    }

    /// Bind to a logical catalog name and pinned publisher key. Reads then
    /// unwrap and verify every `SignedEntry` envelope.
    pub fn with_catalog(mut self, name: &str, publisher: PublicKey) -> Self {
        self.catalog = Some((name.into(), publisher));
        self
    }

    /// Attach the publisher key used to sign staged values.
    pub fn with_signer(mut self, sk: SecretKey) -> Result<Self> {
        let (_, pk) = self.binding()?;
        if sk.public_key() != *pk {
            return Err(CatalogError::Config(
                "publisher secret key does not match pinned catalog.publisher_pubkey".into(),
            ));
        }
        self.signer = Some(sk);
        Ok(self)
    }

    /// Confirmation polling interval and timeout.
    pub fn with_confirm_policy(mut self, poll_secs: u64, timeout_secs: u64) -> Self {
        self.poll = Duration::from_secs(poll_secs.max(1));
        self.timeout = Duration::from_secs(timeout_secs.max(1));
        self
    }

    /// Like [`Self::with_confirm_policy`] with millisecond precision (tests).
    pub fn with_confirm_policy_ms(mut self, poll_ms: u64, timeout_ms: u64) -> Self {
        self.poll = Duration::from_millis(poll_ms.max(1));
        self.timeout = Duration::from_millis(timeout_ms.max(1));
        self
    }

    fn binding(&self) -> Result<(&str, &PublicKey)> {
        self.catalog
            .as_ref()
            .map(|(n, k)| (n.as_str(), k))
            .ok_or_else(|| {
                CatalogError::Config("chia backend has no catalog name and pinned key".into())
            })
    }

    /// POST a DataLayer RPC call and return the JSON response, checking `success`.
    pub async fn rpc(&self, method: &str, body: Value) -> Result<Value> {
        let url = format!("{}/{method}", self.cfg.rpc_url.trim_end_matches('/'));
        let resp: Value = self.http.post(url).json(&body).send().await?.json().await?;
        check_success(resp)
    }

    async fn write_changelist(
        &self,
        catalog: &CatalogId,
        values: Vec<(String, Option<Vec<u8>>)>,
    ) -> Result<()> {
        let ops: Vec<CatalogOp> = values
            .into_iter()
            .map(|(key, v)| match v {
                Some(value) => CatalogOp::Upsert { key, value },
                None => CatalogOp::Delete { key },
            })
            .collect();
        // Keep each call well under rpc_server_max_request_body_size (25 MiB of
        // hex JSON): batches of at most about 8 MiB of raw values.
        let mut batch: Vec<CatalogOp> = Vec::new();
        let mut bytes = 0usize;
        for op in ops {
            let n = match &op {
                CatalogOp::Upsert { key, value } => key.len() + value.len(),
                CatalogOp::Delete { key } => key.len(),
            };
            if !batch.is_empty() && bytes + n > MAX_BATCH_RAW_BYTES {
                self.batch_update(catalog, &std::mem::take(&mut batch))
                    .await?;
                bytes = 0;
            }
            bytes += n;
            batch.push(op);
        }
        if !batch.is_empty() {
            self.batch_update(catalog, &batch).await?;
        }
        Ok(())
    }

    async fn batch_update(&self, catalog: &CatalogId, batch: &[CatalogOp]) -> Result<()> {
        self.rpc(
            "batch_update",
            json!({
                "id": catalog.0,
                "changelist": changelist(batch),
                "submit_on_chain": false,
                "fee": self.cfg.fee_mojos,
            }),
        )
        .await?;
        Ok(())
    }

    /// Stage already signed entries unchanged (for `catalog import`).
    pub async fn stage_signed(&self, catalog: &CatalogId, entries: &[SignedEntry]) -> Result<()> {
        let (name, pk) = self.binding()?;
        let mut values = Vec::with_capacity(entries.len());
        for e in entries {
            e.verify(pk, name)?;
            values.push((e.key.clone(), Some(e.to_bytes()?)));
        }
        self.write_changelist(catalog, values).await
    }

    /// `add_mirror` with an explicit coin amount and fee.
    pub async fn add_mirror_with(
        &self,
        catalog: &CatalogId,
        urls: &[String],
        amount: u64,
        fee: u64,
    ) -> Result<()> {
        self.rpc(
            "add_mirror",
            json!({ "id": catalog.0, "urls": urls, "amount": amount, "fee": fee }),
        )
        .await?;
        Ok(())
    }

    /// Latest root: (hash, confirmed).
    pub async fn root(&self, catalog: &CatalogId) -> Result<(String, bool)> {
        let r = self.rpc("get_root", json!({ "id": catalog.0 })).await?;
        let hash = r
            .get("hash")
            .and_then(Value::as_str)
            .ok_or_else(|| CatalogError::Backend("get_root without hash".into()))?
            .trim_start_matches("0x")
            .to_string();
        Ok((
            hash,
            r.get("confirmed").and_then(Value::as_bool).unwrap_or(false),
        ))
    }
}

/// Largest raw key plus value bytes per `batch_update` call.
pub const MAX_BATCH_RAW_BYTES: usize = 8 << 20;

/// Validate the `success` flag that Chia RPC responses carry.
pub fn check_success(resp: Value) -> Result<Value> {
    if resp.get("success").and_then(Value::as_bool) == Some(true) {
        Ok(resp)
    } else {
        let msg = resp
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown DataLayer error")
            .to_string();
        Err(CatalogError::Backend(msg))
    }
}

/// Hex encode a UTF-8 key or value for the wire (DataLayer requires hex).
pub fn to_hex(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

/// Decode a hex string from DataLayer, accepting an optional `0x` prefix.
pub fn from_hex(s: &str) -> Result<Vec<u8>> {
    let s = s.strip_prefix("0x").unwrap_or(s);
    hex::decode(s).map_err(|e| CatalogError::Backend(format!("bad hex from DataLayer: {e}")))
}

/// Convert staged ops into the `changelist` format used by `batch_update`.
pub fn changelist(batch: &[CatalogOp]) -> Value {
    Value::Array(
        batch
            .iter()
            .map(|op| match op {
                CatalogOp::Upsert { key, value } => json!({
                    "action": "upsert",
                    "key": to_hex(key.as_bytes()),
                    "value": to_hex(value),
                }),
                CatalogOp::Delete { key } => json!({
                    "action": "delete",
                    "key": to_hex(key.as_bytes()),
                }),
            })
            .collect(),
    )
}

fn is_missing_key(e: &CatalogError) -> bool {
    matches!(e, CatalogError::Backend(m) if m.to_ascii_lowercase().contains("not found"))
}

#[async_trait]
impl CatalogBackend for ChiaDataLayer {
    fn kind(&self) -> &'static str {
        "chia-datalayer"
    }

    async fn create_catalog(&self) -> Result<CatalogId> {
        let r = self
            .rpc("create_data_store", json!({ "fee": self.cfg.fee_mojos }))
            .await?;
        let id = r
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| CatalogError::Backend("create_data_store without id".into()))?;
        Ok(CatalogId(id.trim_start_matches("0x").to_string()))
    }

    async fn get(&self, catalog: &CatalogId, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .get_signed(catalog, key)
            .await?
            .map(|e| e.value.into_bytes()))
    }

    async fn get_signed(&self, catalog: &CatalogId, key: &str) -> Result<Option<SignedEntry>> {
        // Stored values are SignedEntry envelopes (spec section 6.9); they are
        // unwrapped and verified against the pinned key here.
        let (name, pk) = self.binding()?;
        let resp = match self
            .rpc(
                "get_value",
                json!({ "id": catalog.0, "key": to_hex(key.as_bytes()) }),
            )
            .await
        {
            Ok(r) => r,
            Err(e) if is_missing_key(&e) => return Ok(None),
            Err(e) => return Err(e),
        };
        let Some(v) = resp.get("value").and_then(Value::as_str) else {
            return Ok(None);
        };
        let entry = SignedEntry::from_bytes(&from_hex(v)?)?;
        if entry.key != key {
            return Err(CatalogError::Signature(format!(
                "value under {key:?} is an envelope for {:?}",
                entry.key
            )));
        }
        entry.verify(pk, name)?;
        Ok(Some(entry))
    }

    async fn keys(&self, catalog: &CatalogId, prefix: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        let mut page = 0u64;
        loop {
            let resp = self
                .rpc(
                    "get_keys",
                    json!({ "id": catalog.0, "page": page, "max_page_size": 1_000_000 }),
                )
                .await?;
            for k in resp
                .get("keys")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(k) = k.as_str() {
                    let key = String::from_utf8(from_hex(k)?)
                        .map_err(|e| CatalogError::Backend(format!("non UTF-8 key: {e}")))?;
                    if key.starts_with(prefix) {
                        out.push(key);
                    }
                }
            }
            let total = resp.get("total_pages").and_then(Value::as_u64).unwrap_or(1);
            page += 1;
            if page >= total || page > 100_000 {
                break;
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }

    async fn stage(&self, catalog: &CatalogId, batch: &[CatalogOp]) -> Result<()> {
        let (name, _) = self.binding()?;
        let sk = self
            .signer
            .as_ref()
            .ok_or_else(|| CatalogError::Config("no publisher key loaded".into()))?;
        let mut values = Vec::with_capacity(batch.len());
        for op in batch {
            values.push(match op {
                CatalogOp::Upsert { key, value } => (
                    key.clone(),
                    Some(SignedEntry::sign(sk, name, key, value)?.to_bytes()?),
                ),
                CatalogOp::Delete { key } => (key.clone(), None),
            });
        }
        self.write_changelist(catalog, values).await
    }

    async fn commit(&self, catalog: &CatalogId) -> Result<PendingCommit> {
        let previous = self.root(catalog).await.ok().map(|(h, _)| h);
        let r = self
            .rpc(
                "submit_pending_root",
                json!({ "id": catalog.0, "fee": self.cfg.fee_mojos }),
            )
            .await?;
        Ok(PendingCommit {
            catalog: catalog.clone(),
            expected_root: None,
            tx_id: r.get("tx_id").and_then(Value::as_str).map(str::to_owned),
            previous_root: previous,
        })
    }

    async fn wait_confirmed(&self, pending: &PendingCommit) -> Result<Generation> {
        let deadline = tokio::time::Instant::now() + self.timeout;
        loop {
            let (hash, confirmed) = self.root(&pending.catalog).await?;
            let changed = pending.previous_root.as_deref() != Some(hash.as_str());
            if confirmed && changed && pending.expected_root.as_deref().is_none_or(|e| e == hash) {
                let hist = self
                    .rpc("get_root_history", json!({ "id": pending.catalog.0 }))
                    .await?;
                let n = hist
                    .get("root_history")
                    .and_then(Value::as_array)
                    .map_or(0, |a| a.len() as u64);
                return Ok(Generation {
                    number: n.saturating_sub(1),
                    root: hash,
                });
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(CatalogError::Backend(format!(
                    "root for {} not confirmed within {:?}",
                    pending.catalog, self.timeout
                )));
            }
            tokio::time::sleep(self.poll).await;
        }
    }

    async fn subscribe(&self, catalog: &CatalogId, mirror_urls: &[String]) -> Result<()> {
        self.rpc("subscribe", json!({ "id": catalog.0, "urls": mirror_urls }))
            .await?;
        Ok(())
    }

    async fn sync_status(&self, catalog: &CatalogId) -> Result<SyncStatus> {
        let r = self
            .rpc("get_sync_status", json!({ "id": catalog.0 }))
            .await?;
        let s = r
            .get("sync_status")
            .ok_or_else(|| CatalogError::Backend("get_sync_status without sync_status".into()))?;
        let num = |k: &str| {
            s.get(k)
                .and_then(Value::as_u64)
                .ok_or_else(|| CatalogError::Backend(format!("get_sync_status without {k}")))
        };
        Ok(SyncStatus {
            local_generation: num("generation")?,
            target_generation: num("target_generation")?,
        })
    }

    async fn add_mirror(&self, catalog: &CatalogId, urls: &[String]) -> Result<()> {
        self.add_mirror_with(catalog, urls, 0, self.cfg.fee_mojos)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        assert_eq!(to_hex(b"ts/v1/catalog"), "74732f76312f636174616c6f67");
        assert_eq!(from_hex("0x6869").unwrap(), b"hi");
        assert_eq!(from_hex("6869").unwrap(), b"hi");
        assert!(from_hex("zz").is_err());
    }

    #[test]
    fn changelist_shape() {
        let cl = changelist(&[
            CatalogOp::Upsert {
                key: "a".into(),
                value: b"{}".to_vec(),
            },
            CatalogOp::Delete { key: "b".into() },
        ]);
        assert_eq!(
            cl,
            json!([
                {"action": "upsert", "key": "61", "value": "7b7d"},
                {"action": "delete", "key": "62"}
            ])
        );
    }

    #[test]
    fn success_flag() {
        assert!(check_success(json!({"success": true})).is_ok());
        let err = check_success(json!({"success": false, "error": "boom"})).unwrap_err();
        assert!(err.to_string().contains("boom"));
    }

    use crate::minisign::SecretKey;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const STORE: &str = "aa00000000000000000000000000000000000000000000000000000000000000";

    fn key() -> SecretKey {
        SecretKey::from_seed(&[5; 32])
    }

    async fn backend(server: &MockServer) -> ChiaDataLayer {
        ChiaDataLayer::with_client(
            ChiaConfig {
                rpc_url: server.uri(),
                ssl_cert: "unused".into(),
                ssl_key: "unused".into(),
                ca_cert: None,
                fee_mojos: 7,
            },
            reqwest::Client::new(),
        )
        .with_catalog("main", key().public_key())
        .with_signer(key())
        .unwrap()
        .with_confirm_policy_ms(1, 2000)
    }

    async fn rpc_ok(server: &MockServer, m: &str, body: Value, resp: Value) {
        let mut r = resp;
        r["success"] = json!(true);
        Mock::given(method("POST"))
            .and(path(format!("/{m}")))
            .and(body_partial_json(body))
            .respond_with(ResponseTemplate::new(200).set_body_json(r))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn get_unwraps_and_verifies_envelopes() {
        let server = MockServer::start().await;
        let good = SignedEntry::sign(&key(), "main", "ts/v1/catalog", br#"{"a":1}"#).unwrap();
        rpc_ok(
            &server,
            "get_value",
            json!({"id": STORE, "key": to_hex(b"ts/v1/catalog")}),
            json!({"value": to_hex(&good.to_bytes().unwrap())}),
        )
        .await;
        let forged =
            SignedEntry::sign(&SecretKey::from_seed(&[6; 32]), "main", "ts/v1/evil", b"{}")
                .unwrap();
        rpc_ok(
            &server,
            "get_value",
            json!({"key": to_hex(b"ts/v1/evil")}),
            json!({"value": to_hex(&forged.to_bytes().unwrap())}),
        )
        .await;
        Mock::given(method("POST"))
            .and(path("/get_value"))
            .and(body_partial_json(json!({"key": to_hex(b"ts/v1/none")})))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"success": false, "error": "Key not found"})),
            )
            .mount(&server)
            .await;
        let b = backend(&server).await;
        let id = CatalogId(STORE.into());
        assert_eq!(
            b.get(&id, "ts/v1/catalog").await.unwrap().unwrap(),
            br#"{"a":1}"#
        );
        assert!(matches!(
            b.get(&id, "ts/v1/evil").await,
            Err(CatalogError::Signature(_))
        ));
        assert!(b.get(&id, "ts/v1/none").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn write_path_create_stage_commit_confirm() {
        let server = MockServer::start().await;
        rpc_ok(
            &server,
            "create_data_store",
            json!({"fee": 7}),
            json!({"id": format!("0x{STORE}"), "txs": []}),
        )
        .await;
        rpc_ok(
            &server,
            "batch_update",
            json!({"id": STORE, "submit_on_chain": false, "fee": 7}),
            json!({}),
        )
        .await;
        rpc_ok(
            &server,
            "submit_pending_root",
            json!({"id": STORE, "fee": 7}),
            json!({"tx_id": "0xabc"}),
        )
        .await;
        // get_root: old root first, then unconfirmed new root, then confirmed.
        Mock::given(method("POST"))
            .and(path("/get_root"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"success": true, "hash": "0x01", "confirmed": true})),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/get_root"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"success": true, "hash": "0x02", "confirmed": false})),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/get_root"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"success": true, "hash": "0x02", "confirmed": true})),
            )
            .mount(&server)
            .await;
        rpc_ok(
            &server,
            "get_root_history",
            json!({"id": STORE}),
            json!({"root_history": [{}, {}, {}]}),
        )
        .await;

        let b = backend(&server).await;
        let id = b.create_catalog().await.unwrap();
        assert_eq!(id.0, STORE);
        b.stage(
            &id,
            &[CatalogOp::Upsert {
                key: "ts/v1/catalog".into(),
                value: b"{}".to_vec(),
            }],
        )
        .await
        .unwrap();
        let p = b.commit(&id).await.unwrap();
        assert_eq!(p.tx_id.as_deref(), Some("0xabc"));
        assert_eq!(p.previous_root.as_deref(), Some("01"));
        let g = b.wait_confirmed(&p).await.unwrap();
        assert_eq!(
            g,
            Generation {
                number: 2,
                root: "02".into()
            }
        );

        // The staged value is a verifiable envelope.
        let reqs = server.received_requests().await.unwrap();
        let bu = reqs
            .iter()
            .find(|r| r.url.path() == "/batch_update")
            .unwrap();
        let body: Value = serde_json::from_slice(&bu.body).unwrap();
        let v = body["changelist"][0]["value"].as_str().unwrap();
        let e = SignedEntry::from_bytes(&from_hex(v).unwrap()).unwrap();
        e.verify(&key().public_key(), "main").unwrap();
    }

    #[tokio::test]
    async fn subscribe_sync_status_mirror_and_keys() {
        let server = MockServer::start().await;
        rpc_ok(
            &server,
            "subscribe",
            json!({"id": STORE, "urls": ["http://m:8575"]}),
            json!({}),
        )
        .await;
        rpc_ok(
            &server,
            "get_sync_status",
            json!({"id": STORE}),
            json!({"sync_status": {"generation": 4, "target_generation": 5, "root_hash": "0x1", "target_root_hash": "0x2"}}),
        )
        .await;
        rpc_ok(
            &server,
            "add_mirror",
            json!({"id": STORE, "urls": ["http://m:8575"], "amount": 0, "fee": 7}),
            json!({}),
        )
        .await;
        rpc_ok(
            &server,
            "get_keys",
            json!({"page": 0}),
            json!({"keys": [format!("0x{}", to_hex(b"ts/v1/b")), to_hex(b"other")], "total_pages": 2}),
        )
        .await;
        rpc_ok(
            &server,
            "get_keys",
            json!({"page": 1}),
            json!({"keys": [to_hex(b"ts/v1/a")], "total_pages": 2}),
        )
        .await;
        let b = backend(&server).await;
        let id = CatalogId(STORE.into());
        let urls = vec!["http://m:8575".to_string()];
        b.subscribe(&id, &urls).await.unwrap();
        let st = b.sync_status(&id).await.unwrap();
        assert_eq!((st.local_generation, st.target_generation), (4, 5));
        assert!(!st.is_synced());
        b.add_mirror(&id, &urls).await.unwrap();
        assert_eq!(
            b.keys(&id, "ts/").await.unwrap(),
            vec!["ts/v1/a", "ts/v1/b"]
        );
    }

    #[tokio::test]
    async fn stage_signed_rejects_foreign_entries() {
        let server = MockServer::start().await;
        let b = backend(&server).await;
        let foreign = SignedEntry::sign(
            &SecretKey::from_seed(&[9; 32]),
            "main",
            "ts/v1/catalog",
            b"{}",
        )
        .unwrap();
        assert!(
            b.stage_signed(&CatalogId(STORE.into()), &[foreign])
                .await
                .is_err()
        );
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[test]
    fn missing_tls_files_is_config_error() {
        let err = ChiaDataLayer::new(ChiaConfig {
            rpc_url: DEFAULT_RPC_URL.into(),
            ssl_cert: "/nonexistent/cert.crt".into(),
            ssl_key: "/nonexistent/key.key".into(),
            ca_cert: None,
            fee_mojos: 0,
        })
        .unwrap_err();
        assert!(matches!(err, CatalogError::Config(_)));
    }
}
