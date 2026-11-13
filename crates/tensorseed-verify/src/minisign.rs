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
//! Secret key files use minisign's format too: base64 of
//! `"Ed" || kdf_alg || "B2" || salt[32] || opslimit[8] || memlimit[8] ||
//! keynum_sk[104]` where `keynum_sk = key_id || seed || pk || checksum`,
//! XORed with scrypt(password) when `kdf_alg` is `"Sc"` (or stored in the
//! clear when it is two zero bytes, as `minisign -W` writes). Key files
//! made by `tensorseed catalog keygen` load in `minisign -S` and the other
//! way round (tested against fixtures from minisign 0.12).

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use blake2::digest::consts::U32;
use blake2::{Blake2b, Blake2b512, Digest};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

use crate::{Error as CatalogError, Result};

const ALG_PUBKEY: [u8; 2] = *b"Ed";
const ALG_PREHASHED: [u8; 2] = *b"ED";
const ALG_LEGACY: [u8; 2] = *b"Ed";
const KDF_SCRYPT: [u8; 2] = *b"Sc";
const KDF_NONE: [u8; 2] = [0, 0];
const CHK_BLAKE2B: [u8; 2] = *b"B2";
const SECRET_FILE_LEN: usize = 158;
const UNTRUSTED_PREFIX: &str = "untrusted comment: ";
const TRUSTED_PREFIX: &str = "trusted comment: ";

fn sig_err(msg: impl Into<String>) -> CatalogError {
    CatalogError::Signature(msg.into())
}

#[cfg(feature = "keygen")]
fn fill_random(buf: &mut [u8]) -> Result<()> {
    getrandom::fill(buf).map_err(|e| sig_err(format!("getrandom: {e}")))
}

#[cfg(not(feature = "keygen"))]
fn fill_random(_buf: &mut [u8]) -> Result<()> {
    Err(sig_err("key generation requires the keygen feature"))
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
    ///
    /// # Errors
    ///
    /// [`CatalogError::Signature`] when the text is not a valid Ed25519
    /// minisign public key.
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
    ///
    /// # Errors
    ///
    /// [`CatalogError::Signature`] for a malformed signature file, a key id
    /// that does not match, or a signature (global or over the trusted comment)
    /// that does not verify.
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

/// scrypt cost of an encrypted key file, in libsodium's
/// `crypto_pwhash_scryptsalsa208sha256` terms (what minisign stores).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfLimits {
    /// Operations limit.
    pub opslimit: u64,
    /// Memory limit in bytes.
    pub memlimit: u64,
}

impl KdfLimits {
    /// minisign's defaults (`OPSLIMIT_SENSITIVE`, `MEMLIMIT_SENSITIVE`):
    /// scrypt N = 2^20, r = 8, p = 1, about 1 GiB of memory.
    pub const MINISIGN_DEFAULT: Self = Self {
        opslimit: 33_554_432,
        memlimit: 1_073_741_824,
    };

    /// Cheap parameters for tests only.
    pub const INSECURE_TEST: Self = Self {
        opslimit: 32_768,
        memlimit: 1 << 20,
    };

    /// libsodium `pickparams`: (log2 N, r, p).
    ///
    /// # Errors
    ///
    /// [`CatalogError::Signature`] when the stored limits are outside what
    /// libsodium would accept.
    pub fn scrypt_params(self) -> Result<(u8, u32, u32)> {
        let opslimit = self.opslimit.max(32_768);
        let r: u64 = 8;
        let pick = |max_n: u64| {
            let mut n_log2 = 1u32;
            while n_log2 < 63 && (1u64 << n_log2) <= max_n / 2 {
                n_log2 += 1;
            }
            n_log2
        };
        let (n_log2, p) = if opslimit < self.memlimit / 32 {
            (pick(opslimit / (r * 4)), 1u64)
        } else {
            let n_log2 = pick(self.memlimit / (r * 128));
            let maxrp = ((opslimit / 4) / (1u64 << n_log2)).min(0x3fff_ffff);
            (n_log2, maxrp / r)
        };
        // Refuse absurd costs from a corrupt or hostile key file.
        if n_log2 > 24 || p == 0 || p > 64 {
            return Err(sig_err(format!(
                "unsupported key file scrypt parameters (N=2^{n_log2}, p={p})"
            )));
        }
        Ok((n_log2 as u8, r as u32, p as u32))
    }
}

#[cfg(feature = "keygen")]
fn kdf_stream(password: &[u8], salt: &[u8], limits: KdfLimits) -> Result<[u8; 104]> {
    let (log_n, r, p) = limits.scrypt_params()?;
    let params =
        scrypt::Params::new(log_n, r, p).map_err(|e| sig_err(format!("scrypt params: {e}")))?;
    let mut out = [0u8; 104];
    scrypt::scrypt(password, salt, &params, &mut out)
        .map_err(|e| sig_err(format!("scrypt: {e}")))?;
    Ok(out)
}

#[cfg(not(feature = "keygen"))]
fn kdf_stream(_password: &[u8], _salt: &[u8], _limits: KdfLimits) -> Result<[u8; 104]> {
    Err(sig_err("encrypted key files require the keygen feature"))
}

