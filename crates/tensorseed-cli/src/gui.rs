// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! `tensorseed gui`: start Tensorseed Desktop if it is installed (SPEC 19).
//! The CLI links no GUI code; it only finds and starts another program.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context;
use tensorseed_app::links::{self, Route};

use crate::cli::GuiArgs;

/// Desktop's binary name (Tauri `mainBinaryName`).
const BINARY: &str = if cfg!(windows) {
    "tensorseed-desktop.exe"
} else {
    "tensorseed-desktop"
};
/// Desktop's application id (Flatpak and bundle identifier).
const APP_ID: &str = "dev.tensorseed.desktop";
/// Where to get Desktop.
const DOWNLOAD_URL: &str = "https://tensorseed.dev";

/// How to start Desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Launcher {
    program: PathBuf,
    /// Arguments before the link.
    args: Vec<String>,
    /// Where it was found, for messages.
    found: String,
}

fn exe(dir: &Path) -> Option<PathBuf> {
    let p = dir.join(BINARY);
    p.is_file().then_some(p)
}

fn home() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// The newest-looking `tensorseed*.AppImage` in `dir`.
fn appimage_in(dir: &Path) -> Option<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                    let n = n.to_ascii_lowercase();
                    n.starts_with("tensorseed") && n.ends_with(".appimage")
                })
        })
        .collect();
    v.sort();
    v.pop()
}

/// Standard install locations, in order.
fn standard_locations() -> Vec<(PathBuf, &'static str)> {
    let mut v = Vec::new();
    if cfg!(target_os = "linux") {
        v.push((PathBuf::from("/usr/bin"), "the system package"));
        v.push((PathBuf::from("/usr/local/bin"), "/usr/local/bin"));
        if let Some(h) = home() {
            v.push((h.join(".local/bin"), "~/.local/bin"));
        }
    } else if cfg!(windows) {
        let name = "Tensorseed Desktop";
        for var in ["LOCALAPPDATA", "ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(base) = std::env::var_os(var).filter(|v| !v.is_empty()) {
                v.push((PathBuf::from(base).join(name), "the installer"));
            }
        }
    } else if cfg!(target_os = "macos") {
        let app = "Tensorseed Desktop.app/Contents/MacOS";
        v.push((Path::new("/Applications").join(app), "/Applications"));
        if let Some(h) = home() {
            v.push((h.join("Applications").join(app), "~/Applications"));
        }
    }
    v
}

fn find_flatpak() -> Option<Launcher> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let flatpak = which("flatpak")?;
    let mut roots = vec![PathBuf::from("/var/lib/flatpak/app")];
    if let Some(h) = home() {
        roots.push(h.join(".local/share/flatpak/app"));
    }
    roots
        .iter()
        .any(|r| r.join(APP_ID).is_dir())
        .then(|| Launcher {
            program: flatpak,
            args: vec!["run".into(), APP_ID.into()],
            found: format!("Flatpak {APP_ID}"),
        })
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .filter(|d| !d.as_os_str().is_empty())
        .find_map(|d| {
            let p = d.join(name);
            p.is_file().then_some(p)
        })
}

/// Find Desktop: `TENSORSEED_DESKTOP`, next to this binary, PATH, the
/// standard locations, AppImages, then Flatpak.
fn locate() -> Result<Launcher, Vec<String>> {
    let mut looked = Vec::new();
    if let Some(p) = std::env::var_os("TENSORSEED_DESKTOP").filter(|v| !v.is_empty()) {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Ok(Launcher {
                program: p,
                args: vec![],
                found: "TENSORSEED_DESKTOP".into(),
            });
        }
        looked.push(format!("TENSORSEED_DESKTOP ({})", p.display()));
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_owned))
    {
        if let Some(p) = exe(&dir) {
            return Ok(Launcher {
                program: p,
                args: vec![],
                found: "next to tensorseed".into(),
            });
        }
        looked.push(dir.display().to_string());
    }
    if let Some(p) = which(BINARY) {
        return Ok(Launcher {
            program: p,
            args: vec![],
            found: "PATH".into(),
        });
    }
    looked.push("PATH".into());
    for (dir, what) in standard_locations() {
        if let Some(p) = exe(&dir) {
            return Ok(Launcher {
                program: p,
                args: vec![],
                found: what.into(),
            });
        }
        looked.push(dir.display().to_string());
    }
    if cfg!(target_os = "linux")
        && let Some(h) = home()
    {
        for d in [h.join("Applications"), h.join(".local/bin")] {
            if let Some(p) = appimage_in(&d) {
                return Ok(Launcher {
                    program: p,
                    args: vec![],
                    found: "AppImage".into(),
                });
            }
        }
        looked.push("~/Applications/*.AppImage".into());
    }
    if let Some(l) = find_flatpak() {
        return Ok(l);
    }
    if cfg!(target_os = "linux") {
        looked.push(format!("Flatpak {APP_ID}"));
    }
    Err(looked)
}

