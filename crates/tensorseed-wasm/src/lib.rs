// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! WebAssembly bindings for [`tensorseed_verify`].
//!
//! The browser explorer fetches catalog files and coin records over HTTPS,
//! then asks this crate to check signatures, walk the file catalog chain,
//! verify a DataLayer inclusion proof, and check a coin record. No
//! verification logic is reimplemented in TypeScript. Methods that return
//! structured data return canonical JSON strings; the page parses them.

#![warn(missing_docs)]

use serde::Serialize;
use tensorseed_core::manifest::Manifest;
use tensorseed_verify::anchor::{
    ChainProof, CoinRecord, VerifiedProof, check_coin_record, coin_id, h32,
};
use tensorseed_verify::file::{ChainState, Head, HeadStatus, SignedFile};
use tensorseed_verify::minisign::PublicKey;
use wasm_bindgen::prelude::*;

fn js_err(err: impl std::fmt::Display) -> JsError {
    JsError::new(&err.to_string())
}

fn to_json<T: Serialize>(v: &T) -> Result<String, JsError> {
    serde_json::to_string(v).map_err(js_err)
}

fn signed(body: &[u8], sig: &str) -> SignedFile {
    SignedFile {
        body: body.to_vec(),
        sig: sig.to_string(),
    }
}

/// In-memory verified catalog state, walked from signed heads, snapshots,
/// and deltas.
#[wasm_bindgen]
pub struct CatalogVerifier {
    state: ChainState,
    pk: PublicKey,
}

#[wasm_bindgen]
impl CatalogVerifier {
    /// Empty state for `catalog`, pinning `pubkey` (a minisign public key).
    ///
    /// # Errors
    ///
    /// The catalog name or public key is invalid.
    #[wasm_bindgen(constructor)]
    pub fn new(catalog: &str, pubkey: &str) -> Result<CatalogVerifier, JsError> {
        let pk = PublicKey::parse(pubkey).map_err(js_err)?;
        let state = ChainState::empty(catalog).map_err(js_err)?;
        Ok(Self { state, pk })
    }

    /// Catalog name this verifier was created for.
    #[wasm_bindgen(getter)]
    pub fn catalog(&self) -> String {
        self.state.catalog.clone()
    }

    /// Highest accepted revision (0 if nothing has been applied).
    #[wasm_bindgen(getter)]
    pub fn revision(&self) -> u64 {
        self.state.revision
    }

    /// Root of the accepted revision.
    #[wasm_bindgen(getter)]
    pub fn root(&self) -> String {
        self.state.root.clone()
    }

    /// Number of signed entries in the current state.
    #[wasm_bindgen(getter)]
    pub fn entry_count(&self) -> u32 {
        u32::try_from(self.state.entries.len()).unwrap_or(u32::MAX)
    }

    /// Verify a signed `head.json` against current state.
    ///
    /// Returns JSON `{ status, target, revision, root, prev_root,
    /// expires_unix, snapshot }`. `status` is `"up_to_date"` or `"behind"`.
    ///
    /// # Errors
    ///
    /// Signature, rollback, fork, expiry, or catalog mismatch failures.
    pub fn verify_head(&self, body: &[u8], sig: &str, now_unix: f64) -> Result<String, JsError> {
        let now = if now_unix.is_finite() && now_unix > 0.0 {
            now_unix as u64
        } else {
            0
        };
        let status = self
            .state
            .check_head(&self.pk, &signed(body, sig), now)
            .map_err(js_err)?;
        let head: Head = serde_json::from_slice(body).map_err(js_err)?;
        let (kind, target) = match status {
            HeadStatus::UpToDate => ("up_to_date", head.revision),
            HeadStatus::Behind { target } => ("behind", target),
        };
        to_json(&serde_json::json!({
            "status": kind,
            "target": target,
            "revision": head.revision,
            "root": head.root,
            "prev_root": head.prev_root,
            "expires_unix": head.expires_unix,
            "snapshot": head.snapshot,
        }))
    }

    /// Apply a signed delta, advancing local state by one revision.
    ///
    /// # Errors
    ///
    /// Signature or chain failures.
    pub fn apply_delta(&mut self, body: &[u8], sig: &str) -> Result<(), JsError> {
        self.state
            .apply_delta(&self.pk, &signed(body, sig))
            .map_err(js_err)
    }

