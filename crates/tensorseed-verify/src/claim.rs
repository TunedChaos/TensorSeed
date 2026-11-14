// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! The signed served-weights claim, `ts.served-weights/1`
//! (docs/plans/verified-serving.md, section 3).
//!
//! `claim_sha256` is the SHA-256 of the canonical JSON (SPEC 6.4) of the
//! claim without `claim_sha256`, `sig`, and `signed`. The seller wallet may
//! sign [`claim_message`] with EIP-191 outside Tensorseed; [`recover_eip191`]
//! verifies it. Tensorseed holds no wallet key.
//!
//! A claim may carry `expires_at` (added in 0.3.1). It is omitted when not
//! set, so claims written without it hash exactly as before and every older
//! claim still loads. [`ServedClaim::verify`] does not look at the clock;
//! [`ServedClaim::expired_at`] and the checks in [`crate::claim_check`] do.
//! Revocation is in [`crate::revocation`].

use serde::{Deserialize, Serialize};
use sha3::Digest as _;
use tensorseed_core::canonical::to_canonical_vec;
use tensorseed_core::commercial::CommercialServing;
use tensorseed_core::hash::{is_lower_hex, sha256_bytes};
use tensorseed_core::keys::RepoType;
use tensorseed_core::manifest::LicenseClass;

use crate::response::{parse_rfc3339, valid_model_id};
use crate::{Error, Result};

/// Schema of the signed claim.
pub const CLAIM_SCHEMA: &str = "ts.served-weights/1";
/// Schema of the unsigned version 0 record (not a claim).
pub const SERVED_WEIGHTS_V0_SCHEMA: &str = "ts.served-weights/0";
/// Prefix of the EIP-191 claim message.
pub const CLAIM_MESSAGE_PREFIX: &str = "tensorseed served-weights/1:\n";
/// Path where the signing proxy publishes the claim.
pub const WELL_KNOWN_CLAIM: &str = "/.well-known/tensorseed/served-weights.json";

fn bad(msg: impl Into<String>) -> Error {
    Error::Claim(msg.into())
}

/// Supported OpenAI compatible runtimes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Runtime {
    /// llama.cpp `llama-server` (GGUF).
    LlamaCpp,
    /// vLLM `vllm serve` (safetensors).
    Vllm,
}

impl Runtime {
    /// Both runtimes.
    pub const ALL: [Self; 2] = [Self::LlamaCpp, Self::Vllm];

    /// `llama.cpp` or `vLLM`.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::LlamaCpp => "llama.cpp",
            Self::Vllm => "vLLM",
        }
    }
}

/// What the runtime loads: one GGUF file (llama.cpp) or the directory
/// (vLLM).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "path", rename_all = "snake_case")]
pub enum Entrypoint {
    /// GGUF file, relative to the copy (`/` separators). For split GGUF
    /// files this is the first shard; llama.cpp loads the rest.
    Gguf(String),
    /// The whole directory (config.json plus safetensors).
    Directory,
}

/// A file that passed verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedFile {
    /// Path relative to the copy (`/` separators).
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// SHA-256 from the signed manifest.
    pub sha256: String,
}

/// Catalog provenance in a claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServedCatalog {
    /// Catalog name.
    pub name: String,
    /// Publisher key id (hex).
    pub publisher_key_id: Option<String>,
    /// Catalog key of the manifest entry.
    pub manifest_key: String,
    /// SHA-256 of the signed manifest value.
    pub manifest_sha256: String,
}

/// License facts in a claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServedLicense {
    /// License id.
    pub id: Option<String>,
    /// Publisher license class.
    pub class: LicenseClass,
    /// Derived commercial serving mark.
    pub commercial_serving: CommercialServing,
    /// The operator passed `--i-have-rights`.
    pub rights_asserted: bool,
}

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

