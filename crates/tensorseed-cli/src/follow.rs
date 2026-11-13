// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Follow several catalogs at once (spec 18.1): agreement, conflicts,
//! and a single hash authority for `list`, `pull`, and `show`.

use tensorseed_core::manifest::{FileEntry, Manifest};

use crate::config::Subscription;

/// A compact fingerprint of a revision's files, sizes, SHA-256 hashes, and
/// torrent infohash (spec 18.1).
#[must_use]
pub fn revision_fingerprint(m: &Manifest) -> String {
    let mut files: Vec<(&str, u64, &str)> = m
        .files
        .iter()
        .map(|f: &FileEntry| (f.path.as_str(), f.size, f.sha256.as_str()))
        .collect();
    files.sort_unstable();
    let ih = m
        .torrent
        .as_ref()
        .map(|t| t.infohash_v2.as_str())
        .unwrap_or("");
    format!("{ih}|{files:?}")
}

/// Pick the hash authority: the default catalog if it agrees, otherwise
/// the lowest priority (then the name, for a stable tie-break).
///
/// # Errors
///
/// Returns an error if `hits` is empty.
pub fn authority<T>(hits: &[(Subscription, T)]) -> anyhow::Result<&(Subscription, T)> {
    hits.iter()
        .find(|(s, _)| s.is_default)
        .or_else(|| {
            hits.iter()
                .min_by_key(|(s, _)| (s.priority, s.name.as_str()))
        })
        .ok_or_else(|| anyhow::anyhow!("internal error: no catalog hits"))
}

/// Merge webseeds from `extras` into `authority` without duplicates.
pub fn merge_webseeds(authority: &mut Manifest, extras: &[Manifest]) {
    let mut seen = authority.webseeds.clone();
    for m in extras {
        for u in &m.webseeds {
            if !seen.iter().any(|s| s == u) {
                seen.push(u.clone());
            }
        }
    }
    authority.webseeds = seen;
}

/// Human-readable agreement tag for `list`.
#[must_use]
pub fn agreement_line(agreeing: usize, listed: usize, conflict: bool) -> String {
    if conflict {
        format!("{agreeing} of {listed} catalogs agree; CONFLICT (hashes differ)")
    } else {
        format!("{agreeing} of {listed} catalogs agree")
    }
}

/// Loud conflict message for `pull` and `show`.
#[must_use]
pub fn conflict_error(repo: &str, commit: &str, groups: &[(String, Vec<String>)]) -> String {
    let mut lines = vec![
        format!(
            "CONFLICT: {repo}@{commit} is listed with different file hashes in more than one catalog."
        ),
        "This is not resolved by majority vote. Pass --catalog <name> to choose one.".into(),
    ];
    for (fp, names) in groups {
        let short = if fp.len() <= 24 {
            fp.clone()
        } else {
            format!("{}...", &fp[..24])
        };
        lines.push(format!("  {}: {short}", names.join(", ")));
    }
    lines.join("\n")
}
