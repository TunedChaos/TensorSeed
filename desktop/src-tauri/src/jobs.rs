// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Pull and verify jobs started from an entry page.
//!
//! Verify runs natively through `tensorseed_app::inference::verify_copy`
//! for models. The pull pipeline still lives in the CLI (its extraction
//! into `tensorseed-app` is M3.3), so a pull runs the installed
//! `tensorseed pull` with the same config file and streams its output
//! lines to the window. The result is the same copy, `pull.json`, and state
//! the CLI writes, because it is the CLI.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, BufReader};

/// Event carrying a [`JobEvent`].
pub const JOB_EVENT: &str = "job://event";

/// The CLI binary name.
const CLI: &str = if cfg!(windows) {
    "tensorseed.exe"
} else {
    "tensorseed"
};

/// What a job does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum JobKind {
    /// `tensorseed pull`.
    Pull,
    /// Re-hash a local copy against its signed manifest.
    Verify,
}

/// Job state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum JobState {
    /// Still going.
    Running,
    /// Finished successfully.
    Ok,
    /// Failed.
    Failed,
}

/// One job update (`JobEvent` in `src/lib/native.ts`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobEvent {
    /// Job id.
    pub job: u64,
    /// What it does.
    pub kind: JobKind,
    /// `model` or `dataset`.
    pub repo_type: String,
    /// Repo id.
    pub id: String,
    /// State after this update.
    pub state: JobState,
    /// A new output line, if this update carries one.
    pub line: Option<String>,
    /// Summary when the job ends.
    pub message: Option<String>,
}

/// Running jobs, for the tray and status.
#[derive(Default, Clone)]
pub struct Jobs {
    next: Arc<AtomicU64>,
    running: Arc<Mutex<BTreeMap<u64, JobEvent>>>,
}

impl Jobs {
    /// Running jobs of `kind`.
    pub fn count(&self, kind: JobKind) -> usize {
        self.running
            .lock()
            .map(|m| m.values().filter(|j| j.kind == kind).count())
            .unwrap_or(0)
    }

    /// Running jobs.
    pub fn list(&self) -> Vec<JobEvent> {
        self.running
            .lock()
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default()
    }

    fn begin(&self, kind: JobKind, repo_type: &str, id: &str) -> JobEvent {
        let job = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let ev = JobEvent {
            job,
            kind,
            repo_type: repo_type.to_string(),
            id: id.to_string(),
            state: JobState::Running,
            line: None,
            message: None,
        };
        if let Ok(mut m) = self.running.lock() {
            m.insert(job, ev.clone());
        }
        ev
    }

    fn end(&self, job: u64) {
        if let Ok(mut m) = self.running.lock() {
            m.remove(&job);
        }
    }
}

fn exe_in(dir: &Path) -> Option<PathBuf> {
    let p = dir.join(CLI);
    p.is_file().then_some(p)
}

/// Find the `tensorseed` CLI: `TENSORSEED_CLI`, next to this binary, PATH,
/// then the usual install directories.
pub fn cli_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("TENSORSEED_CLI").filter(|v| !v.is_empty()) {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    if let Some(p) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().and_then(exe_in))
    {
        return Some(p);
    }
    if let Some(path) = std::env::var_os("PATH")
        && let Some(p) = std::env::split_paths(&path)
            .filter(|d| !d.as_os_str().is_empty())
            .find_map(|d| exe_in(&d))
    {
        return Some(p);
    }
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let mut dirs = vec![PathBuf::from("/usr/bin"), PathBuf::from("/usr/local/bin")];
    if let Some(h) = home {
        dirs.push(h.join(".local/bin"));
        dirs.push(h.join(".cargo/bin"));
    }
    dirs.iter().find_map(|d| exe_in(d))
}

fn emit(app: &AppHandle, ev: &JobEvent) {
    let _ = app.emit(JOB_EVENT, ev);
}

/// Arguments for `tensorseed pull`.
fn pull_args(
    repo_type: &str,
    id: &str,
    revision: Option<&str>,
    catalog: Option<&str>,
) -> Vec<String> {
    let spec = match revision {
        Some(r) => format!("{id}@{r}"),
        None => id.to_string(),
    };
    let mut v = vec!["pull".into(), spec, "--type".into(), repo_type.into()];
    if let Some(c) = catalog {
        v.push("--catalog".into());
        v.push(c.into());
    }
    v
}

/// Run the CLI with `args` as job `ev`, streaming output lines.
fn spawn_cli(
    app: AppHandle,
    jobs: Jobs,
    mut ev: JobEvent,
    config: PathBuf,
    args: Vec<String>,
    done: String,
) -> anyhow::Result<()> {
    let Some(cli) = cli_path() else {
        jobs.end(ev.job);
        anyhow::bail!(
            "the tensorseed CLI was not found (set TENSORSEED_CLI or put tensorseed on PATH); Desktop runs it for pulls until the pull pipeline moves into the app"
        );
    };
    let mut cmd = tokio::process::Command::new(&cli);
    cmd.args(&args)
        .env("TENSORSEED_CONFIG", &config)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            jobs.end(ev.job);
            anyhow::bail!("starting {}: {e}", cli.display());
        }
    };
    ev.line = Some(format!("$ tensorseed {}", args.join(" ")));
    emit(&app, &ev);
    let out = child.stdout.take();
    let err = child.stderr.take();
    tauri::async_runtime::spawn(async move {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        if let Some(o) = out {
            let tx = tx.clone();
            tauri::async_runtime::spawn(async move {
                let mut l = BufReader::new(o).lines();
                while let Ok(Some(line)) = l.next_line().await {
                    let _ = tx.send(line);
                }
            });
        }
        if let Some(e) = err {
            let tx = tx.clone();
            tauri::async_runtime::spawn(async move {
                let mut l = BufReader::new(e).lines();
                while let Ok(Some(line)) = l.next_line().await {
                    let _ = tx.send(line);
                }
            });
        }
        drop(tx);
        while let Some(line) = rx.recv().await {
            ev.line = Some(line);
            emit(&app, &ev);
        }
        ev.line = None;
        match child.wait().await {
            Ok(st) if st.success() => {
                ev.state = JobState::Ok;
                ev.message = Some(done);
            }
            Ok(st) => {
                ev.state = JobState::Failed;
                ev.message = Some(match st.code() {
                    Some(c) => format!("tensorseed exited with code {c}; see the log above"),
                    None => "tensorseed was stopped".into(),
                });
            }
            Err(e) => {
                ev.state = JobState::Failed;
                ev.message = Some(format!("waiting for tensorseed: {e}"));
            }
        }
        jobs.end(ev.job);
        emit(&app, &ev);
    });
    Ok(())
}

