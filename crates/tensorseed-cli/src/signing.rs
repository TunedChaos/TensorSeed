// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! `tensorseed response-key`, `claim-serving`, `sign-responses`, and
//! `verify-response` (docs/plans/verified-serving.md sections 3 and 5.1).
//! The logic lives in [`tensorseed_app::signing`] and
//! [`tensorseed_app::sign_proxy`]; this module resolves paths and prints.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, bail};
use tensorseed_app::inference::{
    Runtime, ServedWeights, VerifyCopyOptions, check_extra_files, choose_runtime, default_model_id,
    find_binary, license_gate, verify_copy,
};
use tensorseed_app::sign_proxy::SignProxy;
use tensorseed_app::signing::{
    ClaimCheck, ClaimExtras, DEFAULT_MAX_SKEW_SECS, Expect, LogprobsMode, RESPONSE_KEY_FILE,
    ResponseKey, ServedClaim, WELL_KNOWN_CLAIM, WalletStatus, claim_from_bytes, claim_path,
    extract_signed, load_claim, verify_fields,
};
use tensorseed_core::commercial::DISCLAIMER;

use crate::challenge::runtime_version;
use crate::cli::{
    ClaimServingArgs, LogprobsArg, ResponseKeyCmd, RuntimeArg, SignResponsesArgs,
    VerifyResponseArgs,
};
use crate::config::Config;
use crate::inference::resolve_target;

/// Largest claim document fetched over HTTP.
const MAX_CLAIM_BYTES: usize = 4 * 1024 * 1024;

/// `response-key.json` next to the config file.
pub fn default_key_path(config_path: &Path) -> PathBuf {
    config_path.parent().map_or_else(
        || PathBuf::from(RESPONSE_KEY_FILE),
        |p| p.join(RESPONSE_KEY_FILE),
    )
}

fn key_path(config_path: &Path, given: Option<&Path>) -> PathBuf {
    given.map_or_else(|| default_key_path(config_path), Path::to_path_buf)
}

/// Load the response key, with a hint when it does not exist yet.
pub fn load_key(
    config_path: &Path,
    given: Option<&Path>,
) -> anyhow::Result<(ResponseKey, PathBuf)> {
    let p = key_path(config_path, given);
    if !p.exists() {
        bail!(
            "no response key at {}; create one with `tensorseed response-key create`",
            p.display()
        );
    }
    Ok((ResponseKey::load(&p)?, p))
}

impl From<LogprobsArg> for LogprobsMode {
    fn from(a: LogprobsArg) -> Self {
        match a {
            LogprobsArg::Required => Self::Required,
            LogprobsArg::None => Self::None,
        }
    }
}

