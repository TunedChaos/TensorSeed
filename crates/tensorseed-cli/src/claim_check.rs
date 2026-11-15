// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! `tensorseed check-claim`: fetch a seller's `ts.served-weights/1` claim
//! and check it against a catalog you trust. The checks live in
//! [`tensorseed_app::claim_check`]; this module fetches and prints.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, bail};
use tensorseed_app::claim_check::{
    ClaimCheckOptions, ClaimReport, ClaimVerdict, Origin, RevocationInput, Status,
    check_claim_bytes, revocations_url_next_to,
};
use tensorseed_app::config::Config;
use tensorseed_app::signing::WELL_KNOWN_CLAIM;

use crate::cli::CheckClaimArgs;

/// Largest claim document read.
const MAX_CLAIM_BYTES: usize = 4 * 1024 * 1024;

/// Where to fetch the claim for `src`: a `.json` URL as is, otherwise the
/// well-known path under the base URL (without a trailing `/v1`).
fn claim_url(src: &str) -> anyhow::Result<reqwest::Url> {
    let url = reqwest::Url::parse(src).with_context(|| format!("{src:?} is not a URL"))?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("{src} must be an http or https URL");
    }
    if !url.username().is_empty() || url.password().is_some() {
        bail!("do not put credentials in the URL");
    }
    if url.query().is_some() || url.fragment().is_some() {
        bail!("{src}: pass the endpoint base URL without a query or fragment");
    }
    if url.path().ends_with(".json") {
        return Ok(url);
    }
    let root = src.trim_end_matches('/');
    let root = root.strip_suffix("/v1").unwrap_or(root);
    Ok(reqwest::Url::parse(&format!("{root}{WELL_KNOWN_CLAIM}"))?)
}

/// Read the claim bytes and where they really came from (the final URL
/// after at most 3 redirects, so a redirect to another host is caught by
/// the public URL check).
async fn read_claim(src: &str) -> anyhow::Result<(Vec<u8>, Origin)> {
    if !(src.starts_with("http://") || src.starts_with("https://")) {
        let p = Path::new(src);
        let meta = std::fs::metadata(p).with_context(|| format!("reading {src}"))?;
        if meta.len() > MAX_CLAIM_BYTES as u64 {
            bail!("{src} is larger than {MAX_CLAIM_BYTES} bytes");
        }
        let raw = std::fs::read(p).with_context(|| format!("reading {src}"))?;
        return Ok((raw, Origin::File(src.to_string())));
    }
    let url = claim_url(src)?;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::limited(3))
        .user_agent(concat!("tensorseed/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let mut r = http
        .get(url.clone())
        .send()
        .await
        .with_context(|| format!("fetching {url}"))?;
    if !r.status().is_success() {
        bail!("fetching {url}: HTTP {}", r.status());
    }
    let final_url = r.url().to_string();
    let mut buf = Vec::new();
    while let Some(c) = r.chunk().await? {
        if buf.len() + c.len() > MAX_CLAIM_BYTES {
            bail!("{final_url} is larger than {MAX_CLAIM_BYTES} bytes");
        }
        buf.extend_from_slice(&c);
    }
    Ok((buf, Origin::Url(final_url)))
}

/// Revocations for a claim read from `origin`: the seller's list next to
/// a claim fetched from its well-known path (a read failure there is a
/// warning), plus the lists passed with `--revocations` (a read failure
/// there is an error). `no_seller` skips the seller's list.
async fn revocations(
    origin: &Origin,
    extra: &[String],
    no_seller: bool,
) -> anyhow::Result<RevocationInput> {
    let seller = match origin {
        Origin::Url(u) if !no_seller => revocations_url_next_to(u),
        _ => None,
    };
    let why = if no_seller {
        "--no-revocations was given".to_string()
    } else {
        match origin {
            Origin::File(_) => {
                "read from a file; pass --revocations, or check the URL, to check revocations"
                    .into()
            }
            Origin::Url(_) => {
                "the claim was not read from its well-known path; pass --revocations".into()
            }
        }
    };
    gather_revocations(seller.as_deref(), extra, why).await
}

/// Read the seller's list at `seller` (404: none) and the `extra` lists
/// (a read failure there is an error). A seller list that cannot be read,
/// does not parse, or is too large is [`RevocationInput::Unavailable`]
/// when no other list was read, and is recorded as `incomplete` in the
/// merged [`RevocationInput::List`] otherwise. Either way a verdict that
/// would pass becomes inconclusive. `not_checked` is the reason given when
/// no list was looked at.
pub(crate) async fn gather_revocations(
    seller: Option<&str>,
    extra: &[String],
    not_checked: String,
) -> anyhow::Result<RevocationInput> {
    let (mut list, mut from, mut absent) = crate::signing::read_revocation_lists(extra).await?;
    let mut seller_err = None;
    if let Some(u) = seller {
        match crate::signing::read_revocation_lists(&[u.to_string()]).await {
            Ok((l, f, a)) => {
                list.merge(l);
                from.extend(f);
                absent.extend(a);
            }
            Err(e) => seller_err = Some(format!("{e:#}")),
        }
    }
    Ok(if !from.is_empty() {
        RevocationInput::List {
            list,
            from: from.join(", "),
            incomplete: seller_err,
        }
    } else if let Some(e) = seller_err {
        RevocationInput::Unavailable(e)
    } else if !absent.is_empty() {
        RevocationInput::Absent(absent.join(", "))
    } else {
        RevocationInput::NotChecked(not_checked)
    })
}

/// `tensorseed check-claim`.
pub async fn run(cfg: &Config, args: &CheckClaimArgs) -> ExitCode {
    let opts = ClaimCheckOptions {
        require_wallet_sig: args.require_wallet_sig,
        expect_wallet: args.expect_wallet.clone(),
        expect_url: args.expect_url.clone(),
        require_fresh: args.require_fresh,
        require_expiry: args.require_expiry,
        now_unix: crate::signing::now_unix(),
    };
    let rep = match read_claim(&args.source).await {
        Ok((raw, origin)) => {
            let revs = match revocations(&origin, &args.revocations, args.no_revocations).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("error: {e:#}");
                    return ExitCode::from(2);
                }
            };
            check_claim_bytes(
                cfg,
                &args.source,
                &raw,
                origin,
                args.catalog.as_deref(),
                args.no_wait,
                &revs,
                &opts,
            )
            .await
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            return ExitCode::from(2);
        }
    };
    if args.json {
        match serde_json::to_string_pretty(&rep) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        print_text(&rep);
    }
    ExitCode::from(u8::try_from(rep.verdict.exit_code()).unwrap_or(2))
}

