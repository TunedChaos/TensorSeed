// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Pulls in process, on `tensorseed_app::pull_job`: live progress, pause,
//! resume, and cancel.
//!
//! The job database, the `.tensorseed/pull-pending.json` options file, and
//! the per folder pull lock are the CLI's, so a pull started in either one
//! shows in both (`tensorseed jobs` and the Downloads view) and either can
//! resume it. Pause stops the transfers and keeps partial downloads. Cancel
//! stops them and deletes the partial downloads; finished, verified files
//! are always kept.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;
use tauri::AppHandle;
use tensorseed_app::config::Config;
use tensorseed_app::pull_job::{self, FileLine, PullControl, PullDone, PullEvent, PullOptions};
use tensorseed_core::keys::RepoRef;
use tensorseed_fetch::{FetchEvent, Outcome};
use tokio_util::sync::CancellationToken;

use crate::jobs::{JobEvent, JobKind, JobState, Jobs, emit};

/// Progress updates go out at most this often.
const TICK_MS: u128 = 300;
/// In flight files listed in a progress update.
const SHOWN: usize = 4;

/// Live progress of a pull (`PullProgress` in `src/lib/native.ts`).
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    /// `resolving`, `swarm`, or `downloading`.
    pub phase: String,
    /// Selected files.
    pub files_total: usize,
    /// Files finished (downloaded, already present, or failed).
    pub files_done: usize,
    /// Selected bytes.
    pub bytes_total: u64,
    /// Bytes on disk and verified or in flight.
    pub bytes_done: u64,
    /// Network rate, bytes per second.
    pub rate: f64,
    /// Files in flight.
    pub current: Vec<String>,
}

/// Folds pull events into [`Progress`].
#[derive(Debug)]
struct Tracker {
    p: Progress,
    /// Bytes on disk per file in flight; `None` until its first progress
    /// event, so a resumed partial does not count as network bytes.
    inflight: BTreeMap<String, Option<u64>>,
    finished: u64,
    net: u64,
    last: Option<(Instant, u64)>,
}

impl Default for Tracker {
    fn default() -> Self {
        Self {
            p: Progress {
                phase: "resolving".into(),
                ..Progress::default()
            },
            inflight: BTreeMap::new(),
            finished: 0,
            net: 0,
            last: None,
        }
    }
}

impl Tracker {
    fn apply(&mut self, ev: &PullEvent) {
        match ev {
            PullEvent::Started { files, bytes, .. } => {
                self.p.files_total = *files;
                self.p.bytes_total = *bytes;
                self.p.phase = "downloading".into();
            }
            PullEvent::SwarmStarted => self.p.phase = "swarm".into(),
            PullEvent::Fetch(f) => {
                self.p.phase = "downloading".into();
                match f {
                    FetchEvent::Checking { path, .. } => {
                        self.inflight.entry(path.clone()).or_insert(None);
                    }
                    FetchEvent::Progress { path, have, .. } => {
                        if let Some(Some(prev)) = self.inflight.insert(path.clone(), Some(*have)) {
                            self.net += have.saturating_sub(prev);
                        }
                    }
                    FetchEvent::File(r) => {
                        self.inflight.remove(&r.path);
                        if !matches!(r.outcome, Outcome::Failed) {
                            self.finished += r.size;
                        }
                        self.p.files_done += 1;
                    }
                }
            }
            _ => {}
        }
        self.p.bytes_done = (self.finished + self.inflight.values().flatten().sum::<u64>())
            .min(self.p.bytes_total.max(self.finished));
        self.p.current = self.inflight.keys().take(SHOWN).cloned().collect();
    }

    /// Update the rate; true when a progress update is due.
    fn due(&mut self, now: Instant) -> bool {
        match self.last {
            None => {
                self.last = Some((now, self.net));
                true
            }
            Some((t, n)) => {
                let dt = now.duration_since(t);
                if dt.as_millis() < TICK_MS {
                    return false;
                }
                let inst = (self.net - n) as f64 / dt.as_secs_f64();
                self.p.rate = if self.p.rate == 0.0 {
                    inst
                } else {
                    0.7 * self.p.rate + 0.3 * inst
                };
                self.last = Some((now, self.net));
                true
            }
        }
    }
}

/// What a stop request means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Intent {
    Run,
    Pause,
    /// Paused by "Pause all"; "Resume all" brings it back.
    PauseAll,
    Cancel,
}

/// Stop handle of a running pull.
#[derive(Debug, Clone)]
pub struct Control {
    token: CancellationToken,
    intent: Arc<Mutex<Intent>>,
}

