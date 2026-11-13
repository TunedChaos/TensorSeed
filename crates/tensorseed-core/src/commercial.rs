// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Commercial serving marks: may a model be served to paying users?
//!
//! Every catalog entry already records its license id (the Hugging Face
//! license tag, `license_spdx` in index entries, `license.spdx` and
//! `license.hf_tag` in manifests) and the publisher's license class. The
//! mark is derived from those fields on the client; it is never stored in
//! the signed catalog, so older clients and existing signatures are not
//! affected, and a better table only needs a client update.
//!
//! Values ([`CommercialServing`]):
//!
//! - `allowed`: a permissive license with no field of use limits. Keep the
//!   license notice with the weights.
//! - `restricted`: commercial use is allowed with conditions, such as a user
//!   cap, an acceptable use policy, or attribution. The [`Assessment::note`]
//!   names the main ones.
//! - `forbidden`: the license excludes commercial use (non-commercial or
//!   research only licenses).
//! - `unknown`: the license is missing, `other`, custom, or not in the table.
//!
//! [`LICENSE_TABLE`] is the single mapping table; the CLI, the explorer
//! (through the wasm bindings), and Tensorseed Desktop all call [`assess`].
//! The marks are a convenience reading of a license id and are **not legal
//! advice** ([`DISCLAIMER`]).

use serde::{Deserialize, Serialize};

use crate::manifest::{LicenseClass, LicenseInfo};

/// Shown wherever marks are shown.
pub const DISCLAIMER: &str = "Commercial serving marks are derived from the license id only and are not legal advice. Read the model's license before selling access to it.";

/// Whether a license allows serving the model to paying users.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommercialServing {
    /// Permissive license without field of use limits.
    Allowed,
    /// Commercial use allowed with conditions (see the note).
    Restricted,
    /// Non-commercial or research only.
    Forbidden,
    /// Missing, custom, or unrecognized license.
    Unknown,
}

impl CommercialServing {
    /// `allowed`, `restricted`, `forbidden`, or `unknown`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Restricted => "restricted",
            Self::Forbidden => "forbidden",
            Self::Unknown => "unknown",
        }
    }

    /// Parse the lowercase name.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "allowed" => Some(Self::Allowed),
            "restricted" => Some(Self::Restricted),
            "forbidden" => Some(Self::Forbidden),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    /// True for `allowed` and `restricted`: commercial use is possible.
    #[must_use]
    pub fn permits_commercial(self) -> bool {
        matches!(self, Self::Allowed | Self::Restricted)
    }
}

impl std::fmt::Display for CommercialServing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The mark for one license, with a short reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assessment {
    /// The mark.
    pub value: CommercialServing,
    /// License id the mark was derived from (lowercased), if any.
    pub license: Option<String>,
    /// One line reason or the main conditions.
    pub note: String,
}

const NOTE_PERMISSIVE: &str = "permissive license; keep the license and notices with the weights";
const NOTE_COPYLEFT: &str = "copyleft license: serving is allowed; distributing modified weights brings source and license obligations";
const NOTE_LLAMA2: &str = "Llama 2 Community License: needs a separate license from Meta above 700M monthly active users, and the Acceptable Use Policy applies";
const NOTE_LLAMA3: &str = "Llama Community License: needs a separate license from Meta above 700M monthly active users, the Acceptable Use Policy applies, and products must show \"Built with Llama\"";
const NOTE_GEMMA: &str = "Gemma Terms of Use: commercial use allowed, the Gemma Prohibited Use Policy applies and must be passed on to your users";
const NOTE_RAIL: &str = "OpenRAIL style license: commercial use allowed, but its use based restrictions apply and must be passed on to your users";
const NOTE_AGPL: &str =
    "AGPL: serving over a network can oblige you to offer the corresponding source to users";
const NOTE_ND: &str = "no derivatives: serve the weights unmodified (quantizing or merging may count as a derivative)";
const NOTE_SHARE_ALIKE: &str =
    "share alike: adapted weights you distribute must keep the same license";
