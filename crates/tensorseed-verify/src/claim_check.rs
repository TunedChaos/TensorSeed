// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Everything a buyer or marketplace checks about a seller's
//! `ts.served-weights/1` claim before trusting it
//! (docs/plans/verified-serving.md, section 3.2), without any I/O.
//!
//! The claim is checked on its own ([`check_document`]: `claim_sha256`
//! under the canonical rule, the fields, the optional wallet signature, and
//! expiry), against where it was fetched ([`check_origin`]:
//! `public_base_url` must be the place the claim was read from), against
//! revocations ([`check_revocation`]), and against a catalog the checker
//! trusts ([`check_catalog`]: the signed manifest named by the claim, its
//! files, takedowns, freshness, and a license mark recomputed from the
//! catalog rather than taken from the seller). [`build_report`] runs them
//! all and fills a [`ClaimReport`] (`ts.claim-check/1`).
//!
//! Fetching the claim, the revocation list, and the catalog entry is the
//! caller's job: `tensorseed check-claim` does it over HTTP with a synced
//! catalog, the npm package does it with `fetch` in a browser or Node.
//!
//! None of this proves which weights answer requests; logprob challenges
//! (`check-endpoint`) do that. It proves the claim is well formed, signed
//! by the wallet it names, current, and describes a real catalog entry.

use serde::{Deserialize, Serialize};
use tensorseed_core::commercial::{CommercialServing, assess_license_info};
use tensorseed_core::manifest::{LicenseClass, Manifest, Tombstone};

pub use crate::claim::parse_claim;
use crate::claim::{Entrypoint, ServedClaim, WELL_KNOWN_CLAIM, normalize_wallet, recover_eip191};
use crate::manifest::tombstone_covers;
use crate::revocation::{REASON_KEY_COMPROMISED, Revocations, WELL_KNOWN_REVOCATIONS};

/// Schema of `check-claim --json`.
pub const CLAIM_CHECK_SCHEMA: &str = "ts.claim-check/1";

/// Outcome of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The check passed.
    Pass,
    /// The check failed; the claim should not be trusted.
    Fail,
    /// Worth knowing, not a failure.
    Warn,
    /// Not checked (no input for it).
    Skip,
}

impl Status {
    /// `pass`, `fail`, `warn`, or `skip`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Warn => "warn",
            Self::Skip => "skip",
        }
    }
}

/// One named check and what it found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    /// Stable name, for example `claim_sha256`.
    pub name: String,
    /// Outcome.
    pub status: Status,
    /// One line explanation.
    pub detail: String,
}

/// A check result.
#[must_use]
pub fn check(name: &str, status: Status, detail: impl Into<String>) -> Check {
    Check {
        name: name.into(),
        status,
        detail: detail.into(),
    }
}

/// Overall result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimVerdict {
    /// Every check passed (warnings allowed).
    Pass,
    /// At least one check failed.
    Fail,
    /// A check could not run (for example the catalog was unreachable).
    Inconclusive,
}

impl ClaimVerdict {
    /// Process exit code: 0 pass, 1 fail, 2 inconclusive.
    #[must_use]
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Pass => 0,
            Self::Fail => 1,
            Self::Inconclusive => 2,
        }
    }

    /// `pass`, `fail`, or `inconclusive`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Inconclusive => "inconclusive",
        }
    }
}

/// Where the claim was read from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "location", rename_all = "snake_case")]
pub enum Origin {
    /// Fetched over HTTP(S) from this URL (after redirects).
    Url(String),
    /// Read from a local file.
    File(String),
}

/// What the checker's own trusted catalog says about the claimed entry.
#[derive(Debug, Clone)]
pub struct CatalogMatch {
    /// Catalog name (bound into its signatures).
    pub name: String,
    /// Key id (hex) of the publisher key pinned for that catalog.
    pub publisher_key_id: String,
    /// The signature verified manifest for the claimed revision.
    pub manifest: Manifest,
    /// SHA-256 of the signed manifest value.
    pub manifest_sha256: String,
    /// Catalog key of the manifest.
    pub manifest_key: String,
    /// Tombstone for the repo, if any.
    pub tombstone: Option<Tombstone>,
    /// Freshness of the catalog copy the manifest came from.
    pub freshness: Freshness,
}

/// How current the catalog copy was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Freshness {
    /// True when the catalog was synced during this check (an expired or
    /// stale head is refused by the sync).
    pub synced: bool,
    /// Local catalog revision after the sync (file catalogs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    /// Chain check mode in effect (`off`, `warn`, `strict`).
    pub chain_check: String,
    /// The last successful chain check: anchored revision, coin state, and
    /// its age in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain: Option<ChainSeen>,
}

/// Summary of the last successful chain check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChainSeen {
    /// Anchored file catalog revision.
    pub revision: u64,
    /// Coin state label.
    pub coin_state: String,
    /// Seconds since the check.
    pub age_secs: u64,
}

/// The license mark recomputed from the catalog manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicenseMark {
    /// License id the mark was derived from.
    pub id: Option<String>,
    /// Publisher license class.
    pub class: LicenseClass,
    /// The recomputed mark.
    pub commercial_serving: CommercialServing,
    /// Why.
    pub note: String,
    /// What the claim says, for comparison.
    pub claimed: CommercialServing,
    /// The seller asserted rights (`--i-have-rights`) for this serving.
    pub rights_asserted: bool,
}

/// `ts.claim-check/1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimReport {
    /// [`CLAIM_CHECK_SCHEMA`].
    pub schema: String,
    /// What was asked for (URL or path).
    pub source: String,
    /// Where the claim was actually read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    /// `claim_sha256` as written in the claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_sha256: Option<String>,
    /// Repo id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// Revision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// Entrypoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<Entrypoint>,
    /// Served model id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub served_model_id: Option<String>,
    /// Public base URL from the claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_base_url: Option<String>,
    /// `expires_at` as written in the claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// True when a verified revocation of the claim is in effect.
    #[serde(default)]
    pub revoked: bool,
    /// Seller wallet as written in the claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seller_wallet: Option<String>,
    /// True only when the wallet signature over `claim_sha256` recovered
    /// to `seller_wallet`.
    #[serde(default)]
    pub wallet_verified: bool,
    /// Response public key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_pubkey: Option<String>,
    /// Catalog the claim was matched against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog: Option<String>,
    /// Catalog freshness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness: Option<Freshness>,
    /// License mark recomputed from the catalog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<LicenseMark>,
    /// Every check in order.
    pub checks: Vec<Check>,
    /// Overall verdict.
    pub verdict: ClaimVerdict,
    /// Counts by status.
    pub counts: Counts,
}

