// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Minisign compatible Ed25519 signatures (spec section 6.9).
//!
//! Tensorseed signs every catalog entry and every signed-file catalog
//! artifact with one publisher key. The wire format is the minisign format
//! (<https://jedisct1.github.io/minisign/>), so anyone can check a signature
//! with the stock `minisign -V` tool without trusting Tensorseed code.
//!
//! Format summary:
//! - Public key: base64 of `"Ed" || key_id[8] || ed25519_pk[32]`.
//! - Signature file: an untrusted comment line, base64 of
//!   `"ED" || key_id[8] || ed25519_sig[64]` where the signed message is
//!   BLAKE2b-512 of the data (prehashed mode; legacy `"Ed"` signs the raw
//!   data and is accepted for verification only), a `trusted comment:` line,
//!   and base64 of a global signature over `ed25519_sig || trusted_comment`.
//!
//! The trusted comment is covered by the global signature, so Tensorseed puts
//! binding context (catalog id, key, revision, root) there.
//!
//! This module only handles keys already in memory. Loading minisign's
//! scrypt encrypted secret key files is a TODO for M1.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use blake2::{Blake2b512, Digest};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

use crate::{CatalogError, Result};

const ALG_PUBKEY: [u8; 2] = *b"Ed";
const ALG_PREHASHED: [u8; 2] = *b"ED";
const ALG_LEGACY: [u8; 2] = *b"Ed";
const UNTRUSTED_PREFIX: &str = "untrusted comment: ";
const TRUSTED_PREFIX: &str = "trusted comment: ";

fn sig_err(msg: impl Into<String>) -> CatalogError {
    CatalogError::Signature(msg.into())
}

/// A pinned publisher public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey {
    /// Minisign key id (8 bytes, displayed as 16 uppercase hex in little endian order).
    pub key_id: [u8; 8],
    key: VerifyingKey,
}

impl PublicKey {
    /// Parse the base64 line of a minisign public key (for example `RWS...`),
    /// or a full `.pub` file whose second line is that base64 string.
    pub fn parse(text: &str) -> Result<Self> {
        let line = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with(UNTRUSTED_PREFIX))
            .ok_or_else(|| sig_err("empty public key"))?;
        let raw = B64
            .decode(line)
            .map_err(|e| sig_err(format!("public key base64: {e}")))?;
        if raw.len() != 42 || raw[..2] != ALG_PUBKEY {
            return Err(sig_err("not a minisign Ed25519 public key"));
        }
        let mut key_id = [0u8; 8];
        key_id.copy_from_slice(&raw[2..10]);
        let mut pk = [0u8; 32];
        pk.copy_from_slice(&raw[10..42]);
        let key = VerifyingKey::from_bytes(&pk)
            .map_err(|e| sig_err(format!("invalid Ed25519 public key: {e}")))?;
        Ok(Self { key_id, key })
    }

    /// The base64 form used in config files (`catalog.publisher_pubkey`).
    pub fn to_base64(&self) -> String {
        let mut raw = Vec::with_capacity(42);
        raw.extend_from_slice(&ALG_PUBKEY);
        raw.extend_from_slice(&self.key_id);
        raw.extend_from_slice(self.key.as_bytes());
        B64.encode(raw)
    }

    /// Key id as minisign displays it (16 uppercase hex, little endian).
    pub fn key_id_hex(&self) -> String {
        key_id_hex(&self.key_id)
    }

    /// Verify a minisign signature file over `data`.
    ///
    /// Returns the trusted comment on success. Callers must check that the
    /// trusted comment carries the context they expect.
    pub fn verify(&self, data: &[u8], sig_text: &str) -> Result<String> {
        let parsed = SignatureBox::parse(sig_text)?;
        if parsed.key_id != self.key_id {
            return Err(sig_err(format!(
                "signed by key {}, expected pinned key {}",
                key_id_hex(&parsed.key_id),
                self.key_id_hex()
            )));
        }
        let ok = match parsed.alg {
            ALG_PREHASHED => self
                .key
                .verify(&Blake2b512::digest(data), &parsed.signature),
            _ => self.key.verify(data, &parsed.signature),
        };
        ok.map_err(|_| sig_err("signature does not match data"))?;
        let mut global_msg = parsed.signature.to_bytes().to_vec();
        global_msg.extend_from_slice(parsed.trusted_comment.as_bytes());
        self.key
            .verify(&global_msg, &parsed.global_signature)
            .map_err(|_| sig_err("trusted comment signature does not match"))?;
        Ok(parsed.trusted_comment)
    }
}

/// An in-memory publisher signing key.
pub struct SecretKey {
    key_id: [u8; 8],
    key: SigningKey,
}

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretKey")
            .field("key_id", &key_id_hex(&self.key_id))
            .finish_non_exhaustive()
    }
}