const NOTE_DEEPSEEK: &str = "DeepSeek License: commercial use allowed, its use based restrictions apply and must be passed on";
const NOTE_NC: &str = "non-commercial license: selling access to this model is not allowed";
const NOTE_RESEARCH: &str = "research or evaluation only license: commercial use is not allowed";
const NOTE_UNKNOWN: &str = "license is missing or custom; read it before serving commercially";
const NOTE_UNMAPPED: &str = "license id is not in the commercial serving table; read the license";

/// The single mapping table: lowercase Hugging Face license id, mark, note.
///
/// Ids not listed fall through to the non-commercial rule (any id with an
/// `nc` part, or naming non-commercial or research use, is `forbidden`) and
/// then to `unknown`. Keep entries sorted by mark, then id.
pub const LICENSE_TABLE: &[(&str, CommercialServing, &str)] = {
    use CommercialServing::{Allowed as A, Forbidden as F, Restricted as R, Unknown as U};
    &[
        ("afl-3.0", A, NOTE_PERMISSIVE),
        ("apache-2.0", A, NOTE_PERMISSIVE),
        ("artistic-2.0", A, NOTE_PERMISSIVE),
        ("bsd", A, NOTE_PERMISSIVE),
        ("bsd-2-clause", A, NOTE_PERMISSIVE),
        ("bsd-3-clause", A, NOTE_PERMISSIVE),
        ("bsd-3-clause-clear", A, NOTE_PERMISSIVE),
        ("bsl-1.0", A, NOTE_PERMISSIVE),
        ("cc-by-2.0", A, NOTE_PERMISSIVE),
        ("cc-by-2.5", A, NOTE_PERMISSIVE),
        ("cc-by-3.0", A, NOTE_PERMISSIVE),
        ("cc-by-4.0", A, NOTE_PERMISSIVE),
        ("cc0-1.0", A, NOTE_PERMISSIVE),
        ("cdla-permissive-1.0", A, NOTE_PERMISSIVE),
        ("cdla-permissive-2.0", A, NOTE_PERMISSIVE),
        ("ecl-2.0", A, NOTE_PERMISSIVE),
        ("epl-1.0", A, NOTE_COPYLEFT),
        ("epl-2.0", A, NOTE_COPYLEFT),
        ("etalab-2.0", A, NOTE_PERMISSIVE),
        ("gpl", A, NOTE_COPYLEFT),
        ("gpl-2.0", A, NOTE_COPYLEFT),
        ("gpl-3.0", A, NOTE_COPYLEFT),
        ("isc", A, NOTE_PERMISSIVE),
        ("lgpl", A, NOTE_COPYLEFT),
        ("lgpl-2.1", A, NOTE_COPYLEFT),
        ("lgpl-3.0", A, NOTE_COPYLEFT),
        ("mit", A, NOTE_PERMISSIVE),
        ("mpl-2.0", A, NOTE_COPYLEFT),
        ("ms-pl", A, NOTE_PERMISSIVE),
        ("odc-by", A, NOTE_PERMISSIVE),
        ("pddl", A, NOTE_PERMISSIVE),
        ("postgresql", A, NOTE_PERMISSIVE),
        ("unlicense", A, NOTE_PERMISSIVE),
        ("wtfpl", A, NOTE_PERMISSIVE),
        ("zlib", A, NOTE_PERMISSIVE),
        ("agpl-3.0", R, NOTE_AGPL),
        ("bigcode-openrail-m", R, NOTE_RAIL),
        ("bigscience-bloom-rail-1.0", R, NOTE_RAIL),
        ("bigscience-openrail-m", R, NOTE_RAIL),
        ("cc-by-nd-4.0", R, NOTE_ND),
        ("cc-by-sa-3.0", R, NOTE_SHARE_ALIKE),
        ("cc-by-sa-4.0", R, NOTE_SHARE_ALIKE),
        ("cdla-sharing-1.0", R, NOTE_SHARE_ALIKE),
        ("creativeml-openrail-m", R, NOTE_RAIL),
        ("deepseek", R, NOTE_DEEPSEEK),
        ("gemma", R, NOTE_GEMMA),
        ("llama2", R, NOTE_LLAMA2),
        ("llama3", R, NOTE_LLAMA3),
        ("llama3.1", R, NOTE_LLAMA3),
        ("llama3.2", R, NOTE_LLAMA3),
        ("llama3.3", R, NOTE_LLAMA3),
        ("llama4", R, NOTE_LLAMA3),
        ("odbl", R, NOTE_SHARE_ALIKE),
        ("openrail", R, NOTE_RAIL),
        ("openrail++", R, NOTE_RAIL),
        ("apple-amlr", F, NOTE_RESEARCH),
        ("cc-by-nc-2.0", F, NOTE_NC),
        ("cc-by-nc-3.0", F, NOTE_NC),
        ("cc-by-nc-4.0", F, NOTE_NC),
        ("cc-by-nc-nd-3.0", F, NOTE_NC),
        ("cc-by-nc-nd-4.0", F, NOTE_NC),
        ("cc-by-nc-sa-2.0", F, NOTE_NC),
        ("cc-by-nc-sa-3.0", F, NOTE_NC),
        ("cc-by-nc-sa-4.0", F, NOTE_NC),
        ("deepfloyd-if-license", F, NOTE_RESEARCH),
        ("fair-noncommercial-research-license", F, NOTE_NC),
        ("intel-research", F, NOTE_RESEARCH),
        ("mrl", F, NOTE_RESEARCH),
        ("other", U, NOTE_UNKNOWN),
        ("unknown", U, NOTE_UNKNOWN),
    ]
};

