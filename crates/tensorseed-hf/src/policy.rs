// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Publishing policy (spec section 11): license allowlist, gated repos, and
//! pickle based files. Defaults are conservative: only allowlisted open
//! licenses, no gated repos, no pickle files.

use tensorseed_core::manifest::{FileEntry, LicenseClass};

use crate::{RepoInfo, TreeEntry};

/// Default open license allowlist (HF license ids, lowercase).
pub const DEFAULT_ALLOWLIST: &[&str] = &[
    "apache-2.0",
    "mit",
    "bsd-3-clause",
    "bsd-2-clause",
    "cc-by-4.0",
    "cc0-1.0",
    "openrail",
    "openrail++",
];

/// Policy settings (mirrors `[policy]` in config).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    /// Open licenses that may be mirrored.
    pub license_allowlist: Vec<String>,
    /// Licenses allowed only with attribution handling.
    pub attribution_licenses: Vec<String>,
    /// Allow gated repos (only with an open or attribution license).
    pub allow_gated: bool,
    /// Allow pickle based files (`.bin`, `.pt`, `.ckpt`, ...).
    pub allow_pickle_formats: bool,
    /// License ids the publisher explicitly accepts for this run or repo
    /// (`--allow-license`, watchlist `allow_license`). A matching repo is not
    /// refused, but its recorded class stays what [`Policy::classify`] says,
    /// so an `other` license is still published as `unknown`, never `open`.
    pub license_overrides: Vec<String>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            license_allowlist: DEFAULT_ALLOWLIST.iter().map(|s| s.to_string()).collect(),
            attribution_licenses: Vec::new(),
            allow_gated: false,
            allow_pickle_formats: false,
            license_overrides: Vec::new(),
        }
    }
}

/// Outcome of evaluating a repo against a [`Policy`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyReport {
    /// License id the decision was based on (first one listed upstream).
    pub license: Option<String>,
    /// Resulting class.
    pub class: LicenseClass,
    /// Repo is gated upstream.
    pub gated: bool,
    /// Reasons the repo must not be mirrored; empty means allowed.
    pub refusals: Vec<String>,
    /// The license was allowed only by an explicit override.
    pub license_overridden: bool,
    /// Pickle based files excluded by policy.
    pub excluded_pickle: Vec<String>,
}

impl PolicyReport {
    /// True when no rule refuses the repo.
    pub fn allowed(&self) -> bool {
        self.refusals.is_empty()
    }
}

impl Policy {
    /// Classify a license id.
    pub fn classify(&self, license: Option<&str>) -> LicenseClass {
        let Some(l) = license else {
            return LicenseClass::Unknown;
        };
        let has = |list: &[String]| list.iter().any(|x| x.eq_ignore_ascii_case(l));
        if has(&self.license_allowlist) {
            LicenseClass::Open
        } else if has(&self.attribution_licenses) {
            LicenseClass::Attribution
        } else if matches!(l, "other" | "unknown") {
            LicenseClass::Unknown
        } else {
            LicenseClass::Restricted
        }
    }

