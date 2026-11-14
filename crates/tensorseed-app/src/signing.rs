// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Signed responses and the signed served-weights claim
//! (docs/plans/verified-serving.md, sections 3 and 5.1).
//!
//! - [`ResponseKey`]: the seller's Ed25519 response key, kept in a file
//!   only the owner can read. The secret is never printed, logged, or taken
//!   on a command line; `Debug` shows only the public key.
//! - [`ServedClaim`]: `ts.served-weights/1`. `claim_sha256` is the SHA-256
//!   of the canonical JSON (SPEC 6.4) of the claim without `claim_sha256`,
//!   `sig`, and `signed`. The seller wallet may sign [`claim_message`] with
//!   EIP-191 outside Tensorseed; [`recover_eip191`] verifies it. Tensorseed
//!   holds no wallet key.
//! - [`signing_message`]: the canonical `ts.response-sig/1` digest over the
//!   claim hash, request id, served model id, timestamp, and body hash.
//!   [`ResponseKey::sign_response`] signs it; [`verify_fields`] checks a
//!   response against a claim's key.
//! - Streaming (SSE) responses carry the same fields in a final SSE comment
//!   line ([`sse_trailer`], [`split_sse_trailer`]), which every SSE parser
//!   ignores.

use std::path::Path;

use anyhow::{Context, bail};
use ed25519_dalek::{Signer, Verifier};
use serde::{Deserialize, Serialize};
use sha3::Digest as _;
use tensorseed_core::canonical::to_canonical_vec;
use tensorseed_core::hash::{is_lower_hex, sha256_bytes};
use tensorseed_core::keys::RepoType;

use crate::inference::{
    Entrypoint, Runtime, SERVED_WEIGHTS_SCHEMA, ServedCatalog, ServedLicense, ServedWeights,
    VerifiedFile,
};

/// Schema of the response key file.
pub const RESPONSE_KEY_SCHEMA: &str = "ts.response-key/1";
/// Default response key file name (in the Tensorseed config directory).
pub const RESPONSE_KEY_FILE: &str = "response-key.json";
/// Schema of the signed claim.
pub const CLAIM_SCHEMA: &str = "ts.served-weights/1";
/// First line of the response signing message.
pub const RESPONSE_SIG_SCHEMA: &str = "ts.response-sig/1";
/// Prefix of the EIP-191 claim message.
pub const CLAIM_MESSAGE_PREFIX: &str = "tensorseed served-weights/1:\n";
/// Path where the signing proxy publishes the claim.
pub const WELL_KNOWN_CLAIM: &str = "/.well-known/tensorseed/served-weights.json";
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

// ---------------------------------------------------------------------------
// Response key
// ---------------------------------------------------------------------------

/// The seller's Ed25519 response signing key.
pub struct ResponseKey {
    key: ed25519_dalek::SigningKey,
}

impl std::fmt::Debug for ResponseKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResponseKey")
            .field("pubkey", &self.pubkey_hex())
            .finish_non_exhaustive()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyFile {
    schema: String,
    alg: String,
    /// 32-byte Ed25519 seed, 64 lowercase hex. Secret.
    secret: String,
    pubkey: String,
    created_at: String,
}

impl ResponseKey {
    /// A new key from the OS random source.
    ///
    /// # Errors
    ///
    /// Fails when the random source fails.
    pub fn generate() -> anyhow::Result<Self> {
        let seed = crate::challenge::random_32()?;
        Ok(Self::from_seed(&seed))
    }