/// True for ids that name non-commercial or research only use.
fn looks_non_commercial(id: &str) -> bool {
    id.split(['-', '_', '.', ' ']).any(|p| p == "nc")
        || id.contains("noncommercial")
        || id.contains("non-commercial")
        || id.contains("research")
}

/// Mark a license id, using the publisher's class only as context.
///
/// The id is the Hugging Face license tag (`license_spdx` in an index
/// entry). `class` is the manifest's license class when known; an
/// `attribution` class adds a reminder to an `unknown` note.
#[must_use]
pub fn assess(license: Option<&str>, class: Option<LicenseClass>) -> Assessment {
    let id = license
        .map(|l| l.trim().to_ascii_lowercase())
        .filter(|l| !l.is_empty());
    let Some(id) = id else {
        return Assessment {
            value: CommercialServing::Unknown,
            license: None,
            note: NOTE_UNKNOWN.into(),
        };
    };
    let (value, note) = if let Some((_, v, n)) = LICENSE_TABLE.iter().find(|(k, _, _)| *k == id) {
        (*v, (*n).to_string())
    } else if looks_non_commercial(&id) {
        (CommercialServing::Forbidden, NOTE_NC.to_string())
    } else if class == Some(LicenseClass::Attribution) {
        (
            CommercialServing::Unknown,
            format!("{NOTE_UNMAPPED}; the publisher marks it as needing attribution"),
        )
    } else {
        (CommercialServing::Unknown, NOTE_UNMAPPED.to_string())
    };
    Assessment {
        value,
        license: Some(id),
        note,
    }
}

/// Mark the license recorded in a manifest (`spdx`, else `hf_tag`).
#[must_use]
pub fn assess_license_info(l: &LicenseInfo) -> Assessment {
    assess(l.spdx.as_deref().or(l.hf_tag.as_deref()), Some(l.class))
}

#[cfg(test)]
mod tests {
    use super::CommercialServing::{Allowed, Forbidden, Restricted, Unknown};
    use super::*;

    fn v(id: &str) -> CommercialServing {
        assess(Some(id), None).value
    }

