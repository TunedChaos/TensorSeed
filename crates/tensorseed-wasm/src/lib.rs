// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! WebAssembly bindings for [`tensorseed_verify`].
//!
//! The browser explorer fetches catalog files and coin records over HTTPS,
//! then asks this crate to check signatures, walk the file catalog chain,
//! verify a DataLayer inclusion proof, and check a coin record. The
//! `@tensorseed/verify` npm package (in `npm/verify`) uses the same build
//! to check served-weights claims, revocations, and signed responses in a
//! browser or Node ([`serving`]), and signed follow lists ([`follow`]).
//! No verification logic is reimplemented in JavaScript. Methods that
//! return structured data return JSON strings; the caller parses them.

#![warn(missing_docs)]

use serde::Serialize;
use tensorseed_core::keys::RepoRef;
use tensorseed_core::manifest::Manifest;
use tensorseed_core::manifest::Tombstone;
use tensorseed_verify::anchor::{
    ChainProof, CoinRecord, VerifiedProof, check_coin_record, coin_id, h32,
};
use tensorseed_verify::claim_check::{CatalogMatch, CatalogResult, Freshness, Status, check};
use tensorseed_verify::file::{ChainState, Head, HeadStatus, SignedFile};
use tensorseed_verify::manifest::{merge_file_pages, verify_manifest_entry};
use tensorseed_verify::minisign::PublicKey;
use wasm_bindgen::prelude::*;

pub mod follow;
pub mod serving;
pub use follow::*;
pub use serving::*;

pub(crate) fn js_err(err: impl std::fmt::Display) -> JsError {
    JsError::new(&err.to_string())
}

pub(crate) fn to_json<T: Serialize>(v: &T) -> Result<String, JsError> {
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
    /// A signed head said this state is current, and the state has not
    /// changed since.
    fresh: std::cell::Cell<bool>,
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
        Ok(Self {
            state,
            pk,
            fresh: std::cell::Cell::new(false),
        })
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
        self.fresh.set(matches!(status, HeadStatus::UpToDate));
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
        self.fresh.set(false);
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
        self.fresh.set(false);
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

    /// Check a `ts.served-weights/1` claim against this verified catalog
    /// state: everything [`check_claim`] checks, plus the claimed manifest
    /// (signature, key, revision, files), takedowns, freshness (a signed
    /// head said this state is current), and the license mark recomputed
    /// from the catalog. Returns a `ts.claim-check/1` report as JSON.
    /// `options_json` is the same as for [`check_claim`].
    ///
    /// # Errors
    ///
    /// Malformed options or revocation list.
    pub fn check_claim(&self, claim_json: &str, options_json: &str) -> Result<String, JsError> {
        let opts = serving::parse_options(options_json)?;
        serving::report(claim_json, &opts, |c| self.catalog_match(c))
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

impl CatalogVerifier {
    fn catalog_match(&self, c: &tensorseed_verify::claim::ServedClaim) -> CatalogResult {
        let name = &self.state.catalog;
        if c.catalog.name != *name {
            return CatalogResult::Failed(check(
                "catalog",
                Status::Fail,
                format!(
                    "the claim names catalog {}, but this verifier holds {name}",
                    c.catalog.name
                ),
            ));
        }
        let fail = |n: &str, d: String| CatalogResult::Failed(check(n, Status::Fail, d));
        let repo = match RepoRef::new(c.repo_type, c.repo.clone()) {
            Ok(r) => r,
            Err(e) => return fail("manifest", format!("bad repo id {:?}: {e}", c.repo)),
        };
        let key = match repo.manifest_key(&c.revision) {
            Ok(k) => k,
            Err(_) => {
                return fail(
                    "manifest",
                    format!("revision {:?} is not a 40 hex commit", c.revision),
                );
            }
        };
        let Some(entry) = self.state.entries.get(&key) else {
            return fail(
                "manifest",
                format!("{}@{} is not in catalog {name}", c.repo, c.revision),
            );
        };
        let mut manifest = match verify_manifest_entry(entry, &self.pk, name) {
            Ok(m) => m,
            Err(e) => return fail("manifest", format!("the {name} entry does not verify: {e}")),
        };
        if manifest.file_pages > 0 {
            let mut pages = Vec::new();
            for i in 0..manifest.file_pages {
                let k = repo.file_page_key(&c.revision, i).unwrap_or_default();
                match self.state.entries.get(&k) {
                    Some(e) if e.verify(&self.pk, name).is_ok() => pages.push(e.value.clone()),
                    _ => return fail("manifest", format!("file page {k} is missing or invalid")),
                }
            }
            if let Err(e) = merge_file_pages(&mut manifest, &pages) {
                return fail("manifest", e.to_string());
            }
        }
        let tombstone = self
            .state
            .entries
            .get(&repo.tombstone_key())
            .filter(|e| e.verify(&self.pk, name).is_ok())
            .and_then(|e| serde_json::from_str::<Tombstone>(&e.value).ok());
        CatalogResult::Matched(Box::new(CatalogMatch {
            name: name.clone(),
            publisher_key_id: self.pk.key_id_hex(),
            manifest_sha256: tensorseed_core::hash::sha256_bytes(entry.value.as_bytes()),
            manifest_key: key,
            manifest,
            tombstone,
            freshness: Freshness {
                synced: self.fresh.get(),
                revision: Some(self.state.revision),
                chain_check: "off".into(),
                chain: None,
            },
        }))
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

/// Commercial serving mark for a license id (`license_spdx` of an index
/// entry, or a manifest's `license.spdx`), with the publisher's license
/// class when known (`open`, `attribution`, `restricted`, `unknown`).
/// Returns `{"value", "license", "note"}` as JSON; `value` is `allowed`,
/// `restricted`, `forbidden`, or `unknown`. Derived on the client; not legal
/// advice ([`commercial_disclaimer`]).
///
/// # Errors
///
/// Only if the result cannot be serialized.
#[wasm_bindgen]
pub fn commercial_serving(
    license: Option<String>,
    class: Option<String>,
) -> Result<String, JsError> {
    let class = class
        .as_deref()
        .and_then(|c| serde_json::from_value(serde_json::Value::String(c.to_string())).ok());
    to_json(&tensorseed_core::commercial::assess(
        license.as_deref(),
        class,
    ))
}

/// The "not legal advice" text shown next to commercial serving marks.
#[wasm_bindgen]
pub fn commercial_disclaimer() -> String {
    tensorseed_core::commercial::DISCLAIMER.to_string()
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

    #[test]
    fn commercial_serving_json() {
        let v: serde_json::Value =
            serde_json::from_str(&commercial_serving(Some("Apache-2.0".into()), None).unwrap())
                .unwrap();
        assert_eq!(v["value"], "allowed");
        let v: serde_json::Value = serde_json::from_str(
            &commercial_serving(Some("vendor-terms".into()), Some("attribution".into())).unwrap(),
        )
        .unwrap();
        assert_eq!(v["value"], "unknown");
        assert!(v["note"].as_str().unwrap().contains("attribution"));
        assert!(commercial_disclaimer().contains("not legal advice"));
    }
}