    /// Load a signed snapshot, replacing local state.
    ///
    /// # Errors
    ///
    /// Signature, rollback, or chain failures.
    pub fn load_snapshot(&mut self, body: &[u8], sig: &str) -> Result<(), JsError> {
        self.state
            .load_snapshot(&self.pk, &signed(body, sig))
            .map_err(js_err)
    }

    /// Verify a `chain-proof.json` body against the pinned key, this catalog,
    /// and `store_id`. When local state is non-empty, the proven revision and
    /// root must match it.
    ///
    /// # Errors
    ///
    /// Signature or proof failures, or a mismatch with local state.
    pub fn verify_chain_proof(&self, body: &[u8], store_id: &str) -> Result<String, JsError> {
        let proof = ChainProof::from_bytes(body).map_err(js_err)?;
        let verified = proof
            .verify(&self.pk, &self.state.catalog, store_id)
            .map_err(js_err)?;
        if self.state.revision > 0
            && !verified
                .anchor
                .matches(self.state.revision, &self.state.root)
        {
            return Err(js_err(format!(
                "chain proof is for revision {} root {}, but the file catalog is at revision {} root {}",
                verified.anchor.revision,
                verified.anchor.root,
                self.state.revision,
                self.state.root
            )));
        }
        to_json(&verified)
    }

    /// JSON document stored under `key`, or an empty string if absent.
    pub fn get_entry(&self, key: &str) -> String {
        self.state
            .entries
            .get(key)
            .map(|e| e.value.clone())
            .unwrap_or_default()
    }

    /// Every catalog key in this state, optionally filtered by prefix.
    pub fn list_keys(&self, prefix: &str) -> Vec<String> {
        self.state
            .entries
            .keys()
            .filter(|k| prefix.is_empty() || k.starts_with(prefix))
            .cloned()
            .collect()
    }
}

/// Coin id from parent coin, puzzle hash, and amount (hex in, hex out).
///
/// # Errors
///
/// The hex is not 32 bytes.
#[wasm_bindgen]
pub fn coin_id_hex(parent: &str, puzzle_hash: &str, amount: u64) -> Result<String, JsError> {
    let p = h32(parent, "parent").map_err(js_err)?;
    let ph = h32(puzzle_hash, "puzzle_hash").map_err(js_err)?;
    Ok(hex::encode(coin_id(&p, &ph, amount)))
}

/// Check a `get_coin_record_by_name` JSON body against a verified proof
/// (the JSON object returned by [`CatalogVerifier::verify_chain_proof`]).
///
/// `peak_height` may be omitted (pass `undefined`) when the coin is unspent.
/// `grace_blocks` is the same window the CLI uses (default 192).
///
/// # Errors
///
/// The record contradicts the proof, or the JSON is malformed.
#[wasm_bindgen]
pub fn check_coin_rpc(
    verified_proof_json: &str,
    coin_rpc_json: &str,
    peak_height: Option<u64>,
    grace_blocks: u64,
) -> Result<String, JsError> {
    let proof: VerifiedProof = serde_json::from_str(verified_proof_json).map_err(js_err)?;
    let rpc: serde_json::Value = serde_json::from_str(coin_rpc_json).map_err(js_err)?;
    let rec = CoinRecord::from_rpc(&rpc)
        .map_err(js_err)?
        .ok_or_else(|| js_err("store coin does not exist on chain"))?;
    let state = check_coin_record(&proof, &rec, peak_height, grace_blocks).map_err(js_err)?;
    to_json(&serde_json::json!({
        "state": state,
        "confirmed_block_index": rec.confirmed_block_index,
        "spent": rec.spent,
        "spent_block_index": rec.spent_block_index,
    }))
}