    /// Evaluate repo info and its file list.
    pub fn evaluate(&self, info: &RepoInfo, files: &[TreeEntry]) -> PolicyReport {
        let licenses = info.licenses();
        let license = licenses.first().cloned();
        let class = self.classify(license.as_deref());
        let gated = info.gated.is_gated();
        let mut refusals = Vec::new();
        let license_overridden = !matches!(class, LicenseClass::Open | LicenseClass::Attribution)
            && license.as_deref().is_some_and(|l| {
                self.license_overrides
                    .iter()
                    .any(|o| o.eq_ignore_ascii_case(l))
            });
        match class {
            LicenseClass::Open | LicenseClass::Attribution => {}
            _ if license_overridden => {}
            LicenseClass::Unknown => refusals.push(format!(
                "license {} is missing or unrecognized; unknown licenses are skipped",
                license.as_deref().unwrap_or("(none)")
            )),
            LicenseClass::Restricted => refusals.push(format!(
                "license {} is not on policy.license_allowlist or policy.attribution_licenses",
                license.as_deref().unwrap_or("(none)")
            )),
        }
        if gated && !self.allow_gated {
            refusals.push("repo is gated upstream and policy.allow_gated is false".into());
        }
        if info.private {
            refusals.push("repo is private".into());
        }
        if info.disabled {
            refusals.push("repo is disabled on the Hub".into());
        }
        let excluded_pickle = if self.allow_pickle_formats {
            Vec::new()
        } else {
            files
                .iter()
                .filter(|f| f.is_file() && FileEntry::path_has_pickle_risk(&f.path))
                .map(|f| f.path.clone())
                .collect()
        };
        PolicyReport {
            license,
            class,
            gated,
            refusals,
            license_overridden,
            excluded_pickle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(json: &str) -> RepoInfo {
        serde_json::from_str(json).unwrap()
    }

    fn file(path: &str) -> TreeEntry {
        TreeEntry {
            kind: "file".into(),
            path: path.into(),
            size: 1,
            oid: Some("0".repeat(40)),
            lfs: None,
        }
    }

    #[test]
    fn defaults_allow_open_and_drop_pickle() {
        let p = Policy::default();
        let r = p.evaluate(
            &info(r#"{"id":"o/n","sha":"x","cardData":{"license":"apache-2.0"}}"#),
            &[file("model.safetensors"), file("pytorch_model.bin")],
        );
        assert!(r.allowed(), "{:?}", r.refusals);
        assert_eq!(r.class, LicenseClass::Open);
        assert_eq!(r.excluded_pickle, vec!["pytorch_model.bin"]);
    }

    #[test]
    fn refuses_gated_unknown_and_restricted() {
        let p = Policy::default();
        let gated =
            info(r#"{"id":"o/n","sha":"x","gated":"manual","cardData":{"license":"llama3.2"}}"#);
        let r = p.evaluate(&gated, &[]);
        assert_eq!(r.class, LicenseClass::Restricted);
        assert_eq!(r.refusals.len(), 2);
        let r = p.evaluate(&info(r#"{"id":"o/n","sha":"x"}"#), &[]);
        assert_eq!(r.class, LicenseClass::Unknown);
        assert!(!r.allowed());
        let r = p.evaluate(
            &info(r#"{"id":"o/n","sha":"x","cardData":{"license":["other"]}}"#),
            &[],
        );
        assert_eq!(r.class, LicenseClass::Unknown);

        let lenient = Policy {
            attribution_licenses: vec!["llama3.2".into()],
            allow_gated: true,
            allow_pickle_formats: true,
            ..Policy::default()
        };
        let r = lenient.evaluate(&gated, &[file("a.bin")]);
        assert!(r.allowed(), "{:?}", r.refusals);
        assert_eq!(r.class, LicenseClass::Attribution);
        assert!(r.excluded_pickle.is_empty());
    }

    #[test]
    fn license_override_allows_but_keeps_class() {
        let other = info(r#"{"id":"o/n","sha":"x","cardData":{"license":"other"}}"#);
        let p = Policy {
            license_overrides: vec!["OTHER".into()],
            ..Policy::default()
        };
        let r = p.evaluate(&other, &[]);
        assert!(r.allowed(), "{:?}", r.refusals);
        assert!(r.license_overridden);
        assert_eq!(r.class, LicenseClass::Unknown);
        // The override names one license; others stay refused.
        let llama = info(r#"{"id":"o/n","sha":"x","cardData":{"license":"llama3.2"}}"#);
        assert!(!p.evaluate(&llama, &[]).allowed());
        // No license at all is never overridden.
        assert!(
            !p.evaluate(&info(r#"{"id":"o/n","sha":"x"}"#), &[])
                .allowed()
        );
        // An override never lifts the gated rule.
        let gated = info(r#"{"id":"o/n","sha":"x","gated":"auto","cardData":{"license":"other"}}"#);
        assert!(!p.evaluate(&gated, &[]).allowed());
        // Default policy still refuses `other`.
        assert!(!Policy::default().evaluate(&other, &[]).allowed());
    }
}
