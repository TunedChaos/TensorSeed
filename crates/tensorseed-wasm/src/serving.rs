// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Verified serving in WebAssembly: claims (`ts.served-weights/1`),
//! revocations (`ts.claim-revocations/1`), and signed responses
//! (`ts.response-sig/1`). Every check runs in
//! [`tensorseed_verify`]; these functions only convert JSON in and out.

use serde::Deserialize;
use tensorseed_verify::claim::{ServedClaim, WalletStatus, claim_message, parse_claim};
use tensorseed_verify::claim_check::{
    CatalogResult, ClaimCheckOptions, Origin, RevocationInput, build_report, unparsable_report,
};
use tensorseed_verify::response::{DEFAULT_MAX_SKEW_SECS, Expect, extract_signed, verify_fields};
use tensorseed_verify::revocation::{Revocations, revocation_message};
use wasm_bindgen::prelude::*;

use crate::{js_err, to_json};

/// Options for [`check_claim`] and `CatalogVerifier.check_claim` (JSON,
/// every field optional).
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct CheckOptions {
    /// The URL the claim was fetched from (after redirects).
    origin_url: Option<String>,
    /// A label for a claim that did not come from a URL.
    origin_file: Option<String>,
    /// For a claim not fetched from a URL: the public base URL it must
    /// name.
    expect_url: Option<String>,
    require_wallet_sig: bool,
    expect_wallet: Option<String>,
    require_expiry: bool,
    /// Current time, Unix seconds.
    now_unix: f64,
    /// A `ts.claim-revocations/1` document (object or JSON string), already
    /// merged from every list the caller fetched.
    revocations: Option<serde_json::Value>,
    /// Where the revocations came from (for the report).
    revocations_from: Option<String>,
    /// The URL the seller's list was looked for at, when it answered 404.
    revocations_absent: Option<String>,
    /// The seller's list could not be read. The revocation check warns and
    /// a verdict that would pass is inconclusive.
    revocations_error: Option<String>,
    /// The seller's list as fetched (object or JSON string), merged with
    /// `revocations`. Unlike `revocations`, one that does not parse is not
    /// an error: it counts as `revocations_error`.
    seller_revocations: Option<serde_json::Value>,
    /// Where the seller's list came from (for the report).
    seller_revocations_from: Option<String>,
}

fn secs(v: f64) -> i64 {
    if v.is_finite() { v as i64 } else { 0 }
}

pub(crate) fn parse_options(json: &str) -> Result<CheckOptions, JsError> {
    if json.trim().is_empty() {
        return Ok(CheckOptions::default());
    }
    serde_json::from_str(json).map_err(|e| js_err(format!("options: {e}")))
}

fn revocations_value(v: &serde_json::Value) -> Result<Revocations, JsError> {
    parse_revocations(v).map_err(js_err)
}

fn parse_revocations(v: &serde_json::Value) -> Result<Revocations, String> {
    let raw = match v {
        serde_json::Value::String(s) => s.as_bytes().to_vec(),
        other => serde_json::to_vec(other).map_err(|e| e.to_string())?,
    };
    Revocations::parse(&raw).map_err(|e| e.to_string())
}

impl CheckOptions {
    pub(crate) fn origin(&self) -> Origin {
        match (&self.origin_url, &self.origin_file) {
            (Some(u), _) => Origin::Url(u.clone()),
            (None, Some(f)) => Origin::File(f.clone()),
            (None, None) => Origin::File("(provided)".into()),
        }
    }

    pub(crate) fn source(&self) -> String {
        self.origin_url
            .clone()
            .or_else(|| self.origin_file.clone())
            .unwrap_or_else(|| "(provided)".into())
    }

    pub(crate) fn claim_options(&self) -> ClaimCheckOptions {
        ClaimCheckOptions {
            require_wallet_sig: self.require_wallet_sig,
            expect_wallet: self.expect_wallet.clone(),
            expect_url: self.expect_url.clone(),
            require_fresh: false,
            require_expiry: self.require_expiry,
            now_unix: secs(self.now_unix),
        }
    }

