// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Mirror lists (`ts.mirrors/1`, spec section 6.12).
//!
//! The same TOML shape serves two purposes: a user maintained local
//! `mirrors.toml`, and a publisher signed list served next to the catalog as
//! `<catalog>/mirrors.toml` plus `mirrors.toml.minisig`, which gives the file
//! backend mirror discovery. Mirrors are untrusted: catalog files are
//! signature checked and every byte is hash checked, so a mirror can only be
//! slow or absent, never harmful.
//!
//! ```toml
//! schema = "ts.mirrors/1"
//! catalog = "tensorseed"
//!
//! [[mirror]]
//! name = "eu-1"
//! provider = "http"        # http | s3 | r2 | b2 | vultr | wasabi | minio | aws | torrent
//! url = "https://mirror.example.org/tensorseed/"
//! priority = 10            # lower is tried first (default 100)
//! ```

use serde::{Deserialize, Serialize};

use crate::minisign::{PublicKey, SecretKey};
use crate::{CatalogError, Result};

/// Schema id.
pub const MIRRORS_SCHEMA: &str = "ts.mirrors/1";
/// File name of the published list (relative to the catalog directory).
pub const MIRRORS_PATH: &str = "mirrors.toml";
/// Largest mirror list accepted.
pub const MAX_MIRRORS_BYTES: usize = 1 << 20;

/// Providers a mirror may name. Everything except `torrent` is read over
/// HTTPS from `url`; the provider is informational (and lets tools show
/// where a mirror lives).
pub const MIRROR_PROVIDERS: &[&str] = &[
    "http", "s3", "r2", "b2", "vultr", "wasabi", "minio", "aws", "torrent",
];

fn default_true() -> bool {
    true
}
fn default_priority() -> i64 {
    100
}
fn default_provider() -> String {
    "http".into()
}

/// One mirror.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mirror {
    /// Short unique name.
    #[serde(default)]
    pub name: String,
    /// Provider (see [`MIRROR_PROVIDERS`]).
    #[serde(default = "default_provider")]
    pub provider: String,
    /// Public base URL of a published tree: catalog files under
    /// `<url><catalog>/` and webseed objects under `<url>t/`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    /// Magnet for `provider = "torrent"` (catalog snapshot torrent; reserved, not used yet).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub magnet: String,
    /// Free text.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    /// Lower is tried first.
    #[serde(default = "default_priority")]
    pub priority: i64,
    /// Disabled mirrors are kept in the file but not used.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Only use this mirror for catalogs signed by this key (minisign base64).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher_pubkey: Option<String>,
}

impl Mirror {
    /// HTTP mirror with defaults.
    pub fn http(name: &str, url: &str) -> Self {
        Self {
            name: name.into(),
            provider: "http".into(),
            url: url.into(),
            magnet: String::new(),
            label: String::new(),
            priority: default_priority(),
            enabled: true,
            publisher_pubkey: None,
        }
    }

    /// The URL with a trailing `/`.
    pub fn base(&self) -> String {
        if self.url.ends_with('/') {
            self.url.clone()
        } else {
            format!("{}/", self.url)
        }
    }

    /// True if this mirror is served over HTTP(S).
    pub fn is_http(&self) -> bool {
        self.provider != "torrent"
    }

    fn validate(&self) -> Result<()> {
        if !MIRROR_PROVIDERS.contains(&self.provider.as_str()) {
            return Err(bad(format!(
                "mirror {:?}: unknown provider {:?}",
                self.name, self.provider
            )));
        }
        if self.is_http() {
            if !(self.url.starts_with("https://") || self.url.starts_with("http://")) {
                return Err(bad(format!(
                    "mirror {:?}: url must be http(s), got {:?}",
                    self.name, self.url
                )));
            }
        } else if !self.magnet.starts_with("magnet:?") {
            return Err(bad(format!(
                "mirror {:?}: torrent mirrors need a magnet",
                self.name
            )));
        }
        if let Some(k) = &self.publisher_pubkey {
            PublicKey::parse(k)?;
        }
        Ok(())
    }

    /// Usable for a catalog pinned to `pinned`: enabled, and either unpinned
    /// or pinned to the same key.
    pub fn usable_for(&self, pinned: Option<&PublicKey>) -> bool {
        if !self.enabled {
            return false;
        }
        match (&self.publisher_pubkey, pinned) {
            (Some(k), Some(p)) => PublicKey::parse(k).is_ok_and(|k| k == *p),
            _ => true,
        }
    }
}

fn bad(m: String) -> CatalogError {
    CatalogError::Config(m)
}

/// A mirror list document.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MirrorList {
    /// [`MIRRORS_SCHEMA`] (optional in local files).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Catalog name (required in published lists).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<String>,
    /// Mirrors.
    #[serde(default, rename = "mirror")]
    pub mirrors: Vec<Mirror>,
}

impl MirrorList {
    /// Parse and validate TOML.
    pub fn parse(text: &str) -> Result<Self> {
        if text.len() > MAX_MIRRORS_BYTES {
            return Err(bad("mirror list larger than 1 MiB".into()));
        }
        let l: Self = toml::from_str(text).map_err(|e| bad(format!("mirror list: {e}")))?;
        if let Some(s) = &l.schema
            && s != MIRRORS_SCHEMA
        {
            return Err(bad(format!(
                "mirror list schema {s:?}, expected {MIRRORS_SCHEMA}"
            )));
        }
        let mut names = std::collections::BTreeSet::new();
        for m in &l.mirrors {
            m.validate()?;
            if !m.name.is_empty() && !names.insert(m.name.clone()) {
                return Err(bad(format!("duplicate mirror name {:?}", m.name)));
            }
        }
        Ok(l)
    }

