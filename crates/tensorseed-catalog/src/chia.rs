// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Chia DataLayer backend (spec sections 6.1 to 6.7).
//!
//! Talks to the local `chia data` service RPC (default `https://localhost:8562`)
//! with mutual TLS using `private_data_layer.crt` / `.key`. The RPC port must
//! never be exposed publicly.
//!
//! Status: stub. The RPC transport, hex helpers, and read calls are written but
//! unverified against a live node; write calls return
//! [`CatalogError::NotImplemented`]. M0 must confirm exact response fields.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{Value, json};

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
#[derive(Debug, Clone)]
pub struct ChiaDataLayer {
    cfg: ChiaConfig,
    http: reqwest::Client,
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
        Ok(Self {
            cfg,
            http: builder.build()?,
        })
    }

    /// POST a DataLayer RPC call and return the JSON response, checking `success`.
    pub async fn rpc(&self, method: &str, body: Value) -> Result<Value> {
        let url = format!("{}/{method}", self.cfg.rpc_url.trim_end_matches('/'));
        let resp: Value = self.http.post(url).json(&body).send().await?.json().await?;
        check_success(resp)
    }
}

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

#[async_trait]
impl CatalogBackend for ChiaDataLayer {
    fn kind(&self) -> &'static str {
        "chia-datalayer"
    }

    async fn create_catalog(&self) -> Result<CatalogId> {
        // TODO(M1): create_data_store {fee} then wait for confirmation.
        Err(CatalogError::NotImplemented("chia create_data_store"))
    }

    async fn get(&self, catalog: &CatalogId, key: &str) -> Result<Option<Vec<u8>>> {
        // Unverified until M0: missing keys may surface as an error rather than null.
        let resp = self
            .rpc(
                "get_value",
                json!({ "id": catalog.0, "key": to_hex(key.as_bytes()) }),
            )
            .await?;
        match resp.get("value").and_then(Value::as_str) {
            Some(v) => Ok(Some(from_hex(v)?)),
            None => Ok(None),
        }
    }

    async fn keys(&self, catalog: &CatalogId, prefix: &str) -> Result<Vec<String>> {
        // TODO(M1): use paginated get_keys for large catalogs.
        let resp = self.rpc("get_keys", json!({ "id": catalog.0 })).await?;
        let mut out = Vec::new();
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
        out.sort();
        Ok(out)
    }

    async fn stage(&self, _catalog: &CatalogId, _batch: &[CatalogOp]) -> Result<()> {
        // TODO(M1): batch_update {id, changelist, submit_on_chain: false, fee}
        let _ = self.cfg.fee_mojos;
        Err(CatalogError::NotImplemented(
            "chia batch_update(submit_on_chain=false)",
        ))
    }

    async fn commit(&self, _catalog: &CatalogId) -> Result<PendingCommit> {
        // TODO(M1): submit_pending_root through the single write queue.
        Err(CatalogError::NotImplemented("chia submit_pending_root"))
    }

    async fn wait_confirmed(&self, _pending: &PendingCommit) -> Result<Generation> {
        // TODO(M1): poll get_root until confirmed and matching expected root.
        Err(CatalogError::NotImplemented("chia confirmation polling"))
    }

    async fn subscribe(&self, _catalog: &CatalogId, _mirror_urls: &[String]) -> Result<()> {
        Err(CatalogError::NotImplemented("chia subscribe"))
    }

    async fn sync_status(&self, _catalog: &CatalogId) -> Result<SyncStatus> {
        // TODO(M1): get_sync_status {id}; confirm field names in M0.
        Err(CatalogError::NotImplemented("chia get_sync_status"))
    }

    async fn add_mirror(&self, _catalog: &CatalogId, _urls: &[String]) -> Result<()> {
        Err(CatalogError::NotImplemented("chia add_mirror"))
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
