// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Native path helpers.
//!
//! Repo ids (`org/name`), manifest file paths (`sub/model.bin`), and storage
//! keys always use `/`. Joining such a string onto a [`Path`] in one piece
//! keeps the `/` inside it, so on Windows the result mixes separators
//! (`C:\dl\datasets\openai/gsm8k`). These helpers split the string into
//! components first, and normalize paths a user typed into a config file or
//! on the command line, so the paths Tensorseed builds and prints use the
//! platform separator. On Unix nothing changes: `/` is already native and a
//! backslash is an ordinary file name character there.
//!
//! These helpers do not validate anything. Use
//! `verify::safe_join` for untrusted manifest paths.
//!
//! ```
//! use std::path::Path;
//! use tensorseed_core::paths::{join_slash, rel_path};
//!
//! let rel = rel_path("openai/gsm8k");
//! assert_eq!(rel.components().count(), 2);
//! let out = join_slash(Path::new("downloads"), "datasets/openai/gsm8k");
//! assert_eq!(out, Path::new("downloads").join("datasets").join("openai").join("gsm8k"));
//! ```

use std::path::{Path, PathBuf};

/// A relative path built from a `/` separated string one component at a
/// time. Empty components (leading, trailing, or doubled `/`) are skipped.
pub fn rel_path(slash: &str) -> PathBuf {
    let mut p = PathBuf::new();
    for c in slash.split('/').filter(|c| !c.is_empty()) {
        p.push(c);
    }
    p
}

/// `base` joined with a `/` separated relative path, component by
/// component. An empty `rel` returns `base` unchanged (no trailing
/// separator).
pub fn join_slash(base: &Path, rel: &str) -> PathBuf {
    let mut p = base.to_path_buf();
    for c in rel.split('/').filter(|c| !c.is_empty()) {
        p.push(c);
    }
    p
}

/// A user supplied path string with native separators.
///
/// On Windows, `/` becomes `\` (verbatim `\\?\` paths, where `/` is a
/// literal character, are left alone). Everywhere else the string is used
/// as is.
pub fn native(path: &str) -> PathBuf {
    PathBuf::from(native_string(path))
}

/// [`native`] for a path that is already a [`Path`] (command line
/// arguments). Paths that are not valid UTF-8 are returned unchanged.
pub fn native_path(path: &Path) -> PathBuf {
    match path.to_str() {
        Some(s) => native(s),
        None => path.to_path_buf(),
    }
}

/// [`native`] as a string, for config values that stay strings.
pub fn native_string(path: &str) -> String {
    if cfg!(windows) && !path.starts_with(r"\\?\") {
        path.replace('/', "\\")
    } else {
        path.to_string()
    }
}

/// True when `path` contains both `/` and `\`. Only meaningful on Windows,
/// where both are separators; used by tests and diagnostics.
pub fn has_mixed_separators(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.contains('/') && s.contains('\\')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Component;

    fn parts(p: &Path) -> Vec<String> {
        p.components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn rel_path_splits_repo_ids_into_components() {
        assert_eq!(parts(&rel_path("openai/gsm8k")), ["openai", "gsm8k"]);
        assert_eq!(parts(&rel_path("a/b/c.json")), ["a", "b", "c.json"]);
        assert_eq!(parts(&rel_path("/a//b/")), ["a", "b"]);
        assert_eq!(rel_path(""), PathBuf::new());
        assert!(
            rel_path("org/name")
                .components()
                .all(|c| matches!(c, Component::Normal(_)))
        );
    }

    #[test]
    fn join_slash_matches_per_component_joins() {
        let base = Path::new("dl");
        assert_eq!(
            join_slash(base, "datasets/openai/gsm8k"),
            base.join("datasets").join("openai").join("gsm8k")
        );
        assert_eq!(join_slash(base, ""), base);
        // No component of the result contains a `/`.
        for c in join_slash(base, "x/y/z.bin").components() {
            assert!(!c.as_os_str().to_string_lossy().contains('/'));
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_layout_is_unchanged() {
        assert_eq!(
            join_slash(Path::new("/home/u/dl"), "datasets/openai/gsm8k"),
            Path::new("/home/u/dl/datasets/openai/gsm8k")
        );
        // A backslash is a file name character on Unix; leave it.
        assert_eq!(native(r"a\b/c"), Path::new(r"a\b/c"));
        assert_eq!(native_string("~/x/y"), "~/x/y");
        assert!(!has_mixed_separators(&join_slash(Path::new("/a"), "b/c")));
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_use_backslashes() {
        let p = native("C:/Users/x/tensorseed-test/downloads");
        assert_eq!(p, Path::new(r"C:\Users\x\tensorseed-test\downloads"));
        let out = join_slash(&p.join("datasets"), "openai/gsm8k");
        assert_eq!(
            out.to_str().unwrap(),
            r"C:\Users\x\tensorseed-test\downloads\datasets\openai\gsm8k"
        );
        assert!(!has_mixed_separators(&out));
        assert!(!out.to_str().unwrap().contains('/'));
        assert_eq!(native(r"\\?\C:\a/b"), Path::new(r"\\?\C:\a/b"));
        assert_eq!(native_string("~/x/y"), r"~\x\y");
    }

    #[test]
    fn mixed_separator_detection() {
        assert!(has_mixed_separators(Path::new(r"C:/a\b")));
        assert!(!has_mixed_separators(Path::new("a/b")));
        assert!(!has_mixed_separators(Path::new(r"a\b")));
    }
}