/// Number of checks by status.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    /// Passed.
    pub pass: usize,
    /// Failed.
    pub fail: usize,
    /// Warnings.
    pub warn: usize,
    /// Not checked.
    pub skip: usize,
}

/// Options that change what counts as a failure.
#[derive(Debug, Clone, Default)]
pub struct ClaimCheckOptions {
    /// Fail a claim that has no wallet signature.
    pub require_wallet_sig: bool,
    /// The wallet the claim must name (any case).
    pub expect_wallet: Option<String>,
    /// For a claim read from a file: the public base URL it must name.
    pub expect_url: Option<String>,
    /// Fail when the catalog copy was not synced during the check.
    pub require_fresh: bool,
    /// Fail a claim that has no `expires_at`.
    pub require_expiry: bool,
    /// The time to check expiry and revocation at (Unix seconds). Set it;
    /// the default of 0 makes every claim look current.
    pub now_unix: i64,
}

impl ClaimReport {
    /// A report with no claim details yet.
    #[must_use]
    pub fn new(source: &str) -> Self {
        Self {
            schema: CLAIM_CHECK_SCHEMA.into(),
            source: source.into(),
            origin: None,
            claim_sha256: None,
            repo: None,
            revision: None,
            entrypoint: None,
            served_model_id: None,
            public_base_url: None,
            expires_at: None,
            revoked: false,
            seller_wallet: None,
            wallet_verified: false,
            response_pubkey: None,
            catalog: None,
            freshness: None,
            license: None,
            checks: Vec::new(),
            verdict: ClaimVerdict::Inconclusive,
            counts: Counts::default(),
        }
    }

    /// Fill in the claim's own fields.
    pub fn describe(&mut self, c: &ServedClaim) {
        self.claim_sha256 = Some(c.claim_sha256.clone());
        self.repo = Some(c.repo.clone());
        self.revision = Some(c.revision.clone());
        self.entrypoint = Some(c.entrypoint.clone());
        self.served_model_id = Some(c.served_model_id.clone());
        self.public_base_url.clone_from(&c.public_base_url);
        self.expires_at.clone_from(&c.expires_at);
        self.seller_wallet.clone_from(&c.seller_wallet);
        self.response_pubkey = Some(c.response_key.pubkey.clone());
    }

    /// Add checks.
    pub fn extend(&mut self, checks: impl IntoIterator<Item = Check>) {
        self.checks.extend(checks);
    }

    /// Count the checks and set the verdict: any `fail` is `fail`;
    /// otherwise an `inconclusive` marker (a `catalog` check that could not
    /// run) makes the run inconclusive; otherwise `pass`.
    pub fn finish(&mut self, inconclusive: bool) {
        let mut n = Counts::default();
        for c in &self.checks {
            match c.status {
                Status::Pass => n.pass += 1,
                Status::Fail => n.fail += 1,
                Status::Warn => n.warn += 1,
                Status::Skip => n.skip += 1,
            }
        }
        self.counts = n;
        self.verdict = if n.fail > 0 {
            ClaimVerdict::Fail
        } else if inconclusive {
            ClaimVerdict::Inconclusive
        } else {
            ClaimVerdict::Pass
        };
    }
}

/// Checks on the claim document alone: `claim_sha256`, the fields, and
/// the wallet signature.
#[must_use]
pub fn check_document(c: &ServedClaim, opts: &ClaimCheckOptions) -> Vec<Check> {
    let mut out = Vec::new();
    let hash_ok = match c.compute_hash() {
        Ok(h) if h == c.claim_sha256 => {
            out.push(check(
                "claim_sha256",
                Status::Pass,
                format!(
                    "{h} is the SHA-256 of the canonical JSON without claim_sha256, sig, and signed"
                ),
            ));
            true
        }
        Ok(h) => {
            out.push(check(
                "claim_sha256",
                Status::Fail,
                format!(
                    "the claim says {} but its contents hash to {h}; it was changed after it was written",
                    c.claim_sha256
                ),
            ));
            false
        }
        Err(e) => {
            out.push(check("claim_sha256", Status::Fail, format!("{e:#}")));
            false
        }
    };
    // The fields, checked without the wallet signature so the two results
    // are reported apart (sig and signed are outside claim_sha256).
    let mut bare = c.clone();
    bare.sig = None;
    bare.signed = false;
    if hash_ok {
        match bare.verify() {
            Ok(_) => out.push(check(
                "claim_fields",
                Status::Pass,
                "schema, response key, model id, wallet, and expires_at formats are valid",
            )),
            Err(e) => out.push(check("claim_fields", Status::Fail, format!("{e:#}"))),
        }
    } else {
        out.push(check(
            "claim_fields",
            Status::Skip,
            "not checked: claim_sha256 does not match",
        ));
    }
    out.push(if hash_ok {
        expiry_check(c, opts)
    } else {
        check(
            "expiry",
            Status::Skip,
            "not checked: claim_sha256 does not match",
        )
    });
    if hash_ok {
        out.push(wallet_check(c, opts));
    } else {
        // The signature covers the stated hash, not these contents.
        out.push(check(
            "wallet_signature",
            Status::Skip,
            "not checked: a signature over claim_sha256 says nothing about contents that hash differently",
        ));
    }
    out
}

