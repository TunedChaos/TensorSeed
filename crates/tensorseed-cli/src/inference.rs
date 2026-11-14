// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! `tensorseed serve-inference`: verify a pulled copy, check its license,
//! and run it behind an OpenAI compatible server. The logic lives in
//! [`tensorseed_app::inference`]; this module only prints.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, bail};
use tensorseed_app::inference::{
    LaunchPlan, PlanInput, Runtime, ServedWeights, VerifyCopyOptions, check_extra_files,
    choose_runtime, default_model_id, find_binary, license_gate, verify_copy, wait_ready,
};
use tensorseed_core::commercial::DISCLAIMER;
use tensorseed_core::keys::{RepoRef, RepoType};
use tensorseed_core::paths::native_path;

use crate::cli::{RuntimeArg, ServeInferenceArgs};
use crate::config::Config;
use crate::consumer::{default_out, parse_repo};

/// A resolved target: the directory, and the repo and revision when named.
pub type Target = (PathBuf, Option<(RepoRef, Option<String>)>);

/// Resolve a `serve-inference` style target: a pulled directory, or a repo
/// id `org/name[@rev]` read from `dir` or the pull default directory.
///
/// # Errors
///
/// Fails when the target is neither a directory nor a valid repo id.
pub fn resolve_target(cfg: &Config, target: &str, dir: Option<&Path>) -> anyhow::Result<Target> {
    let t = Path::new(target);
    if t.is_dir() && dir.is_none() {
        return Ok((native_path(t), None));
    }
    let (r, rev) = parse_repo(target, RepoType::Model, None)?;
    let dir = dir.map(native_path).unwrap_or_else(|| default_out(cfg, &r));
    Ok((dir, Some((r, rev))))
}

