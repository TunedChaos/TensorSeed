// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! `tensorseed reference-logprobs` and `tensorseed check-endpoint`
//! (docs/plans/verified-serving.md section 5.2). The logic lives in
//! [`tensorseed_app::challenge`]; this module resolves the copy, starts or
//! reaches the runtime, and prints.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, bail};
use tensorseed_app::challenge::{
    CHECK_SCHEMA, CheckOptions, CheckReference, CheckReport, CheckSample, Counts, Endpoint,
    GEN_FILE, GEN_METHOD, GENERATOR_NOTE, GENERATOR_TOOL, Generation, Generator, Outcome,
    REFERENCE_SCHEMA, ReferenceFile, Sampling, SigContext, SigStatus, SignatureSummary, Tolerance,
    Verdict, engine_version_line, expected_from, generate_prompts, no_prompt_cache, overall,
    parse_prompt_file, parse_seed, random_32, reference_request_extra, run_checks, sample_indexes,
};
use tensorseed_app::inference::{
    Entrypoint, LaunchPlan, PlanInput, Runtime, VerifyCopyOptions, check_extra_files,
    choose_runtime, default_model_id, find_binary, missing_engine_message, verify_copy, wait_ready,
};
use tensorseed_core::hash::is_lower_hex;

use crate::cli::{CheckEndpointArgs, ReferenceLogprobsArgs, RuntimeArg};
use crate::config::Config;
use crate::inference::resolve_target;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Read an API key from the environment variable `var` (never from argv).
pub(crate) fn api_key_from_env(var: Option<&str>) -> anyhow::Result<Option<String>> {
    let Some(var) = var else { return Ok(None) };
    if var.is_empty() || !var.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        bail!("--api-key-env takes an environment variable name, not {var:?}");
    }
    let v = std::env::var(var)
        .ok()
        .filter(|v| !v.is_empty())
        .with_context(|| format!("--api-key-env names {var}, which is not set"))?;
    Ok(Some(v))
}

/// Write `data` to `path`, readable only by the owner on Unix.
pub(crate) fn write_private(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o
        .open(path)
        .with_context(|| format!("writing {}", path.display()))?;
    std::io::Write::write_all(&mut f, data)?;
    Ok(())
}

fn pretty(v: &impl serde::Serialize) -> anyhow::Result<Vec<u8>> {
    let mut d = serde_json::to_vec_pretty(v)?;
    d.push(b'\n');
    Ok(d)
}

/// `program --version`, first version line.
pub(crate) async fn runtime_version(program: &Path) -> Option<String> {
    let out = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::process::Command::new(program)
            .arg("--version")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    engine_version_line(&text)
}

/// llama.cpp `/props` build info of an endpoint, best effort.
pub(crate) async fn endpoint_version(base_url: &str, api_key: Option<&str>) -> Option<String> {
    let root = base_url.trim_end_matches('/');
    let root = root.strip_suffix("/v1").unwrap_or(root);
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .ok()?;
    let mut req = http.get(format!("{root}/props"));
    if let Some(k) = api_key {
        req = req.bearer_auth(k);
    }
    let v: serde_json::Value = req.send().await.ok()?.json().await.ok()?;
    v.get("build_info")
        .and_then(|b| b.as_str())
        .map(|b| format!("llama.cpp {b}"))
}

/// A free loopback port for a private runtime.
pub(crate) fn free_port() -> anyhow::Result<u16> {
    let l = std::net::TcpListener::bind(("127.0.0.1", 0)).context("finding a free port")?;
    Ok(l.local_addr()?.port())
}