    /// A key from a 32-byte seed (tests and fixed vectors).
    #[must_use]
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Self {
            key: ed25519_dalek::SigningKey::from_bytes(seed),
        }
    }

    /// Public key bytes.
    #[must_use]
    pub fn pubkey(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    /// Public key, 64 lowercase hex.
    #[must_use]
    pub fn pubkey_hex(&self) -> String {
        hex::encode(self.pubkey())
    }

    /// Write a new key file readable only by the owner (mode 0600 on Unix).
    /// Refuses to replace an existing file unless `force`.
    ///
    /// # Errors
    ///
    /// Fails when the file exists (without `force`) or cannot be written.
    pub fn save_new(&self, path: &Path, force: bool) -> anyhow::Result<()> {
        if path.exists() && !force {
            bail!(
                "{} already exists; a new key needs a new claim. Pass --force to replace it",
                path.display()
            );
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let kf = KeyFile {
            schema: RESPONSE_KEY_SCHEMA.into(),
            alg: "ed25519".into(),
            secret: hex::encode(self.key.to_bytes()),
            pubkey: self.pubkey_hex(),
            created_at: tensorseed_core::time::now_rfc3339(),
        };
        let mut data = serde_json::to_vec_pretty(&kf)?;
        data.push(b'\n');
        // Write a sibling temp file with owner-only permissions, then rename.
        let tmp = path.with_extension("json.tmp");
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        {
            let mut f = o
                .open(&tmp)
                .with_context(|| format!("writing {}", tmp.display()))?;
            std::io::Write::write_all(&mut f, &data)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// Load a key file. On Unix the file must not be readable by group or
    /// others.
    ///
    /// # Errors
    ///
    /// Fails when the file is missing, too open, or malformed.
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let md = std::fs::metadata(path).with_context(|| {
                format!(
                    "reading {}; create one with `tensorseed response-key create`",
                    path.display()
                )
            })?;
            if md.permissions().mode() & 0o077 != 0 {
                bail!(
                    "{} is readable by other users; run chmod 600 on it (or create a new key)",
                    path.display()
                );
            }
        }
        let raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let kf: KeyFile = serde_json::from_slice(&raw).map_err(|_| {
            anyhow::anyhow!("{} is not a {RESPONSE_KEY_SCHEMA} file", path.display())
        })?;
        if kf.schema != RESPONSE_KEY_SCHEMA || kf.alg != "ed25519" {
            bail!(
                "{} is not a {RESPONSE_KEY_SCHEMA} ed25519 key",
                path.display()
            );
        }
        let seed: [u8; 32] = hex::decode(&kf.secret)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| anyhow::anyhow!("{} has a malformed key", path.display()))?;
        let k = Self::from_seed(&seed);
        if k.pubkey_hex() != kf.pubkey {
            bail!("{} public key does not match its secret", path.display());
        }
        Ok(k)
    }

    /// Sign a response body.
    #[must_use]
    pub fn sign_response(
        &self,
        claim_sha256: &str,
        request_id: &str,
        served_model_id: &str,
        timestamp: &str,
        body: &[u8],
    ) -> SignedFields {
        self.sign_digest(
            claim_sha256,
            request_id,
            served_model_id,
            timestamp,
            &sha256_bytes(body),
        )
    }

    /// Sign with a precomputed body hash (streams).
    #[must_use]
    pub fn sign_digest(
        &self,
        claim_sha256: &str,
        request_id: &str,
        served_model_id: &str,
        timestamp: &str,
        body_sha256: &str,
    ) -> SignedFields {
        let msg = signing_message(
            claim_sha256,
            request_id,
            served_model_id,
            timestamp,
            body_sha256,
        );
        let sig = self.key.sign(msg.as_bytes());
        SignedFields {
            claim_sha256: claim_sha256.into(),
            request_id: request_id.into(),
            served_model_id: served_model_id.into(),
            timestamp: timestamp.into(),
            body_sha256: body_sha256.into(),
            signature: hex::encode(sig.to_bytes()),
        }
    }
}

// ---------------------------------------------------------------------------
// Response signatures
// ---------------------------------------------------------------------------

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

/// A fresh request id: 32 random bytes as 64 lowercase hex.
///
/// # Errors
///
/// Fails when the random source fails.
pub fn mint_request_id() -> anyhow::Result<String> {
    Ok(hex::encode(crate::challenge::random_32()?))
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
        .map_err(|_| SigError::BadSignature)
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

// ---------------------------------------------------------------------------
// The signed claim (`ts.served-weights/1`)
// ---------------------------------------------------------------------------

/// `response_key` in a claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResponseKeyRef {
    /// Always `ed25519`.
    pub alg: String,
    /// 32-byte public key, 64 lowercase hex.
    pub pubkey: String,
}

/// Whether the offer returns logprobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogprobsMode {
    /// Logprob-verified tier.
    Required,
    /// The engine cannot return logprobs.
    None,
}

/// Wallet signature on a claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimSig {
    /// `eip191`.
    pub alg: String,
    /// `0x` and 65 bytes as hex.
    pub value: String,
}

/// `ts.served-weights/1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServedClaim {
    /// [`CLAIM_SCHEMA`].
    pub schema: String,
    /// True only when `sig` is present (not part of `claim_sha256`).
    pub signed: bool,
    /// Catalog provenance.
    pub catalog: ServedCatalog,
    /// Repo type.
    #[serde(rename = "type")]
    pub repo_type: RepoType,
    /// Repo id.
    pub repo: String,
    /// Commit.
    pub revision: String,
    /// License facts.
    pub license: ServedLicense,
    /// Runtime.
    pub runtime: Runtime,
    /// Version string the runtime reports.
    pub engine_version: String,
    /// What the runtime loads.
    pub entrypoint: Entrypoint,
    /// Model id on `/v1/models` and in `X-Tensorseed-Served-Model`.
    pub served_model_id: String,
    /// Local base URL (the signing proxy).
    pub base_url: String,
    /// Public base URL buyers call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_base_url: Option<String>,
    /// Every verified file.
    pub files: Vec<VerifiedFile>,
    /// Seller wallet, `0x` and 40 lowercase hex, as plain data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seller_wallet: Option<String>,
    /// Response signing key.
    pub response_key: ResponseKeyRef,
    /// Logprob tier.
    pub logprobs: LogprobsMode,
    /// Start of this serving period (RFC 3339 UTC).
    pub started_at: String,
    /// When this document was written.
    pub created_at: String,
    /// SHA-256 of the canonical body (64 lowercase hex).
    pub claim_sha256: String,
    /// Wallet signature, added after the seller signs externally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig: Option<ClaimSig>,
}