fn key_checksum(key_id: &[u8; 8], sk64: &[u8]) -> [u8; 32] {
    let mut h = Blake2b::<U32>::new();
    h.update(ALG_PUBKEY);
    h.update(key_id);
    h.update(sk64);
    h.finalize().into()
}

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    fill_random(&mut b)?;
    Ok(b)
}

/// An in-memory publisher signing key.
#[derive(Clone)]
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

    /// Generate a new random key with a random key id (as minisign does).
    ///
    /// # Errors
    ///
    /// [`CatalogError::Signature`] if the operating system random source fails.
    pub fn generate() -> Result<Self> {
        let seed = random_bytes::<32>()?;
        let key_id = random_bytes::<8>()?;
        Ok(Self {
            key_id,
            key: SigningKey::from_bytes(&seed),
        })
    }

    /// Key id as minisign displays it.
    pub fn key_id_hex(&self) -> String {
        key_id_hex(&self.key_id)
    }

    /// True if a minisign secret key file is password protected.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Signature`] if `text` is not a minisign secret key file.
    pub fn file_is_encrypted(text: &str) -> Result<bool> {
        let raw = decode_secret_file(text)?;
        Ok(raw[2..4] == KDF_SCRYPT)
    }

    /// Serialize as a minisign secret key file. With a password the key is
    /// encrypted with scrypt at `limits`; without one it is stored in the
    /// clear like `minisign -W`.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Signature`] if randomness or key derivation fails.
    pub fn to_minisign_file(&self, password: Option<&[u8]>, limits: KdfLimits) -> Result<String> {
        let mut keynum = Vec::with_capacity(104);
        keynum.extend_from_slice(&self.key_id);
        keynum.extend_from_slice(&self.key.to_bytes());
        keynum.extend_from_slice(self.key.verifying_key().as_bytes());
        let mut raw = Vec::with_capacity(SECRET_FILE_LEN);
        raw.extend_from_slice(&ALG_PUBKEY);
        match password {
            Some(pw) => {
                let chk = key_checksum(&self.key_id, &keynum[8..72]);
                keynum.extend_from_slice(&chk);
                let salt = random_bytes::<32>()?;
                let stream = kdf_stream(pw, &salt, limits)?;
                for (b, k) in keynum.iter_mut().zip(stream.iter()) {
                    *b ^= k;
                }
                raw.extend_from_slice(&KDF_SCRYPT);
                raw.extend_from_slice(&CHK_BLAKE2B);
                raw.extend_from_slice(&salt);
                raw.extend_from_slice(&limits.opslimit.to_le_bytes());
                raw.extend_from_slice(&limits.memlimit.to_le_bytes());
            }
            None => {
                // minisign -W leaves salt, limits, and checksum zeroed.
                keynum.extend_from_slice(&[0u8; 32]);
                raw.extend_from_slice(&KDF_NONE);
                raw.extend_from_slice(&CHK_BLAKE2B);
                raw.extend_from_slice(&[0u8; 48]);
            }
        }
        raw.extend_from_slice(&keynum);
        debug_assert_eq!(raw.len(), SECRET_FILE_LEN);
        let what = if password.is_some() {
            "encrypted"
        } else {
            "unencrypted"
        };
        Ok(format!(
            "{UNTRUSTED_PREFIX}minisign {what} secret key\n{}\n",
            B64.encode(raw)
        ))
    }

    /// Load a minisign secret key file. `password` is required when the
    /// file is encrypted; a wrong password fails the checksum.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Signature`] for a malformed file, a missing or wrong
    /// password (checksum mismatch), or unsupported algorithms.
    pub fn from_minisign_file(text: &str, password: Option<&[u8]>) -> Result<Self> {
        let raw = decode_secret_file(text)?;
        let kdf = [raw[2], raw[3]];
        let mut keynum = [0u8; 104];
        keynum.copy_from_slice(&raw[54..158]);
        if kdf == KDF_SCRYPT {
            let pw = password
                .ok_or_else(|| sig_err("secret key file is encrypted; a password is required"))?;
            let limits = KdfLimits {
                opslimit: u64::from_le_bytes(raw[38..46].try_into().unwrap_or_default()),
                memlimit: u64::from_le_bytes(raw[46..54].try_into().unwrap_or_default()),
            };
            let stream = kdf_stream(pw, &raw[6..38], limits)?;
            for (b, k) in keynum.iter_mut().zip(stream.iter()) {
                *b ^= k;
            }
        } else if kdf != KDF_NONE {
            return Err(sig_err("unknown key derivation in secret key file"));
        }
        let mut key_id = [0u8; 8];
        key_id.copy_from_slice(&keynum[..8]);
        let chk = &keynum[72..104];
        let encrypted = kdf == KDF_SCRYPT;
        if (encrypted || chk.iter().any(|b| *b != 0))
            && key_checksum(&key_id, &keynum[8..72]) != chk
        {
            return Err(sig_err(if encrypted {
                "wrong password (secret key checksum mismatch)"
            } else {
                "secret key checksum mismatch"
            }));
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&keynum[8..40]);
        let key = SigningKey::from_bytes(&seed);
        if key.verifying_key().as_bytes() != &keynum[40..72] {
            return Err(sig_err(
                "secret key file is inconsistent (public half does not match)",
            ));
        }
        Ok(Self { key_id, key })
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
    ///
    /// # Errors
    ///
    /// [`CatalogError::Signature`] if `trusted_comment` contains a line break.
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

fn decode_secret_file(text: &str) -> Result<Vec<u8>> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with(UNTRUSTED_PREFIX))
        .ok_or_else(|| sig_err("empty secret key file"))?;
    let raw = B64
        .decode(line)
        .map_err(|e| sig_err(format!("secret key base64: {e}")))?;
    if raw.len() != SECRET_FILE_LEN || raw[..2] != ALG_PUBKEY || raw[4..6] != CHK_BLAKE2B {
        return Err(sig_err("not a minisign Ed25519 secret key file"));
    }
    Ok(raw)
}

