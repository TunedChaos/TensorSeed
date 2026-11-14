// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Signed responses (`ts.response-sig/1`, docs/plans/verified-serving.md
//! section 5.1).
//!
//! [`signing_message`] is the canonical digest over the claim hash, request
//! id, served model id, timestamp, and body hash. [`verify_fields`] checks a
//! response against a claim's Ed25519 response key. Streaming (SSE)
//! responses carry the same fields in a final SSE comment line
//! ([`sse_trailer`], [`split_sse_trailer`]); [`extract_signed`] reads either
//! form. Signing lives with the seller's key in `tensorseed-app`; this
//! module only verifies.

use ed25519_dalek::Verifier;
use serde::{Deserialize, Serialize};
use tensorseed_core::hash::{is_lower_hex, sha256_bytes};

/// First line of the response signing message.
pub const RESPONSE_SIG_SCHEMA: &str = "ts.response-sig/1";
/// Proposed default timestamp skew window (seconds).
pub const DEFAULT_MAX_SKEW_SECS: u64 = 300;

/// Request and response header: request id.
pub const H_REQUEST_ID: &str = "x-tensorseed-request-id";
/// Response header: claim hash.
pub const H_CLAIM: &str = "x-tensorseed-claim-sha256";
/// Response header: served model id.
pub const H_MODEL: &str = "x-tensorseed-served-model";
/// Response header: signing time.
pub const H_TIMESTAMP: &str = "x-tensorseed-timestamp";
/// Response header: body hash.
pub const H_BODY: &str = "x-tensorseed-body-sha256";
/// Response header: signature.
pub const H_SIGNATURE: &str = "x-tensorseed-signature";
/// Response header on streams: where the signature is (`sse-trailer`).
pub const H_SIGNATURE_MODE: &str = "x-tensorseed-signature-mode";
/// Value of [`H_SIGNATURE_MODE`] for SSE.
pub const MODE_SSE_TRAILER: &str = "sse-trailer";

/// The canonical `ts.response-sig/1` message (section 5.1): fixed field
/// order, a newline after every line including the last.
#[must_use]
pub fn signing_message(
    claim_sha256: &str,
    request_id: &str,
    served_model_id: &str,
    timestamp: &str,
    body_sha256: &str,
) -> String {
    format!(
        "{RESPONSE_SIG_SCHEMA}\nclaim_sha256={claim_sha256}\nrequest_id={request_id}\nserved_model_id={served_model_id}\ntimestamp={timestamp}\nbody_sha256={body_sha256}\n"
    )
}

/// True for a request id that can go into the message: 1 to 128 of
/// `A-Z a-z 0-9 . _ ~ : -` (UUIDs and hex fit).
#[must_use]
pub fn valid_request_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._~:-".contains(&b))
}

/// True for a served model id that can go into the message: 1 to 256
/// printable ASCII characters, no spaces.
#[must_use]
pub fn valid_model_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 256 && s.bytes().all(|b| b.is_ascii_graphic())
}

/// The signed fields of one response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedFields {
    /// `X-Tensorseed-Claim-SHA256`.
    pub claim_sha256: String,
    /// `X-Tensorseed-Request-Id`.
    pub request_id: String,
    /// `X-Tensorseed-Served-Model`.
    pub served_model_id: String,
    /// `X-Tensorseed-Timestamp`.
    pub timestamp: String,
    /// `X-Tensorseed-Body-SHA256`.
    pub body_sha256: String,
    /// `X-Tensorseed-Signature`.
    pub signature: String,
}

impl SignedFields {
    /// The six headers, lowercase names.
    #[must_use]
    pub fn headers(&self) -> [(&'static str, &str); 6] {
        [
            (H_CLAIM, &self.claim_sha256),
            (H_REQUEST_ID, &self.request_id),
            (H_MODEL, &self.served_model_id),
            (H_TIMESTAMP, &self.timestamp),
            (H_BODY, &self.body_sha256),
            (H_SIGNATURE, &self.signature),
        ]
    }
}

/// Why a response signature did not verify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SigError {
    /// No signature headers at all.
    Absent,
    /// A required header is missing.
    Missing(&'static str),
    /// A header value is malformed.
    Malformed(&'static str),
    /// The response names a different claim.
    ClaimMismatch,
    /// The request id is not the one sent.
    RequestId,
    /// The served model is not the claimed or requested one.
    Model(String),
    /// The timestamp is outside the window.
    Skew(i64),
    /// The body does not hash to the signed value.
    BodyHash,
    /// The signature does not verify with the claim's key.
    BadSignature,
    /// The response was signed after the claim's `expires_at`.
    ClaimExpired,
    /// The claim was revoked before the response was signed (or with
    /// reason `key_compromised`, at any time).
    ClaimRevoked,
}

impl SigError {
    /// Short machine label.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Missing(_) => "missing_header",
            Self::Malformed(_) => "malformed_header",
            Self::ClaimMismatch => "claim_mismatch",
            Self::RequestId => "request_id_mismatch",
            Self::Model(_) => "model_mismatch",
            Self::Skew(_) => "timestamp_skew",
            Self::BodyHash => "body_hash_mismatch",
            Self::BadSignature => "bad_signature",
            Self::ClaimExpired => "claim_expired",
            Self::ClaimRevoked => "claim_revoked",
        }
    }
}