impl Control {
    fn stop(&self, intent: Intent) {
        if let Ok(mut i) = self.intent.lock() {
            *i = intent;
        }
        self.token.cancel();
    }

    fn intent(&self) -> Intent {
        self.intent.lock().map_or(Intent::Run, |i| *i)
    }
}

/// Start a pull. Returns the window job id.
///
/// # Errors
///
/// Fails when the repo id is invalid.
pub fn start(app: AppHandle, jobs: Jobs, cfg: Config, opts: PullOptions) -> anyhow::Result<u64> {
    let repo = RepoRef::new(opts.repo_type, opts.repo.clone())?;
    let mut ev = jobs.begin(JobKind::Pull, opts.repo_type.as_str(), &opts.repo);
    let job = ev.job;
    let out = opts
        .out
        .clone()
        .unwrap_or_else(|| tensorseed_app::pull::default_out(&cfg, &repo));
    ev.out = Some(out.display().to_string());
    ev.progress = Some(Tracker::default().p);
    jobs.update(job, |j| {
        j.out = ev.out.clone();
        j.progress = ev.progress.clone();
    });
    let control = Control {
        token: CancellationToken::new(),
        intent: Arc::new(Mutex::new(Intent::Run)),
    };
    if let Ok(mut c) = jobs.controls.lock() {
        c.insert(job, control.clone());
    }
    ev.line = Some(match &opts.revision {
        Some(r) => format!("pull {} {}@{r}", repo.repo_type, repo.id),
        None => format!("pull {} {}", repo.repo_type, repo.id),
    });
    emit(&app, &ev);

    let tracker = Arc::new(Mutex::new(Tracker::default()));
    let ctl = {
        let (app, jobs, tracker) = (app.clone(), jobs.clone(), tracker.clone());
        PullControl::new(move |e: PullEvent| {
            // Files are listed as they finish; of the end of run lines only
            // the swarm ones add anything.
            let line = match &e {
                PullEvent::Fetch(FetchEvent::File(r)) => Some(pull_job::live_file_line(r)),
                PullEvent::File(FileLine::Swarm { .. }) => e.line().map(|(l, _)| l),
                PullEvent::File(_) => None,
                _ => e.line().map(|(l, _)| l),
            };
            let Ok(mut t) = tracker.lock() else { return };
            let phase = t.p.phase.clone();
            t.apply(&e);
            // Phase changes always go out; the swarm sends no byte events.
            let changed = t.p.phase != phase;
            if let PullEvent::Started { job: db, out, .. } = &e {
                jobs.update(job, |j| {
                    j.db_job = Some(db.clone());
                    j.out = Some(out.display().to_string());
                });
            }
            let due = t.due(Instant::now());
            if line.is_none() && !due && !changed {
                return;
            }
            let p = t.p.clone();
            drop(t);
            if let Some(mut snap) = jobs.update(job, |j| j.progress = Some(p)) {
                snap.line = line;
                emit(&app, &snap);
            }
        })
        .stop_on(control.token.clone())
    };

    tauri::async_runtime::spawn(async move {
        let r = pull_job::run(&cfg, &opts, &ctl).await;
        let mut ev = jobs.update(job, |_| {}).unwrap_or_else(|| ev.clone());
        ev.line = None;
        if let Ok(t) = tracker.lock() {
            ev.progress = Some(t.p.clone());
        }
        match r {
            Ok(d) => finish(&mut ev, &d),
            Err(e) if pull_job::is_stopped(&e) => {
                stopped(&cfg, &jobs, &mut ev, control.intent(), out);
            }
            Err(e) => {
                ev.state = JobState::Failed;
                ev.message = Some(format!("{e:#}"));
            }
        }
        if let Ok(mut c) = jobs.controls.lock() {
            c.remove(&job);
        }
        jobs.end(job);
        emit(&app, &ev);
    });
    Ok(job)
}

fn finish(ev: &mut JobEvent, d: &PullDone) {
    if d.ok {
        ev.state = JobState::Ok;
        let mut msg = format!(
            "pulled {} into {} and verified every file",
            d.repo.id,
            d.out.display()
        );
        if d.swarm_files > 0 {
            msg.push_str(&format!("; {} from the swarm", d.swarm_files));
        }
        if !d.signed {
            msg.push_str(" (not from the signed catalog)");
        }
        ev.message = Some(msg);
    } else {
        ev.state = JobState::Failed;
        ev.message = Some(format!(
            "{} of {} files did not download; resume to retry them (finished files are kept)",
            d.failed,
            d.failed + d.downloaded + d.present + d.swarm_files
        ));
    }
}