    pub(crate) fn revocation_input(&self) -> Result<RevocationInput, JsError> {
        let mut list = None;
        let mut from = Vec::new();
        if let Some(v) = &self.revocations {
            list = Some(revocations_value(v)?);
            from.push(
                self.revocations_from
                    .clone()
                    .unwrap_or_else(|| "the provided list".into()),
            );
        }
        let mut error = self.revocations_error.clone();
        if let Some(v) = &self.seller_revocations {
            let label = self
                .seller_revocations_from
                .clone()
                .unwrap_or_else(|| "the seller's list".into());
            match parse_revocations(v) {
                Ok(l) => {
                    list.get_or_insert_with(Revocations::default).merge(l);
                    from.push(label);
                }
                Err(e) => error = Some(format!("{label}: {e}")),
            }
        }
        Ok(match (list, error) {
            (Some(list), incomplete) => RevocationInput::List {
                list,
                from: from.join(", "),
                incomplete,
            },
            (None, Some(e)) => RevocationInput::Unavailable(e),
            (None, None) => match &self.revocations_absent {
                Some(u) => RevocationInput::Absent(u.clone()),
                None => RevocationInput::NotChecked(
                    "no revocation list was provided; fetch the seller's list next to the claim"
                        .into(),
                ),
            },
        })
    }
}

/// Run the claim checks against `catalog` and return the report JSON.
pub(crate) fn report(
    claim_json: &str,
    opts: &CheckOptions,
    catalog: impl FnOnce(&ServedClaim) -> CatalogResult,
) -> Result<String, JsError> {
    let origin = opts.origin();
    let source = opts.source();
    let rep = match parse_claim(claim_json.as_bytes()) {
        Ok(c) => {
            let cat = catalog(&c);
            build_report(
                &source,
                &origin,
                &c,
                &cat,
                &opts.revocation_input()?,
                &opts.claim_options(),
            )
        }
        Err(e) => unparsable_report(&source, &origin, &e.to_string()),
    };
    to_json(&rep)
}

/// Check a `ts.served-weights/1` claim without a catalog: hash, fields,
/// wallet signature, expiry, where it was fetched, and revocations.
/// Returns a `ts.claim-check/1` report as JSON. Without a catalog the best
/// verdict is `inconclusive`; use `CatalogVerifier.check_claim` for the
/// full check.
///
/// `options_json` (every field optional): `origin_url`, `origin_file`,
/// `expect_url`, `require_wallet_sig`, `expect_wallet`, `require_expiry`,
/// `now_unix`, `revocations` (a `ts.claim-revocations/1` object or string),
/// `revocations_from`, `revocations_absent`, `revocations_error`.
///
/// # Errors
///
/// Malformed options or revocation list (a bad claim is a failed report,
/// not an error).
#[wasm_bindgen]
pub fn check_claim(claim_json: &str, options_json: &str) -> Result<String, JsError> {
    let opts = parse_options(options_json)?;
    report(claim_json, &opts, |_| {
        CatalogResult::Unavailable(
            "no catalog was given; use CatalogVerifier.check_claim to match the claim to a catalog you trust".into(),
        )
    })
}

/// Options for [`verify_response`].
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ResponseOptions {
    request_id: Option<String>,
    requested_model: Option<String>,
    now_unix: f64,
    max_skew_secs: Option<u64>,
    revocations: Option<serde_json::Value>,
}

/// Headers as an object (`{"name": "value"}`) or a list of pairs.
fn headers_from(json: &str) -> Result<Vec<(String, String)>, JsError> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| js_err(format!("headers: {e}")))?;
    let mut out = Vec::new();
    match v {
        serde_json::Value::Object(m) => {
            for (k, v) in m {
                if let Some(s) = v.as_str() {
                    out.push((k, s.to_string()));
                }
            }
        }
        serde_json::Value::Array(a) => {
            for p in a {
                if let (Some(k), Some(v)) = (
                    p.get(0).and_then(|x| x.as_str()),
                    p.get(1).and_then(|x| x.as_str()),
                ) {
                    out.push((k.to_string(), v.to_string()));
                }
            }
        }
        _ => return Err(js_err("headers must be an object or a list of pairs")),
    }
    Ok(out)
}