impl SecretKey {
    /// Build from a raw 32 byte Ed25519 seed. The key id is derived from the
    /// public key (first 8 bytes of its BLAKE2b-512 hash) so it is stable.
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        let key = SigningKey::from_bytes(seed);
        let digest = Blake2b512::digest(key.verifying_key().as_bytes());
        let mut key_id = [0u8; 8];
        key_id.copy_from_slice(&digest[..8]);
        Self { key_id, key }
    }

    /// The matching public key.
    pub fn public_key(&self) -> PublicKey {
        PublicKey {
            key_id: self.key_id,
            key: self.key.verifying_key(),
        }
    }

    /// Produce a minisign signature file (prehashed `ED` mode) over `data`.
    ///
    /// `trusted_comment` must be a single line.
    pub fn sign(&self, data: &[u8], trusted_comment: &str) -> Result<String> {
        if trusted_comment.contains(['\n', '\r']) {
            return Err(sig_err("trusted comment must be one line"));
        }
        let signature = self.key.sign(&Blake2b512::digest(data));
        let mut sig_raw = Vec::with_capacity(74);
        sig_raw.extend_from_slice(&ALG_PREHASHED);
        sig_raw.extend_from_slice(&self.key_id);
        sig_raw.extend_from_slice(&signature.to_bytes());
        let mut global_msg = signature.to_bytes().to_vec();
        global_msg.extend_from_slice(trusted_comment.as_bytes());
        let global = self.key.sign(&global_msg);
        Ok(format!(
            "{UNTRUSTED_PREFIX}signature from tensorseed key {}\n{}\n{TRUSTED_PREFIX}{trusted_comment}\n{}\n",
            key_id_hex(&self.key_id),
            B64.encode(sig_raw),
            B64.encode(global.to_bytes())
        ))
    }
}

fn key_id_hex(id: &[u8; 8]) -> String {
    id.iter().rev().map(|b| format!("{b:02X}")).collect()
}

struct SignatureBox {
    alg: [u8; 2],
    key_id: [u8; 8],
    signature: Signature,
    trusted_comment: String,
    global_signature: Signature,
}

impl SignatureBox {
    fn parse(text: &str) -> Result<Self> {
        let mut lines = text.lines();
        let first = lines.next().ok_or_else(|| sig_err("empty signature"))?;
        if !first.starts_with(UNTRUSTED_PREFIX) {
            return Err(sig_err("missing untrusted comment line"));
        }
        let sig_line = lines.next().ok_or_else(|| sig_err("missing signature"))?;
        let tc_line = lines
            .next()
            .ok_or_else(|| sig_err("missing trusted comment"))?;
        let global_line = lines
            .next()
            .ok_or_else(|| sig_err("missing global signature"))?;
        let raw = B64
            .decode(sig_line.trim())
            .map_err(|e| sig_err(format!("signature base64: {e}")))?;
        if raw.len() != 74 {
            return Err(sig_err("signature has wrong length"));
        }
        let mut alg = [0u8; 2];
        alg.copy_from_slice(&raw[..2]);
        if alg != ALG_PREHASHED && alg != ALG_LEGACY {
            return Err(sig_err("unknown signature algorithm"));
        }
        let mut key_id = [0u8; 8];
        key_id.copy_from_slice(&raw[2..10]);
        let signature = Signature::from_slice(&raw[10..74])
            .map_err(|e| sig_err(format!("bad signature bytes: {e}")))?;
        let trusted_comment = tc_line
            .strip_prefix(TRUSTED_PREFIX)
            .ok_or_else(|| sig_err("malformed trusted comment line"))?
            .to_owned();
        let global_raw = B64
            .decode(global_line.trim())
            .map_err(|e| sig_err(format!("global signature base64: {e}")))?;
        let global_signature = Signature::from_slice(&global_raw)
            .map_err(|e| sig_err(format!("bad global signature bytes: {e}")))?;
        Ok(Self {
            alg,
            key_id,
            signature,
            trusted_comment,
            global_signature,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Deterministic key for tests only.
    pub(crate) fn test_key(n: u8) -> SecretKey {
        SecretKey::from_seed(&[n; 32])
    }

    const FIXTURE_PUB: &str = include_str!("../tests/fixtures/minisign-test.pub");
    const FIXTURE_MSG: &[u8] = include_bytes!("../tests/fixtures/minisign-test.msg");
    const FIXTURE_SIG: &str = include_str!("../tests/fixtures/minisign-test.msg.minisig");

    #[test]
    fn verifies_signature_made_by_minisign_tool() {
        let pk = PublicKey::parse(FIXTURE_PUB).unwrap();
        assert_eq!(pk.key_id_hex(), "545F782D5F430392");
        let tc = pk.verify(FIXTURE_MSG, FIXTURE_SIG).unwrap();
        assert_eq!(tc, "tensorseed test vector");
    }

    #[test]
    fn rejects_tampered_data_and_comment() {
        let pk = PublicKey::parse(FIXTURE_PUB).unwrap();
        assert!(pk.verify(b"tampered\n", FIXTURE_SIG).is_err());
        let forged = FIXTURE_SIG.replace("tensorseed test vector", "tensorseed forged");
        assert!(pk.verify(FIXTURE_MSG, &forged).is_err());
    }

    #[test]
    fn sign_verify_round_trip_and_pubkey_encoding() {
        let sk = test_key(7);
        let pk = sk.public_key();
        let reparsed = PublicKey::parse(&pk.to_base64()).unwrap();
        assert_eq!(reparsed, pk);
        let sig = sk.sign(b"hello", "ctx=1").unwrap();
        assert_eq!(pk.verify(b"hello", &sig).unwrap(), "ctx=1");
        assert!(sk.sign(b"x", "two\nlines").is_err());
    }

    #[test]
    fn rejects_other_key() {
        let sig = test_key(1).sign(b"data", "c").unwrap();
        let err = test_key(2).public_key().verify(b"data", &sig).unwrap_err();
        assert!(matches!(err, CatalogError::Signature(_)));
    }
}
