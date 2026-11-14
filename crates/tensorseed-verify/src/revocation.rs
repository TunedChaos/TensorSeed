// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Claim revocation, `ts.claim-revocations/1` (docs/plans/verified-serving.md,
//! section 3.4).
//!
//! A revocation names one `claim_sha256`, a time, and a reason, and is
//! signed by the claim's own response key (Ed25519 over
//! [`revocation_message`]), by the claim's `seller_wallet` (EIP-191 over the
//! same message), or both. Either key can revoke; neither can take a
//! revocation back, so a leaked response key can at worst revoke the
//! seller's own claim. A revocation is checked against the claim it names,
//! so a list can be copied, merged, and mirrored by anyone without trusting
//! whoever hosts it. Entries that do not verify against the claim are
//! ignored.
//!
//! The seller publishes the list at [`WELL_KNOWN_REVOCATIONS`], next to the
//! claim. A marketplace may also keep its own list.

use ed25519_dalek::Verifier;
use serde::{Deserialize, Serialize};
use tensorseed_core::hash::is_lower_hex;

use crate::claim::{ClaimSig, ServedClaim, recover_eip191};
use crate::response::parse_rfc3339;
use crate::{Error, Result};

/// Schema of a revocation list.
pub const REVOCATIONS_SCHEMA: &str = "ts.claim-revocations/1";
/// First line of the revocation signing message.
pub const REVOCATION_SIG_SCHEMA: &str = "ts.claim-revocation/1";
/// Where the signing proxy publishes the seller's revocation list.
pub const WELL_KNOWN_REVOCATIONS: &str = "/.well-known/tensorseed/revocations.json";
/// Reason that makes every response under the claim untrusted, whenever it
/// was signed.
pub const REASON_KEY_COMPROMISED: &str = "key_compromised";
/// Reasons Tensorseed writes. Readers accept any `[a-z0-9_]{1,32}`.
pub const REASONS: [&str; 4] = ["superseded", "stopped", REASON_KEY_COMPROMISED, "other"];
/// Most entries read from one list.
pub const MAX_REVOCATIONS: usize = 10_000;

fn bad(msg: impl Into<String>) -> Error {
    Error::Claim(msg.into())
}

/// True for a reason that can go into the message.
#[must_use]
pub fn valid_reason(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// The canonical message both signatures cover: fixed field order, a
/// newline after every line including the last.
#[must_use]
pub fn revocation_message(claim_sha256: &str, revoked_at: &str, reason: &str) -> String {
    format!(
        "{REVOCATION_SIG_SCHEMA}\nclaim_sha256={claim_sha256}\nrevoked_at={revoked_at}\nreason={reason}\n"
    )
}

/// One revocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revocation {
    /// The revoked claim.
    pub claim_sha256: String,
    /// When the revocation takes effect (RFC 3339 UTC).
    pub revoked_at: String,
    /// Why (see [`REASONS`]).
    pub reason: String,
    /// Ed25519 signature by the claim's response key over
    /// [`revocation_message`], 128 lowercase hex.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_sig: Option<String>,
    /// EIP-191 signature by the claim's `seller_wallet` over the same
    /// message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wallet_sig: Option<ClaimSig>,
}

/// Which keys signed a revocation that verified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevokedBy {
    /// The claim's response key.
    ResponseKey,
    /// The claim's seller wallet.
    Wallet,
    /// Both.
    Both,
}

impl RevokedBy {
    /// Label for reports.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ResponseKey => "the response key",
            Self::Wallet => "the seller wallet",
            Self::Both => "the response key and the seller wallet",
        }
    }
}

impl Revocation {
    /// The message the signatures cover.
    #[must_use]
    pub fn message(&self) -> String {
        revocation_message(&self.claim_sha256, &self.revoked_at, &self.reason)
    }