/// Normalize a wallet address to `0x` plus 40 lowercase hex.
///
/// # Errors
///
/// Fails unless the input is `0x` and 40 hex digits (any case).
pub fn normalize_wallet(s: &str) -> anyhow::Result<String> {
    let t = s.trim();
    let h = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .context("a wallet address starts with 0x")?;
    if h.len() != 40 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("a wallet address is 0x and 40 hex digits");
    }
    Ok(format!("0x{}", h.to_ascii_lowercase()))
}

/// Wallet signature status of a claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "status", content = "address")]
pub enum WalletStatus {
    /// `sig` recovers to `seller_wallet`.
    Verified(String),
    /// No wallet signature yet.
    NotSigned,
}

/// Result of [`ServedClaim::verify`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimCheck {
    /// Wallet signature status.
    pub wallet: WalletStatus,
    /// Non-fatal notes (for example an http public URL).
    pub warnings: Vec<String>,
}

impl ServedClaim {
    /// `claim_sha256` for the current contents.
    ///
    /// # Errors
    ///
    /// Fails only if the claim cannot be serialized.
    pub fn compute_hash(&self) -> anyhow::Result<String> {
        let mut v = serde_json::to_value(self)?;
        if let Some(o) = v.as_object_mut() {
            o.remove("claim_sha256");
            o.remove("sig");
            o.remove("signed");
        }
        Ok(sha256_bytes(&to_canonical_vec(&v)?))
    }

    /// Fill in `claim_sha256`.
    ///
    /// # Errors
    ///
    /// Same as [`Self::compute_hash`].
    pub fn seal(&mut self) -> anyhow::Result<()> {
        self.claim_sha256 = self.compute_hash()?;
        Ok(())
    }

    /// The exact EIP-191 message the seller wallet signs.
    #[must_use]
    pub fn message(&self) -> String {
        claim_message(&self.claim_sha256)
    }

    /// Response public key bytes.
    ///
    /// # Errors
    ///
    /// Fails unless `response_key` is an Ed25519 key of 32 bytes.
    pub fn response_pubkey(&self) -> anyhow::Result<[u8; 32]> {
        if self.response_key.alg != "ed25519" || !is_lower_hex(&self.response_key.pubkey, 64) {
            bail!("response_key must be ed25519 with a 64 lowercase hex public key");
        }
        let b: [u8; 32] = hex::decode(&self.response_key.pubkey)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("response_key.pubkey is not 32 bytes"))?;
        ed25519_dalek::VerifyingKey::from_bytes(&b)
            .map_err(|_| anyhow::anyhow!("response_key.pubkey is not a valid Ed25519 key"))?;
        Ok(b)
    }

    /// Verify the claim (section 3.2 steps 1 to 3, 5 as a warning, and 6).
    /// The catalog match (step 4) needs a local catalog and is done by the
    /// caller.
    ///
    /// # Errors
    ///
    /// Fails on a wrong schema, a hash mismatch, a bad response key, a bad
    /// model id or wallet, or a wallet signature that does not recover to
    /// `seller_wallet`.
    pub fn verify(&self) -> anyhow::Result<ClaimCheck> {
        if self.schema != CLAIM_SCHEMA {
            bail!("schema is {:?}, expected {CLAIM_SCHEMA}", self.schema);
        }
        if !is_lower_hex(&self.claim_sha256, 64) || !is_lower_hex(&self.catalog.manifest_sha256, 64)
        {
            bail!("claim_sha256 and catalog.manifest_sha256 must be 64 lowercase hex");
        }
        let want = self.compute_hash()?;
        if want != self.claim_sha256 {
            bail!(
                "claim_sha256 does not match the contents (file says {}, contents hash to {want}); the claim was changed after it was written",
                self.claim_sha256
            );
        }
        self.response_pubkey()?;
        if !valid_model_id(&self.served_model_id) {
            bail!("served_model_id must be printable ASCII without spaces");
        }
        if let Some(w) = &self.seller_wallet
            && normalize_wallet(w)? != *w
        {
            bail!("seller_wallet must be 0x and 40 lowercase hex");
        }
        let mut warnings = Vec::new();
        match &self.public_base_url {
            Some(u) if !u.starts_with("https://") => warnings.push(format!(
                "public_base_url {u} is not https; use an https URL before publishing the claim"
            )),
            None => warnings.push("no public_base_url; set it before publishing the claim".into()),
            _ => {}
        }
        let wallet = match &self.sig {
            None => {
                if self.signed {
                    bail!("signed is true but there is no sig");
                }
                WalletStatus::NotSigned
            }
            Some(s) => {
                if !self.signed {
                    bail!("sig is present but signed is false");
                }
                if s.alg != "eip191" {
                    bail!("sig.alg {:?} is not supported (eip191)", s.alg);
                }
                let wallet = self
                    .seller_wallet
                    .as_deref()
                    .context("sig is present but seller_wallet is not")?;
                let got = recover_eip191(self.message().as_bytes(), &s.value)?;
                if got != wallet {
                    bail!("the wallet signature recovers to {got}, not seller_wallet {wallet}");
                }
                WalletStatus::Verified(got)
            }
        };
        Ok(ClaimCheck { wallet, warnings })
    }

    /// Attach an externally made wallet signature after checking it.
    ///
    /// # Errors
    ///
    /// Fails when the claim has no wallet or the signature does not recover
    /// to it.
    pub fn attach_signature(&mut self, sig_hex: &str) -> anyhow::Result<String> {
        let wallet = self
            .seller_wallet
            .clone()
            .context("this claim has no seller_wallet; write it again with --wallet")?;
        let v = sig_hex.trim().to_ascii_lowercase();
        let got = recover_eip191(self.message().as_bytes(), &v)?;
        if got != wallet {
            bail!("the signature recovers to {got}, not seller_wallet {wallet}");
        }
        self.sig = Some(ClaimSig {
            alg: "eip191".into(),
            value: if v.starts_with("0x") {
                v
            } else {
                format!("0x{v}")
            },
        });
        self.signed = true;
        Ok(got)
    }

    /// Write pretty JSON to `path`.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be written.
    pub fn write(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let mut data = serde_json::to_vec_pretty(self)?;
        data.push(b'\n');
        std::fs::write(path, data).with_context(|| format!("writing {}", path.display()))
    }
}