impl PublicKey {
    /// Contents of a minisign `.pub` file for this key.
    pub fn to_minisign_file(&self) -> String {
        format!(
            "{UNTRUSTED_PREFIX}minisign public key {}\n{}\n",
            self.key_id_hex(),
            self.to_base64()
        )
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
    /// Split a minisign signature file into its four lines: untrusted comment,
    /// signature (algorithm, key id, signature), trusted comment, and global
    /// signature over the signature plus trusted comment.
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

    const ENC_KEY: &str = include_str!("../tests/fixtures/minisign-enc.key");
    const ENC_PUB: &str = include_str!("../tests/fixtures/minisign-enc.pub");
    const PLAIN_KEY: &str = include_str!("../tests/fixtures/minisign-plain.key");
    const PLAIN_PUB: &str = include_str!("../tests/fixtures/minisign-plain.pub");

    #[test]
    fn loads_minisign_tool_key_files() {
        // Unencrypted key from `minisign -G -W`.
        assert!(!SecretKey::file_is_encrypted(PLAIN_KEY).unwrap());
        let sk = SecretKey::from_minisign_file(PLAIN_KEY, None).unwrap();
        assert_eq!(sk.public_key(), PublicKey::parse(PLAIN_PUB).unwrap());
        // Encrypted key from `minisign -G` with password "testpass" and
        // minisign's default (1 GiB) scrypt parameters.
        assert!(SecretKey::file_is_encrypted(ENC_KEY).unwrap());
        assert!(SecretKey::from_minisign_file(ENC_KEY, None).is_err());
        let err = SecretKey::from_minisign_file(ENC_KEY, Some(b"wrong")).unwrap_err();
        assert!(err.to_string().contains("wrong password"), "{err}");
        let sk = SecretKey::from_minisign_file(ENC_KEY, Some(b"testpass")).unwrap();
        let pk = PublicKey::parse(ENC_PUB).unwrap();
        assert_eq!(sk.public_key(), pk);
        let sig = sk.sign(b"x", "c").unwrap();
        assert_eq!(pk.verify(b"x", &sig).unwrap(), "c");
    }

    #[test]
    fn key_file_round_trip() {
        let sk = SecretKey::generate().unwrap();
        let enc = sk
            .to_minisign_file(Some(b"pw"), KdfLimits::INSECURE_TEST)
            .unwrap();
        assert!(enc.starts_with("untrusted comment: minisign encrypted secret key\n"));
        let back = SecretKey::from_minisign_file(&enc, Some(b"pw")).unwrap();
        assert_eq!(back.public_key(), sk.public_key());
        assert!(SecretKey::from_minisign_file(&enc, Some(b"nope")).is_err());
        let plain = sk.to_minisign_file(None, KdfLimits::INSECURE_TEST).unwrap();
        let back = SecretKey::from_minisign_file(&plain, None).unwrap();
        assert_eq!(back.public_key(), sk.public_key());
        let pubfile = sk.public_key().to_minisign_file();
        assert_eq!(PublicKey::parse(&pubfile).unwrap(), sk.public_key());
        assert!(pubfile.contains(&sk.key_id_hex()));
        assert_ne!(SecretKey::generate().unwrap().key_id_hex(), sk.key_id_hex());
    }

    #[test]
    fn scrypt_params_match_libsodium() {
        assert_eq!(
            KdfLimits::MINISIGN_DEFAULT.scrypt_params().unwrap(),
            (20, 8, 1)
        );
        assert_eq!(
            KdfLimits::INSECURE_TEST.scrypt_params().unwrap(),
            (10, 8, 1)
        );
        let huge = KdfLimits {
            opslimit: u64::MAX,
            memlimit: u64::MAX,
        };
        assert!(huge.scrypt_params().is_err());
    }

    #[test]
    fn rejects_other_key() {
        let sig = test_key(1).sign(b"data", "c").unwrap();
        let err = test_key(2).public_key().verify(b"data", &sig).unwrap_err();
        assert!(matches!(err, CatalogError::Signature(_)));
    }
}
