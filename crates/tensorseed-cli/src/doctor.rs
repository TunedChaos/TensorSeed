// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! `tensorseed doctor`: render the checks from `tensorseed_app::doctor`.

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use tensorseed_app::doctor::{self, Options, Report, Status};

use crate::cli::DoctorArgs;

/// Run the checks and print them. Exit 0 when nothing failed, 1 otherwise.
pub async fn run(config_path: &Path, args: &DoctorArgs) -> anyhow::Result<ExitCode> {
    let opts = Options {
        offline: args.offline,
        timeout: Duration::from_secs(args.timeout.max(1)),
        warn_free_bytes: args.min_free_gib.saturating_mul(1024 * 1024 * 1024),
        whoami: !args.no_whoami,
        ..Options::default()
    };
    let report = doctor::run(config_path, &opts).await;
    let mut out = std::io::stdout().lock();
    if args.json {
        serde_json::to_writer_pretty(&mut out, &report)?;
        writeln!(out)?;
    } else {
        out.write_all(render(&report).as_bytes())?;
    }
    Ok(if report.has_failures() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// Text form: one aligned line per check, details indented below it.
pub fn render(r: &Report) -> String {
    let width = r.checks.iter().map(|c| c.title.len()).max().unwrap_or(0);
    let mut s = format!(
        "tensorseed {} doctor{}\n",
        r.version,
        if r.offline { " (offline)" } else { "" }
    );
    for c in &r.checks {
        s.push_str(&format!(
            "{:<4}  {:<width$}  {}\n",
            c.status.label(),
            c.title,
            c.message
        ));
        for d in &c.details {
            s.push_str(&format!("      {:<width$}    {d}\n", ""));
        }
    }
    s.push_str(&format!(
        "{} ok, {} warn, {} fail ({:.1}s)\n",
        r.count(Status::Ok),
        r.count(Status::Warn),
        r.count(Status::Fail),
        r.elapsed_ms as f64 / 1000.0
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tensorseed_app::doctor::{Check, DOCTOR_SCHEMA};

    fn check(title: &str, status: Status, msg: &str, details: &[&str]) -> Check {
        Check {
            id: title.replace(' ', "."),
            title: title.into(),
            status,
            message: msg.into(),
            details: details.iter().map(|d| (*d).to_string()).collect(),
        }
    }

    #[test]
    fn render_aligns_and_summarizes() {
        let r = Report {
            schema: DOCTOR_SCHEMA,
            version: "9.9.9".into(),
            config_path: PathBuf::from("ts.toml"),
            offline: false,
            elapsed_ms: 1500,
            checks: vec![
                check("config", Status::Ok, "loaded ts.toml", &[]),
                check(
                    "catalog main",
                    Status::Fail,
                    "unreachable",
                    &["a  timed out"],
                ),
                check("disk space", Status::Warn, "1.0 GiB free", &[]),
            ],
        };
        let text = render(&r);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "tensorseed 9.9.9 doctor");
        assert_eq!(lines[1], "ok    config        loaded ts.toml");
        assert_eq!(lines[2], "fail  catalog main  unreachable");
        assert_eq!(lines[3], "                      a  timed out");
        assert_eq!(lines[4], "warn  disk space    1.0 GiB free");
        assert_eq!(lines[5], "1 ok, 1 warn, 1 fail (1.5s)");
        assert!(r.has_failures());
    }
}