impl std::fmt::Display for SigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Absent => write!(f, "the response is not signed"),
            Self::Missing(h) => write!(f, "missing header {h}"),
            Self::Malformed(h) => write!(f, "malformed header {h}"),
            Self::ClaimMismatch => write!(f, "the response names a different claim"),
            Self::RequestId => write!(f, "the request id is not the one sent"),
            Self::Model(m) => write!(
                f,
                "served model {m:?} is not the claimed or requested model"
            ),
            Self::Skew(s) => write!(f, "timestamp is {s} seconds off, outside the window"),
            Self::BodyHash => write!(f, "the body does not match the signed body hash"),
            Self::BadSignature => write!(
                f,
                "the signature does not verify with the claim's response key"
            ),
            Self::ClaimExpired => write!(
                f,
                "the response was signed after the claim expired; fetch the seller's current claim"
            ),
            Self::ClaimRevoked => {
                write!(f, "the claim was revoked before this response was signed")
            }
        }
    }
}

impl std::error::Error for SigError {}

/// Read the signed fields from response headers (case-insensitive names).
/// For an SSE trailer, call [`split_sse_trailer`] and merge.
///
/// # Errors
///
/// [`SigError::Absent`] when no signature header is present,
/// [`SigError::Missing`] when only some are.
pub fn fields_from_headers<'a>(
    headers: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<SignedFields, SigError> {
    let mut f = SignedFields {
        claim_sha256: String::new(),
        request_id: String::new(),
        served_model_id: String::new(),
        timestamp: String::new(),
        body_sha256: String::new(),
        signature: String::new(),
    };
    let mut any = false;
    for (k, v) in headers {
        let v = v.trim().to_string();
        match k.to_ascii_lowercase().as_str() {
            H_CLAIM => f.claim_sha256 = v,
            H_REQUEST_ID => f.request_id = v,
            H_MODEL => f.served_model_id = v,
            H_TIMESTAMP => f.timestamp = v,
            H_BODY => f.body_sha256 = v,
            H_SIGNATURE => {
                any = true;
                f.signature = v;
            }
            _ => {}
        }
    }
    if !any && f.claim_sha256.is_empty() && f.body_sha256.is_empty() {
        return Err(SigError::Absent);
    }
    for (name, v) in [
        (H_CLAIM, &f.claim_sha256),
        (H_REQUEST_ID, &f.request_id),
        (H_MODEL, &f.served_model_id),
        (H_TIMESTAMP, &f.timestamp),
        (H_BODY, &f.body_sha256),
        (H_SIGNATURE, &f.signature),
    ] {
        if v.is_empty() {
            return Err(SigError::Missing(name));
        }
    }
    Ok(f)
}

/// What a verifier expects.
#[derive(Debug, Clone)]
pub struct Expect<'a> {
    /// `response_key.pubkey` from the claim.
    pub pubkey: [u8; 32],
    /// `claim_sha256` of that claim.
    pub claim_sha256: &'a str,
    /// The claim's `served_model_id`.
    pub served_model_id: &'a str,
    /// The request id that was sent, if any (required echo).
    pub request_id: Option<&'a str>,
    /// The model id that was requested, if known.
    pub requested_model: Option<&'a str>,
    /// Current time (Unix seconds).
    pub now_unix: i64,
    /// Allowed skew either way (seconds).
    pub max_skew_secs: u64,
    /// The claim's `expires_at` (Unix seconds), if it has one. A response
    /// signed after it fails with [`SigError::ClaimExpired`]. See
    /// [`crate::claim::ServedClaim::expires_unix`].
    pub claim_expires_unix: Option<i64>,
    /// When a verified revocation of the claim takes effect (Unix seconds,
    /// `i64::MIN` for `key_compromised`). A response signed at or after it
    /// fails with [`SigError::ClaimRevoked`]. See
    /// [`crate::revocation::Revocations::revoked_from`].
    pub claim_revoked_unix: Option<i64>,
}