    /// `revoked_at` as Unix seconds.
    ///
    /// # Errors
    ///
    /// Fails when `revoked_at` is not RFC 3339.
    pub fn revoked_unix(&self) -> Result<i64> {
        parse_rfc3339(&self.revoked_at).ok_or_else(|| {
            bad(format!(
                "revoked_at {:?} is not an RFC 3339 time",
                self.revoked_at
            ))
        })
    }

    /// From when responses under the claim are untrusted: `i64::MIN` for
    /// [`REASON_KEY_COMPROMISED`], otherwise `revoked_at`.
    ///
    /// # Errors
    ///
    /// Same as [`Self::revoked_unix`].
    pub fn effective_unix(&self) -> Result<i64> {
        let t = self.revoked_unix()?;
        Ok(if self.reason == REASON_KEY_COMPROMISED {
            i64::MIN
        } else {
            t
        })
    }

    /// Attach an externally made EIP-191 wallet signature after checking it
    /// recovers to `wallet`.
    ///
    /// # Errors
    ///
    /// Fails when the signature is malformed or recovers to another address.
    pub fn attach_wallet_sig(&mut self, wallet: &str, sig_hex: &str) -> Result<()> {
        let v = sig_hex.trim().to_ascii_lowercase();
        let got = recover_eip191(self.message().as_bytes(), &v)?;
        if got != wallet {
            return Err(bad(format!(
                "the signature recovers to {got}, not seller_wallet {wallet}"
            )));
        }
        self.wallet_sig = Some(ClaimSig {
            alg: "eip191".into(),
            value: if v.starts_with("0x") {
                v
            } else {
                format!("0x{v}")
            },
        });
        Ok(())
    }

    /// Verify this revocation against the claim it names. Every signature
    /// present must verify, and at least one must be present.
    ///
    /// # Errors
    ///
    /// Fails when it names another claim, a field is malformed, there is
    /// no signature, or a signature does not verify with the claim's keys.
    pub fn verify_for(&self, c: &ServedClaim) -> Result<RevokedBy> {
        if self.claim_sha256 != c.claim_sha256 || !is_lower_hex(&self.claim_sha256, 64) {
            return Err(bad("the revocation names another claim"));
        }
        if !valid_reason(&self.reason) {
            return Err(bad(format!(
                "reason {:?} is not [a-z0-9_]{{1,32}}",
                self.reason
            )));
        }
        self.revoked_unix()?;
        let msg = self.message();
        let by_key = match &self.response_sig {
            None => false,
            Some(s) => {
                let pk = c.response_pubkey()?;
                let vk = ed25519_dalek::VerifyingKey::from_bytes(&pk)
                    .map_err(|_| bad("bad response key"))?;
                let sig: [u8; 64] = (is_lower_hex(s, 128))
                    .then(|| hex::decode(s).ok())
                    .flatten()
                    .and_then(|b| b.try_into().ok())
                    .ok_or_else(|| bad("response_sig is not 128 lowercase hex"))?;
                vk.verify(msg.as_bytes(), &ed25519_dalek::Signature::from_bytes(&sig))
                    .map_err(|_| {
                        bad("response_sig does not verify with the claim's response key")
                    })?;
                true
            }
        };
        let by_wallet = match &self.wallet_sig {
            None => false,
            Some(s) => {
                if s.alg != "eip191" {
                    return Err(bad(format!("wallet_sig.alg {:?} is not eip191", s.alg)));
                }
                let wallet = c
                    .seller_wallet
                    .as_deref()
                    .ok_or_else(|| bad("wallet_sig is present but the claim names no wallet"))?;
                let got = recover_eip191(msg.as_bytes(), &s.value)?;
                if got != wallet {
                    return Err(bad(format!(
                        "wallet_sig recovers to {got}, not seller_wallet {wallet}"
                    )));
                }
                true
            }
        };
        match (by_key, by_wallet) {
            (true, true) => Ok(RevokedBy::Both),
            (true, false) => Ok(RevokedBy::ResponseKey),
            (false, true) => Ok(RevokedBy::Wallet),
            (false, false) => Err(bad("the revocation is not signed")),
        }
    }
}

