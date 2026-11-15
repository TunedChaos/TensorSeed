// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Signed follow lists (`ts.follow-list/1`) in WebAssembly. The checks run
//! in [`tensorseed_verify::follow_list`].

use serde::Deserialize;
use tensorseed_core::hash::sha256_bytes;
use tensorseed_verify::Error;
use tensorseed_verify::follow_list::{Seen, check_update, verify_list};
use tensorseed_verify::minisign::PublicKey;
use wasm_bindgen::prelude::*;

use crate::{js_err, to_json};

/// Options for [`verify_follow_list`] (JSON, every field optional).
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FollowOptions {
    /// The highest revision accepted before.
    seen_rev: Option<u64>,
    /// SHA-256 of the bytes accepted at `seen_rev`.
    seen_sha256: Option<String>,
}

/// Verify a follow list: the exact bytes, its minisign signature, and the
/// signer key the caller pinned (never a key from the list).
///
/// `options_json` (every field optional): `seen_rev` and `seen_sha256`,
/// the revision accepted last, so a lower revision (`rollback`) or other
/// bytes under the same revision (`fork`) are refused.
///
/// Returns JSON: `{ valid: true, name, rev, issued_at, sha256, update,
/// catalogs }` with `update` one of `new`, `same`, `newer`, or
/// `{ valid: false, error, detail }` with `error` one of `signature`,
/// `invalid`, `rollback`, `fork`.
///
/// # Errors
///
/// `pubkey` or `options_json` is malformed.
#[wasm_bindgen]
pub fn verify_follow_list(
    body: &[u8],
    sig: &str,
    pubkey: &str,
    options_json: &str,
) -> Result<String, JsError> {
    let o: FollowOptions = if options_json.trim().is_empty() {
        FollowOptions::default()
    } else {
        serde_json::from_str(options_json).map_err(|e| js_err(format!("options: {e}")))?
    };
    let pk = PublicKey::parse(pubkey).map_err(js_err)?;
    let seen = match (o.seen_rev, o.seen_sha256) {
        (Some(rev), Some(sha256)) => Some(Seen { rev, sha256 }),
        (None, None) => None,
        _ => {
            return Err(js_err(
                "options: give both seen_rev and seen_sha256, or neither",
            ));
        }
    };
    let fail = |error: &str, e: &Error| serde_json::json!({ "valid": false, "error": error, "detail": e.to_string() });
    let list = match verify_list(&pk, body, sig) {
        Ok(l) => l,
        Err(e @ Error::Signature(_)) => return to_json(&fail("signature", &e)),
        Err(e) => return to_json(&fail("invalid", &e)),
    };
    let update = match check_update(seen.as_ref(), &list, body) {
        Ok(u) => u,
        Err(e @ Error::Rollback { .. }) => return to_json(&fail("rollback", &e)),
        Err(e) => return to_json(&fail("fork", &e)),
    };
    to_json(&serde_json::json!({
        "valid": true,
        "name": list.name,
        "rev": list.rev,
        "issued_at": list.issued_at,
        "sha256": sha256_bytes(body),
        "update": update,
        "catalogs": list.catalogs,
    }))
}