fn stopped(cfg: &Config, jobs: &Jobs, ev: &mut JobEvent, intent: Intent, out: PathBuf) {
    let out = ev.out.as_deref().map_or(out, PathBuf::from);
    match (intent, ev.db_job.clone()) {
        (Intent::Cancel, Some(db)) => match pull_job::discard(cfg, &out, &db) {
            Ok(()) => {
                ev.state = JobState::Cancelled;
                ev.message =
                    Some("cancelled; partial downloads deleted, finished files kept".into());
            }
            Err(e) => {
                ev.state = JobState::Failed;
                ev.message = Some(format!("cancelled, but cleaning up failed: {e:#}"));
            }
        },
        (_, None) => {
            pull_job::clear_pending(&out);
            ev.state = JobState::Cancelled;
            ev.message = Some("stopped before the download started".into());
        }
        (_, Some(db)) => {
            ev.state = JobState::Paused;
            ev.message = Some("paused; partial downloads kept, resume to continue".into());
            if intent == Intent::PauseAll
                && let Ok(mut p) = jobs.paused.lock()
            {
                p.insert(db);
            }
        }
    }
}

/// Pause running pull `job`, or every running pull.
pub fn pause(jobs: &Jobs, job: Option<u64>) -> usize {
    let Ok(c) = jobs.controls.lock() else {
        return 0;
    };
    let mut n = 0;
    for (id, ctl) in c.iter() {
        if job.is_none_or(|j| j == *id) {
            ctl.stop(if job.is_none() {
                Intent::PauseAll
            } else {
                Intent::Pause
            });
            n += 1;
        }
    }
    n
}

/// Cancel running pull `job`.
pub fn cancel(jobs: &Jobs, job: u64) -> bool {
    let Ok(c) = jobs.controls.lock() else {
        return false;
    };
    c.get(&job).map(|ctl| ctl.stop(Intent::Cancel)).is_some()
}

/// Running pulls.
pub fn running(jobs: &Jobs) -> usize {
    jobs.controls.lock().map_or(0, |c| c.len())
}

/// One job of the shared database (`JobRow` in `src/lib/native.ts`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobRow {
    /// Database id.
    pub id: String,
    /// `model org/name@commit`.
    pub label: String,
    /// Output folder.
    pub out_dir: String,
    /// `pending`, `running`, `paused`, `done`, `failed`, or `interrupted`
    /// (recorded as running, but no pull holds its folder).
    pub status: String,
    /// Selected files.
    pub files_total: u64,
    /// Finished files.
    pub files_done: u64,
    /// Last error.
    pub error: Option<String>,
    /// Last change, Unix seconds.
    pub updated_unix: i64,
    /// A pull (here or in the CLI) is writing to its folder now.
    pub busy: bool,
    /// The window job running it, when it runs here.
    pub window_job: Option<u64>,
    /// Can be resumed.
    pub resumable: bool,
}

/// Jobs in the shared database, newest first.
///
/// # Errors
///
/// Fails when the database cannot be read.
pub fn history(cfg: &Config, jobs: &Jobs) -> anyhow::Result<Vec<JobRow>> {
    let here: BTreeMap<String, u64> = jobs
        .list()
        .into_iter()
        .filter_map(|j| j.db_job.map(|d| (d, j.job)))
        .collect();
    let mut rows: Vec<JobRow> = tensorseed_app::pull::state_db(cfg)?
        .jobs()?
        .into_iter()
        .map(|r| {
            let window_job = here.get(&r.id).copied();
            let busy =
                window_job.is_some() || pull_job::pull_running_in(&PathBuf::from(&r.out_dir));
            let status = if r.status == "running" && !busy {
                "interrupted".to_string()
            } else {
                r.status.clone()
            };
            let resumable = !busy && status != "done" && pull_job::resume_options(&r).is_some();
            JobRow {
                id: r.id,
                label: r.label,
                out_dir: r.out_dir,
                status,
                files_total: r.files_total,
                files_done: r.files_done,
                error: r.error,
                updated_unix: r.updated_unix,
                busy,
                window_job,
                resumable,
            }
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.updated_unix));
    Ok(rows)
}

fn record(cfg: &Config, id: &str) -> anyhow::Result<tensorseed_fetch::state::JobRecord> {
    tensorseed_app::pull::state_db(cfg)?
        .jobs()?
        .into_iter()
        .find(|r| r.id == id)
        .ok_or_else(|| anyhow::anyhow!("no job {id}"))
}