/// What a claim adds to a version 0 record.
#[derive(Debug, Clone)]
pub struct ClaimExtras {
    /// Version string the runtime reports.
    pub engine_version: String,
    /// Response public key (64 lowercase hex).
    pub response_pubkey: String,
    /// Seller wallet as plain data (normalized here).
    pub seller_wallet: Option<String>,
    /// Logprob tier.
    pub logprobs: LogprobsMode,
    /// Start of the serving period (RFC 3339 UTC).
    pub started_at: String,
}

impl ServedClaim {
    /// Build and seal a claim from a version 0 record.
    ///
    /// # Errors
    ///
    /// Fails on a bad wallet, model id, engine version, or response key.
    pub fn from_record(r: ServedWeights, x: ClaimExtras) -> anyhow::Result<Self> {
        let seller_wallet = x
            .seller_wallet
            .as_deref()
            .map(normalize_wallet)
            .transpose()?;
        let engine_version = x.engine_version.trim().to_string();
        if engine_version.is_empty() || engine_version.len() > 200 {
            bail!("the engine version must be 1 to 200 characters");
        }
        let mut c = Self {
            schema: CLAIM_SCHEMA.into(),
            signed: false,
            catalog: r.catalog,
            repo_type: r.repo_type,
            repo: r.repo,
            revision: r.revision,
            license: r.license,
            runtime: r.runtime,
            engine_version,
            entrypoint: r.entrypoint,
            served_model_id: r.served_model_id,
            base_url: r.base_url,
            public_base_url: r.public_base_url,
            files: r.files,
            seller_wallet,
            response_key: ResponseKeyRef {
                alg: "ed25519".into(),
                pubkey: x.response_pubkey,
            },
            logprobs: x.logprobs,
            started_at: x.started_at,
            created_at: r.created_at,
            claim_sha256: String::new(),
            sig: None,
        };
        c.seal()?;
        c.verify()?;
        Ok(c)
    }

    /// When `old` makes the same statement as `self` (ignoring the two
    /// timestamps), return `old`, so a wallet signature on it stays valid.
    #[must_use]
    pub fn reuse(self, old: Option<Self>) -> (Self, bool) {
        let Some(old) = old else { return (self, false) };
        let mut probe = self.clone();
        probe.started_at.clone_from(&old.started_at);
        probe.created_at.clone_from(&old.created_at);
        match probe.compute_hash() {
            Ok(h) if h == old.claim_sha256 && old.verify().is_ok() => (old, true),
            _ => (self, false),
        }
    }
}