/// What to pull.
#[derive(Debug, Clone)]
pub struct PullRequest {
    /// `model` or `dataset`.
    pub repo_type: String,
    /// Repo id.
    pub id: String,
    /// Commit, or the latest.
    pub revision: Option<String>,
    /// Catalog to resolve through.
    pub catalog: Option<String>,
    /// Where the copy lands (`general.download_dir`), for the summary.
    pub out: PathBuf,
}

/// Start `tensorseed pull` for an entry. Returns the job id.
///
/// # Errors
///
/// Fails when the CLI cannot be found or started.
pub fn start_pull(
    app: AppHandle,
    jobs: Jobs,
    config: PathBuf,
    p: &PullRequest,
) -> anyhow::Result<u64> {
    let ev = jobs.begin(JobKind::Pull, &p.repo_type, &p.id);
    let job = ev.job;
    let done = format!(
        "pulled {} into {} and verified every file",
        p.id,
        p.out.display()
    );
    let args = pull_args(
        &p.repo_type,
        &p.id,
        p.revision.as_deref(),
        p.catalog.as_deref(),
    );
    spawn_cli(app, jobs, ev, config, args, done)?;
    Ok(job)
}

/// Verify a local copy. Models verify natively against the signed
/// manifest (`verify_copy`); datasets run `tensorseed verify`, which
/// handles them, until that moves into the app crate too.
///
/// # Errors
///
/// Fails when a dataset verify cannot start the CLI.
pub fn start_verify(
    app: AppHandle,
    jobs: Jobs,
    config: PathBuf,
    cfg: tensorseed_app::config::Config,
    repo: tensorseed_core::keys::RepoRef,
    dir: PathBuf,
    catalog: Option<String>,
) -> anyhow::Result<u64> {
    let ty = repo.repo_type.to_string();
    let mut ev = jobs.begin(JobKind::Verify, &ty, &repo.id);
    let job = ev.job;
    if repo.repo_type != tensorseed_core::keys::RepoType::Model {
        let mut args = vec!["verify".into(), dir.display().to_string()];
        if let Some(c) = catalog {
            args.push("--catalog".into());
            args.push(c);
        }
        let done = format!("{} matches its signed manifest", dir.display());
        spawn_cli(app, jobs, ev, config, args, done)?;
        return Ok(job);
    }
    ev.line = Some(format!("verifying {} natively", dir.display()));
    emit(&app, &ev);
    tauri::async_runtime::spawn(async move {
        let opts = tensorseed_app::inference::VerifyCopyOptions {
            no_wait: false,
            catalog,
            threads: 4,
        };
        let r = tensorseed_app::inference::verify_copy(&cfg, &dir, Some((repo, None)), &opts).await;
        ev.line = None;
        match r {
            Ok(v) => {
                ev.state = JobState::Ok;
                let mut msg = format!(
                    "{} files match the manifest signed for catalog {} ({}@{})",
                    v.files.len(),
                    v.catalog,
                    v.manifest.repo.id,
                    &v.manifest.hf_commit[..12.min(v.manifest.hf_commit.len())]
                );
                if !v.skipped.is_empty() {
                    msg.push_str(&format!("; {} skipped by choice", v.skipped.len()));
                }
                if !v.extra.is_empty() {
                    msg.push_str(&format!(
                        "; {} extra files not in the manifest",
                        v.extra.len()
                    ));
                }
                ev.message = Some(msg);
            }
            Err(e) => {
                ev.state = JobState::Failed;
                ev.message = Some(format!("{e:#}"));
            }
        }
        jobs.end(ev.job);
        emit(&app, &ev);
    });
    Ok(job)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_arguments() {
        assert_eq!(
            pull_args("model", "org/name", None, None),
            ["pull", "org/name", "--type", "model"]
        );
        assert_eq!(
            pull_args("dataset", "org/d", Some(&"a".repeat(40)), Some("main")),
            [
                "pull".to_string(),
                format!("org/d@{}", "a".repeat(40)),
                "--type".into(),
                "dataset".into(),
                "--catalog".into(),
                "main".into()
            ]
        );
    }

    #[test]
    fn job_events_use_camel_case() {
        let v = serde_json::to_value(JobEvent {
            job: 1,
            kind: JobKind::Pull,
            repo_type: "model".into(),
            id: "org/name".into(),
            state: JobState::Running,
            line: Some("x".into()),
            message: None,
        })
        .unwrap();
        assert_eq!(v["repoType"], "model");
        assert_eq!(v["kind"], "pull");
        assert_eq!(v["state"], "running");
    }
}