/// Resume database job `id` in the window.
///
/// # Errors
///
/// Fails when there is no such job, it is finished or running, or its
/// options cannot be recovered.
pub fn resume(app: AppHandle, jobs: Jobs, cfg: Config, id: &str) -> anyhow::Result<u64> {
    let r = record(&cfg, id)?;
    if r.status == "done" {
        anyhow::bail!("job {id} is finished");
    }
    if pull_job::pull_running_in(&PathBuf::from(&r.out_dir)) {
        anyhow::bail!("a pull is already writing to {}", r.out_dir);
    }
    let opts = pull_job::resume_options(&r)
        .ok_or_else(|| anyhow::anyhow!("job {id} has no options to resume with"))?;
    if let Ok(mut p) = jobs.paused.lock() {
        p.remove(id);
    }
    start(app, jobs, cfg, opts)
}

/// Resume the pulls "Pause all" stopped.
pub fn resume_all(app: &AppHandle, jobs: &Jobs, cfg: &Config) -> usize {
    let ids: Vec<String> = jobs
        .paused
        .lock()
        .map(|p| p.iter().cloned().collect())
        .unwrap_or_default();
    ids.iter()
        .filter(|id| resume(app.clone(), jobs.clone(), cfg.clone(), id).is_ok())
        .count()
}

/// Pulls "Pause all" stopped, still waiting for "Resume all".
pub fn paused_count(jobs: &Jobs) -> usize {
    jobs.paused.lock().map_or(0, |p| p.len())
}

/// Delete the partial downloads of database job `id` and forget it.
///
/// # Errors
///
/// Fails when there is no such job or a pull is writing to its folder.
pub fn discard(cfg: &Config, jobs: &Jobs, id: &str) -> anyhow::Result<()> {
    let r = record(cfg, id)?;
    pull_job::discard(cfg, &PathBuf::from(&r.out_dir), id)?;
    if let Ok(mut p) = jobs.paused.lock() {
        p.remove(id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tensorseed_fetch::FileResult;

    #[test]
    fn progress_follows_fetch_events() {
        let mut t = Tracker::default();
        assert_eq!(t.p.phase, "resolving");
        t.apply(&PullEvent::Started {
            job: "j".into(),
            label: "model org/name@c".into(),
            files: 2,
            bytes: 300,
            out: PathBuf::from("/x"),
            source: "catalog".into(),
        });
        assert_eq!((t.p.files_total, t.p.bytes_total), (2, 300));
        t.apply(&PullEvent::Fetch(FetchEvent::Checking {
            path: "a".into(),
            size: 100,
        }));
        // b resumes from a 40 byte partial: on disk, but not network bytes.
        t.apply(&PullEvent::Fetch(FetchEvent::Progress {
            path: "b".into(),
            have: 40,
            size: 200,
        }));
        assert_eq!((t.p.bytes_done, t.net), (40, 0));
        t.apply(&PullEvent::Fetch(FetchEvent::Progress {
            path: "b".into(),
            have: 50,
            size: 200,
        }));
        assert_eq!(t.p.current, ["a", "b"]);
        assert_eq!(t.p.bytes_done, 50);
        t.apply(&PullEvent::Fetch(FetchEvent::File(FileResult {
            path: "a".into(),
            size: 100,
            outcome: Outcome::AlreadyPresent,
            errors: vec![],
        })));
        assert_eq!((t.p.files_done, t.p.bytes_done), (1, 150));
        assert_eq!(t.net, 10, "already present bytes are not network bytes");
        t.apply(&PullEvent::Fetch(FetchEvent::Progress {
            path: "b".into(),
            have: 200,
            size: 200,
        }));
        assert_eq!(t.net, 160);
        assert_eq!(t.p.current, ["b"]);
        assert!(t.due(Instant::now()));
        assert!(!t.due(Instant::now()), "throttled");
    }

    #[test]
    fn pause_and_cancel_reach_the_token() {
        let jobs = Jobs::default();
        let c = Control {
            token: CancellationToken::new(),
            intent: Arc::new(Mutex::new(Intent::Run)),
        };
        jobs.controls.lock().unwrap().insert(7, c.clone());
        assert_eq!(running(&jobs), 1);
        assert!(!cancel(&jobs, 8));
        assert_eq!(pause(&jobs, Some(7)), 1);
        assert!(c.token.is_cancelled());
        assert_eq!(c.intent(), Intent::Pause);
        assert!(cancel(&jobs, 7));
        assert_eq!(c.intent(), Intent::Cancel);
    }

    #[test]
    fn rows_serialize_camel_case() {
        let v = serde_json::to_value(Progress {
            phase: "downloading".into(),
            files_total: 1,
            ..Progress::default()
        })
        .unwrap();
        assert_eq!(v["filesTotal"], 1);
        assert_eq!(v["bytesDone"], 0);
    }
}