/// Parse an RFC 3339 timestamp to Unix seconds.
#[must_use]
pub fn parse_rfc3339(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.timestamp())
}

/// Check a signed response (section 5.1, steps 2 to 6). Replay detection
/// (step 7) needs state and is left to the caller.
///
/// # Errors
///
/// The first check that fails, as a [`SigError`].
pub fn verify_fields(f: &SignedFields, body: &[u8], e: &Expect<'_>) -> Result<(), SigError> {
    if !is_lower_hex(&f.claim_sha256, 64) {
        return Err(SigError::Malformed(H_CLAIM));
    }
    if !is_lower_hex(&f.body_sha256, 64) {
        return Err(SigError::Malformed(H_BODY));
    }
    if !is_lower_hex(&f.signature, 128) {
        return Err(SigError::Malformed(H_SIGNATURE));
    }
    if !valid_request_id(&f.request_id) {
        return Err(SigError::Malformed(H_REQUEST_ID));
    }
    if !valid_model_id(&f.served_model_id) {
        return Err(SigError::Malformed(H_MODEL));
    }
    if f.claim_sha256 != e.claim_sha256 {
        return Err(SigError::ClaimMismatch);
    }
    if f.served_model_id != e.served_model_id
        || e.requested_model.is_some_and(|m| m != f.served_model_id)
    {
        return Err(SigError::Model(f.served_model_id.clone()));
    }
    if let Some(id) = e.request_id
        && id != f.request_id
    {
        return Err(SigError::RequestId);
    }
    let ts = parse_rfc3339(&f.timestamp).ok_or(SigError::Malformed(H_TIMESTAMP))?;
    let skew = ts - e.now_unix;
    if skew.unsigned_abs() > e.max_skew_secs {
        return Err(SigError::Skew(skew));
    }
    if sha256_bytes(body) != f.body_sha256 {
        return Err(SigError::BodyHash);
    }
    let msg = signing_message(
        &f.claim_sha256,
        &f.request_id,
        &f.served_model_id,
        &f.timestamp,
        &f.body_sha256,
    );
    let vk =
        ed25519_dalek::VerifyingKey::from_bytes(&e.pubkey).map_err(|_| SigError::BadSignature)?;
    let sig_bytes: [u8; 64] = hex::decode(&f.signature)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or(SigError::Malformed(H_SIGNATURE))?;
    let sig = ed25519_dalek::Signature::from_bytes(&sig_bytes);
    vk.verify(msg.as_bytes(), &sig)
        .map_err(|_| SigError::BadSignature)?;
    // Expiry and revocation are reported only for responses the key really
    // signed, so a forged response never reads as "expired".
    if e.claim_expires_unix.is_some_and(|x| ts > x) {
        return Err(SigError::ClaimExpired);
    }
    if e.claim_revoked_unix.is_some_and(|r| ts >= r) {
        return Err(SigError::ClaimRevoked);
    }
    Ok(())
}

/// The SSE comment line that closes a signed stream. Request id, claim,
/// and model travel in the response headers; this line carries the rest.
#[must_use]
pub fn sse_trailer(f: &SignedFields) -> String {
    format!(
        ": {RESPONSE_SIG_SCHEMA} timestamp={} body_sha256={} signature={}\n\n",
        f.timestamp, f.body_sha256, f.signature
    )
}

/// Fields of an SSE trailer line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseTrailer {
    /// Signing time.
    pub timestamp: String,
    /// Hash of every stream byte before the trailer line.
    pub body_sha256: String,
    /// Signature.
    pub signature: String,
}

/// Split a complete signed SSE stream into the signed bytes and the
/// trailer. Returns `None` when the stream does not end with a trailer.
#[must_use]
pub fn split_sse_trailer(raw: &[u8]) -> Option<(&[u8], SseTrailer)> {
    let marker = format!(": {RESPONSE_SIG_SCHEMA} ");
    let m = marker.as_bytes();
    let start = raw.windows(m.len()).rposition(|w| w == m)?;
    if start > 0 && raw[start - 1] != b'\n' && raw[start - 1] != b'\r' {
        return None;
    }
    let line = std::str::from_utf8(&raw[start + m.len()..]).ok()?;
    let line = line.trim_end_matches(['\r', '\n']);
    if line.contains('\n') {
        return None;
    }
    let mut t = SseTrailer {
        timestamp: String::new(),
        body_sha256: String::new(),
        signature: String::new(),
    };
    for part in line.split(' ') {
        let (k, v) = part.split_once('=')?;
        match k {
            "timestamp" => t.timestamp = v.into(),
            "body_sha256" => t.body_sha256 = v.into(),
            "signature" => t.signature = v.into(),
            _ => return None,
        }
    }
    if t.timestamp.is_empty() || t.body_sha256.is_empty() || t.signature.is_empty() {
        return None;
    }
    Some((&raw[..start], t))
}