fn print_text(r: &ClaimReport) {
    match &r.origin {
        Some(Origin::Url(u)) => println!("claim      {u}"),
        Some(Origin::File(p)) => println!("claim      {p}"),
        None => println!("claim      {}", r.source),
    }
    if let Some(h) = &r.claim_sha256 {
        println!("  sha256   {h}");
    }
    if let (Some(repo), Some(rev)) = (&r.repo, &r.revision) {
        println!("  weights  {repo}@{rev}");
    }
    if let Some(e) = &r.entrypoint {
        match e {
            tensorseed_app::inference::Entrypoint::Gguf(p) => println!("  file     {p}"),
            tensorseed_app::inference::Entrypoint::Directory => {
                println!("  file     (whole directory)");
            }
        }
    }
    if let Some(m) = &r.served_model_id {
        println!("  model id {m}");
    }
    if let Some(u) = &r.public_base_url {
        println!("  endpoint {u}");
    }
    if let Some(w) = &r.seller_wallet {
        let v = if r.wallet_verified {
            "verified"
        } else {
            "not verified"
        };
        println!("  wallet   {w} ({v})");
    }
    if let Some(x) = &r.expires_at {
        println!("  expires  {x}");
    }
    if let Some(c) = &r.catalog {
        println!("  catalog  {c}");
    }
    if let Some(l) = &r.license {
        println!(
            "  license  {} -> {} (from the catalog; the claim says {})",
            l.id.as_deref().unwrap_or("(none)"),
            l.commercial_serving.as_str(),
            l.claimed.as_str()
        );
    }
    println!();
    for c in &r.checks {
        let tag = match c.status {
            Status::Pass => "ok  ",
            Status::Fail => "FAIL",
            Status::Warn => "warn",
            Status::Skip => "skip",
        };
        println!("  {tag} {:<16} {}", c.name, c.detail);
    }
    println!();
    let n = r.counts;
    println!(
        "verdict: {} ({} pass, {} fail, {} warn, {} skip)",
        r.verdict.as_str(),
        n.pass,
        n.fail,
        n.warn,
        n.skip
    );
    if r.verdict == ClaimVerdict::Pass {
        println!(
            "note: a valid claim says what the seller serves; logprob challenges (check-endpoint) test that the endpoint really serves it"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_urls() {
        let u = |s: &str| claim_url(s).unwrap().to_string();
        let w = "https://llm.example.com/.well-known/tensorseed/served-weights.json";
        assert_eq!(u("https://llm.example.com/v1"), w);
        assert_eq!(u("https://llm.example.com/v1/"), w);
        assert_eq!(u("https://llm.example.com"), w);
        assert_eq!(u(w), w);
        assert_eq!(
            u("https://h.example/sub/v1"),
            "https://h.example/sub/.well-known/tensorseed/served-weights.json"
        );
        assert!(claim_url("https://user:pw@llm.example.com/v1").is_err());
        assert!(claim_url("https://llm.example.com/v1?x=1").is_err());
        assert!(claim_url("ftp://llm.example.com/").is_err());
    }
}