/// Verify a signed response against a claim. `headers_json` is an object
/// or a list of `[name, value]` pairs; `body` is the exact bytes received
/// (for streams, the whole stream including the closing signature line).
///
/// Returns JSON `{ valid, error, detail, claim_sha256, served_model_id,
/// request_id, timestamp, body_sha256, seller_wallet, claim_expires_at }`.
/// `error` is a stable label (`absent`, `bad_signature`, `body_hash_mismatch`,
/// `timestamp_skew`, `claim_expired`, `claim_revoked`, and so on).
///
/// `options_json` (every field optional): `request_id`, `requested_model`,
/// `now_unix`, `max_skew_secs` (default 300), `revocations`.
///
/// # Errors
///
/// The claim itself does not verify, or the inputs are malformed.
#[wasm_bindgen]
pub fn verify_response(
    claim_json: &str,
    headers_json: &str,
    body: &[u8],
    options_json: &str,
) -> Result<String, JsError> {
    let o: ResponseOptions = if options_json.trim().is_empty() {
        ResponseOptions::default()
    } else {
        serde_json::from_str(options_json).map_err(|e| js_err(format!("options: {e}")))?
    };
    let claim = parse_claim(claim_json.as_bytes()).map_err(js_err)?;
    let check = claim.verify().map_err(js_err)?;
    let pubkey = claim.response_pubkey().map_err(js_err)?;
    let revoked = match &o.revocations {
        Some(v) => revocations_value(v)?.revoked_from(&claim),
        None => None,
    };
    let headers = headers_from(headers_json)?;
    let result = extract_signed(&headers, body).and_then(|(f, signed)| {
        let e = Expect {
            pubkey,
            claim_sha256: &claim.claim_sha256,
            served_model_id: &claim.served_model_id,
            request_id: o.request_id.as_deref(),
            requested_model: o.requested_model.as_deref(),
            now_unix: secs(o.now_unix),
            max_skew_secs: o.max_skew_secs.unwrap_or(DEFAULT_MAX_SKEW_SECS),
            claim_expires_unix: claim.expires_unix().ok().flatten(),
            claim_revoked_unix: revoked,
        };
        verify_fields(&f, signed, &e).map(|()| f)
    });
    let wallet = match &check.wallet {
        WalletStatus::Verified(a) => Some(a.clone()),
        WalletStatus::NotSigned => None,
    };
    to_json(&match result {
        Ok(f) => serde_json::json!({
            "valid": true,
            "claim_sha256": claim.claim_sha256,
            "served_model_id": f.served_model_id,
            "request_id": f.request_id,
            "timestamp": f.timestamp,
            "body_sha256": f.body_sha256,
            "seller_wallet": wallet,
            "claim_expires_at": claim.expires_at,
        }),
        Err(e) => serde_json::json!({
            "valid": false,
            "error": e.label(),
            "detail": e.to_string(),
            "claim_sha256": claim.claim_sha256,
            "seller_wallet": wallet,
            "claim_expires_at": claim.expires_at,
        }),
    })
}

/// The exact EIP-191 message a seller wallet signs for a claim hash.
#[wasm_bindgen]
pub fn claim_signing_message(claim_sha256: &str) -> String {
    claim_message(claim_sha256)
}

/// The exact message a revocation's signatures cover.
#[wasm_bindgen]
pub fn revocation_signing_message(claim_sha256: &str, revoked_at: &str, reason: &str) -> String {
    revocation_message(claim_sha256, revoked_at, reason)
}

/// The address (`0x` and 40 lowercase hex) that signed `message` with
/// EIP-191 `personal_sign`.
///
/// # Errors
///
/// The signature is malformed or does not recover.
#[wasm_bindgen]
pub fn recover_wallet(message: &str, signature: &str) -> Result<String, JsError> {
    tensorseed_verify::claim::recover_eip191(message.as_bytes(), signature).map_err(js_err)
}
