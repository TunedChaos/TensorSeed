// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Which catalog revisions a `serve` daemon seeds (spec section 8.3).
//!
//! Rules come from `[serve]`: repo globs (`org/*`, `org/name`, `*`) with
//! excludes, repo types, license classes, latest revision only or all
//! revisions, a per revision size cap, and a total disk budget. Revisions
//! that are taken down are never selected. Order decides who fits the
//! budget: the first matching `select` pattern wins, then the latest
//! revision of a repo before older ones, then newer publications, then the
//! repo id.

use serde::Serialize;

/// `*` matches any run of characters (including `/`), `?` one character.
/// Matching is case sensitive, like Hugging Face repo ids in URLs.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// One catalog revision that could be seeded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Candidate {
    /// `model` or `dataset`.
    pub repo_type: String,
    /// `org/name`.
    pub repo_id: String,
    /// Full commit SHA.
    pub commit: String,
    /// Payload bytes.
    pub bytes: u64,
    /// Manifest license class (`open`, `attribution`, `restricted`, `unknown`).
    pub license_class: String,
    /// The repo's latest revision in the catalog.
    pub latest: bool,
    /// Manifest `published_at` (RFC 3339), used for ordering.
    pub published_at: String,
    /// A tombstone covers this revision.
    pub taken_down: bool,
}

/// Selection rules.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rules {
    /// Repo id globs; empty selects nothing.
    pub select: Vec<String>,
    /// Repo id globs removed again.
    pub exclude: Vec<String>,
    /// Allowed repo types; empty allows all.
    pub types: Vec<String>,
    /// Allowed license classes; empty allows all.
    pub license_classes: Vec<String>,
    /// Only each repo's latest revision.
    pub latest_only: bool,
    /// Largest single revision in bytes (0 = no cap).
    pub max_revision_bytes: u64,
    /// Total budget in bytes for everything selected (0 = unlimited).
    pub max_bytes: u64,
}

/// What happened to a candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "decision", content = "reason", rename_all = "snake_case")]
pub enum Decision {
    /// Seed it.
    Selected,
    /// Not seeded, with the reason.
    Skipped(String),
}

impl Rules {
    /// Index of the first `select` pattern matching `repo_id`, unless excluded.
    pub fn rank(&self, repo_id: &str) -> Option<usize> {
        if self.exclude.iter().any(|x| glob_match(x, repo_id)) {
            return None;
        }
        self.select.iter().position(|s| glob_match(s, repo_id))
    }

    /// Decide every candidate. Candidates that match no `select` pattern
    /// are dropped silently; the rest come back in priority order.
    pub fn plan(&self, candidates: Vec<Candidate>) -> Vec<(Candidate, Decision)> {
        let mut ranked: Vec<(usize, Candidate)> = candidates
            .into_iter()
            .filter_map(|c| self.rank(&c.repo_id).map(|r| (r, c)))
            .collect();
        ranked.sort_by(|(ra, a), (rb, b)| {
            ra.cmp(rb)
                .then(b.latest.cmp(&a.latest))
                .then(b.published_at.cmp(&a.published_at))
                .then(a.repo_id.cmp(&b.repo_id))
                .then(a.commit.cmp(&b.commit))
        });
        let mut used = 0u64;
        let mut out = Vec::with_capacity(ranked.len());
        for (_, c) in ranked {
            let d = if c.taken_down {
                Decision::Skipped("taken down".into())
            } else if self.latest_only && !c.latest {
                Decision::Skipped("not the latest revision (latest_only)".into())
            } else if !self.types.is_empty() && !self.types.contains(&c.repo_type) {
                Decision::Skipped(format!("repo type {} not selected", c.repo_type))
            } else if !self.license_classes.is_empty()
                && !self.license_classes.contains(&c.license_class)
            {
                Decision::Skipped(format!("license class {} not selected", c.license_class))
            } else if self.max_revision_bytes > 0 && c.bytes > self.max_revision_bytes {
                Decision::Skipped(format!(
                    "{} bytes is over max_revision_bytes ({})",
                    c.bytes, self.max_revision_bytes
                ))
            } else if self.max_bytes > 0 && used.saturating_add(c.bytes) > self.max_bytes {
                Decision::Skipped(format!(
                    "over the disk budget ({} of {} bytes already selected)",
                    used, self.max_bytes
                ))
            } else {
                used += c.bytes;
                Decision::Selected
            };
            out.push((c, d));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: &str, commit: &str, bytes: u64, latest: bool) -> Candidate {
        Candidate {
            repo_type: "model".into(),
            repo_id: id.into(),
            commit: commit.into(),
            bytes,
            license_class: "open".into(),
            latest,
            published_at: "2026-11-12T00:00:00Z".into(),
            taken_down: false,
        }
    }

    #[test]
    fn globs() {
        assert!(glob_match("*", "a/b"));
        assert!(glob_match("org/*", "org/name"));
        assert!(!glob_match("org/*", "other/name"));
        assert!(glob_match("org/na?e", "org/name"));
        assert!(glob_match(
            "*/*-GGUF",
            "TunedChaos/ChaosNexus_Tuned_v1-GGUF"
        ));
        assert!(!glob_match("*-GGUF", "org/model"));
        assert!(glob_match("a*b*c", "aXXbYYc"));
        assert!(!glob_match("a*b*c", "aXXbYY"));
        assert!(glob_match("", ""));
        assert!(!glob_match("", "x"));
    }

    #[test]
    fn rules_order_filter_and_budget() {
        let r = Rules {
            select: vec!["big/*".into(), "*".into()],
            exclude: vec!["*/secret".into()],
            latest_only: true,
            max_bytes: 150,
            ..Default::default()
        };
        let mut down = c("org/gone", "c", 1, true);
        down.taken_down = true;
        let mut restricted = c("org/r", "d", 1, true);
        restricted.license_class = "restricted".into();
        let plan = r.plan(vec![
            c("org/a", "1", 60, true),
            c("big/x", "2", 100, true),
            c("big/x", "0", 100, false),
            c("org/secret", "3", 1, true),
            down,
            restricted,
            c("org/b", "4", 60, true),
        ]);
        let got: Vec<(String, bool)> = plan
            .iter()
            .map(|(c, d)| {
                (
                    format!("{}@{}", c.repo_id, c.commit),
                    *d == Decision::Selected,
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("big/x@2".into(), true),
                ("big/x@0".into(), false),
                ("org/a@1".into(), false), // 100 + 60 > 150
                ("org/b@4".into(), false),
                ("org/gone@c".into(), false),
                ("org/r@d".into(), true),
            ]
        );
        assert!(matches!(&plan[1].1, Decision::Skipped(s) if s.contains("latest_only")));
        assert!(matches!(&plan[2].1, Decision::Skipped(s) if s.contains("disk budget")));
        assert!(matches!(&plan[4].1, Decision::Skipped(s) if s == "taken down"));

        let strict = Rules {
            select: vec!["*".into()],
            license_classes: vec!["open".into()],
            types: vec!["model".into()],
            max_revision_bytes: 10,
            ..Default::default()
        };
        let mut ds = c("org/ds", "5", 1, true);
        ds.repo_type = "dataset".into();
        let mut restricted = c("org/r", "d", 1, true);
        restricted.license_class = "restricted".into();
        let plan = strict.plan(vec![restricted, ds, c("org/huge", "6", 11, true)]);
        assert!(plan.iter().all(|(_, d)| *d != Decision::Selected));
        assert!(
            Rules::default()
                .plan(vec![c("a/b", "1", 1, true)])
                .is_empty()
        );
    }
}
