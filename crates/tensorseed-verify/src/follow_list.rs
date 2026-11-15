// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Signed follow lists, `ts.follow-list/1`.
//!
//! A follow list is a publisher signed list of catalogs (each one a name,
//! a publisher key, and where to read it) that a marketplace, a community,
//! or a single user can subscribe to instead of collecting `[[catalogs]]`
//! entries by hand. The list only says which catalogs to follow: every
//! catalog is still verified against its own publisher key, so a list can
//! add or drop catalogs but cannot forge a catalog entry.
//!
//! ```json
//! {
//!   "schema": "ts.follow-list/1",
//!   "name": "example-list",
//!   "rev": 3,
//!   "issued_at": "2026-11-14T00:00:00Z",
//!   "catalogs": [
//!     {
//!       "name": "example-catalog",
//!       "label": "Example publisher",
//!       "publisher_pubkey": "RW...",
//!       "base_url": "https://catalog.example.org/",
//!       "store_id": "<64 hex, optional>",
//!       "note": "optional free text"
//!     }
//!   ]
//! }
//! ```
//!
//! The list is published as the JSON file plus a detached minisign
//! signature (`<file>.minisig`) over the exact bytes, whose trusted
//! comment is [`list_comment`]:
//! `ts.follow-list/1 name=<name> rev=<rev> sha256=<hex of the bytes>`.
//! Any minisign tool can check it. A follower pins the list's key itself
//! (the list never carries its own key), keeps the highest `rev` and its
//! hash, and refuses a lower `rev` or different bytes under the same `rev`
//! ([`check_update`]).

use serde::{Deserialize, Serialize};
use tensorseed_core::hash::{is_lower_hex, sha256_bytes};

use crate::minisign::{PublicKey, SecretKey};
use crate::response::parse_rfc3339;
use crate::signed::check_catalog_name;
use crate::{Error, Result};

/// Schema id, also the first word of the trusted comment.
pub const FOLLOW_LIST_SCHEMA: &str = "ts.follow-list/1";
/// Largest list accepted.
pub const MAX_FOLLOW_LIST_BYTES: usize = 1 << 20;
/// Most catalogs in one list.
pub const MAX_FOLLOWED_CATALOGS: usize = 1000;
/// Longest `label`, in bytes.
pub const MAX_LABEL_BYTES: usize = 64;
/// Longest `note`, in bytes.
pub const MAX_NOTE_BYTES: usize = 280;

fn bad(m: impl Into<String>) -> Error {
    Error::Config(format!("follow list: {}", m.into()))
}

/// One catalog in a follow list. The fields are the ones a `[[catalogs]]`
/// entry needs; how strictly to check the chain and the priority stay the
/// follower's choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FollowedCatalog {
    /// Catalog name bound into its signatures.
    pub name: String,
    /// Display name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// The catalog's publisher key (minisign base64).
    pub publisher_pubkey: String,
    /// File catalog HTTP(S) base URL.
    pub base_url: String,
    /// Chia DataLayer store id (64 lowercase hex), when the catalog is
    /// anchored.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub store_id: String,
    /// Free text from the list publisher, for example why it is listed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

fn check_text(what: &str, s: &str, max: usize) -> Result<()> {
    if s.len() > max {
        return Err(bad(format!("{what} longer than {max} bytes")));
    }
    if s.chars().any(char::is_control) {
        return Err(bad(format!("{what} contains control characters")));
    }
    Ok(())
}

impl FollowedCatalog {
    fn validate(&self) -> Result<()> {
        check_catalog_name(&self.name).map_err(|e| bad(e.to_string()))?;
        let at = format!("catalog {:?}", self.name);
        PublicKey::parse(&self.publisher_pubkey)
            .map_err(|e| bad(format!("{at}: publisher_pubkey: {e}")))?;
        if !(self.base_url.starts_with("https://") || self.base_url.starts_with("http://")) {
            return Err(bad(format!("{at}: base_url must be an http(s) URL")));
        }
        url::Url::parse(&self.base_url).map_err(|e| bad(format!("{at}: base_url: {e}")))?;
        if !self.store_id.is_empty() && !is_lower_hex(&self.store_id, 64) {
            return Err(bad(format!("{at}: store_id must be 64 lowercase hex")));
        }
        check_text(&format!("{at}: label"), &self.label, MAX_LABEL_BYTES)?;
        check_text(&format!("{at}: note"), &self.note, MAX_NOTE_BYTES)
    }
}

/// A follow list document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FollowList {
    /// Always [`FOLLOW_LIST_SCHEMA`].
    pub schema: String,
    /// The list's name, 1 to 64 of `[a-z0-9-]` (same rule as a catalog).
    pub name: String,
    /// Revision, 1 or more; every published change raises it.
    pub rev: u64,
    /// When this revision was signed (RFC 3339).
    pub issued_at: String,
    /// Catalogs to follow, unique by name.
    pub catalogs: Vec<FollowedCatalog>,
}

