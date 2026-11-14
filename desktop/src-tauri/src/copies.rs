// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! What a local copy is, compared with the catalog entry it is shown next
//! to: complete and signed, partial, unsigned, or at a different commit.
//!
//! A copy is judged only by its own pull record (`.tensorseed/pull.json`)
//! and the entry's file list. Nothing here ever marks the catalog entry
//! itself as bad: a partial or unsigned copy is a property of this machine.

use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;
use tensorseed_app::pull::{PullRecord, allow_missing_from};
use tensorseed_core::verify::{ExpectedFile, FileStatus, VerifyOptions, verify_dir};

/// The copy's main state, most important first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum CopyKind {
    /// A different commit from the catalog entry.
    CommitMismatch,
    /// Fewer files than the catalog entry, or a subset selection.
    Partial,
    /// Every file, but pulled from a manifest file, URL, or Hugging Face
    /// instead of the signed catalog.
    Unsigned,
    /// Every file of the catalog entry, from a signed manifest.
    Complete,
    /// Signed, but there is no catalog entry to compare it with.
    Unchecked,
}

/// How a local copy compares with the catalog entry (`CopyStatus` in
/// `src/lib/native.ts`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CopyStatus {
    pub kind: CopyKind,
    /// Fewer files than the entry (or a subset selection).
    pub partial: bool,
    /// Not pulled from a signature verified manifest.
    pub unsigned: bool,
    /// The copy's commit differs from the entry's.
    pub commit_mismatch: bool,
    /// Files the pull downloaded (the record minus skipped files).
    pub present_files: usize,
    /// Bytes of those files.
    pub present_bytes: u64,
    /// Files in the catalog entry, when it is known.
    pub catalog_files: Option<usize>,
    /// The catalog entry's commit, when it is known.
    pub catalog_commit: Option<String>,
    /// Catalog files this copy does not have (same commit only).
    pub missing_files: usize,
    /// Neutral or warning one liner for the badge.
    pub summary: String,
    /// `catalog` (the signed manifest) or `record` (this copy's own pull
    /// record) for a plain Verify.
    pub verify_default: &'static str,
    /// The signed catalog has the missing files at this commit.
    pub can_complete: bool,
}

/// The catalog entry a copy is compared with.
#[derive(Debug, Clone)]
pub struct CatalogRef {
    /// Commit of the entry's manifest.
    pub commit: String,
    /// Every file path in the entry.
    pub paths: Vec<String>,
}

fn short(c: &str) -> &str {
    &c[..12.min(c.len())]
}

fn source_label(source: &str) -> &str {
    match source {
        "manifest-file" => "a manifest file",
        "manifest-url" => "a manifest URL",
        "huggingface" => "Hugging Face",
        "catalog" => "the catalog",
        other => other,
    }
}

/// Classify `record` against `catalog` (the signed catalog entry, if any).
pub fn classify(record: &PullRecord, catalog: Option<&CatalogRef>) -> CopyStatus {
    let skipped: HashSet<&str> = record.skipped.iter().map(String::as_str).collect();
    let unselected: HashSet<&str> = record.unselected.iter().map(String::as_str).collect();
    let present: Vec<_> = record
        .files
        .iter()
        .filter(|f| !skipped.contains(f.path.as_str()) && !unselected.contains(f.path.as_str()))
        .collect();
    let present_files = present.len();
    let present_bytes = present.iter().map(|f| f.size).sum();
    // Pickle files skipped by policy still count as part of the copy.
    let kept: HashSet<&str> = record
        .files
        .iter()
        .map(|f| f.path.as_str())
        .filter(|p| !unselected.contains(p))
        .collect();

    let unsigned = !record.signed;
    let commit_mismatch = catalog.is_some_and(|c| !c.commit.eq_ignore_ascii_case(&record.commit));
    let missing_files = match catalog {
        Some(c) if !commit_mismatch => c
            .paths
            .iter()
            .filter(|p| !kept.contains(p.as_str()))
            .count(),
        _ => 0,
    };
    let partial = !commit_mismatch && (missing_files > 0 || !record.unselected.is_empty());

    let kind = if commit_mismatch {
        CopyKind::CommitMismatch
    } else if partial {
        CopyKind::Partial
    } else if unsigned {
        CopyKind::Unsigned
    } else if catalog.is_some() {
        CopyKind::Complete
    } else {
        CopyKind::Unchecked
    };

    let not_signed = if unsigned {
        ", not from the signed catalog"
    } else {
        ""
    };
    let pickles = record
        .skipped
        .iter()
        .filter(|p| !unselected.contains(p.as_str()))
        .count();
    let summary = match kind {
        CopyKind::CommitMismatch => format!(
            "Different revision: this copy is at {}, the catalog entry at {}{not_signed}",
            short(&record.commit),
            short(&catalog.map(|c| c.commit.clone()).unwrap_or_default())
        ),
        CopyKind::Partial => {
            let total = catalog.map_or(record.files.len(), |c| c.paths.len());
            let what = if catalog.is_some() {
                "files"
            } else {
                "files selected"
            };
            format!("Partial copy: {present_files} of {total} {what}{not_signed}")
        }
        CopyKind::Unsigned => format!(
            "Unsigned copy: pulled from {}, not from the signed catalog",
            source_label(&record.source)
        ),
        CopyKind::Complete if pickles > 0 => format!(
            "Complete signed copy; {pickles} pickle file{} skipped by choice",
            if pickles == 1 { "" } else { "s" }
        ),
        CopyKind::Complete => "Complete signed copy of the catalog entry".to_string(),
        CopyKind::Unchecked => "Signed copy; no catalog entry to compare it with".to_string(),
    };
    let verify_default = match kind {
        CopyKind::Complete | CopyKind::Unchecked => "catalog",
        _ => "record",
    };
    CopyStatus {
        kind,
        partial,
        unsigned,
        commit_mismatch,
        present_files,
        present_bytes,
        catalog_files: catalog.map(|c| c.paths.len()),
        catalog_commit: catalog.map(|c| c.commit.clone()),
        missing_files,
        summary,
        verify_default,
        can_complete: partial && missing_files > 0,
    }
}