    /// Serialize to TOML.
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|e| bad(format!("mirror list: {e}")))
    }

    /// Add or replace (by name, else by URL) a mirror.
    pub fn upsert(&mut self, m: Mirror) -> Result<()> {
        m.validate()?;
        let pos = self.mirrors.iter().position(|x| {
            (!m.name.is_empty() && x.name == m.name) || (!m.url.is_empty() && x.url == m.url)
        });
        match pos {
            Some(i) => self.mirrors[i] = m,
            None => self.mirrors.push(m),
        }
        Ok(())
    }

    /// Remove by name or URL. Returns true if something was removed.
    pub fn remove(&mut self, name_or_url: &str) -> bool {
        let before = self.mirrors.len();
        self.mirrors
            .retain(|m| m.name != name_or_url && m.url != name_or_url);
        self.mirrors.len() != before
    }
}

/// Trusted comment for a published list.
pub fn list_comment(catalog: &str, body: &[u8]) -> String {
    format!(
        "{MIRRORS_SCHEMA} catalog={catalog} sha256={}",
        tensorseed_core::hash::sha256_bytes(body)
    )
}

/// Sign a published list: returns (TOML body, minisign signature). The list
/// must name `catalog`.
pub fn sign_list(sk: &SecretKey, catalog: &str, list: &MirrorList) -> Result<(Vec<u8>, String)> {
    let mut l = list.clone();
    l.schema = Some(MIRRORS_SCHEMA.into());
    l.catalog = Some(catalog.into());
    let body = l.to_toml()?.into_bytes();
    MirrorList::parse(std::str::from_utf8(&body).map_err(|e| bad(e.to_string()))?)?;
    let sig = sk.sign(&body, &list_comment(catalog, &body))?;
    Ok((body, sig))
}

/// Verify a published list against the pinned key and catalog name.
pub fn verify_list(pk: &PublicKey, catalog: &str, body: &[u8], sig: &str) -> Result<MirrorList> {
    let tc = pk.verify(body, sig)?;
    if tc != list_comment(catalog, body) {
        return Err(CatalogError::Signature(format!(
            "mirror list trusted comment {tc:?} does not match catalog {catalog:?}"
        )));
    }
    let text = std::str::from_utf8(body).map_err(|e| bad(format!("mirror list: {e}")))?;
    let l = MirrorList::parse(text)?;
    if l.schema.as_deref() != Some(MIRRORS_SCHEMA) || l.catalog.as_deref() != Some(catalog) {
        return Err(CatalogError::Signature(
            "published mirror list must carry schema and catalog".into(),
        ));
    }
    Ok(l)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::minisign::tests::test_key;

    const LIST: &str = r#"
[[mirror]]
name = "a"
url = "https://a.example.org/x"
priority = 5

[[mirror]]
name = "b"
provider = "b2"
url = "https://f000.backblazeb2.com/file/bucket/"

[[mirror]]
name = "t"
provider = "torrent"
magnet = "magnet:?xt=urn:btmh:1220aa"
enabled = false
"#;

    #[test]
    fn parse_edit_and_roundtrip() {
        let mut l = MirrorList::parse(LIST).unwrap();
        assert_eq!(l.mirrors.len(), 3);
        assert_eq!(l.mirrors[1].priority, 100);
        assert_eq!(l.mirrors[0].base(), "https://a.example.org/x/");
        l.upsert(Mirror::http("c", "https://c.example.org/"))
            .unwrap();
        assert!(l.remove("https://a.example.org/x"));
        assert!(!l.remove("nope"));
        let back = MirrorList::parse(&l.to_toml().unwrap()).unwrap();
        assert_eq!(back, l);
        assert!(MirrorList::parse("[[mirror]]\nurl = \"ftp://x\"\n").is_err());
        assert!(
            MirrorList::parse("[[mirror]]\nprovider = \"nope\"\nurl = \"https://x\"\n").is_err()
        );
        assert!(MirrorList::parse("[[mirror]]\nname=\"a\"\nurl=\"https://x\"\n[[mirror]]\nname=\"a\"\nurl=\"https://y\"\n").is_err());
    }

    #[test]
    fn pinned_keys_filter_mirrors() {
        let k1 = test_key(1).public_key();
        let k2 = test_key(2).public_key();
        let mut m = Mirror::http("a", "https://a/");
        assert!(m.usable_for(Some(&k1)));
        m.publisher_pubkey = Some(k1.to_base64());
        assert!(m.usable_for(Some(&k1)));
        assert!(!m.usable_for(Some(&k2)));
        m.enabled = false;
        assert!(!m.usable_for(Some(&k1)));
    }

    #[test]
    fn signed_list_verifies_and_rejects_tampering() {
        let sk = test_key(3);
        let l = MirrorList::parse(LIST).unwrap();
        let (body, sig) = sign_list(&sk, "main", &l).unwrap();
        let v = verify_list(&sk.public_key(), "main", &body, &sig).unwrap();
        assert_eq!(v.mirrors, l.mirrors);
        assert!(verify_list(&sk.public_key(), "other", &body, &sig).is_err());
        assert!(verify_list(&test_key(4).public_key(), "main", &body, &sig).is_err());
        let mut evil = body.clone();
        evil.extend_from_slice(b"\n[[mirror]]\nurl = \"https://evil/\"\n");
        assert!(verify_list(&sk.public_key(), "main", &evil, &sig).is_err());
    }
}