/// `tensorseed reference-logprobs`.
pub async fn reference(cfg: &Config, args: &ReferenceLogprobsArgs) -> anyhow::Result<ExitCode> {
    let sampling = Sampling {
        temperature: 0.0,
        seed: args.sampling_seed,
        max_tokens: args.max_tokens,
        logprobs: true,
        top_logprobs: args.top_logprobs,
    };
    sampling.validate().map_err(|e| {
        anyhow::anyhow!(
            "{}",
            e.to_string()
                .replace("sampling.max_tokens", "--max-tokens")
                .replace("sampling.top_logprobs", "--top-logprobs")
        )
    })?;
    if args.timeout == 0 {
        bail!("--timeout must be at least 1");
    }
    // Prompts first, so a bad file fails before any hashing.
    let (mut prompts, generation) = match &args.prompts {
        Some(p) => {
            let raw = std::fs::read(p).with_context(|| format!("reading {}", p.display()))?;
            let v = parse_prompt_file(&raw).with_context(|| format!("reading {}", p.display()))?;
            let n = u32::try_from(v.len()).context("too many prompts")?;
            (
                v,
                Generation {
                    method: GEN_FILE.into(),
                    seed: None,
                    count: n,
                },
            )
        }
        None => {
            if args.count == 0 || args.count > 100_000 {
                bail!("--count must be 1 to 100000");
            }
            let seed = match &args.gen_seed {
                Some(s) => parse_seed(s)?,
                None => random_32()?,
            };
            (
                generate_prompts(&seed, args.count),
                Generation {
                    method: GEN_METHOD.into(),
                    seed: Some(hex::encode(seed)),
                    count: args.count,
                },
            )
        }
    };
    let endpoint_key = api_key_from_env(args.api_key_env.as_deref())?;
    if endpoint_key.is_some() && args.endpoint.is_none() {
        bail!(
            "--api-key-env applies to --endpoint; a started runtime listens on loopback without a key"
        );
    }

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
        "verified {} files of {}@{} (manifest sha256 {})",
        copy.files.len(),
        copy.manifest.repo.id,
        copy.manifest.hf_commit,
        &copy.manifest_sha256[..16]
    );
    let requested = match args.runtime {
        RuntimeArg::Auto => None,
        RuntimeArg::LlamaCpp => Some(Runtime::LlamaCpp),
        RuntimeArg::Vllm => Some(Runtime::Vllm),
    };
    let (runtime, entry) = choose_runtime(requested, &copy.paths(), args.file.as_deref())?;
    check_extra_files(runtime, &copy.extra)?;
    let entry_sha = match &entry {
        Entrypoint::Gguf(f) => copy
            .files
            .iter()
            .find(|v| &v.path == f)
            .map(|v| v.sha256.clone()),
        Entrypoint::Directory => None,
    };
    let entry_label = match &entry {
        Entrypoint::Gguf(f) => f.clone(),
        Entrypoint::Directory => "(directory)".into(),
    };
    let model_id = args
        .model_id
        .clone()
        .unwrap_or_else(|| default_model_id(&copy.manifest));

    // Reach the runtime: an endpoint the operator names, or a private one.
    let mut child = None;
    let (base_url, engine_version) = if let Some(url) = &args.endpoint {
        let v = match &args.engine_version {
            Some(v) => v.clone(),
            None => endpoint_version(url, endpoint_key.as_deref())
                .await
                .unwrap_or_else(|| "unknown".into()),
        };
        eprintln!(
            "note: using {url}; it must be serving {entry_label} of this verified copy, or the reference is meaningless"
        );
        (url.trim_end_matches('/').to_string(), v)
    } else {
        let mut inf = cfg.inference();
        inf.host = "127.0.0.1".into();
        inf.port = free_port()?;
        inf.api_key_env.clear();
        if let Some(t) = args.ready_timeout {
            inf.ready_timeout_secs = t;
        }
        inf.validate()?;
        let lookup = find_binary(&inf, runtime);
        let Some(program) = lookup.found.clone() else {
            bail!(
                "{} (or point --endpoint at an engine that is already running)",
                missing_engine_message(runtime, &lookup)
            );
        };
        let plan = LaunchPlan::build(PlanInput {
            runtime,
            entrypoint: entry.clone(),
            program,
            dir: &copy.dir,
            model_id: &model_id,
            listen: inf.listen()?,
            cfg: &inf,
            extra: &args.extra,
        });
        let v = match &args.engine_version {
            Some(v) => v.clone(),
            None => runtime_version(&plan.program)
                .await
                .unwrap_or_else(|| "unknown".into()),
        };
        println!("starting {}: {}", runtime.label(), plan.shell_line());
        let mut c = tokio::process::Command::new(&plan.program);
        c.args(&plan.args)
            .kill_on_drop(true)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut c = c
            .spawn()
            .with_context(|| format!("starting {}", plan.program.display()))?;
        let base = plan.base_url();
        let ready = {
            let c = &mut c;
            let wait = wait_ready(
                &base,
                None,
                Duration::from_secs(inf.ready_timeout_secs),
                move || match c.try_wait() {
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
        if let Err(e) = ready {
            let _ = c.kill().await;
            return Err(e);
        }
        child = Some(c);
        (base, v)
    };

    let request_extra = reference_request_extra(runtime);
    let result = async {
        let mut ep = Endpoint::new(
            &base_url,
            &model_id,
            endpoint_key.clone(),
            Duration::from_secs(args.timeout),
        )?;
        ep.extra.clone_from(&request_extra);
        let total = prompts.len();
        for (i, p) in prompts.iter_mut().enumerate() {
            match ep.ask(p, &sampling).await {
                Outcome::Steps(steps) => {
                    p.expected = expected_from(steps, sampling.top_logprobs);
                }
                Outcome::NoLogprobs => bail!(
                    "{base_url} returned no logprobs for {}; this engine cannot produce a reference",
                    p.id
                ),
                Outcome::Error(e) => bail!("{}: {e}", p.id),
            }
            if (i + 1) % 8 == 0 || i + 1 == total {
                println!("  {}/{total} challenges", i + 1);
            }
        }
        anyhow::Ok(())
    };
    let result = tokio::select! {
        r = result => r,
        _ = tokio::signal::ctrl_c() => Err(anyhow::anyhow!("interrupted")),
    };
    if let Some(mut c) = child {
        let _ = c.kill().await;
    }
    result?;

    let mut file = ReferenceFile {
        schema: REFERENCE_SCHEMA.into(),
        manifest_sha256: copy.manifest_sha256.clone(),
        manifest_key: copy.manifest_key.clone(),
        repo: copy.manifest.repo.id.clone(),
        revision: copy.manifest.hf_commit.clone(),
        entrypoint: entry,
        entrypoint_sha256: entry_sha,
        sampling,
        generator: Generator {
            tool: GENERATOR_TOOL.into(),
            tensorseed_version: VERSION.into(),
            runtime,
            engine_version,
            request_extra,
            note: GENERATOR_NOTE.into(),
        },
        generation,
        salt: hex::encode(random_32()?),
        created_at: tensorseed_core::time::now_rfc3339(),
        set_hash: String::new(),
        prompts,
    };
    file.seal()?;
    file.validate()?;
    write_private(&args.out, &pretty(&file)?)?;
    println!();
    println!(
        "wrote {} ({} challenges, {REFERENCE_SCHEMA})",
        args.out.display(),
        file.prompts.len()
    );
    println!("  entrypoint {entry_label}");
    println!(
        "  engine     {} {}",
        file.generator.runtime.label(),
        file.generator.engine_version
    );
    println!("  set_hash   {}", file.set_hash);
    if let Some(c) = &args.commitment {
        let data = pretty(&file.commitment())?;
        if let Some(parent) = c.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(c, data).with_context(|| format!("writing {}", c.display()))?;
        println!("  commitment {} (public)", c.display());
    }
    eprintln!(
        "note: {} holds the challenge prompts and expected values; keep it secret until the set is retired, and publish only set_hash{}",
        args.out.display(),
        if args.commitment.is_some() {
            " or the commitment file"
        } else {
            ""
        }
    );
    Ok(ExitCode::SUCCESS)
}

/// `tensorseed check-endpoint`. Errors that prevent a verdict exit with 2.
pub async fn check(args: &CheckEndpointArgs) -> ExitCode {
    match check_inner(args).await {
        Ok(v) => ExitCode::from(v.exit_code()),
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(Verdict::Inconclusive.exit_code())
        }
    }
}

async fn check_inner(args: &CheckEndpointArgs) -> anyhow::Result<Verdict> {
    if args.timeout == 0 {
        bail!("--timeout must be at least 1");
    }
    if args.concurrency == 0 || args.concurrency > 64 {
        bail!("--concurrency must be 1 to 64");
    }
    if args.repeats == 0 || args.repeats > 10 {
        bail!("--repeats must be 1 to 10");
    }
    let tolerance = Tolerance {
        max_mean_abs: args.max_mean_abs,
        max_token_abs: args.max_token_abs,
    };
    if !(tolerance.max_mean_abs.is_finite() && tolerance.max_mean_abs >= 0.0)
        || !(tolerance.max_token_abs.is_finite() && tolerance.max_token_abs >= 0.0)
    {
        bail!("tolerances must be finite and not negative");
    }
    let url = reqwest::Url::parse(&args.base_url)
        .with_context(|| format!("{:?} is not a URL", args.base_url))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("{} must be an http or https URL", args.base_url);
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("do not put credentials in the URL; use --api-key-env");
    }
    let reference = ReferenceFile::load(&args.reference)?;
    if let Some(h) = &args.expect_set_hash {
        let h = h.trim().to_ascii_lowercase();
        if !is_lower_hex(&h, 64) {
            bail!("--expect-set-hash must be 64 hex digits");
        }
        if h != reference.set_hash {
            bail!(
                "{} has set_hash {}, not the expected {h}",
                args.reference.display(),
                reference.set_hash
            );
        }
    }
    let api_key = api_key_from_env(args.api_key_env.as_deref())?;
    let seed = match &args.sample_seed {
        Some(s) => parse_seed(s)?,
        None => random_32()?,
    };
    let claim = match &args.claim {
        Some(src) => Some(crate::signing::fetch_claim(src).await?),
        None => crate::signing::discover_claim(&args.base_url).await?,
    };
    if claim.is_none() && args.require_signatures {
        bail!(
            "the endpoint serves no claim at its well-known path; pass --claim or drop --require-signatures"
        );
    }
    if let Some((c, _, src)) = &claim {
        if c.catalog.manifest_sha256 != reference.manifest_sha256 {
            bail!(
                "the claim at {src} is for manifest {}, the reference for {}",
                c.catalog.manifest_sha256,
                reference.manifest_sha256
            );
        }
        if c.entrypoint != reference.entrypoint {
            bail!(
                "the claim at {src} serves {:?}, the reference is for {:?}",
                c.entrypoint,
                reference.entrypoint
            );
        }
        if let Some(m) = &args.model_id
            && *m != c.served_model_id
        {
            bail!(
                "--model-id {m} differs from the claim's served_model_id {}",
                c.served_model_id
            );
        }
    }
    let model_id = args.model_id.clone().unwrap_or_else(|| {
        claim.as_ref().map_or_else(
            || reference.repo.clone(),
            |(c, _, _)| c.served_model_id.clone(),
        )
    });
    let idx = sample_indexes(&reference.prompts, args.sample, &seed);
    let chosen: Vec<_> = idx.iter().map(|&i| &reference.prompts[i]).collect();
    let mut ep = Endpoint::new(
        &args.base_url,
        &model_id,
        api_key,
        Duration::from_secs(args.timeout),
    )?;
    if args.no_prompt_cache {
        ep.extra = Some(no_prompt_cache());
    }
    if let Some((c, _, _)) = &claim {
        ep.sig = Some(SigContext {
            pubkey: c.response_pubkey()?,
            claim_sha256: c.claim_sha256.clone(),
            served_model_id: c.served_model_id.clone(),
            max_skew_secs: args.max_skew,
        });
    }
    let opts = CheckOptions {
        tolerance,
        repeats: args.repeats,
        concurrency: args.concurrency,
    };
    if !args.json {
        let entry = match &reference.entrypoint {
            Entrypoint::Gguf(f) => f.clone(),
            Entrypoint::Directory => "(directory)".into(),
        };
        println!(
            "checking {} (model {model_id}) against {}@{} {entry}",
            ep.base_url, reference.repo, reference.revision
        );
        println!(
            "  set_hash {}, {} of {} challenges, sample seed {}",
            reference.set_hash,
            chosen.len(),
            reference.prompts.len(),
            hex::encode(seed)
        );
        println!(
            "  tolerance mean <= {} nats, token <= {} nats, {} attempt(s)",
            tolerance.max_mean_abs, tolerance.max_token_abs, args.repeats
        );
        if let Some((c, _, src)) = &claim {
            println!(
                "  signatures required: claim {} from {src}",
                &c.claim_sha256[..16]
            );
        }
    }
    let results = tokio::select! {
        r = run_checks(&ep, &chosen, &reference.sampling, &opts) => r,
        _ = tokio::signal::ctrl_c() => bail!("interrupted"),
    };
    let verdict = overall(&results);
    let counts = Counts::of(&results);
    let (verified, invalid, absent, unverifiable) = SignatureSummary::count(&results);
    let signatures = (claim.is_some() || unverifiable > 0).then(|| SignatureSummary {
        claim_source: claim.as_ref().map(|(_, _, s)| s.clone()),
        claim_sha256: claim.as_ref().map(|(c, _, _)| c.claim_sha256.clone()),
        response_pubkey: claim
            .as_ref()
            .map(|(c, _, _)| c.response_key.pubkey.clone()),
        seller_wallet: claim.as_ref().and_then(|(_, k, _)| match &k.wallet {
            tensorseed_app::signing::WalletStatus::Verified(a) => Some(a.clone()),
            tensorseed_app::signing::WalletStatus::NotSigned => None,
        }),
        verified,
        invalid,
        absent,
        unverifiable,
    });
    if args.json {
        let report = CheckReport {
            schema: CHECK_SCHEMA.into(),
            tensorseed_version: VERSION.into(),
            base_url: ep.base_url.clone(),
            model_id,
            reference: CheckReference::of(&reference),
            sampling: reference.sampling.clone(),
            tolerance,
            repeats: args.repeats,
            concurrency: args.concurrency,
            timeout_secs: args.timeout,
            sample: CheckSample {
                requested: args.sample,
                sent: chosen.len(),
                seed: hex::encode(seed),
            },
            verdict,
            counts,
            signatures,
            challenges: results,
            checked_at: tensorseed_core::time::now_rfc3339(),
        };
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        let w = results.iter().map(|r| r.id.len()).max().unwrap_or(2);
        for r in &results {
            let metrics = r.comparison.as_ref().map_or_else(String::new, |c| {
                format!(
                    "mean {:>6.3}  max {:>6.3}  top {}",
                    c.mean_abs_dev,
                    c.max_abs_dev,
                    if c.first_token_match {
                        "same"
                    } else {
                        "differs"
                    }
                )
            });
            let sig = match r.signature.as_ref().map(|s| s.status) {
                Some(SigStatus::Verified) => "  sig ok",
                Some(SigStatus::Invalid) => "  sig BAD",
                Some(SigStatus::Absent) => "  sig none",
                Some(SigStatus::Unverifiable) => "  sig ?",
                None => "",
            };
            println!(
                "  {:<w$}  {:<12}  {:>2}x  {metrics}{sig}{}",
                r.id,
                r.verdict.label(),
                r.attempts,
                r.reason
                    .as_deref()
                    .filter(|_| r.verdict != Verdict::Pass)
                    .map(|s| format!("  ({s})"))
                    .unwrap_or_default()
            );
        }
        println!(
            "verdict: {} ({} pass, {} fail, {} inconclusive, {} unsupported)",
            verdict.label(),
            counts.pass,
            counts.fail,
            counts.inconclusive,
            counts.unsupported
        );
        if let Some(s) = &signatures {
            println!(
                "signatures: {} verified, {} invalid, {} absent, {} unverifiable{}",
                s.verified,
                s.invalid,
                s.absent,
                s.unverifiable,
                s.seller_wallet
                    .as_deref()
                    .map(|w| format!(" (claim signed by wallet {w})"))
                    .unwrap_or_default()
            );
        }
    }
    Ok(verdict)
}