/// `ts.claim-revocations/1`: a list of revocations, each verified on its
/// own. Unknown fields are ignored so later versions can add some.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Revocations {
    /// [`REVOCATIONS_SCHEMA`].
    pub schema: String,
    /// The entries.
    pub revocations: Vec<Revocation>,
}

impl Default for Revocations {
    fn default() -> Self {
        Self {
            schema: REVOCATIONS_SCHEMA.into(),
            revocations: Vec::new(),
        }
    }
}

/// What a list says about one claim.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Finding {
    /// The earliest verified revocation of the claim, and who signed it.
    pub revoked: Option<(Revocation, RevokedBy)>,
    /// Entries naming the claim that did not verify, with the reason.
    pub ignored: Vec<String>,
}

impl Revocations {
    /// Parse a list.
    ///
    /// # Errors
    ///
    /// Fails on bad JSON, another schema, or more than
    /// [`MAX_REVOCATIONS`] entries.
    pub fn parse(raw: &[u8]) -> Result<Self> {
        let r: Self = serde_json::from_slice(raw)
            .map_err(|e| bad(format!("malformed {REVOCATIONS_SCHEMA}: {e}")))?;
        if r.schema != REVOCATIONS_SCHEMA {
            return Err(bad(format!(
                "schema is {:?}, expected {REVOCATIONS_SCHEMA}",
                r.schema
            )));
        }
        if r.revocations.len() > MAX_REVOCATIONS {
            return Err(bad(format!(
                "more than {MAX_REVOCATIONS} revocations in one list"
            )));
        }
        Ok(r)
    }

    /// Add `r`, replacing an entry for the same claim signed by the same
    /// kinds of key. Entries stay sorted by claim hash, then time.
    pub fn upsert(&mut self, r: Revocation) {
        self.revocations.retain(|o| {
            !(o.claim_sha256 == r.claim_sha256
                && o.response_sig.is_some() == r.response_sig.is_some()
                && o.wallet_sig.is_some() == r.wallet_sig.is_some())
        });
        self.revocations.push(r);
        self.revocations.sort_by(|a, b| {
            (a.claim_sha256.as_str(), a.revoked_at.as_str())
                .cmp(&(b.claim_sha256.as_str(), b.revoked_at.as_str()))
        });
    }

    /// Add every entry of `other` that is not already here.
    pub fn merge(&mut self, other: Self) {
        for r in other.revocations {
            if !self.revocations.contains(&r) {
                self.revocations.push(r);
            }
        }
    }

    /// Check the entries naming `c`. The earliest effective verified one
    /// wins.
    #[must_use]
    pub fn find(&self, c: &ServedClaim) -> Finding {
        let mut f = Finding::default();
        let mut best: Option<(i64, Revocation, RevokedBy)> = None;
        for r in self
            .revocations
            .iter()
            .filter(|r| r.claim_sha256 == c.claim_sha256)
        {
            match r.verify_for(c).and_then(|by| Ok((r.effective_unix()?, by))) {
                Ok((t, by)) => {
                    if best.as_ref().is_none_or(|(bt, _, _)| t < *bt) {
                        best = Some((t, r.clone(), by));
                    }
                }
                Err(e) => f.ignored.push(e.to_string()),
            }
        }
        f.revoked = best.map(|(_, r, by)| (r, by));
        f
    }