    #[test]
    fn permissive_licenses_are_allowed() {
        for id in ["apache-2.0", "mit", "bsd-3-clause", "cc-by-4.0", "cc0-1.0"] {
            assert_eq!(v(id), Allowed, "{id}");
        }
        assert_eq!(v("Apache-2.0"), Allowed, "ids are case insensitive");
        assert_eq!(v("  mit "), Allowed);
    }

    #[test]
    fn community_licenses_are_restricted_with_notes() {
        for id in [
            "llama2", "llama3", "llama3.1", "llama3.2", "llama3.3", "llama4",
        ] {
            let a = assess(Some(id), None);
            assert_eq!(a.value, Restricted, "{id}");
            assert!(a.note.contains("700M"), "{id}: {}", a.note);
            assert!(a.note.contains("Acceptable Use Policy"), "{id}");
        }
        let g = assess(Some("gemma"), None);
        assert_eq!(g.value, Restricted);
        assert!(g.note.contains("Prohibited Use Policy"));
        assert_eq!(v("openrail"), Restricted);
        assert_eq!(v("creativeml-openrail-m"), Restricted);
        assert_eq!(v("agpl-3.0"), Restricted);
    }

    #[test]
    fn non_commercial_licenses_are_forbidden() {
        for id in [
            "cc-by-nc-4.0",
            "cc-by-nc-sa-4.0",
            "cc-by-nc-nd-4.0",
            "apple-amlr",
            "mrl",
            "deepfloyd-if-license",
        ] {
            assert_eq!(v(id), Forbidden, "{id}");
        }
        // Not in the table, caught by the rule.
        assert_eq!(v("cc-by-nc-5.0"), Forbidden);
        assert_eq!(v("acme-research-only"), Forbidden);
        assert_eq!(v("foo-noncommercial"), Forbidden);
        // `nc` must be a whole part.
        assert_eq!(v("ncsa"), Unknown);
    }

    #[test]
    fn missing_and_custom_licenses_are_unknown() {
        assert_eq!(assess(None, None).value, Unknown);
        assert_eq!(assess(Some(""), Some(LicenseClass::Open)).value, Unknown);
        assert_eq!(v("other"), Unknown);
        assert_eq!(v("unknown"), Unknown);
        let a = assess(Some("acme-model-license"), Some(LicenseClass::Attribution));
        assert_eq!(a.value, Unknown);
        assert!(a.note.contains("attribution"));
        assert_eq!(a.license.as_deref(), Some("acme-model-license"));
    }

    #[test]
    fn table_is_lowercase_unique_and_noted() {
        let mut seen = std::collections::HashSet::new();
        for (id, _, note) in LICENSE_TABLE {
            assert_eq!(*id, id.to_ascii_lowercase());
            assert!(seen.insert(*id), "duplicate {id}");
            assert!(!note.is_empty());
            assert!(!note.contains('\u{2013}') && !note.contains('\u{2014}'));
        }
        assert!(!DISCLAIMER.contains('\u{2014}'));
    }

    #[test]
    fn manifest_license_prefers_spdx_then_tag() {
        let mut l = LicenseInfo {
            spdx: Some("mit".into()),
            hf_tag: Some("other".into()),
            class: LicenseClass::Open,
            license_files: Vec::new(),
            attribution: None,
            gated_upstream: false,
        };
        assert_eq!(assess_license_info(&l).value, Allowed);
        l.spdx = None;
        assert_eq!(assess_license_info(&l).value, Unknown);
    }

    #[test]
    fn values_round_trip() {
        for c in [Allowed, Restricted, Forbidden, Unknown] {
            assert_eq!(CommercialServing::parse(c.as_str()), Some(c));
            assert_eq!(serde_json::to_string(&c).unwrap(), format!("\"{c}\""));
        }
        assert!(Allowed.permits_commercial() && Restricted.permits_commercial());
        assert!(!Forbidden.permits_commercial() && !Unknown.permits_commercial());
        assert_eq!(CommercialServing::parse("nope"), None);
    }
}