/// Wallet signature on a claim or a revocation.
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
    /// When the claim stops being valid (RFC 3339 UTC), if it expires.
    /// Part of `claim_sha256` when present; absent in claims written
    /// without an expiry and in every claim before 0.3.1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
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
pub fn normalize_wallet(s: &str) -> Result<String> {
    let t = s.trim();
    let h = t
        .strip_prefix("0x")
        .or_else(|| t.strip_prefix("0X"))
        .ok_or_else(|| bad("a wallet address starts with 0x"))?;
    if h.len() != 40 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(bad("a wallet address is 0x and 40 hex digits"));
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
    pub fn compute_hash(&self) -> Result<String> {
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
    pub fn seal(&mut self) -> Result<()> {
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
    pub fn response_pubkey(&self) -> Result<[u8; 32]> {
        if self.response_key.alg != "ed25519" || !is_lower_hex(&self.response_key.pubkey, 64) {
            return Err(bad(
                "response_key must be ed25519 with a 64 lowercase hex public key",
            ));
        }
        let b: [u8; 32] = hex::decode(&self.response_key.pubkey)
            .ok()
            .and_then(|v| v.try_into().ok())
            .ok_or_else(|| bad("response_key.pubkey is not 32 bytes"))?;
        ed25519_dalek::VerifyingKey::from_bytes(&b)
            .map_err(|_| bad("response_key.pubkey is not a valid Ed25519 key"))?;
        Ok(b)
    }

    /// `expires_at` as Unix seconds, `None` when the claim does not expire.
    ///
    /// # Errors
    ///
    /// Fails when `expires_at` is present but not RFC 3339.
    pub fn expires_unix(&self) -> Result<Option<i64>> {
        self.expires_at
            .as_deref()
            .map(|s| {
                parse_rfc3339(s)
                    .ok_or_else(|| bad(format!("expires_at {s:?} is not an RFC 3339 time")))
            })
            .transpose()
    }

    /// `Some(expires_at)` when the claim has expired at `now_unix`.
    #[must_use]
    pub fn expired_at(&self, now_unix: i64) -> Option<&str> {
        match self.expires_unix() {
            Ok(Some(x)) if now_unix > x => self.expires_at.as_deref(),
            _ => None,
        }
    }

    /// Verify the claim (section 3.2 steps 1 to 3, 5 as a warning, and 6).
    /// The catalog match (step 4) needs a catalog and is done by the
    /// caller; expiry needs a clock and revocation needs a revocation list,
    /// so both are checked separately (see [`crate::claim_check`]). An
    /// expired or revoked claim still loads here.
    ///
    /// # Errors
    ///
    /// Fails on a wrong schema, a hash mismatch, a bad response key, a bad
    /// model id or wallet, a malformed `expires_at`, or a wallet signature
    /// that does not recover to `seller_wallet`.
    pub fn verify(&self) -> Result<ClaimCheck> {
        if self.schema != CLAIM_SCHEMA {
            return Err(bad(format!(
                "schema is {:?}, expected {CLAIM_SCHEMA}",
                self.schema
            )));
        }
        if !is_lower_hex(&self.claim_sha256, 64) || !is_lower_hex(&self.catalog.manifest_sha256, 64)
        {
            return Err(bad(
                "claim_sha256 and catalog.manifest_sha256 must be 64 lowercase hex",
            ));
        }
        let want = self.compute_hash()?;
        if want != self.claim_sha256 {
            return Err(bad(format!(
                "claim_sha256 does not match the contents (file says {}, contents hash to {want}); the claim was changed after it was written",
                self.claim_sha256
            )));
        }
        self.response_pubkey()?;
        if !valid_model_id(&self.served_model_id) {
            return Err(bad(
                "served_model_id must be printable ASCII without spaces",
            ));
        }
        if let Some(w) = &self.seller_wallet
            && normalize_wallet(w)? != *w
        {
            return Err(bad("seller_wallet must be 0x and 40 lowercase hex"));
        }
        if let Some(x) = self.expires_unix()? {
            if let Some(created) = parse_rfc3339(&self.created_at)
                && x <= created
            {
                return Err(bad(format!(
                    "expires_at {} is not after created_at {}",
                    self.expires_at.as_deref().unwrap_or_default(),
                    self.created_at
                )));
            }
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
                    return Err(bad("signed is true but there is no sig"));
                }
                WalletStatus::NotSigned
            }
            Some(s) => {
                if !self.signed {
                    return Err(bad("sig is present but signed is false"));
                }
                if s.alg != "eip191" {
                    return Err(bad(format!(
                        "sig.alg {:?} is not supported (eip191)",
                        s.alg
                    )));
                }
                let wallet = self
                    .seller_wallet
                    .as_deref()
                    .ok_or_else(|| bad("sig is present but seller_wallet is not"))?;
                let got = recover_eip191(self.message().as_bytes(), &s.value)?;
                if got != wallet {
                    return Err(bad(format!(
                        "the wallet signature recovers to {got}, not seller_wallet {wallet}"
                    )));
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
    pub fn attach_signature(&mut self, sig_hex: &str) -> Result<String> {
        let wallet = self
            .seller_wallet
            .clone()
            .ok_or_else(|| bad("this claim has no seller_wallet; write it again with --wallet"))?;
        let v = sig_hex.trim().to_ascii_lowercase();
        let got = recover_eip191(self.message().as_bytes(), &v)?;
        if got != wallet {
            return Err(bad(format!(
                "the signature recovers to {got}, not seller_wallet {wallet}"
            )));
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

    /// When `old` makes the same statement as `self` (ignoring the two
    /// timestamps), return `old`, so a wallet signature on it stays valid.
    ///
    /// With an expiry, `old` is kept only while more than half of the
    /// lifetime `self` asks for (`expires_at - created_at`) is left on it;
    /// after that a fresh claim with a later `expires_at` replaces it.
    #[must_use]
    pub fn reuse(self, old: Option<Self>) -> (Self, bool) {
        let Some(old) = old else { return (self, false) };
        let mut probe = self.clone();
        probe.started_at.clone_from(&old.started_at);
        probe.created_at.clone_from(&old.created_at);
        if let (Ok(Some(new_x)), Ok(Some(old_x)), Some(now)) = (
            self.expires_unix(),
            old.expires_unix(),
            parse_rfc3339(&self.created_at),
        ) {
            let lifetime = new_x - now;
            let left = old_x - now;
            if left.saturating_mul(2) <= lifetime {
                return (self, false);
            }
            probe.expires_at.clone_from(&old.expires_at);
        }
        match probe.compute_hash() {
            Ok(h) if h == old.claim_sha256 && old.verify().is_ok() => (old, true),
            _ => (self, false),
        }
    }
}

/// Parse a served-weights document as a version 1 claim without verifying
/// it (use [`ServedClaim::verify`] or [`crate::claim_check::check_document`]).
///
/// # Errors
///
/// Fails when the bytes are not JSON, are a version 0 record, or are not a
/// well formed `ts.served-weights/1` document.
pub fn parse_claim(raw: &[u8]) -> Result<ServedClaim> {
    let v: serde_json::Value =
        serde_json::from_slice(raw).map_err(|e| bad(format!("not JSON: {e}")))?;
    match v.get("schema").and_then(|s| s.as_str()) {
        Some(CLAIM_SCHEMA) => {
            serde_json::from_value(v).map_err(|e| bad(format!("malformed {CLAIM_SCHEMA}: {e}")))
        }
        Some(SERVED_WEIGHTS_V0_SCHEMA) => Err(bad(format!(
            "this is an unsigned {SERVED_WEIGHTS_V0_SCHEMA} record with no response key, not a {CLAIM_SCHEMA} claim"
        ))),
        other => Err(bad(format!("unknown served-weights schema {other:?}"))),
    }
}

/// The EIP-191 message for a claim hash.
#[must_use]
pub fn claim_message(claim_sha256: &str) -> String {
    format!("{CLAIM_MESSAGE_PREFIX}{claim_sha256}")
}

pub(crate) fn keccak256(data: &[u8]) -> [u8; 32] {
    sha3::Keccak256::digest(data).into()
}

/// Recover the address that signed `message` with Ethereum `personal_sign`
/// (EIP-191 version 0x45). `sig_hex` is 65 bytes (`r || s || v`, `v` 27,
/// 28, 0, or 1), with or without `0x`. Returns `0x` and 40 lowercase hex.
///
/// # Errors
///
/// Fails on a malformed signature or one that does not recover.
pub fn recover_eip191(message: &[u8], sig_hex: &str) -> Result<String> {
    let s = sig_hex.trim();
    let s = s.strip_prefix("0x").unwrap_or(s);
    let b = hex::decode(s).map_err(|_| bad("signature is not hex"))?;
    if b.len() != 65 {
        return Err(bad(format!("signature must be 65 bytes, got {}", b.len())));
    }
    let v = match b[64] {
        27 | 28 => b[64] - 27,
        0 | 1 => b[64],
        other => {
            return Err(bad(format!(
                "signature v byte {other} is not 27, 28, 0, or 1"
            )));
        }
    };
    let sig =
        k256::ecdsa::Signature::from_slice(&b[..64]).map_err(|_| bad("malformed signature"))?;
    if sig.normalize_s().is_some() {
        return Err(bad("signature s value is not canonical (EIP-2)"));
    }
    let rid = k256::ecdsa::RecoveryId::from_byte(v).ok_or_else(|| bad("bad recovery id"))?;
    let mut prefixed = format!("\x19Ethereum Signed Message:\n{}", message.len()).into_bytes();
    prefixed.extend_from_slice(message);
    let hash = keccak256(&prefixed);
    let vk = k256::ecdsa::VerifyingKey::recover_from_prehash(&hash, &sig, rid)
        .map_err(|_| bad("signature does not recover to a key"))?;
    let point = vk.to_encoded_point(false);
    let h = keccak256(&point.as_bytes()[1..]);
    Ok(format!("0x{}", hex::encode(&h[12..])))
}

/// Fixtures shared by the unit tests of this crate: a sealed claim, its
/// Ed25519 response key, and a well-known development wallet.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    /// Seed of the test response key (a throwaway value, never a real key).
    pub(crate) const RESPONSE_SEED: [u8; 32] = [7; 32];

    // Well-known development key (Hardhat and Anvil account 0). Used only
    // to make test signatures; Tensorseed itself never holds a wallet key.
    const HARDHAT0_KEY: &str = "ac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
    pub(crate) const HARDHAT0_ADDR: &str = "0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266";

    pub(crate) fn response_key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&RESPONSE_SEED)
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
                pubkey: hex::encode(response_key().verifying_key().to_bytes()),
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

    pub(crate) fn personal_sign(msg: &[u8]) -> String {
        let sk = k256::ecdsa::SigningKey::from_slice(&hex::decode(HARDHAT0_KEY).unwrap()).unwrap();
        let mut p = format!("\x19Ethereum Signed Message:\n{}", msg.len()).into_bytes();
        p.extend_from_slice(msg);
        let (sig, rid) = sk.sign_prehash_recoverable(&keccak256(&p)).unwrap();
        let mut b = sig.to_bytes().to_vec();
        b.push(27 + rid.to_byte());
        format!("0x{}", hex::encode(b))
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{claim, personal_sign};
    use super::*;

    #[test]
    fn claims_without_expiry_hash_as_before() {
        // A claim with no expires_at serializes without the field, so its
        // hash (and any wallet signature on it) is the 0.3.0 hash.
        let c = claim();
        let v = serde_json::to_value(&c).unwrap();
        assert!(v.get("expires_at").is_none());
        let back = parse_claim(&serde_json::to_vec(&c).unwrap()).unwrap();
        assert_eq!(back.compute_hash().unwrap(), c.claim_sha256);
        back.verify().unwrap();
    }

    #[test]
    fn expiry_is_sealed_and_checked() {
        let mut c = claim();
        c.expires_at = Some("2026-12-13T21:01:42Z".into());
        // Changing expires_at without resealing breaks the hash.
        assert!(c.verify().is_err());
        c.seal().unwrap();
        c.verify().unwrap();
        let x = parse_rfc3339("2026-12-13T21:01:42Z").unwrap();
        assert_eq!(c.expires_unix().unwrap(), Some(x));
        assert_eq!(c.expired_at(x), None);
        assert_eq!(c.expired_at(x + 1), Some("2026-12-13T21:01:42Z"));
        // An expired claim still loads and verifies (time free).
        c.verify().unwrap();

        let mut early = claim();
        early.expires_at = Some("2026-11-13T21:01:42Z".into());
        early.seal().unwrap();
        assert!(
            early
                .verify()
                .unwrap_err()
                .to_string()
                .contains("not after")
        );
        let mut junk = claim();
        junk.expires_at = Some("next week".into());
        junk.seal().unwrap();
        assert!(junk.verify().unwrap_err().to_string().contains("RFC 3339"));
    }

    #[test]
    fn reuse_renews_a_claim_past_half_its_lifetime() {
        let mut old = claim();
        old.expires_at = Some("2026-12-13T21:01:42Z".into());
        old.seal().unwrap();
        old.attach_signature(&personal_sign(old.message().as_bytes()))
            .unwrap();
        // Ten days later, asking for 30 days again: 20 of 30 days left.
        let mut new = claim();
        new.created_at = "2026-11-23T21:01:42Z".into();
        new.started_at = new.created_at.clone();
        new.expires_at = Some("2026-12-23T21:01:42Z".into());
        new.seal().unwrap();
        let (kept, reused) = new.clone().reuse(Some(old.clone()));
        assert!(reused);
        assert_eq!(kept, old);
        // Twenty days later: 10 of 30 days left, so renew.
        let mut late = new.clone();
        late.created_at = "2026-12-03T21:01:42Z".into();
        late.expires_at = Some("2027-01-02T21:01:42Z".into());
        late.seal().unwrap();
        let (kept, reused) = late.clone().reuse(Some(old.clone()));
        assert!(!reused);
        assert_eq!(kept, late);
        // Dropping the expiry is a different statement.
        let mut none = claim();
        none.created_at = "2026-11-23T21:01:42Z".into();
        none.seal().unwrap();
        assert!(!none.reuse(Some(old)).1);
    }

    #[test]
    fn version_0_and_junk_are_not_claims() {
        assert!(
            parse_claim(br#"{"schema":"ts.served-weights/0"}"#)
                .unwrap_err()
                .to_string()
                .contains("unsigned")
        );
        assert!(parse_claim(b"nope").is_err());
        assert!(parse_claim(br#"{"schema":"ts.other/1"}"#).is_err());
    }
}