    /// From when `c` is revoked (see [`Revocation::effective_unix`]), if a
    /// verified revocation names it. Feeds
    /// [`crate::response::Expect::claim_revoked_unix`].
    #[must_use]
    pub fn revoked_from(&self, c: &ServedClaim) -> Option<i64> {
        self.find(c)
            .revoked
            .and_then(|(r, _)| r.effective_unix().ok())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::claim::fixtures::{HARDHAT0_ADDR, claim, personal_sign, response_key};
    use ed25519_dalek::Signer;

    pub(crate) fn revoke(c: &ServedClaim, at: &str, reason: &str) -> Revocation {
        let msg = revocation_message(&c.claim_sha256, at, reason);
        Revocation {
            claim_sha256: c.claim_sha256.clone(),
            revoked_at: at.into(),
            reason: reason.into(),
            response_sig: Some(hex::encode(response_key().sign(msg.as_bytes()).to_bytes())),
            wallet_sig: None,
        }
    }

    #[test]
    fn message_vector() {
        assert_eq!(
            revocation_message(&"1".repeat(64), "2026-11-20T00:00:00Z", "superseded"),
            format!(
                "ts.claim-revocation/1\nclaim_sha256={}\nrevoked_at=2026-11-20T00:00:00Z\nreason=superseded\n",
                "1".repeat(64)
            )
        );
    }

    #[test]
    fn response_key_and_wallet_can_revoke() {
        let c = claim();
        let r = revoke(&c, "2026-11-20T00:00:00Z", "superseded");
        assert_eq!(r.verify_for(&c).unwrap(), RevokedBy::ResponseKey);

        let mut w = r.clone();
        w.response_sig = None;
        w.attach_wallet_sig(HARDHAT0_ADDR, &personal_sign(w.message().as_bytes()))
            .unwrap();
        assert_eq!(w.verify_for(&c).unwrap(), RevokedBy::Wallet);
        let mut both = r.clone();
        both.wallet_sig = w.wallet_sig.clone();
        assert_eq!(both.verify_for(&c).unwrap(), RevokedBy::Both);

        // Unsigned, edited, or aimed at another claim: not a revocation.
        let mut none = r.clone();
        none.response_sig = None;
        assert!(none.verify_for(&c).is_err());
        let mut edited = r.clone();
        edited.revoked_at = "2026-11-21T00:00:00Z".into();
        assert!(edited.verify_for(&c).is_err());
        let mut other = claim();
        other.served_model_id = "org/other".into();
        other.seal().unwrap();
        assert!(r.verify_for(&other).is_err());
        // A bad extra signature spoils an otherwise good entry.
        let mut spoiled = r.clone();
        spoiled.wallet_sig = Some(ClaimSig {
            alg: "eip191".into(),
            value: personal_sign(b"something else"),
        });
        assert!(spoiled.verify_for(&c).is_err());
    }

    #[test]
    fn lists_find_the_earliest_and_ignore_forgeries() {
        let c = claim();
        let mut list = Revocations::default();
        assert_eq!(list.find(&c), Finding::default());
        list.upsert(revoke(&c, "2026-11-25T00:00:00Z", "superseded"));
        let mut forged = revoke(&c, "2026-11-01T00:00:00Z", "superseded");
        forged.reason = "stopped".into();
        list.revocations.push(forged);
        let f = list.find(&c);
        assert_eq!(f.ignored.len(), 1);
        assert_eq!(
            f.revoked.as_ref().unwrap().0.revoked_at,
            "2026-11-25T00:00:00Z"
        );
        assert_eq!(list.revoked_from(&c), parse_rfc3339("2026-11-25T00:00:00Z"));
        // Replacing with key_compromised makes it effective from the start.
        list.upsert(revoke(&c, "2026-11-26T00:00:00Z", REASON_KEY_COMPROMISED));
        assert_eq!(list.revoked_from(&c), Some(i64::MIN));

        let raw = serde_json::to_vec(&list).unwrap();
        let back = Revocations::parse(&raw).unwrap();
        assert_eq!(back, list);
        assert!(Revocations::parse(br#"{"schema":"ts.x/1","revocations":[]}"#).is_err());
        let mut merged = Revocations::default();
        merged.merge(back.clone());
        merged.merge(back);
        assert_eq!(merged.revocations.len(), list.revocations.len());
    }
}
