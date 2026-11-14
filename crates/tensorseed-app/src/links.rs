// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! `tensorseed://` deep links, shared by `tensorseed gui` and Tensorseed
//! Desktop.
//!
//! | Link | Opens |
//! |---|---|
//! | `tensorseed://entry/<org>/<name>` | the entry page of a model |
//! | `tensorseed://entry/model/<org>/<name>` | the same, type explicit |
//! | `tensorseed://entry/dataset/<org>/<name>` | the entry page of a dataset |
//! | `tensorseed://settings` | the settings view |
//! | `tensorseed://` or `tensorseed://catalogs` | the catalog list |
//!
//! An entry link may carry `?revision=<40 hex commit>` and
//! `&catalog=<catalog name>`. Unknown query keys are ignored so newer links
//! still open in older apps; anything else is rejected, and a rejected link
//! never triggers an action. Links only navigate: nothing is pulled,
//! seeded, or changed by opening one.

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use tensorseed_core::keys::{RepoRef, RepoType, check_commit};

/// The URL scheme.
pub const SCHEME: &str = "tensorseed";

/// Longest link accepted.
pub const MAX_LEN: usize = 2048;

/// Where a link points.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Route {
    /// The catalog list.
    Catalogs,
    /// The settings view.
    Settings,
    /// One entry page.
    #[serde(rename_all = "camelCase")]
    Entry {
        /// `model` or `dataset`.
        repo_type: RepoType,
        /// `org/name`.
        id: String,
        /// A pinned commit.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revision: Option<String>,
        /// A catalog name.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        catalog: Option<String>,
    },
}

impl Route {
    /// An entry route, validated.
    ///
    /// # Errors
    ///
    /// Fails on an invalid repo id, commit, or catalog name.
    pub fn entry(
        repo_type: RepoType,
        id: &str,
        revision: Option<&str>,
        catalog: Option<&str>,
    ) -> anyhow::Result<Self> {
        let r = RepoRef::new(repo_type, id)?;
        let revision = revision.map(str::to_ascii_lowercase);
        if let Some(c) = &revision {
            check_commit(c).context("the revision must be a full 40 hex commit")?;
        }
        if let Some(c) = catalog {
            tensorseed_catalog::signed::check_catalog_name(c)?;
        }
        Ok(Self::Entry {
            repo_type,
            id: r.id,
            revision,
            catalog: catalog.map(str::to_owned),
        })
    }

    /// The link for this route.
    #[must_use]
    pub fn to_url(&self) -> String {
        match self {
            Self::Catalogs => format!("{SCHEME}://catalogs"),
            Self::Settings => format!("{SCHEME}://settings"),
            Self::Entry {
                repo_type,
                id,
                revision,
                catalog,
            } => {
                let mut u = format!("{SCHEME}://entry/{}/{id}", repo_type.as_str());
                let mut q = Vec::new();
                if let Some(r) = revision {
                    q.push(format!("revision={r}"));
                }
                if let Some(c) = catalog {
                    q.push(format!("catalog={c}"));
                }
                if !q.is_empty() {
                    u.push('?');
                    u.push_str(&q.join("&"));
                }
                u
            }
        }
    }
}

/// Parse a `tensorseed://` link.
///
/// # Errors
///
/// Fails on another scheme, an unknown target, or invalid parts.
pub fn parse(url: &str) -> anyhow::Result<Route> {
    let url = url.trim();
    if url.len() > MAX_LEN {
        bail!("the link is longer than {MAX_LEN} characters");
    }
    let rest = url
        .get(..SCHEME.len() + 3)
        .filter(|p| p.eq_ignore_ascii_case(&format!("{SCHEME}://")))
        .map(|_| &url[SCHEME.len() + 3..])
        .with_context(|| format!("not a {SCHEME}:// link"))?;
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let query = query.split('#').next().unwrap_or("");
    let path = path.split('#').next().unwrap_or("").trim_end_matches('/');
    if path.contains('%') || query.contains('%') {
        bail!("percent-encoded links are not accepted");
    }
    let segs: Vec<&str> = if path.is_empty() {
        Vec::new()
    } else {
        path.split('/').collect()
    };
    let (mut revision, mut catalog) = (None, None);
    for kv in query.split('&').filter(|s| !s.is_empty()) {
        match kv.split_once('=') {
            Some(("revision", v)) => revision = Some(v),
            Some(("catalog", v)) => catalog = Some(v),
            _ => {}
        }
    }
    match segs.as_slice() {
        [] | ["catalogs"] => Ok(Route::Catalogs),
        ["settings"] => Ok(Route::Settings),
        ["entry", t @ ("model" | "dataset"), org, name] => Route::entry(
            if *t == "dataset" {
                RepoType::Dataset
            } else {
                RepoType::Model
            },
            &format!("{org}/{name}"),
            revision,
            catalog,
        ),
        ["entry", org, name] => {
            Route::entry(RepoType::Model, &format!("{org}/{name}"), revision, catalog)
        }
        _ => bail!("unknown {SCHEME}:// link {url:?}"),
    }
}