/// The link to open for `args`, if any.
fn link_for(args: &GuiArgs) -> anyhow::Result<Option<String>> {
    let Some(t) = args.target.as_deref() else {
        return Ok(None);
    };
    if t.contains("://") {
        // Check it here so a bad link fails with a clear message.
        let r = links::parse(t)?;
        return Ok(Some(r.to_url()));
    }
    let (repo, rev) = tensorseed_app::pull::parse_repo(t, args.repo_type.into(), None)?;
    let r = Route::entry(repo.repo_type, &repo.id, rev.as_deref(), None)
        .with_context(|| format!("{t:?} is not a repo id or a tensorseed:// link"))?;
    Ok(Some(r.to_url()))
}

/// `tensorseed gui`.
pub fn run(config_path: &Path, args: &GuiArgs) -> anyhow::Result<ExitCode> {
    let link = link_for(args)?;
    let l = match locate() {
        Ok(l) => l,
        Err(looked) => {
            eprintln!("Tensorseed Desktop is not installed.");
            eprintln!("Looked in: {}", looked.join(", "));
            eprintln!(
                "Get it from {DOWNLOAD_URL}, or set TENSORSEED_DESKTOP to the tensorseed-desktop binary. Everything Desktop does is also available in this CLI."
            );
            return Ok(ExitCode::from(1));
        }
    };
    let mut cmd = std::process::Command::new(&l.program);
    cmd.args(&l.args).args(link.iter());
    cmd.env("TENSORSEED_CONFIG", config_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if args.print_command {
        let mut parts = vec![l.program.display().to_string()];
        parts.extend(l.args.iter().cloned());
        parts.extend(link.iter().cloned());
        println!("{}", parts.join(" "));
        return Ok(ExitCode::SUCCESS);
    }
    detach(&mut cmd);
    let mut child = cmd
        .spawn()
        .with_context(|| format!("starting {}", l.program.display()))?;
    // A second launch forwards the link to the running window and exits
    // at once; only a quick failure is an error.
    std::thread::sleep(Duration::from_millis(400));
    if let Ok(Some(st)) = child.try_wait()
        && !st.success()
    {
        anyhow::bail!(
            "Tensorseed Desktop ({}) exited at once ({st})",
            l.program.display()
        );
    }
    println!(
        "opened Tensorseed Desktop ({}){}",
        l.found,
        link.map(|u| format!(" at {u}")).unwrap_or_default()
    );
    Ok(ExitCode::SUCCESS)
}

/// Keep Desktop running after this process and its terminal go away.
fn detach(cmd: &mut std::process::Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::RepoTypeArg;

    fn a(t: Option<&str>, ty: RepoTypeArg) -> GuiArgs {
        GuiArgs {
            target: t.map(str::to_owned),
            repo_type: ty,
            print_command: false,
        }
    }

    #[test]
    fn links_from_targets() {
        assert_eq!(link_for(&a(None, RepoTypeArg::Model)).unwrap(), None);
        assert_eq!(
            link_for(&a(Some("org/name"), RepoTypeArg::Model))
                .unwrap()
                .unwrap(),
            "tensorseed://entry/model/org/name"
        );
        assert_eq!(
            link_for(&a(Some("org/data"), RepoTypeArg::Dataset))
                .unwrap()
                .unwrap(),
            "tensorseed://entry/dataset/org/data"
        );
        assert_eq!(
            link_for(&a(Some("tensorseed://settings"), RepoTypeArg::Model))
                .unwrap()
                .unwrap(),
            "tensorseed://settings"
        );
        assert!(link_for(&a(Some("org"), RepoTypeArg::Model)).is_err());
        assert!(link_for(&a(Some("https://example.com"), RepoTypeArg::Model)).is_err());
    }

    #[test]
    fn appimages_are_found_by_name() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(appimage_in(d.path()), None);
        std::fs::write(d.path().join("other.AppImage"), b"").unwrap();
        std::fs::write(
            d.path().join("Tensorseed Desktop_0.3.0_amd64.AppImage"),
            b"",
        )
        .unwrap();
        assert_eq!(
            appimage_in(d.path()).unwrap().file_name().unwrap(),
            "Tensorseed Desktop_0.3.0_amd64.AppImage"
        );
    }
}