fn wallet_check(c: &ServedClaim, opts: &ClaimCheckOptions) -> Check {
    const N: &str = "wallet_signature";
    if let Some(want) = &opts.expect_wallet {
        let want = match normalize_wallet(want) {
            Ok(w) => w,
            Err(e) => return check(N, Status::Fail, format!("--expect-wallet: {e:#}")),
        };
        if c.seller_wallet.as_deref() != Some(want.as_str()) {
            return check(
                N,
                Status::Fail,
                format!(
                    "the claim names wallet {}, not the expected {want}",
                    c.seller_wallet.as_deref().unwrap_or("(none)")
                ),
            );
        }
    }
    let Some(sig) = &c.sig else {
        if c.signed {
            return check(N, Status::Fail, "signed is true but there is no sig");
        }
        let status = if opts.require_wallet_sig {
            Status::Fail
        } else {
            Status::Warn
        };
        return check(
            N,
            status,
            match &c.seller_wallet {
                Some(w) => format!("not signed; {w} is only plain data until the wallet signs"),
                None => "not signed and no seller_wallet".into(),
            },
        );
    };
    if !c.signed {
        return check(N, Status::Fail, "sig is present but signed is false");
    }
    if sig.alg != "eip191" {
        return check(
            N,
            Status::Fail,
            format!("sig.alg {:?} is not supported (eip191)", sig.alg),
        );
    }
    let Some(wallet) = &c.seller_wallet else {
        return check(N, Status::Fail, "sig is present but seller_wallet is not");
    };
    match recover_eip191(c.message().as_bytes(), &sig.value) {
        Ok(got) if &got == wallet => check(
            N,
            Status::Pass,
            format!("EIP-191 signature over claim_sha256 recovers to {got}"),
        ),
        Ok(got) => check(
            N,
            Status::Fail,
            format!("the wallet signature recovers to {got}, not seller_wallet {wallet}"),
        ),
        Err(e) => check(N, Status::Fail, format!("bad wallet signature: {e:#}")),
    }
}

/// `scheme://host[:port]` plus path, lowercased scheme and host, no
/// trailing slash. `None` when `u` is not an http(s) URL.
fn norm_url(u: &str) -> Option<url::Url> {
    let p = url::Url::parse(u.trim()).ok()?;
    matches!(p.scheme(), "http" | "https").then_some(p)
}

/// Where a claim for `public_base_url` is published: the base URL without
/// a trailing `/` or `/v1`, plus [`WELL_KNOWN_CLAIM`].
#[must_use]
pub fn well_known_for(public_base_url: &str) -> String {
    let root = public_base_url.trim().trim_end_matches('/');
    let root = root.strip_suffix("/v1").unwrap_or(root);
    format!("{root}{WELL_KNOWN_CLAIM}")
}

fn same_place(a: &url::Url, b: &url::Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
        && a.path().trim_end_matches('/') == b.path().trim_end_matches('/')
}

