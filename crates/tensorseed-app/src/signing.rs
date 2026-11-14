// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Signed responses and the signed served-weights claim
//! (docs/plans/verified-serving.md, sections 3 and 5.1), seller side.
//!
//! Verification lives in the verify-only crate and is re-exported here:
//! [`tensorseed_verify::claim`] (`ts.served-weights/1`, EIP-191 wallet
//! signatures, expiry), [`tensorseed_verify::revocation`]
//! (`ts.claim-revocations/1`), and [`tensorseed_verify::response`]
//! (`ts.response-sig/1`). This module adds what needs the seller's key or
//! the filesystem:
//!
//! - [`ResponseKey`]: the seller's Ed25519 response key, kept in a file
//!   only the owner can read. The secret is never printed, logged, or taken
//!   on a command line; `Debug` shows only the public key. It signs
//!   responses ([`ResponseKey::sign_response`]) and revocations
//!   ([`ResponseKey::sign_revocation`]).
//! - [`ClaimFile`]: building a claim from a version 0 record and writing
//!   it. Tensorseed holds no wallet key; the wallet signs outside.
//! - Loading claims and revocation lists from disk.

use std::path::Path;

use anyhow::{Context, bail};
use ed25519_dalek::Signer;
use serde::{Deserialize, Serialize};
use tensorseed_core::hash::sha256_bytes;

pub use tensorseed_verify::claim::{
    CLAIM_MESSAGE_PREFIX, CLAIM_SCHEMA, ClaimCheck, ClaimSig, LogprobsMode, ResponseKeyRef,
    ServedClaim, WELL_KNOWN_CLAIM, WalletStatus, claim_message, normalize_wallet, recover_eip191,
};
pub use tensorseed_verify::response::{
    DEFAULT_MAX_SKEW_SECS, Expect, H_BODY, H_CLAIM, H_MODEL, H_REQUEST_ID, H_SIGNATURE,
    H_SIGNATURE_MODE, H_TIMESTAMP, MODE_SSE_TRAILER, RESPONSE_SIG_SCHEMA, SigError, SignedFields,
    SseTrailer, extract_signed, fields_from_headers, parse_rfc3339, signing_message,
    split_sse_trailer, sse_trailer, valid_model_id, valid_request_id, verify_fields,
};
pub use tensorseed_verify::revocation::{
    REASON_KEY_COMPROMISED, REASONS, REVOCATIONS_SCHEMA, Revocation, Revocations, RevokedBy,
    WELL_KNOWN_REVOCATIONS, revocation_message, valid_reason,
};

use crate::inference::{SERVED_WEIGHTS_SCHEMA, ServedWeights};

/// Schema of the response key file.
pub const RESPONSE_KEY_SCHEMA: &str = "ts.response-key/1";
/// Default response key file name (in the Tensorseed config directory).
pub const RESPONSE_KEY_FILE: &str = "response-key.json";
/// Revocation list file name, next to the claim.
pub const REVOCATIONS_FILE: &str = "revocations.json";

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

    /// Sign a revocation of the claim `claim_sha256` with this key.
    ///
    /// # Errors
    ///
    /// Fails on a reason outside `[a-z0-9_]{1,32}` or a time that is not
    /// RFC 3339.
    pub fn sign_revocation(
        &self,
        claim_sha256: &str,
        revoked_at: &str,
        reason: &str,
    ) -> anyhow::Result<Revocation> {
        if !valid_reason(reason) {
            bail!("reason {reason:?} must be 1 to 32 of a-z, 0-9, _ (for example superseded)");
        }
        if parse_rfc3339(revoked_at).is_none() {
            bail!("{revoked_at:?} is not an RFC 3339 time");
        }
        let msg = revocation_message(claim_sha256, revoked_at, reason);
        Ok(Revocation {
            claim_sha256: claim_sha256.into(),
            revoked_at: revoked_at.into(),
            reason: reason.into(),
            response_sig: Some(hex::encode(self.key.sign(msg.as_bytes()).to_bytes())),
            wallet_sig: None,
        })
    }
}

/// A fresh request id: 32 random bytes as 64 lowercase hex.
///
/// # Errors
///
/// Fails when the random source fails.
pub fn mint_request_id() -> anyhow::Result<String> {
    Ok(hex::encode(crate::challenge::random_32()?))
}

// ---------------------------------------------------------------------------
// Writing claims
// ---------------------------------------------------------------------------

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
    /// When the claim expires (RFC 3339 UTC), if it should.
    pub expires_at: Option<String>,
}

/// Seller-side claim operations (building from a record, writing a file).
pub trait ClaimFile: Sized {
    /// Build and seal a claim from a version 0 record.
    ///
    /// # Errors
    ///
    /// Fails on a bad wallet, model id, engine version, expiry, or
    /// response key.
    fn from_record(r: ServedWeights, x: ClaimExtras) -> anyhow::Result<Self>;

    /// Write pretty JSON to `path`.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be written.
    fn write(&self, path: &Path) -> anyhow::Result<()>;
}

impl ClaimFile for ServedClaim {
    fn from_record(r: ServedWeights, x: ClaimExtras) -> anyhow::Result<Self> {
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
            expires_at: x.expires_at,
            claim_sha256: String::new(),
            sig: None,
        };
        c.seal()?;
        c.verify()?;
        Ok(c)
    }

    fn write(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let mut data = serde_json::to_vec_pretty(self)?;
        data.push(b'\n');
        std::fs::write(path, data).with_context(|| format!("writing {}", path.display()))
    }
}

/// `<dir>/.tensorseed/served-weights.json`.
#[must_use]
pub fn claim_path(dir: &Path) -> std::path::PathBuf {
    dir.join(tensorseed_core::verify::META_DIR)
        .join(crate::inference::SERVED_WEIGHTS_FILE)
}

/// The revocation list next to a claim file (`revocations.json` in the
/// same directory).
#[must_use]
pub fn revocations_path(claim: &Path) -> std::path::PathBuf {
    claim
        .parent()
        .map_or_else(|| REVOCATIONS_FILE.into(), |p| p.join(REVOCATIONS_FILE))
}

/// Load the revocation list at `path`; an empty list when it does not exist.
///
/// # Errors
///
/// Fails when the file exists but cannot be read or parsed.
pub fn load_revocations(path: &Path) -> anyhow::Result<Revocations> {
    match std::fs::read(path) {
        Ok(raw) => Revocations::parse(&raw).with_context(|| format!("reading {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Revocations::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Write a revocation list as pretty JSON.
///
/// # Errors
///
/// Fails when the file cannot be written.
pub fn write_revocations(path: &Path, r: &Revocations) -> anyhow::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut data = serde_json::to_vec_pretty(r)?;
    data.push(b'\n');
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, data).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))
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
    use crate::inference::{Entrypoint, Runtime, ServedCatalog, ServedLicense};
    use tensorseed_core::canonical::to_canonical_vec;
    use tensorseed_core::commercial::CommercialServing;
    use tensorseed_core::keys::RepoType;
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
            claim_expires_unix: None,
            claim_revoked_unix: None,
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
            expires_at: None,
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

    fn keccak256(data: &[u8]) -> [u8; 32] {
        use sha3::Digest as _;
        sha3::Keccak256::digest(data).into()
    }

    pub(crate) fn personal_sign(msg: &[u8]) -> String {
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