impl FollowList {
    /// An empty revision 1 list.
    #[must_use]
    pub fn new(name: &str, issued_at: &str) -> Self {
        Self {
            schema: FOLLOW_LIST_SCHEMA.into(),
            name: name.into(),
            rev: 1,
            issued_at: issued_at.into(),
            catalogs: Vec::new(),
        }
    }

    /// Check every field.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] naming the first problem.
    pub fn validate(&self) -> Result<()> {
        if self.schema != FOLLOW_LIST_SCHEMA {
            return Err(bad(format!(
                "schema {:?}, expected {FOLLOW_LIST_SCHEMA}",
                self.schema
            )));
        }
        check_catalog_name(&self.name).map_err(|e| bad(format!("list name: {e}")))?;
        if self.rev == 0 {
            return Err(bad("rev must be 1 or more"));
        }
        if parse_rfc3339(&self.issued_at).is_none() {
            return Err(bad(format!(
                "issued_at {:?} is not RFC 3339",
                self.issued_at
            )));
        }
        if self.catalogs.len() > MAX_FOLLOWED_CATALOGS {
            return Err(bad(format!("more than {MAX_FOLLOWED_CATALOGS} catalogs")));
        }
        let mut names = std::collections::BTreeSet::new();
        for c in &self.catalogs {
            c.validate()?;
            if !names.insert(c.name.as_str()) {
                return Err(bad(format!("catalog {:?} is listed twice", c.name)));
            }
        }
        Ok(())
    }

    /// Parse and validate a list (no signature check; see [`verify_list`]).
    ///
    /// # Errors
    ///
    /// [`Error::Config`] for a list that is too large, not JSON in this
    /// shape (unknown fields are refused), or invalid.
    pub fn parse(body: &[u8]) -> Result<Self> {
        if body.len() > MAX_FOLLOW_LIST_BYTES {
            return Err(bad("larger than 1 MiB"));
        }
        let l: Self = serde_json::from_slice(body).map_err(|e| bad(e.to_string()))?;
        l.validate()?;
        Ok(l)
    }

    /// The bytes Tensorseed publishes: pretty JSON with a final newline.
    /// Readers verify the bytes they receive, not this form.
    ///
    /// # Errors
    ///
    /// [`Error::Json`] if serialization fails.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let mut v = serde_json::to_vec_pretty(self)?;
        v.push(b'\n');
        Ok(v)
    }

    /// The listed catalog with this name.
    #[must_use]
    pub fn catalog(&self, name: &str) -> Option<&FollowedCatalog> {
        self.catalogs.iter().find(|c| c.name == name)
    }
}

/// The trusted comment of a list signature.
#[must_use]
pub fn list_comment(name: &str, rev: u64, body: &[u8]) -> String {
    format!(
        "{FOLLOW_LIST_SCHEMA} name={name} rev={rev} sha256={}",
        sha256_bytes(body)
    )
}

/// Sign a list: returns (published bytes, minisign signature).
///
/// # Errors
///
/// [`Error::Config`] for an invalid list and [`Error::Signature`] if
/// signing fails.
pub fn sign_list(sk: &SecretKey, list: &FollowList) -> Result<(Vec<u8>, String)> {
    list.validate()?;
    let body = list.to_bytes()?;
    let sig = sk.sign(&body, &list_comment(&list.name, list.rev, &body))?;
    Ok((body, sig))
}

/// Verify a published list against the key the follower pinned.
///
/// # Errors
///
/// [`Error::Signature`] for a bad or foreign signature or a trusted comment
/// that does not match the bytes, and [`Error::Config`] for an invalid list.
pub fn verify_list(pk: &PublicKey, body: &[u8], sig: &str) -> Result<FollowList> {
    let tc = pk.verify(body, sig)?;
    let list = FollowList::parse(body)?;
    let want = list_comment(&list.name, list.rev, body);
    if tc != want {
        return Err(Error::Signature(format!(
            "follow list trusted comment {tc:?} does not match the list ({want:?})"
        )));
    }
    Ok(list)
}

/// The revision a follower accepted last.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    /// Highest accepted `rev`.
    pub rev: u64,
    /// SHA-256 of the bytes accepted at that revision.
    pub sha256: String,
}

/// How a verified list relates to the one accepted before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Update {
    /// Nothing was accepted before.
    New,
    /// The same revision and bytes as before.
    Same,
    /// A higher revision.
    Newer,
}