fn same_origin(a: &url::Url, b: &url::Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

/// The claim's `public_base_url` must be where it was fetched: the
/// fetched URL must be `well_known_for(public_base_url)`. For a file,
/// `expect_url` (if given) must equal `public_base_url`.
#[must_use]
pub fn check_origin(c: &ServedClaim, origin: &Origin, opts: &ClaimCheckOptions) -> Vec<Check> {
    const N: &str = "public_url";
    let mut out = Vec::new();
    let Some(public) = &c.public_base_url else {
        out.push(check(
            N,
            Status::Fail,
            "the claim has no public_base_url, so it cannot be tied to an endpoint",
        ));
        return out;
    };
    let Some(pu) = norm_url(public) else {
        out.push(check(
            N,
            Status::Fail,
            format!("public_base_url {public:?} is not an http(s) URL"),
        ));
        return out;
    };
    match origin {
        Origin::Url(fetched) => {
            let want = well_known_for(public);
            match (norm_url(fetched), norm_url(&want)) {
                (Some(f), Some(w)) if same_place(&f, &w) => out.push(check(
                    N,
                    Status::Pass,
                    format!("fetched from {fetched}, where {public} publishes its claim"),
                )),
                (Some(f), Some(_)) if same_origin(&f, &pu) && !f.path().ends_with(WELL_KNOWN_CLAIM) => {
                    out.push(check(
                        N,
                        Status::Warn,
                        format!(
                            "fetched from {fetched}, on the same origin as {public} but not at {want}"
                        ),
                    ));
                }
                _ => out.push(check(
                    N,
                    Status::Fail,
                    format!(
                        "fetched from {fetched}, but the claim is for {public} (published at {want}); a claim copied from another endpoint"
                    ),
                )),
            }
        }
        Origin::File(path) => match &opts.expect_url {
            Some(e) => match norm_url(e) {
                Some(eu) if same_place(&eu, &pu) => {
                    out.push(check(N, Status::Pass, format!("{public} is the expected URL")));
                }
                _ => out.push(check(
                    N,
                    Status::Fail,
                    format!("the claim is for {public}, not the expected {e}"),
                )),
            },
            None => out.push(check(
                N,
                Status::Skip,
                format!(
                    "read from {path}; pass --expect-url or check the URL instead to tie it to {public}"
                ),
            )),
        },
    }
    if pu.scheme() != "https" {
        out.push(check(
            "https",
            Status::Warn,
            format!("public_base_url {public} is not https"),
        ));
    }
    out
}

/// Checks against the checker's own catalog entry.
#[must_use]
pub fn check_catalog(
    c: &ServedClaim,
    m: &CatalogMatch,
    opts: &ClaimCheckOptions,
) -> (Vec<Check>, LicenseMark) {
    let mut out = Vec::new();
    out.push(if c.catalog.name == m.name {
        check(
            "catalog",
            Status::Pass,
            format!("catalog {} is one you trust", m.name),
        )
    } else {
        check(
            "catalog",
            Status::Fail,
            format!(
                "the claim names catalog {}, checked against {}",
                c.catalog.name, m.name
            ),
        )
    });
    out.push(match &c.catalog.publisher_key_id {
        Some(k) if k.eq_ignore_ascii_case(&m.publisher_key_id) => check(
            "publisher_key",
            Status::Pass,
            format!("publisher key {k} is the key pinned for {}", m.name),
        ),
        Some(k) => check(
            "publisher_key",
            Status::Fail,
            format!(
                "the claim names publisher key {k}, but {} is pinned to {}",
                m.name, m.publisher_key_id
            ),
        ),
        None => check(
            "publisher_key",
            Status::Warn,
            format!(
                "the claim names no publisher key; {} is pinned to {}",
                m.name, m.publisher_key_id
            ),
        ),
    });
    let id_ok = m.manifest.repo.id == c.repo
        && m.manifest.repo.repo_type == c.repo_type
        && m.manifest.hf_commit == c.revision;
    let key_ok = m.manifest_key == c.catalog.manifest_key;
    let sha_ok = m.manifest_sha256 == c.catalog.manifest_sha256;
    out.push(if id_ok && key_ok && sha_ok {
        check(
            "manifest",
            Status::Pass,
            format!(
                "{}@{} is in {} with manifest sha256 {}",
                c.repo, c.revision, m.name, m.manifest_sha256
            ),
        )
    } else {
        let mut why = Vec::new();
        if !id_ok {
            why.push(format!(
                "the catalog manifest is {}@{}",
                m.manifest.repo.id, m.manifest.hf_commit
            ));
        }
        if !key_ok {
            why.push(format!(
                "manifest_key {} is not {}",
                c.catalog.manifest_key, m.manifest_key
            ));
        }
        if !sha_ok {
            why.push(format!(
                "manifest_sha256 {} is not the catalog's {}",
                c.catalog.manifest_sha256, m.manifest_sha256
            ));
        }
        check("manifest", Status::Fail, why.join("; "))
    });
    out.push(files_check(c, &m.manifest));
    out.push(match &m.tombstone {
        Some(t) if tombstone_covers(t, &c.revision) => check(
            "takedown",
            Status::Fail,
            format!(
                "{}@{} is taken down ({:?}: {})",
                c.repo, c.revision, t.reason, t.detail
            ),
        ),
        _ => check("takedown", Status::Pass, "no takedown covers this revision"),
    });
    out.push(freshness_check(&m.freshness, opts));
    let a = assess_license_info(&m.manifest.license);
    let mark = LicenseMark {
        id: a.license.clone(),
        class: m.manifest.license.class,
        commercial_serving: a.value,
        note: a.note.clone(),
        claimed: c.license.commercial_serving,
        rights_asserted: c.license.rights_asserted,
    };
    let mut bad = Vec::new();
    if c.license.commercial_serving != a.value {
        bad.push(format!(
            "the claim says {} but the catalog license gives {}",
            c.license.commercial_serving.as_str(),
            a.value.as_str()
        ));
    }
    if c.license.class != m.manifest.license.class {
        bad.push(format!(
            "the claim says class {:?} but the catalog says {:?}",
            c.license.class, m.manifest.license.class
        ));
    }
    if c.license.id != a.license {
        bad.push(format!(
            "the claim says license {} but the catalog says {}",
            c.license.id.as_deref().unwrap_or("(none)"),
            a.license.as_deref().unwrap_or("(none)")
        ));
    }
    out.push(if bad.is_empty() {
        let id = a.license.as_deref().unwrap_or("(none)");
        match a.value {
            CommercialServing::Allowed => check(
                "license",
                Status::Pass,
                format!("{id}: allowed (recomputed from the catalog)"),
            ),
            CommercialServing::Forbidden if c.license.rights_asserted => check(
                "license",
                Status::Warn,
                format!(
                    "{id}: forbidden for commercial serving; the seller asserted separate rights (--i-have-rights)"
                ),
            ),
            v => check(
                "license",
                Status::Warn,
                format!("{id}: {} (recomputed from the catalog): {}", v.as_str(), a.note),
            ),
        }
    } else {
        check("license", Status::Fail, bad.join("; "))
    });
    (out, mark)
}

fn files_check(c: &ServedClaim, m: &Manifest) -> Check {
    const N: &str = "files";
    if c.files.is_empty() {
        return check(N, Status::Fail, "the claim lists no files");
    }
    let by_path: std::collections::HashMap<&str, &tensorseed_core::manifest::FileEntry> =
        m.files.iter().map(|f| (f.path.as_str(), f)).collect();
    let mut bad = Vec::new();
    for f in &c.files {
        match by_path.get(f.path.as_str()) {
            None => bad.push(format!("{} is not in the manifest", f.path)),
            Some(e) if e.sha256 != f.sha256 || e.size != f.size => bad.push(format!(
                "{} has sha256 {} size {} in the claim but {} size {} in the manifest",
                f.path, f.sha256, f.size, e.sha256, e.size
            )),
            Some(_) => {}
        }
    }
    match &c.entrypoint {
        Entrypoint::Gguf(p) => {
            if !c.files.iter().any(|f| &f.path == p) {
                bad.push(format!("entrypoint {p} is not among the claimed files"));
            }
        }
        Entrypoint::Directory => {
            let have: std::collections::HashSet<&str> =
                c.files.iter().map(|f| f.path.as_str()).collect();
            let missing: Vec<&str> = m
                .files
                .iter()
                .filter(|f| {
                    let l = f.path.to_ascii_lowercase();
                    (l.ends_with(".safetensors") || l == "config.json")
                        && !have.contains(f.path.as_str())
                })
                .map(|f| f.path.as_str())
                .take(4)
                .collect();
            if !missing.is_empty() {
                bad.push(format!(
                    "the directory entrypoint is missing manifest weights: {}",
                    missing.join(", ")
                ));
            }
        }
    }
    if bad.is_empty() {
        check(
            N,
            Status::Pass,
            format!(
                "{} file(s) match the manifest by path, size, and sha256",
                c.files.len()
            ),
        )
    } else {
        let more = if bad.len() > 4 {
            format!(" (and {} more)", bad.len() - 4)
        } else {
            String::new()
        };
        bad.truncate(4);
        check(N, Status::Fail, format!("{}{more}", bad.join("; ")))
    }
}

fn freshness_check(f: &Freshness, opts: &ClaimCheckOptions) -> Check {
    const N: &str = "freshness";
    let rev = f
        .revision
        .map(|r| format!(" at revision {r}"))
        .unwrap_or_default();
    let chain = match &f.chain {
        Some(c) => format!(
            "; chain check ({}) anchored revision {}, coin {}, {}s ago",
            f.chain_check, c.revision, c.coin_state, c.age_secs
        ),
        None => format!("; chain check {}", f.chain_check),
    };
    if f.synced {
        check(
            N,
            Status::Pass,
            format!("catalog synced during this check{rev} (an expired head is refused){chain}"),
        )
    } else {
        check(
            N,
            if opts.require_fresh {
                Status::Fail
            } else {
                Status::Warn
            },
            format!("local catalog copy{rev}, not synced during this check{chain}"),
        )
    }
}

/// `3d 4h`, `5h 2m`, `40s`: a short rough duration for reports.
#[must_use]
pub fn rough_duration(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3_600, secs % 3_600 / 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{secs}s")
    }
}