/// `tensorseed serve-inference`.
pub async fn run(cfg: &Config, args: &ServeInferenceArgs) -> anyhow::Result<ExitCode> {
    let mut inf = cfg.inference();
    if let Some(h) = &args.host {
        inf.host.clone_from(h);
    }
    if let Some(p) = args.port {
        inf.port = p;
    }
    if let Some(t) = args.ready_timeout {
        inf.ready_timeout_secs = t;
    }
    inf.validate()?;
    let listen = inf.listen()?;

    let (dir, repo) = resolve_target(cfg, &args.target, args.dir.as_deref())?;

    println!("verifying {} against the signed manifest", dir.display());
    let copy = verify_copy(
        cfg,
        &dir,
        repo,
        &VerifyCopyOptions {
            no_wait: args.no_wait,
            catalog: args.catalog.clone(),
            threads: args.threads,
        },
    )
    .await?;
    println!(
        "verified {} files of {}@{} (manifest {}, sha256 {})",
        copy.files.len(),
        copy.manifest.repo.id,
        copy.manifest.hf_commit,
        copy.manifest_source,
        &copy.manifest_sha256[..16]
    );
    if !copy.skipped.is_empty() {
        println!("  {} file(s) skipped at pull time", copy.skipped.len());
    }

    let lic = license_gate(&copy.manifest, args.i_have_rights)?;
    println!(
        "license {}: commercial serving {}",
        lic.assessment.license.as_deref().unwrap_or("(none)"),
        lic.assessment.value
    );
    for w in &lic.warnings {
        eprintln!("warning: {w}");
    }
    eprintln!("note: {DISCLAIMER}");

    let requested = match args.runtime {
        RuntimeArg::Auto => None,
        RuntimeArg::LlamaCpp => Some(Runtime::LlamaCpp),
        RuntimeArg::Vllm => Some(Runtime::Vllm),
    };
    let (runtime, entry) = choose_runtime(requested, &copy.paths(), args.file.as_deref())?;
    check_extra_files(runtime, &copy.extra)?;
    if !copy.extra.is_empty() {
        eprintln!(
            "note: {} file(s) in the directory are not in the manifest and are not served",
            copy.extra.len()
        );
    }
    let lookup = find_binary(&inf, runtime);
    let model_id = args
        .model_id
        .clone()
        .unwrap_or_else(|| default_model_id(&copy.manifest));
    let program = lookup
        .found
        .clone()
        .unwrap_or_else(|| lookup.wanted.clone().into());
    let plan = LaunchPlan::build(PlanInput {
        runtime,
        entrypoint: entry,
        program,
        dir: &copy.dir,
        model_id: &model_id,
        listen,
        cfg: &inf,
        extra: &args.extra,
    });

    if args.print_command {
        if lookup.found.is_none() {
            eprintln!(
                "note: {} was not found; install {} or set {}",
                lookup.wanted,
                runtime.label(),
                runtime.config_key()
            );
        }
        println!("{}", plan.shell_line());
        return Ok(ExitCode::SUCCESS);
    }
    if lookup.found.is_none() {
        bail!(
            "{} ({}) was not found{}; install it, set {}, or rerun with --print-command",
            lookup.wanted,
            runtime.label(),
            if lookup.configured { "" } else { " on PATH" },
            runtime.config_key()
        );
    }
    let api_key = match &plan.api_key {
        Some((_, src)) => Some(
            std::env::var(src)
                .ok()
                .filter(|v| !v.is_empty())
                .with_context(|| format!("inference.api_key_env names {src}, which is not set"))?,
        ),
        None => None,
    };
    if !listen.ip().is_loopback() && api_key.is_none() {
        eprintln!(
            "warning: listening on {listen} without an API key; anyone who can reach it can use it. Prefer host 127.0.0.1 behind an authenticating TLS proxy"
        );
    }
    if let Err(e) = std::net::TcpListener::bind(listen) {
        bail!("cannot listen on {listen}: {e}");
    }

    println!("starting {}: {}", runtime.label(), plan.shell_line());
    let mut cmd = tokio::process::Command::new(&plan.program);
    cmd.args(&plan.args).kill_on_drop(true);
    if let (Some((dst, _)), Some(k)) = (&plan.api_key, &api_key) {
        cmd.env(dst, k);
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("starting {}", plan.program.display()))?;

    let base = plan.base_url();
    let ready = {
        let child = &mut child;
        let wait = wait_ready(
            &base,
            api_key.as_deref(),
            Duration::from_secs(inf.ready_timeout_secs),
            move || match child.try_wait() {
                Ok(Some(st)) => bail!("{} exited before it was ready ({st})", runtime.label()),
                Ok(None) => Ok(()),
                Err(e) => bail!("checking {}: {e}", runtime.label()),
            },
        );
        tokio::select! {
            r = wait => r,
            _ = tokio::signal::ctrl_c() => Err(anyhow::anyhow!("interrupted")),
        }
    };
    let ids = match ready {
        Ok(ids) => ids,
        Err(e) => {
            let _ = child.kill().await;
            return Err(e);
        }
    };
    if !ids.contains(&model_id) {
        eprintln!(
            "warning: /v1/models lists {ids:?}, not {model_id:?}; list the id the server reports"
        );
    }
    let sw = ServedWeights::new(
        &copy,
        &plan,
        &lic,
        args.i_have_rights,
        args.public_url.clone(),
        tensorseed_core::time::now_rfc3339(),
    );
    let sw_path = sw.write(&copy.dir)?;

    println!();
    println!("serving {} with {}", copy.manifest.repo.id, runtime.label());
    println!("  base_url  {base}");
    println!("  model id  {model_id}");
    if let Some(u) = &args.public_url {
        println!("  public    {u}");
    }
    println!(
        "  record    {} (unsigned, ts.served-weights/0)",
        sw_path.display()
    );
    println!(
        "list base_url (through your TLS proxy) and the model id on an OpenAI compatible marketplace; see docs/SELL-INFERENCE.md"
    );
    println!("press Ctrl-C to stop");

    tokio::select! {
        st = child.wait() => {
            let st = st?;
            if st.success() {
                Ok(ExitCode::SUCCESS)
            } else {
                bail!("{} exited ({st})", runtime.label())
            }
        }
        _ = tokio::signal::ctrl_c() => {
            eprintln!("stopping {}", runtime.label());
            let _ = child.kill().await;
            Ok(ExitCode::SUCCESS)
        }
    }
}