/// Result of checking a copy against its own pull record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordCheck {
    /// Files that match.
    pub ok: usize,
    /// Files the pull skipped on purpose.
    pub skipped: usize,
    /// `path: problem` for every file that does not match.
    pub failures: Vec<String>,
    /// Files present but not in the record.
    pub extra: usize,
}

/// Re-hash the files this copy's pull record lists. The record is the
/// list of files the pull downloaded and checked, so this says whether
/// the copy still holds what it was pulled with. It does not say anything
/// about the catalog.
///
/// # Errors
///
/// Fails when the record lists an unknown digest or a path escapes `dir`.
pub fn verify_record(
    dir: &Path,
    record: &PullRecord,
    threads: usize,
) -> anyhow::Result<RecordCheck> {
    let expected = record
        .files
        .iter()
        .map(tensorseed_app::pull::PullFile::to_expected)
        .collect::<anyhow::Result<Vec<ExpectedFile>>>()?;
    let report = verify_dir(
        dir,
        &expected,
        VerifyOptions {
            allow_missing_pickle: true,
            allow_missing: allow_missing_from(&Some(record.clone())),
            threads: threads.max(1),
        },
    )?;
    let mut out = RecordCheck {
        ok: 0,
        skipped: 0,
        failures: Vec::new(),
        extra: report.extra.len(),
    };
    for f in &report.files {
        match &f.status {
            FileStatus::Ok => out.ok += 1,
            FileStatus::Skipped | FileStatus::SkippedPickle => out.skipped += 1,
            FileStatus::Missing => out.failures.push(format!("{}: missing", f.path)),
            FileStatus::SizeMismatch { expected, actual } => out
                .failures
                .push(format!("{}: size {actual}, expected {expected}", f.path)),
            FileStatus::HashMismatch { .. } => {
                out.failures.push(format!("{}: digest differs", f.path))
            }
            FileStatus::Error(e) => out.failures.push(format!("{}: {e}", f.path)),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tensorseed_app::pull::PullFile;
    use tensorseed_core::keys::RepoType;

    const C1: &str = "1111111111111111111111111111111111111111";
    const C2: &str = "2222222222222222222222222222222222222222";

    fn file(path: &str, size: u64) -> PullFile {
        PullFile {
            path: path.into(),
            size,
            algorithm: "sha256".into(),
            digest: "0".repeat(64),
            pickle_risk: path.ends_with(".bin"),
        }
    }

    fn record(files: &[(&str, u64)], signed: bool, source: &str) -> PullRecord {
        PullRecord {
            schema: tensorseed_app::pull::PULL_SCHEMA.into(),
            repo_type: RepoType::Model,
            repo: "org/name".into(),
            commit: C1.into(),
            source: source.into(),
            signed,
            pulled_at: "2026-11-12T00:00:00Z".into(),
            files: files.iter().map(|(p, s)| file(p, *s)).collect(),
            skipped: Vec::new(),
            unselected: Vec::new(),
        }
    }

    fn nine() -> CatalogRef {
        CatalogRef {
            commit: C1.into(),
            paths: (0..8)
                .map(|i| format!("model-{i}.gguf"))
                .chain(["tokenizer_config.json".to_string()])
                .collect(),
        }
    }

    #[test]
    fn subset_manifest_is_partial_and_unsigned_not_broken() {
        // A manifest file listing 1 of the entry's 9 files.
        let r = record(&[("tokenizer_config.json", 23_000)], false, "manifest-file");
        let s = classify(&r, Some(&nine()));
        assert_eq!(s.kind, CopyKind::Partial);
        assert!(s.partial && s.unsigned && !s.commit_mismatch);
        assert_eq!((s.present_files, s.present_bytes), (1, 23_000));
        assert_eq!(s.catalog_files, Some(9));
        assert_eq!(s.missing_files, 8);
        assert_eq!(
            s.summary,
            "Partial copy: 1 of 9 files, not from the signed catalog"
        );
        assert_eq!(s.verify_default, "record");
        assert!(s.can_complete);
    }

    #[test]
    fn include_selection_counts_only_downloaded_files() {
        // A full file list with 8 files unselected: size is what is on disk.
        let cat = nine();
        let files: Vec<(&str, u64)> = cat
            .paths
            .iter()
            .map(|p| {
                (
                    p.as_str(),
                    if p.ends_with(".gguf") {
                        600_000_000
                    } else {
                        23_000
                    },
                )
            })
            .collect();
        let mut r = record(&files, false, "manifest-file");
        r.unselected = cat
            .paths
            .iter()
            .filter(|p| p.ends_with(".gguf"))
            .cloned()
            .collect();
        r.skipped = r.unselected.clone();
        let s = classify(&r, Some(&cat));
        assert_eq!(s.kind, CopyKind::Partial);
        assert_eq!((s.present_files, s.present_bytes), (1, 23_000));
        assert_eq!(s.missing_files, 8);
        assert_eq!(
            s.summary,
            "Partial copy: 1 of 9 files, not from the signed catalog"
        );
    }

    #[test]
    fn complete_signed_copy_skipping_pickles_is_complete() {
        let cat = CatalogRef {
            commit: C1.into(),
            paths: vec!["a.safetensors".into(), "pytorch_model.bin".into()],
        };
        let mut r = record(
            &[("a.safetensors", 10), ("pytorch_model.bin", 20)],
            true,
            "catalog",
        );
        r.skipped = vec!["pytorch_model.bin".into()];
        let s = classify(&r, Some(&cat));
        assert_eq!(s.kind, CopyKind::Complete);
        assert!(!s.partial && !s.unsigned && !s.can_complete);
        assert_eq!((s.present_files, s.present_bytes), (1, 10));
        assert_eq!(
            s.summary,
            "Complete signed copy; 1 pickle file skipped by choice"
        );
        assert_eq!(s.verify_default, "catalog");
    }

    #[test]
    fn every_file_from_a_manifest_file_is_unsigned() {
        let cat = CatalogRef {
            commit: C1.into(),
            paths: vec!["a".into()],
        };
        let s = classify(&record(&[("a", 1)], false, "manifest-file"), Some(&cat));
        assert_eq!(s.kind, CopyKind::Unsigned);
        assert_eq!(
            s.summary,
            "Unsigned copy: pulled from a manifest file, not from the signed catalog"
        );
        assert_eq!(s.verify_default, "record");
    }

    #[test]
    fn other_commit_is_a_mismatch_not_a_partial() {
        let cat = CatalogRef {
            commit: C2.into(),
            paths: vec!["a".into(), "b".into()],
        };
        let s = classify(&record(&[("a", 1)], true, "catalog"), Some(&cat));
        assert_eq!(s.kind, CopyKind::CommitMismatch);
        assert!(s.commit_mismatch && !s.partial && !s.can_complete);
        assert_eq!(s.missing_files, 0);
        assert_eq!(
            s.summary,
            "Different revision: this copy is at 111111111111, the catalog entry at 222222222222"
        );
        assert_eq!(s.verify_default, "record");
    }

    #[test]
    fn without_a_catalog_entry() {
        let s = classify(&record(&[("a", 1)], true, "catalog"), None);
        assert_eq!(s.kind, CopyKind::Unchecked);
        assert_eq!(s.catalog_files, None);
        let mut r = record(&[("a", 1), ("b", 2)], true, "catalog");
        r.unselected = vec!["b".into()];
        r.skipped = vec!["b".into()];
        let s = classify(&r, None);
        assert_eq!(s.kind, CopyKind::Partial);
        assert_eq!(s.summary, "Partial copy: 1 of 2 files selected");
        assert!(!s.can_complete);
    }

    #[test]
    fn record_check_passes_a_partial_copy_and_catches_changes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"abc").unwrap();
        let sha = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let mut r = record(&[("a.txt", 3), ("b.gguf", 9)], false, "manifest-file");
        r.files[0].digest = sha.into();
        r.unselected = vec!["b.gguf".into()];
        r.skipped = vec!["b.gguf".into()];
        let c = verify_record(dir.path(), &r, 2).unwrap();
        assert_eq!((c.ok, c.skipped, c.failures.len()), (1, 1, 0));

        std::fs::write(dir.path().join("a.txt"), b"abd").unwrap();
        let c = verify_record(dir.path(), &r, 2).unwrap();
        assert_eq!(c.failures, vec!["a.txt: digest differs".to_string()]);
        std::fs::remove_file(dir.path().join("a.txt")).unwrap();
        let c = verify_record(dir.path(), &r, 2).unwrap();
        assert_eq!(c.failures, vec!["a.txt: missing".to_string()]);
    }
}