/// The first `tensorseed://` link among command line arguments.
pub fn find_in_args<I, S>(args: I) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter().map(|a| a.as_ref().to_string()).find(|a| {
        a.get(..SCHEME.len() + 3)
            .is_some_and(|p| p.eq_ignore_ascii_case(&format!("{SCHEME}://")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn entry_links() {
        let want = Route::Entry {
            repo_type: RepoType::Model,
            id: "Qwen/Qwen2.5-1.5B-Instruct-GGUF".into(),
            revision: None,
            catalog: None,
        };
        assert_eq!(
            parse("tensorseed://entry/Qwen/Qwen2.5-1.5B-Instruct-GGUF").unwrap(),
            want
        );
        assert_eq!(
            parse("tensorseed://entry/model/Qwen/Qwen2.5-1.5B-Instruct-GGUF/").unwrap(),
            want
        );
        assert_eq!(
            parse("TensorSeed://entry/Qwen/Qwen2.5-1.5B-Instruct-GGUF").unwrap(),
            want
        );
        let d = parse(&format!(
            "tensorseed://entry/dataset/org/data?revision={}&catalog=tensorseed&x=1",
            C.to_uppercase()
        ))
        .unwrap();
        assert_eq!(
            d,
            Route::Entry {
                repo_type: RepoType::Dataset,
                id: "org/data".into(),
                revision: Some(C.into()),
                catalog: Some("tensorseed".into()),
            }
        );
        assert_eq!(
            d.to_url(),
            format!("tensorseed://entry/dataset/org/data?revision={C}&catalog=tensorseed")
        );
        assert_eq!(parse(&d.to_url()).unwrap(), d);
    }

    #[test]
    fn other_routes_and_rejections() {
        assert_eq!(parse("tensorseed://").unwrap(), Route::Catalogs);
        assert_eq!(parse("tensorseed://catalogs").unwrap(), Route::Catalogs);
        assert_eq!(parse("tensorseed://settings").unwrap(), Route::Settings);
        for bad in [
            "https://entry/org/name",
            "tensorseed://entry/org",
            "tensorseed://entry/org/name/extra",
            "tensorseed://entry/../name",
            "tensorseed://entry/org/na%2fme",
            "tensorseed://entry/org/name?revision=abc",
            "tensorseed://pull/org/name",
            "tensorseed://entry/org/name?catalog=Bad Name",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert!(parse(&format!("tensorseed://entry/o/{}", "n".repeat(MAX_LEN))).is_err());
    }

    #[test]
    fn route_json_shape() {
        let v = serde_json::to_value(parse("tensorseed://entry/org/name").unwrap()).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"kind": "entry", "repoType": "model", "id": "org/name"})
        );
        assert_eq!(
            serde_json::to_value(Route::Settings).unwrap(),
            serde_json::json!({"kind": "settings"})
        );
    }

    #[test]
    fn finds_the_link_in_argv() {
        let a = [
            "/usr/bin/tensorseed-desktop",
            "--flag",
            "tensorseed://settings",
        ];
        assert_eq!(find_in_args(a).as_deref(), Some("tensorseed://settings"));
        assert_eq!(find_in_args(["x", "y"]), None);
    }
}