/// `tensorseed response-key`.
pub fn key_cmd(config_path: &Path, cmd: &ResponseKeyCmd) -> anyhow::Result<ExitCode> {
    match cmd {
        ResponseKeyCmd::Create { out, force } => {
            let p = key_path(config_path, out.as_deref());
            let k = ResponseKey::generate()?;
            k.save_new(&p, *force)?;
            println!("created response key {} (owner-only file)", p.display());
            println!("pubkey {}", k.pubkey_hex());
            eprintln!(
                "note: keep the file private and back it up; the secret is never printed. Write a new claim if you replace it"
            );
        }
        ResponseKeyCmd::Show { key } => {
            let (k, p) = load_key(config_path, key.as_deref())?;
            println!("key    {}", p.display());
            println!("pubkey {}", k.pubkey_hex());
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn print_claim_summary(path: &Path, c: &ServedClaim, check: &ClaimCheck) {
    println!("claim {} ({})", path.display(), c.schema);
    println!("  claim_sha256 {}", c.claim_sha256);
    println!(
        "  model        {} ({}@{})",
        c.served_model_id, c.repo, c.revision
    );
    println!("  engine       {} {}", c.runtime.label(), c.engine_version);
    println!("  response key {}", c.response_key.pubkey);
    match (&c.seller_wallet, &check.wallet) {
        (Some(_), WalletStatus::Verified(a)) => println!("  wallet       {a} (signature verified)"),
        (Some(w), WalletStatus::NotSigned) => {
            println!("  wallet       {w} (not signed yet)");
        }
        (None, _) => println!("  wallet       (none)"),
    }
    for w in &check.warnings {
        eprintln!("warning: {w}");
    }
}

/// Print the EIP-191 message and how to attach the signature.
pub fn print_wallet_instructions(c: &ServedClaim, target: &str) {
    if c.seller_wallet.is_none() || c.sig.is_some() {
        return;
    }
    println!();
    println!("To bind the claim to your wallet, sign this exact message with EIP-191");
    println!("(personal_sign) in your own wallet; Tensorseed never holds wallet keys:");
    println!();
    println!("{}", c.message());
    println!();
    println!("then attach it: tensorseed claim-serving {target} --sig 0x...");
}

/// Load the existing v1 claim at `path`, if one is there and verifies.
pub fn existing_claim(path: &Path) -> Option<ServedClaim> {
    load_claim(path).ok().map(|(c, _)| c)
}

/// `tensorseed claim-serving`.
pub async fn claim_serving(
    cfg: &Config,
    config_path: &Path,
    args: &ClaimServingArgs,
) -> anyhow::Result<ExitCode> {
    if let Some(sig) = &args.sig {
        let t = Path::new(&args.target);
        let path = if t.is_file() {
            t.to_path_buf()
        } else if let Some(o) = &args.out {
            o.clone()
        } else {
            claim_path(&resolve_target(cfg, &args.target, args.dir.as_deref())?.0)
        };
        let (mut c, _) = load_claim(&path)?;
        let addr = c.attach_signature(sig)?;
        let check = c.verify()?;
        c.write(&path)?;
        println!("attached the wallet signature of {addr}");
        print_claim_summary(&path, &c, &check);
        return Ok(ExitCode::SUCCESS);
    }

    let (key, kp) = load_key(config_path, args.response_key.as_deref())?;
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
        "verified {} files of {}@{}",
        copy.files.len(),
        copy.manifest.repo.id,
        copy.manifest.hf_commit
    );
    let lic = license_gate(&copy.manifest, args.i_have_rights)?;
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
    let engine_version = match &args.engine_version {
        Some(v) => v.clone(),
        None => {
            let lookup = find_binary(&cfg.inference(), runtime);
            let found = match &lookup.found {
                Some(p) => runtime_version(p).await,
                None => None,
            };
            found.with_context(|| {
                format!(
                    "could not read the {} version; pass --engine-version",
                    runtime.label()
                )
            })?
        }
    };
    let base_url = check_base_url(&args.base_url)?;
    let model_id = args
        .model_id
        .clone()
        .unwrap_or_else(|| default_model_id(&copy.manifest));
    let now = tensorseed_core::time::now_rfc3339();
    let record = ServedWeights::from_parts(
        &copy,
        runtime,
        entry,
        model_id,
        base_url,
        &lic,
        args.i_have_rights,
        args.public_url.clone(),
        now.clone(),
    );
    let claim = ServedClaim::from_record(
        record,
        ClaimExtras {
            engine_version,
            response_pubkey: key.pubkey_hex(),
            seller_wallet: args.wallet.clone(),
            logprobs: args.logprobs.into(),
            started_at: now,
        },
    )?;
    let out = args.out.clone().unwrap_or_else(|| claim_path(&copy.dir));
    let (claim, reused) = claim.reuse(existing_claim(&out));
    if reused {
        println!("the claim at {} already says this; kept it", out.display());
    } else {
        claim.write(&out)?;
    }
    let check = claim.verify()?;
    println!("response key {}", kp.display());
    print_claim_summary(&out, &claim, &check);
    println!(
        "run the signing proxy: tensorseed sign-responses --upstream <engine root URL> --claim {}",
        out.display()
    );
    print_wallet_instructions(&claim, &out.display().to_string());
    Ok(ExitCode::SUCCESS)
}

fn check_base_url(u: &str) -> anyhow::Result<String> {
    let url = reqwest::Url::parse(u).with_context(|| format!("{u:?} is not a URL"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        bail!("{u} must be an http or https URL");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("do not put credentials in the URL");
    }
    Ok(u.trim_end_matches('/').to_string())
}

/// `tensorseed sign-responses`.
pub async fn sign_responses(
    config_path: &Path,
    args: &SignResponsesArgs,
) -> anyhow::Result<ExitCode> {
    let listen: SocketAddr = args.listen.parse().with_context(|| {
        format!(
            "--listen {:?} is not an address like 127.0.0.1:8090",
            args.listen
        )
    })?;
    let (key, _) = load_key(config_path, args.response_key.as_deref())?;
    let proxy = SignProxy::bind(listen, &args.upstream, &args.claim, key).await?;
    let addr = proxy.local_addr()?;
    if !addr.ip().is_loopback() {
        eprintln!(
            "warning: the signing proxy listens on {addr}; prefer 127.0.0.1 behind your TLS reverse proxy"
        );
    }
    println!("signing proxy on http://{addr} -> {}", args.upstream);
    println!("  claim_sha256 {}", proxy.claim_sha256());
    println!("  claim at     http://{addr}{WELL_KNOWN_CLAIM}");
    println!("press Ctrl-C to stop");
    proxy
        .run(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(ExitCode::SUCCESS)
}

/// Load a claim from a file or an http(s) URL. A URL that does not end in
/// `.json` is a base URL: `/v1` is stripped and the well-known path added.
pub async fn fetch_claim(src: &str) -> anyhow::Result<(ServedClaim, ClaimCheck, String)> {
    if !(src.starts_with("http://") || src.starts_with("https://")) {
        let (c, check) = load_claim(Path::new(src))?;
        return Ok((c, check, src.to_string()));
    }
    let url = reqwest::Url::parse(src).with_context(|| format!("{src:?} is not a URL"))?;
    if !url.username().is_empty() || url.password().is_some() {
        bail!("do not put credentials in the URL");
    }
    let url = if url.path().ends_with(".json") {
        url.to_string()
    } else {
        let root = src.trim_end_matches('/');
        let root = root.strip_suffix("/v1").unwrap_or(root);
        format!("{root}{WELL_KNOWN_CLAIM}")
    };
    let raw = fetch_capped(&url).await?;
    let (c, check) = claim_from_bytes(&raw).with_context(|| format!("claim at {url}"))?;
    Ok((c, check, url))
}

/// GET `url`, `Ok(None)` on 404.
async fn fetch_capped_opt(url: &str) -> anyhow::Result<Option<Vec<u8>>> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()?;
    let mut r = http
        .get(url)
        .send()
        .await
        .with_context(|| format!("fetching {url}"))?;
    if r.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !r.status().is_success() {
        bail!("fetching {url}: HTTP {}", r.status());
    }
    let mut buf = Vec::new();
    while let Some(c) = r.chunk().await? {
        if buf.len() + c.len() > MAX_CLAIM_BYTES {
            bail!("{url} is larger than {MAX_CLAIM_BYTES} bytes");
        }
        buf.extend_from_slice(&c);
    }
    Ok(Some(buf))
}

async fn fetch_capped(url: &str) -> anyhow::Result<Vec<u8>> {
    fetch_capped_opt(url)
        .await?
        .with_context(|| format!("{url}: HTTP 404 Not Found"))
}

/// The claim an endpoint serves at its well-known path, `None` when it
/// serves none (404, or not a claim).
pub async fn discover_claim(
    base_url: &str,
) -> anyhow::Result<Option<(ServedClaim, ClaimCheck, String)>> {
    let root = base_url.trim_end_matches('/');
    let root = root.strip_suffix("/v1").unwrap_or(root);
    let url = format!("{root}{WELL_KNOWN_CLAIM}");
    let Some(raw) = fetch_capped_opt(&url).await.ok().flatten() else {
        return Ok(None);
    };
    if serde_json::from_slice::<serde_json::Value>(&raw).is_err() {
        return Ok(None);
    }
    let (c, check) =
        claim_from_bytes(&raw).with_context(|| format!("the claim served at {url}"))?;
    Ok(Some((c, check, url)))
}

/// Parse `curl -D` output: the last response block's header lines.
pub fn parse_header_dump(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with("HTTP/") {
            out.clear();
            continue;
        }
        if let Some((k, v)) = line.split_once(':')
            && !k.is_empty()
            && !k.contains(' ')
        {
            out.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    out
}

/// `tensorseed verify-response`. 0 valid, 1 invalid or unsigned, 2 error.
pub async fn verify_response(args: &VerifyResponseArgs) -> ExitCode {
    match verify_inner(args).await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}

async fn verify_inner(args: &VerifyResponseArgs) -> anyhow::Result<bool> {
    let (claim, check, source) = fetch_claim(&args.claim).await?;
    let pubkey = claim.response_pubkey()?;
    let headers_text = std::fs::read_to_string(&args.headers)
        .with_context(|| format!("reading {}", args.headers.display()))?;
    let headers = parse_header_dump(&headers_text);
    let body =
        std::fs::read(&args.body).with_context(|| format!("reading {}", args.body.display()))?;
    let now = i64::try_from(tensorseed_core::time::unix_now()).unwrap_or(i64::MAX);
    let result = extract_signed(&headers, &body).and_then(|(f, signed)| {
        let e = Expect {
            pubkey,
            claim_sha256: &claim.claim_sha256,
            served_model_id: &claim.served_model_id,
            request_id: args.request_id.as_deref(),
            requested_model: args.model_id.as_deref(),
            now_unix: now,
            max_skew_secs: args.max_skew,
        };
        verify_fields(&f, signed, &e).map(|()| f)
    });
    let wallet = match &check.wallet {
        WalletStatus::Verified(a) => Some(a.clone()),
        WalletStatus::NotSigned => None,
    };
    if args.json {
        let v = match &result {
            Ok(f) => serde_json::json!({
                "valid": true,
                "claim_source": source,
                "claim_sha256": claim.claim_sha256,
                "served_model_id": f.served_model_id,
                "request_id": f.request_id,
                "timestamp": f.timestamp,
                "body_sha256": f.body_sha256,
                "seller_wallet": wallet,
            }),
            Err(e) => serde_json::json!({
                "valid": false,
                "claim_source": source,
                "claim_sha256": claim.claim_sha256,
                "error": e.label(),
                "detail": e.to_string(),
            }),
        };
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        match &result {
            Ok(f) => {
                println!(
                    "valid: signed by the response key of claim {}",
                    claim.claim_sha256
                );
                println!("  model      {}", f.served_model_id);
                println!("  request id {}", f.request_id);
                println!("  timestamp  {}", f.timestamp);
                match &wallet {
                    Some(a) => println!("  wallet     {a} (claim signature verified)"),
                    None => println!("  wallet     (claim not wallet-signed)"),
                }
            }
            Err(e) => println!("invalid: {e}"),
        }
        for w in &check.warnings {
            eprintln!("warning: {w}");
        }
    }
    if args.max_skew != DEFAULT_MAX_SKEW_SECS && result.is_ok() && !args.json {
        eprintln!("note: checked with a skew window of {} s", args.max_skew);
    }
    Ok(result.is_ok())
}