/// Read the signed fields of a whole response, either from the headers
/// or, when `X-Tensorseed-Signature-Mode: sse-trailer`, from the headers
/// plus the closing SSE trailer. Returns the fields and the bytes the
/// signature covers.
///
/// # Errors
///
/// [`SigError::Absent`] when the response is not signed, or the first
/// missing or malformed piece.
pub fn extract_signed<'b>(
    headers: &[(String, String)],
    body: &'b [u8],
) -> Result<(SignedFields, &'b [u8]), SigError> {
    let get = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.trim().to_string())
    };
    match get(H_SIGNATURE_MODE).as_deref() {
        None => {
            let f = fields_from_headers(headers.iter().map(|(k, v)| (k.as_str(), v.as_str())))?;
            Ok((f, body))
        }
        Some(MODE_SSE_TRAILER) => {
            let (signed, t) = split_sse_trailer(body).ok_or(SigError::Missing("sse trailer"))?;
            let f = SignedFields {
                claim_sha256: get(H_CLAIM).ok_or(SigError::Missing(H_CLAIM))?,
                request_id: get(H_REQUEST_ID).ok_or(SigError::Missing(H_REQUEST_ID))?,
                served_model_id: get(H_MODEL).ok_or(SigError::Missing(H_MODEL))?,
                timestamp: t.timestamp,
                body_sha256: t.body_sha256,
                signature: t.signature,
            };
            Ok((f, signed))
        }
        Some(_) => Err(SigError::Malformed(H_SIGNATURE_MODE)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claim::fixtures::{claim, response_key};
    use ed25519_dalek::Signer;

    fn signed(ts: &str, body: &[u8]) -> SignedFields {
        let c = claim();
        let body_sha256 = sha256_bytes(body);
        let msg = signing_message(&c.claim_sha256, "r1", "org/tiny", ts, &body_sha256);
        SignedFields {
            claim_sha256: c.claim_sha256,
            request_id: "r1".into(),
            served_model_id: "org/tiny".into(),
            timestamp: ts.into(),
            body_sha256,
            signature: hex::encode(response_key().sign(msg.as_bytes()).to_bytes()),
        }
    }

    #[test]
    fn expiry_and_revocation_apply_to_genuine_responses() {
        let c = claim();
        let ts = "2026-11-20T00:00:00Z";
        let t = parse_rfc3339(ts).unwrap();
        let f = signed(ts, b"{}");
        let pubkey = c.response_pubkey().unwrap();
        let e = |exp: Option<i64>, rev: Option<i64>| Expect {
            pubkey,
            claim_sha256: &c.claim_sha256,
            served_model_id: &c.served_model_id,
            request_id: Some("r1"),
            requested_model: None,
            now_unix: t,
            max_skew_secs: DEFAULT_MAX_SKEW_SECS,
            claim_expires_unix: exp,
            claim_revoked_unix: rev,
        };
        verify_fields(&f, b"{}", &e(None, None)).unwrap();
        verify_fields(&f, b"{}", &e(Some(t), None)).unwrap();
        assert_eq!(
            verify_fields(&f, b"{}", &e(Some(t - 1), None)),
            Err(SigError::ClaimExpired)
        );
        verify_fields(&f, b"{}", &e(None, Some(t + 1))).unwrap();
        assert_eq!(
            verify_fields(&f, b"{}", &e(None, Some(t))),
            Err(SigError::ClaimRevoked)
        );
        assert_eq!(
            verify_fields(&f, b"{}", &e(None, Some(i64::MIN))),
            Err(SigError::ClaimRevoked)
        );
        // A forged response reports the bad signature, not the expiry.
        assert_eq!(
            verify_fields(&f, b"{\"x\":1}", &e(Some(t - 1), None)),
            Err(SigError::BodyHash)
        );
        let mut forged = f.clone();
        forged.signature = "0".repeat(128);
        assert_eq!(
            verify_fields(&forged, b"{}", &e(Some(t - 1), None)),
            Err(SigError::BadSignature)
        );
        assert_eq!(SigError::ClaimExpired.label(), "claim_expired");
        assert_eq!(SigError::ClaimRevoked.label(), "claim_revoked");
    }
}