/// `<dir>/.tensorseed/served-weights.json`.
#[must_use]
pub fn claim_path(dir: &Path) -> std::path::PathBuf {
    dir.join(tensorseed_core::verify::META_DIR)
        .join(crate::inference::SERVED_WEIGHTS_FILE)
}

/// The EIP-191 message for a claim hash.
#[must_use]
pub fn claim_message(claim_sha256: &str) -> String {
    format!("{CLAIM_MESSAGE_PREFIX}{claim_sha256}")
}

fn keccak256(data: &[u8]) -> [u8; 32] {
    sha3::Keccak256::digest(data).into()
}

/// Recover the address that signed `message` with Ethereum `personal_sign`
/// (EIP-191 version 0x45). `sig_hex` is 65 bytes (`r || s || v`, `v` 27,
/// 28, 0, or 1), with or without `0x`. Returns `0x` and 40 lowercase hex.
///
/// # Errors
///
/// Fails on a malformed signature or one that does not recover.
pub fn recover_eip191(message: &[u8], sig_hex: &str) -> anyhow::Result<String> {
    let s = sig_hex.trim();
    let s = s.strip_prefix("0x").unwrap_or(s);
    let b = hex::decode(s).context("signature is not hex")?;
    if b.len() != 65 {
        bail!("signature must be 65 bytes, got {}", b.len());
    }
    let v = match b[64] {
        27 | 28 => b[64] - 27,
        0 | 1 => b[64],
        other => bail!("signature v byte {other} is not 27, 28, 0, or 1"),
    };
    let sig = k256::ecdsa::Signature::from_slice(&b[..64]).context("malformed signature")?;
    if sig.normalize_s().is_some() {
        bail!("signature s value is not canonical (EIP-2)");
    }
    let rid = k256::ecdsa::RecoveryId::from_byte(v).context("bad recovery id")?;
    let mut prefixed = format!("\x19Ethereum Signed Message:\n{}", message.len()).into_bytes();
    prefixed.extend_from_slice(message);
    let hash = keccak256(&prefixed);
    let vk = k256::ecdsa::VerifyingKey::recover_from_prehash(&hash, &sig, rid)
        .map_err(|_| anyhow::anyhow!("signature does not recover to a key"))?;
    let point = vk.to_encoded_point(false);
    let h = keccak256(&point.as_bytes()[1..]);
    Ok(format!("0x{}", hex::encode(&h[12..])))
}

/// A served-weights file of either version.
#[derive(Debug, Clone)]
pub enum AnyServedWeights {
    /// Unsigned placeholder (`ts.served-weights/0`).
    V0(Box<ServedWeights>),
    /// Signed claim (`ts.served-weights/1`).
    V1(Box<ServedClaim>),
}

/// Parse a served-weights document by its `schema`.
///
/// # Errors
///
/// Fails when the bytes are not JSON or the schema is unknown.
pub fn parse_served_weights(raw: &[u8]) -> anyhow::Result<AnyServedWeights> {
    let v: serde_json::Value = serde_json::from_slice(raw).context("not JSON")?;
    match v.get("schema").and_then(|s| s.as_str()) {
        Some(SERVED_WEIGHTS_SCHEMA) => Ok(AnyServedWeights::V0(Box::new(
            serde_json::from_value(v).context("malformed ts.served-weights/0")?,
        ))),
        Some(CLAIM_SCHEMA) => Ok(AnyServedWeights::V1(Box::new(
            serde_json::from_value(v).context("malformed ts.served-weights/1")?,
        ))),
        other => bail!("unknown served-weights schema {other:?}"),
    }
}

/// Load a `ts.served-weights/1` claim and verify it.
///
/// # Errors
///
/// Fails when the file is missing, version 0, or does not verify.
pub fn load_claim(path: &Path) -> anyhow::Result<(ServedClaim, ClaimCheck)> {
    let raw = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    claim_from_bytes(&raw).with_context(|| format!("checking {}", path.display()))
}