/// Parse and validate a catalog manifest document. Returns the manifest as
/// JSON.
///
/// # Errors
///
/// The document is not a well formed manifest.
#[wasm_bindgen]
pub fn parse_manifest(value_json: &str) -> Result<String, JsError> {
    let m = Manifest::from_json(value_json.as_bytes()).map_err(js_err)?;
    to_json(&m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tensorseed_verify::file::DeltaOp;
    use tensorseed_verify::minisign::SecretKey;
    use tensorseed_verify::signed::SignedEntry;

    const CAT: &str = "main";
    const DOC: &[u8] = br#"{"schema":"ts.catalog/1"}"#;

    fn pair() -> (SecretKey, PublicKey) {
        let sk = SecretKey::from_seed(&[9; 32]);
        let pk = sk.public_key();
        (sk, pk)
    }

    fn publish(sk: &SecretKey, n: u64) -> (ChainState, Vec<SignedFile>, SignedFile) {
        let pk = sk.public_key();
        let mut st = ChainState::empty(CAT).unwrap();
        let mut deltas = Vec::new();
        for i in 1..=n {
            let key = format!("ts/v1/index/model/org/m{i}");
            let op = DeltaOp::Upsert {
                entry: SignedEntry::sign(sk, CAT, &key, DOC).unwrap(),
            };
            let d = st.build_delta(sk, vec![op]).unwrap();
            st.apply_delta(&pk, &d).unwrap();
            deltas.push(d);
        }
        let snap = st.build_snapshot(sk).unwrap();
        (st, deltas, snap)
    }

    fn head_of(st: &ChainState, sk: &SecretKey) -> SignedFile {
        st.build_head(sk, None, u64::MAX).unwrap()
    }

    #[test]
    fn good_head_and_delta_verify() {
        let (sk, pk) = pair();
        let (st, deltas, snap) = publish(&sk, 2);
        let mut v = ChainState::empty(CAT).unwrap();
        v.load_snapshot(&pk, &snap).unwrap();
        assert_eq!(v.revision, 2);
        let h = head_of(&st, &sk);
        assert!(matches!(
            v.check_head(&pk, &h, 1).unwrap(),
            HeadStatus::UpToDate
        ));
        let mut v2 = ChainState::empty(CAT).unwrap();
        for d in &deltas {
            v2.apply_delta(&pk, d).unwrap();
        }
        assert_eq!(v2.root, st.root);
    }

    #[test]
    fn tampered_delta_is_rejected() {
        let (sk, pk) = pair();
        let (_, deltas, _) = publish(&sk, 1);
        let mut tampered = deltas[0].clone();
        let text = String::from_utf8(tampered.body).unwrap();
        tampered.body = text.replace("m1", "m9").into_bytes();
        let err = ChainState::empty(CAT)
            .unwrap()
            .apply_delta(&pk, &tampered)
            .unwrap_err();
        assert!(
            matches!(err, tensorseed_verify::Error::Signature(_)),
            "{err}"
        );
    }

    #[test]
    fn wrong_key_is_rejected() {
        let (sk, _) = pair();
        let (_, deltas, snap) = publish(&sk, 1);
        let other = SecretKey::from_seed(&[10; 32]).public_key();
        let mut v = ChainState::empty(CAT).unwrap();
        assert!(matches!(
            v.apply_delta(&other, &deltas[0]).unwrap_err(),
            tensorseed_verify::Error::Signature(_)
        ));
        assert!(matches!(
            v.load_snapshot(&other, &snap).unwrap_err(),
            tensorseed_verify::Error::Signature(_)
        ));
    }

    #[test]
    fn chain_mismatch_is_rejected() {
        let (sk, pk) = pair();
        let (st, _, _) = publish(&sk, 1);
        let h = head_of(&st, &sk);
        let other = ChainState::empty("other").unwrap();
        assert!(matches!(
            other.check_head(&pk, &h, 1).unwrap_err(),
            tensorseed_verify::Error::Chain(_)
        ));

        // A different revision 1 signed by the same key is a fork, not a
        // later revision of the accepted chain.
        let base = ChainState::empty(CAT).unwrap();
        let f = base
            .build_delta(
                &sk,
                vec![DeltaOp::Upsert {
                    entry: SignedEntry::sign(&sk, CAT, "ts/v1/catalog", DOC).unwrap(),
                }],
            )
            .unwrap();
        let mut forked = base;
        forked.apply_delta(&pk, &f).unwrap();
        let fork_head = forked.build_head(&sk, None, u64::MAX).unwrap();
        assert!(matches!(
            st.check_head(&pk, &fork_head, 1).unwrap_err(),
            tensorseed_verify::Error::Fork { revision: 1 }
        ));
    }
}
