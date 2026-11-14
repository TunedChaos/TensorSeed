// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Signed manifests: the checks that turn a [`SignedEntry`] into a
//! [`Manifest`] a verifier can rely on (SPEC 6.9). Fetching the entries is
//! the caller's job (a catalog backend, an export, or a browser).

use tensorseed_core::hash::sha256_bytes;
use tensorseed_core::keys::RepoRef;
use tensorseed_core::manifest::{FilePage, Manifest, SCHEMA_FILEPAGE, Tombstone};

use crate::minisign::PublicKey;
use crate::signed::SignedEntry;
use crate::{Error, Result};

fn bad(msg: impl Into<String>) -> Error {
    Error::Signature(msg.into())
}

/// SHA-256 of a signed entry's value (what the signature covers), from the
/// stored envelope bytes. This is `catalog.manifest_sha256` in a claim.
///
/// # Errors
///
/// Fails when `raw` is not a signed entry envelope.
pub fn signed_value_sha256(raw: &[u8]) -> Result<String> {
    let e = SignedEntry::from_bytes(raw)?;
    Ok(sha256_bytes(e.value.as_bytes()))
}

/// Verify a manifest entry against the pinned publisher key and catalog
/// name, and check that it is stored under its own key
/// (`ts/v1/repo/{type}/{org}/{name}/rev/{commit}`). A paged manifest is
/// returned with `file_pages > 0`; merge its pages with
/// [`merge_file_pages`].
///
/// # Errors
///
/// Fails when the signature does not verify, the value is not a valid
/// manifest, or the key does not match the manifest it carries.
pub fn verify_manifest_entry(e: &SignedEntry, pk: &PublicKey, catalog: &str) -> Result<Manifest> {
    e.verify(pk, catalog)?;
    let m = Manifest::from_json(e.value.as_bytes())?;
    let repo = RepoRef::new(m.repo.repo_type, m.repo.id.clone())?;
    if e.key != repo.manifest_key(&m.hf_commit)? {
        return Err(bad(format!(
            "signed key {:?} does not match the manifest it carries",
            e.key
        )));
    }
    Ok(m)
}

/// Fill in the files of a paged manifest from its verified file pages, in
/// order (page 0 first; each `pages[i]` is the value of the entry at
/// `file_page_key(commit, i)`). Checks schema, page numbers, every file,
/// and the manifest totals.
///
/// # Errors
///
/// Fails on a missing, misnumbered, or malformed page, or totals that do
/// not match.
pub fn merge_file_pages(m: &mut Manifest, pages: &[String]) -> Result<()> {
    if m.file_pages == 0 {
        return Ok(());
    }
    if pages.len() != m.file_pages as usize {
        return Err(bad(format!(
            "the manifest has {} file pages, got {}",
            m.file_pages,
            pages.len()
        )));
    }
    let mut files = Vec::new();
    for (i, v) in pages.iter().enumerate() {
        let p: FilePage = serde_json::from_str(v)?;
        if p.schema != SCHEMA_FILEPAGE || p.page as usize != i {
            return Err(bad(format!(
                "file page {i} has the wrong schema or page number"
            )));
        }
        for f in &p.files {
            f.validate()?;
        }
        files.extend(p.files);
    }
    let bytes: u64 = files.iter().map(|f| f.size).sum();
    if m.totals.files != files.len() as u64 || m.totals.bytes != bytes {
        return Err(bad("file pages do not match manifest totals"));
    }
    m.files = files;
    m.file_pages = 0;
    Ok(())
}

/// Check that a manifest is for `repo` at `commit`.
///
/// # Errors
///
/// Fails when the repo or commit differs.
pub fn check_manifest_identity(m: &Manifest, repo: &RepoRef, commit: &str) -> Result<()> {
    if m.repo.id != repo.id || m.repo.repo_type != repo.repo_type {
        return Err(bad(format!(
            "manifest is for {} {}, expected {} {}",
            m.repo.repo_type, m.repo.id, repo.repo_type, repo.id
        )));
    }
    if m.hf_commit != commit {
        return Err(bad(format!(
            "manifest is for commit {}, expected {commit}",
            m.hf_commit
        )));
    }
    Ok(())
}

/// True when a takedown covers `commit` (an empty revision list covers
/// every revision).
#[must_use]
pub fn tombstone_covers(t: &Tombstone, commit: &str) -> bool {
    t.revisions.is_empty() || t.revisions.iter().any(|r| r == commit)
}