/// Parse and verify a `ts.served-weights/1` claim.
///
/// # Errors
///
/// Same as [`load_claim`].
pub fn claim_from_bytes(raw: &[u8]) -> anyhow::Result<(ServedClaim, ClaimCheck)> {
    match parse_served_weights(raw)? {
        AnyServedWeights::V1(c) => {
            let chk = c.verify()?;
            Ok((*c, chk))
        }
        AnyServedWeights::V0(_) => bail!(
            "this is an unsigned ts.served-weights/0 record; it has no response key. Write a claim with `tensorseed claim-serving` or `serve-inference --sign`"
        ),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tensorseed_core::commercial::CommercialServing;
    use tensorseed_core::manifest::LicenseClass;

    const CLAIM: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    pub(crate) fn tests_sha(s: &str) -> String {
        sha256_bytes(s.as_bytes())
    }

    pub(crate) fn key() -> ResponseKey {
        ResponseKey::from_seed(&[7; 32])
    }

    #[test]
    fn digest_vector() {
        let m = signing_message(
            CLAIM,
            "req-1",
            "org/tiny",
            "2026-11-13T21:01:42Z",
            &"2".repeat(64),
        );
        assert_eq!(
            m,
            format!(
                "ts.response-sig/1\nclaim_sha256={CLAIM}\nrequest_id=req-1\nserved_model_id=org/tiny\ntimestamp=2026-11-13T21:01:42Z\nbody_sha256={}\n",
                "2".repeat(64)
            )
        );
        // RFC 8032 test 1 key: a fixed seed gives a fixed public key and,
        // Ed25519 being deterministic, a fixed signature.
        let k = ResponseKey::from_seed(
            &hex::decode("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60")
                .unwrap()
                .try_into()
                .unwrap(),
        );
        assert_eq!(
            k.pubkey_hex(),
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
        );
        let f = k.sign_response(CLAIM, "req-1", "org/tiny", "2026-11-13T21:01:42Z", b"{}");
        assert_eq!(
            f.body_sha256,
            "44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a"
        );
        // Produced independently (PyNaCl) over the same message bytes.
        assert_eq!(
            f.signature,
            "9340ba79974fd5313bf723818a68aa52d1fb6bdd8c273601082db832ac97130adf7324ac827d11c9dbebec5d547c7f6120aa33a69a3a20d09e2ccb87c44ecb0c"
        );
    }

    fn expect(k: &ResponseKey, now: i64) -> Expect<'static> {
        Expect {
            pubkey: k.pubkey(),
            claim_sha256: CLAIM,
            served_model_id: "org/tiny",
            request_id: Some("req-1"),
            requested_model: Some("org/tiny"),
            now_unix: now,
            max_skew_secs: DEFAULT_MAX_SKEW_SECS,
        }
    }

    #[test]
    fn verify_accepts_and_rejects() {
        let k = key();
        let ts = "2026-11-13T21:01:42Z";
        let now = parse_rfc3339(ts).unwrap();
        let body = br#"{"choices":[]}"#;
        let f = k.sign_response(CLAIM, "req-1", "org/tiny", ts, body);
        verify_fields(&f, body, &expect(&k, now)).unwrap();
        // Through headers, any case.
        let hs: Vec<(String, String)> = f
            .headers()
            .iter()
            .map(|(n, v)| (n.to_ascii_uppercase(), (*v).to_string()))
            .collect();
        let back = fields_from_headers(hs.iter().map(|(a, b)| (a.as_str(), b.as_str()))).unwrap();
        assert_eq!(back, f);
        // Tampered body.
        assert_eq!(
            verify_fields(&f, br#"{"choices":[1]}"#, &expect(&k, now)),
            Err(SigError::BodyHash)
        );
        // Tampered headers.
        let mut t = f.clone();
        t.body_sha256 = sha256_bytes(b"x");
        assert_eq!(
            verify_fields(&t, b"x", &expect(&k, now)),
            Err(SigError::BadSignature)
        );
        let mut t = f.clone();
        t.timestamp = "2026-11-13T21:01:43Z".into();
        assert_eq!(
            verify_fields(&t, body, &expect(&k, now)),
            Err(SigError::BadSignature)
        );
        let mut t = f.clone();
        t.request_id = "req-2".into();
        assert_eq!(
            verify_fields(&t, body, &expect(&k, now)),
            Err(SigError::RequestId)
        );
        let mut e = expect(&k, now);
        e.request_id = None;
        assert_eq!(verify_fields(&t, body, &e), Err(SigError::BadSignature));
        let mut t = f.clone();
        t.served_model_id = "org/other".into();
        assert!(matches!(
            verify_fields(&t, body, &expect(&k, now)),
            Err(SigError::Model(_))
        ));
        let mut t = f.clone();
        t.claim_sha256 = "3".repeat(64);
        assert_eq!(
            verify_fields(&t, body, &expect(&k, now)),
            Err(SigError::ClaimMismatch)
        );
        let mut t = f.clone();
        t.signature = "zz".into();
        assert!(matches!(
            verify_fields(&t, body, &expect(&k, now)),
            Err(SigError::Malformed(_))
        ));
        // Wrong key.
        let other = ResponseKey::from_seed(&[8; 32]);
        assert_eq!(
            verify_fields(&f, body, &expect(&other, now)),
            Err(SigError::BadSignature)
        );
        // Requested model differs from the served one.
        let mut e = expect(&k, now);
        e.requested_model = Some("org/big");
        assert!(matches!(
            verify_fields(&f, body, &e),
            Err(SigError::Model(_))
        ));
        // Skew, both directions, at and past the window.
        assert!(verify_fields(&f, body, &expect(&k, now + 300)).is_ok());
        assert_eq!(
            verify_fields(&f, body, &expect(&k, now + 301)),
            Err(SigError::Skew(-301))
        );
        assert_eq!(
            verify_fields(&f, body, &expect(&k, now - 301)),
            Err(SigError::Skew(301))
        );
    }

    #[test]
    fn header_parsing_absent_and_partial() {
        assert_eq!(
            fields_from_headers([("content-type", "application/json")]),
            Err(SigError::Absent)
        );
        assert_eq!(
            fields_from_headers([(H_SIGNATURE, "ab")]),
            Err(SigError::Missing(H_CLAIM))
        );
        assert!(valid_request_id("7f1c-uuid:1"));
        assert!(!valid_request_id("a b"));
        assert!(!valid_request_id("a\nb"));
        assert!(!valid_request_id(&"a".repeat(129)));
        assert!(!valid_model_id("a b") && valid_model_id("org/tiny"));
    }

    #[test]
    fn sse_trailer_round_trip() {
        let k = key();
        let stream = b"data: {\"x\":1}\n\ndata: [DONE]\n\n";
        let f = k.sign_response(CLAIM, "req-1", "org/tiny", "2026-11-13T21:01:42Z", stream);
        let mut full = stream.to_vec();
        full.extend_from_slice(sse_trailer(&f).as_bytes());
        let (body, t) = split_sse_trailer(&full).unwrap();
        assert_eq!(body, stream);
        assert_eq!(t.signature, f.signature);
        assert_eq!(t.body_sha256, f.body_sha256);
        assert!(split_sse_trailer(stream).is_none());
        // Data after the trailer means it is not the final line.
        let mut more = full.clone();
        more.extend_from_slice(b"data: late\n\n");
        assert!(split_sse_trailer(&more).is_none());
    }

    pub(crate) fn claim() -> ServedClaim {
        let mut c = ServedClaim {
            schema: CLAIM_SCHEMA.into(),
            signed: false,
            catalog: ServedCatalog {
                name: "main".into(),
                publisher_key_id: Some("CE1E0E3D543DFFB8".into()),
                manifest_key: "ts/v1/repo/model/org/tiny/rev/x".into(),
                manifest_sha256: "a".repeat(64),
            },
            repo_type: RepoType::Model,
            repo: "org/tiny".into(),
            revision: "0".repeat(40),
            license: ServedLicense {
                id: Some("apache-2.0".into()),
                class: LicenseClass::Open,
                commercial_serving: CommercialServing::Allowed,
                rights_asserted: false,
            },
            runtime: Runtime::LlamaCpp,
            engine_version: "version: test".into(),
            entrypoint: Entrypoint::Gguf("tiny-q4.gguf".into()),
            served_model_id: "org/tiny".into(),
            base_url: "http://127.0.0.1:8090/v1".into(),
            public_base_url: Some("https://llm.example.com/v1".into()),
            files: vec![],
            seller_wallet: Some(HARDHAT0_ADDR.into()),
            response_key: ResponseKeyRef {
                alg: "ed25519".into(),
                pubkey: key().pubkey_hex(),
            },
            logprobs: LogprobsMode::Required,
            started_at: "2026-11-13T21:01:42Z".into(),
            created_at: "2026-11-13T21:01:42Z".into(),
            claim_sha256: String::new(),
            sig: None,
        };
        c.seal().unwrap();
        c
    }

    // Well-known development key (Hardhat and Anvil account 0). Used only to
    // make test signatures; Tensorseed itself never holds a wallet key.
    const HARDHAT0_KEY: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
    const HARDHAT0_ADDR: &str = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266";

    fn personal_sign(msg: &[u8]) -> String {
        let sk = k256::ecdsa::SigningKey::from_slice(&hex::decode(HARDHAT0_KEY).unwrap()).unwrap();
        let mut p = format!("\x19Ethereum Signed Message:\n{}", msg.len()).into_bytes();
        p.extend_from_slice(msg);
        let (sig, rid) = sk.sign_prehash_recoverable(&keccak256(&p)).unwrap();
        let mut b = sig.to_bytes().to_vec();
        b.push(27 + rid.to_byte());
        format!("0x{}", hex::encode(b))
    }

    #[test]
    fn eip191_recovers_known_addresses() {
        // Signature by account 0 over "hello", as produced by
        // ethers/viem personal_sign (RFC 6979, low s).
        assert_eq!(
            recover_eip191(b"hello", &personal_sign(b"hello")).unwrap(),
            HARDHAT0_ADDR
        );
        assert_eq!(
            recover_eip191(b"hello", EXTERNAL_HELLO_SIG.trim()).unwrap(),
            HARDHAT0_ADDR,
            "a signature made by an external wallet library"
        );
        assert_eq!(personal_sign(b"hello"), EXTERNAL_HELLO_SIG.trim());
        assert_ne!(
            recover_eip191(b"hellp", &personal_sign(b"hello")).unwrap(),
            HARDHAT0_ADDR
        );
        assert!(recover_eip191(b"hello", "0x1234").is_err());
    }

    // personal_sign("hello") with account 0, produced outside Tensorseed.
    const EXTERNAL_HELLO_SIG: &str = include_str!("../testdata/eip191_hello_hardhat0.txt");

    #[test]
    fn claim_hash_message_and_wallet_signature() {
        let c = claim();
        let chk = c.verify().unwrap();
        assert_eq!(chk.wallet, WalletStatus::NotSigned);
        assert_eq!(
            c.message(),
            format!("tensorseed served-weights/1:\n{}", c.claim_sha256)
        );
        // The hash excludes signed, sig, and claim_sha256.
        let mut v = serde_json::to_value(&c).unwrap();
        let o = v.as_object_mut().unwrap();
        o.remove("claim_sha256");
        o.remove("signed");
        assert_eq!(c.claim_sha256, sha256_bytes(&to_canonical_vec(&v).unwrap()));
        // Attach a wallet signature; the hash is unchanged.
        let mut s = c.clone();
        let addr = s
            .attach_signature(&personal_sign(c.message().as_bytes()))
            .unwrap();
        assert_eq!(addr, HARDHAT0_ADDR);
        assert!(s.signed);
        assert_eq!(s.compute_hash().unwrap(), c.claim_sha256);
        assert_eq!(
            s.verify().unwrap().wallet,
            WalletStatus::Verified(HARDHAT0_ADDR.into())
        );
        // A signature over another message, or by another wallet, is refused.
        let mut t = c.clone();
        assert!(
            t.attach_signature(&personal_sign(b"something else"))
                .is_err()
        );
        let mut t = c.clone();
        t.seller_wallet = Some(format!("0x{}", "1".repeat(40)));
        t.seal().unwrap();
        assert!(
            t.attach_signature(&personal_sign(t.message().as_bytes()))
                .is_err()
        );
        // Changing any bound field breaks the hash.
        let mut t = s.clone();
        t.response_key.pubkey = ResponseKey::from_seed(&[9; 32]).pubkey_hex();
        assert!(
            t.verify()
                .unwrap_err()
                .to_string()
                .contains("claim_sha256 does not match")
        );
        let mut t = s.clone();
        t.signed = false;
        assert!(t.verify().is_err());
        // Version 0 stays readable.
        let v0 = br#"{"schema":"ts.served-weights/0"}"#;
        assert!(
            parse_served_weights(v0).is_err(),
            "malformed v0 is an error, not a v1"
        );
        assert!(claim_from_bytes(&serde_json::to_vec(&s).unwrap()).is_ok());
        assert!(
            normalize_wallet("0xF39FD6E51AAD88F6F4CE6AB8827279CFFFB92266").unwrap()
                == HARDHAT0_ADDR
        );
        assert!(normalize_wallet("f39f").is_err());
    }

    #[test]
    fn key_files_are_private_and_round_trip() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("k/response-key.json");
        let k = ResponseKey::generate().unwrap();
        k.save_new(&p, false).unwrap();
        assert!(k.save_new(&p, false).is_err(), "no silent replace");
        let back = ResponseKey::load(&p).unwrap();
        assert_eq!(back.pubkey(), k.pubkey());
        let dbg = format!("{back:?}");
        let raw = std::fs::read_to_string(&p).unwrap();
        let secret = serde_json::from_str::<serde_json::Value>(&raw).unwrap()["secret"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(!dbg.contains(&secret), "Debug never shows the secret");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(
                ResponseKey::load(&p)
                    .unwrap_err()
                    .to_string()
                    .contains("readable by other users")
            );
        }
    }

    #[test]
    fn reuse_keeps_an_equivalent_signed_claim() {
        let mut old = claim();
        let wallet = HARDHAT0_ADDR;
        let sig = personal_sign(old.message().as_bytes());
        assert_eq!(old.attach_signature(&sig).unwrap(), wallet);
        let mut new = claim();
        new.started_at = "2026-11-14T00:00:00Z".into();
        new.created_at = "2026-11-14T00:00:00Z".into();
        new.seal().unwrap();
        let (kept, reused) = new.clone().reuse(Some(old.clone()));
        assert!(reused);
        assert_eq!(kept, old);
        let mut other = new.clone();
        other.engine_version = "version: other".into();
        other.seal().unwrap();
        let (kept, reused) = other.clone().reuse(Some(old));
        assert!(!reused);
        assert_eq!(kept, other);
    }
}