fn expiry_check(c: &ServedClaim, opts: &ClaimCheckOptions) -> Check {
    const N: &str = "expiry";
    match c.expires_unix() {
        Err(e) => check(N, Status::Fail, e.to_string()),
        Ok(None) => check(
            N,
            if opts.require_expiry {
                Status::Fail
            } else {
                Status::Warn
            },
            "no expires_at; the claim stays valid until it is revoked or replaced",
        ),
        Ok(Some(x)) => {
            let at = c.expires_at.as_deref().unwrap_or_default();
            if opts.now_unix > x {
                check(
                    N,
                    Status::Fail,
                    format!(
                        "expired at {at}, {} ago; the seller must publish a fresh claim",
                        rough_duration(opts.now_unix.abs_diff(x))
                    ),
                )
            } else {
                check(
                    N,
                    Status::Pass,
                    format!(
                        "expires at {at}, in {}",
                        rough_duration(x.abs_diff(opts.now_unix))
                    ),
                )
            }
        }
    }
}

/// Where a seller publishes revocations for `public_base_url`: the base
/// URL without a trailing `/` or `/v1`, plus [`WELL_KNOWN_REVOCATIONS`].
#[must_use]
pub fn well_known_revocations_for(public_base_url: &str) -> String {
    let root = public_base_url.trim().trim_end_matches('/');
    let root = root.strip_suffix("/v1").unwrap_or(root);
    format!("{root}{WELL_KNOWN_REVOCATIONS}")
}

/// The revocation list next to a claim fetched from `claim_url`, when the
/// claim was read from its well-known path.
#[must_use]
pub fn revocations_url_next_to(claim_url: &str) -> Option<String> {
    let u = url::Url::parse(claim_url).ok()?;
    if !u.path().ends_with(WELL_KNOWN_CLAIM) {
        return None;
    }
    let mut r = u.clone();
    let p = u.path();
    r.set_path(&format!(
        "{}{WELL_KNOWN_REVOCATIONS}",
        &p[..p.len() - WELL_KNOWN_CLAIM.len()]
    ));
    r.set_query(None);
    r.set_fragment(None);
    Some(r.to_string())
}

/// What the checker knows about revocations of the claim.
#[derive(Debug, Clone)]
pub enum RevocationInput {
    /// No list was looked at; the reason is reported.
    NotChecked(String),
    /// The seller publishes no list (HTTP 404 at the given URL).
    Absent(String),
    /// One or more lists, merged; `from` says where they came from.
    List {
        /// The merged entries.
        list: Revocations,
        /// URLs or paths, for the report.
        from: String,
    },
    /// A list could not be read; reported as a warning.
    Unavailable(String),
}

/// Check the claim against revocations. A verified revocation in effect
/// fails the claim; one with reason `key_compromised` fails it whatever its
/// time; one dated in the future is a warning.
#[must_use]
pub fn check_revocation(c: &ServedClaim, r: &RevocationInput, opts: &ClaimCheckOptions) -> Check {
    const N: &str = "revocation";
    let (list, from) = match r {
        RevocationInput::NotChecked(why) => return check(N, Status::Skip, why.clone()),
        RevocationInput::Absent(url) => {
            return check(N, Status::Pass, format!("{url} publishes no revocations"));
        }
        RevocationInput::Unavailable(e) => {
            return check(N, Status::Warn, format!("could not read revocations: {e}"));
        }
        RevocationInput::List { list, from } => (list, from),
    };
    let f = list.find(c);
    let ignored = if f.ignored.is_empty() {
        String::new()
    } else {
        format!(
            "; ignored {} entr{} naming this claim that did not verify ({})",
            f.ignored.len(),
            if f.ignored.len() == 1 { "y" } else { "ies" },
            f.ignored[0]
        )
    };
    match f.revoked {
        Some((rev, by)) => {
            let at = rev.revoked_unix().unwrap_or(i64::MIN);
            let what = format!(
                "revoked by {} at {} (reason {}), listed in {from}",
                by.as_str(),
                rev.revoked_at,
                rev.reason
            );
            if rev.reason == REASON_KEY_COMPROMISED || at <= opts.now_unix {
                check(N, Status::Fail, format!("{what}{ignored}"))
            } else {
                check(
                    N,
                    Status::Warn,
                    format!(
                        "{what}; takes effect in {}{ignored}",
                        rough_duration(at.abs_diff(opts.now_unix))
                    ),
                )
            }
        }
        None => check(
            N,
            if ignored.is_empty() {
                Status::Pass
            } else {
                Status::Warn
            },
            format!(
                "not revoked in {from} ({} entr{}){ignored}",
                list.revocations.len(),
                if list.revocations.len() == 1 {
                    "y"
                } else {
                    "ies"
                }
            ),
        ),
    }
}

/// The catalog side of a check.
#[derive(Debug, Clone)]
pub enum CatalogResult {
    /// The claimed entry, from a catalog the checker trusts.
    Matched(Box<CatalogMatch>),
    /// The catalog is not trusted or does not have the entry.
    Failed(Check),
    /// The catalog could not be read; the run is inconclusive.
    Unavailable(String),
}

/// Run every check on a parsed claim and fill a report.
#[must_use]
pub fn build_report(
    source: &str,
    origin: &Origin,
    c: &ServedClaim,
    catalog: &CatalogResult,
    revocations: &RevocationInput,
    opts: &ClaimCheckOptions,
) -> ClaimReport {
    let mut rep = ClaimReport::new(source);
    rep.origin = Some(origin.clone());
    rep.describe(c);
    let doc = check_document(c, opts);
    let hash_ok = doc
        .iter()
        .any(|k| k.name == "claim_sha256" && k.status == Status::Pass);
    rep.extend(doc);
    rep.extend(check_origin(c, origin, opts));
    let rev = if hash_ok {
        check_revocation(c, revocations, opts)
    } else {
        check(
            "revocation",
            Status::Skip,
            "not checked: claim_sha256 does not match",
        )
    };
    rep.revoked = rev.status == Status::Fail;
    rep.extend([rev]);
    let mut inconclusive = false;
    match catalog {
        CatalogResult::Matched(m) => {
            let (checks, mark) = check_catalog(c, m, opts);
            rep.catalog = Some(m.name.clone());
            rep.freshness = Some(m.freshness.clone());
            rep.license = Some(mark);
            rep.extend(checks);
        }
        CatalogResult::Failed(failed) => rep.extend([failed.clone()]),
        CatalogResult::Unavailable(e) => {
            inconclusive = true;
            rep.extend([check(
                "catalog",
                Status::Skip,
                format!("could not read the catalog: {e}"),
            )]);
        }
    }
    rep.wallet_verified = rep
        .checks
        .iter()
        .any(|k| k.name == "wallet_signature" && k.status == Status::Pass);
    rep.finish(inconclusive);
    rep
}