/// Check a verified list against the last accepted revision.
///
/// # Errors
///
/// [`Error::Rollback`] for a lower revision and [`Error::Fork`] for
/// different bytes under the accepted revision.
pub fn check_update(seen: Option<&Seen>, list: &FollowList, body: &[u8]) -> Result<Update> {
    let Some(seen) = seen else {
        return Ok(Update::New);
    };
    if list.rev < seen.rev {
        return Err(Error::Rollback {
            seen: seen.rev,
            offered: list.rev,
        });
    }
    if list.rev == seen.rev {
        if sha256_bytes(body) == seen.sha256 {
            return Ok(Update::Same);
        }
        return Err(Error::Fork { revision: list.rev });
    }
    Ok(Update::Newer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::minisign::tests::test_key;

    fn list() -> FollowList {
        let mut l = FollowList::new("example-list", "2026-11-14T00:00:00Z");
        l.catalogs.push(FollowedCatalog {
            name: "alpha".into(),
            label: "Alpha".into(),
            publisher_pubkey: test_key(1).public_key().to_base64(),
            base_url: "https://alpha.example.org/".into(),
            store_id: "ab".repeat(32),
            note: "listed for testing".into(),
        });
        l.catalogs.push(FollowedCatalog {
            name: "beta".into(),
            label: String::new(),
            publisher_pubkey: test_key(2).public_key().to_base64(),
            base_url: "https://beta.example.org/".into(),
            store_id: String::new(),
            note: String::new(),
        });
        l
    }

    #[test]
    fn signed_lists_verify_and_reject_tampering() {
        let sk = test_key(9);
        let (body, sig) = sign_list(&sk, &list()).unwrap();
        let v = verify_list(&sk.public_key(), &body, &sig).unwrap();
        assert_eq!(v, list());
        assert!(sig.contains("trusted comment: ts.follow-list/1 name=example-list rev=1 sha256="));
        // Another key.
        assert!(verify_list(&test_key(8).public_key(), &body, &sig).is_err());
        // Edited bytes.
        let text = String::from_utf8(body.clone()).unwrap();
        let evil = text.replace("beta.example.org", "evil.example.org");
        assert!(verify_list(&sk.public_key(), evil.as_bytes(), &sig).is_err());
        // A signature by the right key whose comment names another rev.
        let mut l2 = list();
        l2.rev = 2;
        let body2 = l2.to_bytes().unwrap();
        let wrong = sk
            .sign(&body2, &list_comment("example-list", 1, &body2))
            .unwrap();
        let e = verify_list(&sk.public_key(), &body2, &wrong).unwrap_err();
        assert!(e.to_string().contains("does not match the list"), "{e}");
        // Bytes signed as something else (a mirror list comment).
        let other = sk.sign(&body, "ts.mirrors/1 catalog=x sha256=00").unwrap();
        assert!(verify_list(&sk.public_key(), &body, &other).is_err());
    }

    #[test]
    fn lists_are_validated() {
        let ok = list();
        ok.validate().unwrap();
        let check = |f: &dyn Fn(&mut FollowList), want: &str| {
            let mut l = list();
            f(&mut l);
            let e = l.validate().unwrap_err().to_string();
            assert!(e.contains(want), "{e} lacks {want}");
        };
        check(&|l| l.schema = "ts.follow-list/2".into(), "schema");
        check(&|l| l.name = "Bad Name".into(), "list name");
        check(&|l| l.rev = 0, "rev must be");
        check(&|l| l.issued_at = "yesterday".into(), "issued_at");
        check(&|l| l.catalogs[1].name = "alpha".into(), "listed twice");
        check(
            &|l| l.catalogs[0].publisher_pubkey = "RWnope".into(),
            "publisher_pubkey",
        );
        check(&|l| l.catalogs[0].base_url = "ftp://x/".into(), "base_url");
        check(&|l| l.catalogs[0].store_id = "AB".repeat(32), "store_id");
        check(&|l| l.catalogs[0].note = "x".repeat(281), "note longer");
        check(&|l| l.catalogs[0].label = "a\nb".into(), "control");
        // Unknown fields are refused, at the top and in a catalog.
        let body = String::from_utf8(ok.to_bytes().unwrap()).unwrap();
        let extra = body.replacen("\"rev\"", "\"extra\": 1,\n  \"rev\"", 1);
        assert!(FollowList::parse(extra.as_bytes()).is_err());
        let extra = body.replacen("\"note\"", "\"priority\": 1,\n      \"note\"", 1);
        assert!(FollowList::parse(extra.as_bytes()).is_err());
        assert_eq!(FollowList::parse(body.as_bytes()).unwrap(), ok);
    }

    #[test]
    fn followers_refuse_rollbacks_and_forks() {
        let l1 = list();
        let b1 = l1.to_bytes().unwrap();
        assert_eq!(check_update(None, &l1, &b1).unwrap(), Update::New);
        let seen = Seen {
            rev: 1,
            sha256: sha256_bytes(&b1),
        };
        assert_eq!(check_update(Some(&seen), &l1, &b1).unwrap(), Update::Same);
        let mut l1b = l1.clone();
        l1b.catalogs.pop();
        let b1b = l1b.to_bytes().unwrap();
        assert!(matches!(
            check_update(Some(&seen), &l1b, &b1b),
            Err(Error::Fork { revision: 1 })
        ));
        let mut l2 = l1b.clone();
        l2.rev = 2;
        let b2 = l2.to_bytes().unwrap();
        assert_eq!(check_update(Some(&seen), &l2, &b2).unwrap(), Update::Newer);
        let seen2 = Seen {
            rev: 2,
            sha256: sha256_bytes(&b2),
        };
        assert!(matches!(
            check_update(Some(&seen2), &l1, &b1),
            Err(Error::Rollback {
                seen: 2,
                offered: 1
            })
        ));
    }
}