/// The report for bytes that are not a claim at all.
#[must_use]
pub fn unparsable_report(source: &str, origin: &Origin, err: &str) -> ClaimReport {
    let mut rep = ClaimReport::new(source);
    rep.origin = Some(origin.clone());
    rep.extend([check("claim", Status::Fail, err)]);
    rep.finish(false);
    rep
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claim::VerifiedFile;
    use crate::claim::fixtures::{claim, personal_sign};
    use tensorseed_core::manifest::{FileEntry, TombstoneReason};

    fn status(checks: &[Check], name: &str) -> Status {
        checks
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("no {name} in {checks:?}"))
            .status
    }

    fn signed() -> ServedClaim {
        let mut c = claim();
        c.attach_signature(&personal_sign(c.message().as_bytes()))
            .unwrap();
        c
    }

    #[test]
    fn document_checks_hash_fields_and_wallet_apart() {
        let o = ClaimCheckOptions::default();
        let c = signed();
        let k = check_document(&c, &o);
        assert_eq!(status(&k, "claim_sha256"), Status::Pass);
        assert_eq!(status(&k, "claim_fields"), Status::Pass);
        assert_eq!(status(&k, "wallet_signature"), Status::Pass);
        // Unsigned: a warning unless a signature is required.
        let u = claim();
        assert_eq!(
            status(&check_document(&u, &o), "wallet_signature"),
            Status::Warn
        );
        let strict = ClaimCheckOptions {
            require_wallet_sig: true,
            ..ClaimCheckOptions::default()
        };
        assert_eq!(
            status(&check_document(&u, &strict), "wallet_signature"),
            Status::Fail
        );
        // An edit without resealing: the hash fails, the fields are skipped.
        let mut e = c.clone();
        e.served_model_id = "other".into();
        let k = check_document(&e, &o);
        assert_eq!(status(&k, "claim_sha256"), Status::Fail);
        assert_eq!(status(&k, "claim_fields"), Status::Skip);
        assert_eq!(status(&k, "wallet_signature"), Status::Skip);
        // Resealed but the old signature kept: the wallet check fails.
        e.seal().unwrap();
        let k = check_document(&e, &o);
        assert_eq!(status(&k, "claim_sha256"), Status::Pass);
        assert_eq!(status(&k, "wallet_signature"), Status::Fail);
        // signed without sig, and a sig with signed false.
        let mut x = claim();
        x.signed = true;
        assert_eq!(
            status(&check_document(&x, &o), "wallet_signature"),
            Status::Fail
        );
        let mut y = signed();
        y.signed = false;
        assert_eq!(
            status(&check_document(&y, &o), "wallet_signature"),
            Status::Fail
        );
        // Expected wallet, in any case.
        let want = ClaimCheckOptions {
            expect_wallet: Some(
                c.seller_wallet
                    .clone()
                    .unwrap()
                    .to_uppercase()
                    .replace("0X", "0x"),
            ),
            ..ClaimCheckOptions::default()
        };
        assert_eq!(
            status(&check_document(&c, &want), "wallet_signature"),
            Status::Pass
        );
        let other = ClaimCheckOptions {
            expect_wallet: Some(format!("0x{}", "1".repeat(40))),
            ..ClaimCheckOptions::default()
        };
        assert_eq!(
            status(&check_document(&c, &other), "wallet_signature"),
            Status::Fail
        );
    }

    #[test]
    fn origin_must_be_the_claimed_endpoint() {
        let o = ClaimCheckOptions::default();
        let c = claim(); // public_base_url https://llm.example.com/v1
        assert_eq!(
            well_known_for("https://llm.example.com/v1/"),
            "https://llm.example.com/.well-known/tensorseed/served-weights.json"
        );
        let at = |u: &str| status(&check_origin(&c, &Origin::Url(u.into()), &o), "public_url");
        assert_eq!(
            at("https://llm.example.com/.well-known/tensorseed/served-weights.json"),
            Status::Pass
        );
        assert_eq!(
            at("https://LLM.example.com:443/.well-known/tensorseed/served-weights.json"),
            Status::Pass,
            "case and default port do not matter"
        );
        assert_eq!(
            at("https://copy.example.net/.well-known/tensorseed/served-weights.json"),
            Status::Fail
        );
        assert_eq!(
            at("http://llm.example.com/.well-known/tensorseed/served-weights.json"),
            Status::Fail,
            "another scheme is another endpoint"
        );
        assert_eq!(at("https://llm.example.com/claims/mine.json"), Status::Warn);
        // From a file: skipped, or compared with the expected URL.
        let f = Origin::File("claim.json".into());
        assert_eq!(
            status(&check_origin(&c, &f, &o), "public_url"),
            Status::Skip
        );
        let want = |u: &str| ClaimCheckOptions {
            expect_url: Some(u.into()),
            ..ClaimCheckOptions::default()
        };
        assert_eq!(
            status(
                &check_origin(&c, &f, &want("https://llm.example.com/v1/")),
                "public_url"
            ),
            Status::Pass
        );
        assert_eq!(
            status(
                &check_origin(&c, &f, &want("https://other.example/v1")),
                "public_url"
            ),
            Status::Fail
        );
        // No public URL at all, and a non https one.
        let mut n = claim();
        n.public_base_url = None;
        assert_eq!(
            status(&check_origin(&n, &f, &o), "public_url"),
            Status::Fail
        );
        let mut h = claim();
        h.public_base_url = Some("http://llm.example.com/v1".into());
        let k = check_origin(
            &h,
            &Origin::Url(
                "http://llm.example.com/.well-known/tensorseed/served-weights.json".into(),
            ),
            &o,
        );
        assert_eq!(status(&k, "public_url"), Status::Pass);
        assert_eq!(status(&k, "https"), Status::Warn);
    }

    fn matching(c: &ServedClaim) -> CatalogMatch {
        let mut manifest: Manifest = serde_json::from_value(serde_json::json!({
            "schema": "ts.manifest/1",
            "repo": {"type": "model", "id": c.repo, "hf_url": "https://huggingface.co/org/tiny"},
            "hf_commit": c.revision,
            "hf_last_modified": null,
            "license": {"spdx": "apache-2.0", "hf_tag": "apache-2.0", "class": "open",
                "license_files": [], "attribution": null, "gated_upstream": false},
            "totals": {"files": 1, "bytes": 10},
            "files": [],
            "file_pages": 0,
            "torrent": null,
            "webseeds": [],
            "backups": [],
            "published_at": "2026-11-13T00:00:00Z",
            "publisher_tool": "test"
        }))
        .unwrap();
        manifest.files = c
            .files
            .iter()
            .map(|f| FileEntry {
                path: f.path.clone(),
                size: f.size,
                sha256: f.sha256.clone(),
                hf_lfs_oid: None,
                bt_v2_pieces_root: None,
                format: None,
                pickle_risk: false,
            })
            .collect();
        CatalogMatch {
            name: c.catalog.name.clone(),
            publisher_key_id: c.catalog.publisher_key_id.clone().unwrap(),
            manifest,
            manifest_sha256: c.catalog.manifest_sha256.clone(),
            manifest_key: c.catalog.manifest_key.clone(),
            tombstone: None,
            freshness: Freshness {
                synced: true,
                revision: Some(4),
                chain_check: "off".into(),
                chain: None,
            },
        }
    }

    fn with_files() -> ServedClaim {
        let mut c = claim();
        c.files = vec![VerifiedFile {
            path: "tiny-q4.gguf".into(),
            size: 10,
            sha256: "b".repeat(64),
        }];
        c.seal().unwrap();
        c
    }

    #[test]
    fn catalog_checks_and_the_recomputed_license() {
        let o = ClaimCheckOptions::default();
        let c = with_files();
        let m = matching(&c);
        let (k, mark) = check_catalog(&c, &m, &o);
        for n in [
            "catalog",
            "publisher_key",
            "manifest",
            "files",
            "takedown",
            "freshness",
            "license",
        ] {
            assert_eq!(status(&k, n), Status::Pass, "{n}: {k:?}");
        }
        assert_eq!(mark.commercial_serving, CommercialServing::Allowed);
        // The seller's mark is not trusted: it is recomputed from the catalog.
        let mut lic = c.clone();
        lic.license.commercial_serving = CommercialServing::Restricted;
        let (k, mark) = check_catalog(&lic, &m, &o);
        assert_eq!(status(&k, "license"), Status::Fail);
        assert_eq!(
            (mark.commercial_serving, mark.claimed),
            (CommercialServing::Allowed, CommercialServing::Restricted)
        );
        // A catalog license that forbids commercial serving: claiming
        // allowed fails; claiming forbidden with asserted rights warns.
        let mut nc = m.clone();
        nc.manifest.license.spdx = Some("cc-by-nc-4.0".into());
        nc.manifest.license.hf_tag = Some("cc-by-nc-4.0".into());
        let mut honest = c.clone();
        honest.license.id = Some("cc-by-nc-4.0".into());
        assert_eq!(
            status(&check_catalog(&honest, &nc, &o).0, "license"),
            Status::Fail
        );
        honest.license.commercial_serving = CommercialServing::Forbidden;
        honest.license.rights_asserted = true;
        let (k, mark) = check_catalog(&honest, &nc, &o);
        assert_eq!(status(&k, "license"), Status::Warn);
        assert!(mark.rights_asserted);
        // Identity, key, hash, files, entrypoint, takedown, freshness.
        let mut w = c.clone();
        w.catalog.name = "elsewhere".into();
        assert_eq!(
            status(&check_catalog(&w, &m, &o).0, "catalog"),
            Status::Fail
        );
        let mut w = c.clone();
        w.catalog.publisher_key_id = Some("0123456789ABCDEF".into());
        assert_eq!(
            status(&check_catalog(&w, &m, &o).0, "publisher_key"),
            Status::Fail
        );
        w.catalog.publisher_key_id = None;
        assert_eq!(
            status(&check_catalog(&w, &m, &o).0, "publisher_key"),
            Status::Warn
        );
        let mut w = c.clone();
        w.catalog.manifest_sha256 = "c".repeat(64);
        assert_eq!(
            status(&check_catalog(&w, &m, &o).0, "manifest"),
            Status::Fail
        );
        let mut w = c.clone();
        w.revision = "1".repeat(40);
        assert_eq!(
            status(&check_catalog(&w, &m, &o).0, "manifest"),
            Status::Fail
        );
        let mut w = c.clone();
        w.files[0].sha256 = "d".repeat(64);
        assert_eq!(status(&check_catalog(&w, &m, &o).0, "files"), Status::Fail);
        let mut w = c.clone();
        w.files.push(VerifiedFile {
            path: "extra.gguf".into(),
            size: 1,
            sha256: "e".repeat(64),
        });
        assert_eq!(status(&check_catalog(&w, &m, &o).0, "files"), Status::Fail);
        let mut w = c.clone();
        w.entrypoint = Entrypoint::Gguf("other.gguf".into());
        assert_eq!(status(&check_catalog(&w, &m, &o).0, "files"), Status::Fail);
        let mut w = c.clone();
        w.files.clear();
        assert_eq!(status(&check_catalog(&w, &m, &o).0, "files"), Status::Fail);
        let mut t = m.clone();
        t.tombstone = Some(Tombstone {
            schema: "ts.tombstone/1".into(),
            reason: TombstoneReason::Legal,
            detail: "test".into(),
            effective_at: "2026-11-13T00:00:00Z".into(),
            revisions: vec![],
        });
        assert_eq!(
            status(&check_catalog(&c, &t, &o).0, "takedown"),
            Status::Fail
        );
        let mut t2 = t.clone();
        t2.tombstone.as_mut().unwrap().revisions = vec!["f".repeat(40)];
        assert_eq!(
            status(&check_catalog(&c, &t2, &o).0, "takedown"),
            Status::Pass
        );
        let mut s = m.clone();
        s.freshness.synced = false;
        assert_eq!(
            status(&check_catalog(&c, &s, &o).0, "freshness"),
            Status::Warn
        );
        let fresh = ClaimCheckOptions {
            require_fresh: true,
            ..ClaimCheckOptions::default()
        };
        assert_eq!(
            status(&check_catalog(&c, &s, &fresh).0, "freshness"),
            Status::Fail
        );
    }

    #[test]
    fn verdicts_and_report_shape() {
        let mut r = ClaimReport::new("x");
        r.extend([check("a", Status::Pass, ""), check("b", Status::Warn, "")]);
        r.finish(false);
        assert_eq!(r.verdict, ClaimVerdict::Pass);
        assert_eq!((r.counts.pass, r.counts.warn), (1, 1));
        r.finish(true);
        assert_eq!(r.verdict, ClaimVerdict::Inconclusive);
        r.extend([check("c", Status::Fail, "")]);
        r.finish(true);
        assert_eq!(r.verdict, ClaimVerdict::Fail);
        assert_eq!(
            [
                ClaimVerdict::Pass,
                ClaimVerdict::Fail,
                ClaimVerdict::Inconclusive
            ]
            .map(ClaimVerdict::exit_code),
            [0, 1, 2]
        );
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["schema"], CLAIM_CHECK_SCHEMA);
        assert_eq!(v["checks"][2]["status"], "fail");
        assert!(v.get("license").is_none());
        // A version 0 record or another document is not a claim.
        assert!(parse_claim(br#"{"schema":"ts.other/1"}"#).is_err());
        assert!(parse_claim(&serde_json::to_vec(&claim()).unwrap()).is_ok());
    }

    #[test]
    fn expiry_is_checked_against_the_clock() {
        let now = crate::response::parse_rfc3339("2026-11-20T00:00:00Z").unwrap();
        let mut o = ClaimCheckOptions {
            now_unix: now,
            ..ClaimCheckOptions::default()
        };
        // No expiry: a warning, or a failure with require_expiry.
        let c = claim();
        assert_eq!(status(&check_document(&c, &o), "expiry"), Status::Warn);
        o.require_expiry = true;
        assert_eq!(status(&check_document(&c, &o), "expiry"), Status::Fail);
        let mut x = claim();
        x.expires_at = Some("2026-12-13T21:01:42Z".into());
        x.seal().unwrap();
        let k = check_document(&x, &o);
        assert_eq!(status(&k, "expiry"), Status::Pass);
        assert!(k.iter().any(|c| c.detail.contains("in 23d 21h")), "{k:?}");
        o.now_unix = crate::response::parse_rfc3339("2026-12-14T00:00:00Z").unwrap();
        let k = check_document(&x, &o);
        assert_eq!(status(&k, "expiry"), Status::Fail);
        // Still loads and the fields still pass: only the expiry fails.
        assert_eq!(status(&k, "claim_fields"), Status::Pass);
        // An edited expiry is a hash failure, and expiry is not checked.
        let mut edited = x.clone();
        edited.expires_at = Some("2027-12-13T21:01:42Z".into());
        let k = check_document(&edited, &o);
        assert_eq!(status(&k, "claim_sha256"), Status::Fail);
        assert_eq!(status(&k, "expiry"), Status::Skip);
    }

    #[test]
    fn revocations_fail_the_claim() {
        use crate::revocation::tests::revoke;
        let c = claim();
        let now = crate::response::parse_rfc3339("2026-11-20T00:00:00Z").unwrap();
        let o = ClaimCheckOptions {
            now_unix: now,
            ..ClaimCheckOptions::default()
        };
        let list = |rs: Vec<crate::revocation::Revocation>| RevocationInput::List {
            list: Revocations {
                revocations: rs,
                ..Revocations::default()
            },
            from: "test".into(),
        };
        let st = |r: &RevocationInput| check_revocation(&c, r, &o).status;
        assert_eq!(st(&RevocationInput::NotChecked("x".into())), Status::Skip);
        assert_eq!(st(&RevocationInput::Absent("u".into())), Status::Pass);
        assert_eq!(st(&RevocationInput::Unavailable("e".into())), Status::Warn);
        assert_eq!(st(&list(vec![])), Status::Pass);
        let past = revoke(&c, "2026-11-19T00:00:00Z", "superseded");
        assert_eq!(st(&list(vec![past.clone()])), Status::Fail);
        let future = revoke(&c, "2026-11-25T00:00:00Z", "stopped");
        assert_eq!(st(&list(vec![future])), Status::Warn);
        let leaked = revoke(&c, "2026-11-25T00:00:00Z", REASON_KEY_COMPROMISED);
        assert_eq!(st(&list(vec![leaked])), Status::Fail);
        // A forged entry is ignored with a warning, never a failure.
        let mut forged = past.clone();
        forged.revoked_at = "2026-11-18T00:00:00Z".into();
        assert_eq!(st(&list(vec![forged])), Status::Warn);

        // In a full report the revocation fails the verdict and sets
        // `revoked`.
        let origin = Origin::Url(well_known_for("https://llm.example.com/v1"));
        let rep = build_report(
            "s",
            &origin,
            &c,
            &CatalogResult::Unavailable("offline".into()),
            &list(vec![past]),
            &o,
        );
        assert!(rep.revoked);
        assert_eq!(rep.verdict, ClaimVerdict::Fail);
        let rep = build_report(
            "s",
            &origin,
            &c,
            &CatalogResult::Unavailable("offline".into()),
            &RevocationInput::Absent("u".into()),
            &o,
        );
        assert!(!rep.revoked);
        assert_eq!(rep.verdict, ClaimVerdict::Inconclusive);
    }

    #[test]
    fn revocation_urls() {
        assert_eq!(
            revocations_url_next_to(
                "https://llm.example.com/.well-known/tensorseed/served-weights.json"
            )
            .unwrap(),
            "https://llm.example.com/.well-known/tensorseed/revocations.json"
        );
        assert_eq!(
            revocations_url_next_to(
                "https://h.example/sub/.well-known/tensorseed/served-weights.json"
            )
            .unwrap(),
            "https://h.example/sub/.well-known/tensorseed/revocations.json"
        );
        assert!(revocations_url_next_to("https://h.example/claim.json").is_none());
        assert_eq!(
            well_known_revocations_for("https://llm.example.com/v1/"),
            "https://llm.example.com/.well-known/tensorseed/revocations.json"
        );
    }
}
