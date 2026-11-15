// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! `catalog follow-list`: create, sign, verify, follow, and unfollow
//! signed `ts.follow-list/1` lists.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, bail};
use tensorseed_app::config::{CatalogSub, FollowListSub};
use tensorseed_app::follow_list::{Plan, fetch_verified, followed, is_url, plan};
use tensorseed_catalog::follow_list::{
    FOLLOW_LIST_SCHEMA, FollowList, FollowedCatalog, Update, sign_list,
};
use tensorseed_catalog::minisign::SecretKey;
use tensorseed_core::hash::sha256_bytes;
use tensorseed_core::time::now_rfc3339;

use crate::catalog_cmd::{block_value, strip_tables, toml_string};
use crate::cli::FollowListCmd;
use crate::config::Config;
use crate::keys::{load_secret, load_secret_file};

/// Run a `catalog follow-list` subcommand.
pub async fn run(
    cmd: &FollowListCmd,
    cfg: impl FnOnce() -> anyhow::Result<Config>,
    config_path: &Path,
) -> anyhow::Result<ExitCode> {
    match cmd {
        FollowListCmd::Create {
            name,
            out,
            catalogs,
            force,
        } => create(&cfg()?, name, out, catalogs, *force),
        FollowListCmd::Sign { file, rev, key } => {
            let sk = match key {
                Some(k) => load_secret_file(k)?,
                None => load_secret(&cfg()?)?,
            };
            sign(file, *rev, &sk)
        }
        FollowListCmd::Verify {
            source,
            pubkey,
            sig,
            json,
        } => {
            let (list, body) = fetch_verified(source, sig.as_deref(), pubkey).await?;
            print_list(&list, &body, *json)?;
            Ok(ExitCode::SUCCESS)
        }
        FollowListCmd::Follow {
            source,
            pubkey,
            sig,
            yes,
            allow_key_change,
        } => {
            follow(
                &cfg()?,
                config_path,
                source,
                pubkey.as_deref(),
                sig.as_deref(),
                *yes,
                *allow_key_change,
            )
            .await
        }
        FollowListCmd::Unfollow {
            name,
            keep_catalogs,
        } => unfollow(&cfg()?, config_path, name, *keep_catalogs),
    }
}

fn create(
    cfg: &Config,
    name: &str,
    out: &Path,
    only: &[String],
    force: bool,
) -> anyhow::Result<ExitCode> {
    if out.exists() && !force {
        bail!(
            "{} already exists (use --force to overwrite)",
            out.display()
        );
    }
    let subs = cfg.subscriptions()?;
    for n in only {
        if !subs.iter().any(|s| &s.name == n || &s.label == n) {
            bail!("no subscribed catalog named {n:?}");
        }
    }
    let mut list = FollowList::new(name, &now_rfc3339());
    for s in subs
        .iter()
        .filter(|s| only.is_empty() || only.iter().any(|n| *n == s.name || *n == s.label))
    {
        if s.base_url.is_empty() {
            bail!(
                "catalog {:?} has no base_url; a follow list needs one",
                s.name
            );
        }
        list.catalogs.push(FollowedCatalog {
            name: s.name.clone(),
            label: s.label.clone(),
            publisher_pubkey: s.publisher_pubkey.clone(),
            base_url: s.base_url.clone(),
            store_id: s.store_id.clone(),
            note: String::new(),
        });
    }
    list.validate()?;
    std::fs::write(out, list.to_bytes()?).with_context(|| format!("writing {}", out.display()))?;
    println!(
        "wrote {} with {} catalogs (unsigned; edit it, then run `catalog follow-list sign {}`)",
        out.display(),
        list.catalogs.len(),
        out.display()
    );
    Ok(ExitCode::SUCCESS)
}

fn sig_path(file: &Path) -> PathBuf {
    let mut s = file.as_os_str().to_owned();
    s.push(".minisig");
    PathBuf::from(s)
}

/// The revision named in an existing signature's trusted comment for list
/// `name` (not verified: it only stops `sign` from reusing a revision).
fn signed_rev(sig: &str, name: &str) -> Option<u64> {
    let tc = sig
        .lines()
        .find_map(|l| l.strip_prefix("trusted comment: "))?;
    let mut words = tc.split(' ');
    if words.next() != Some(FOLLOW_LIST_SCHEMA) {
        return None;
    }
    let mut rev = None;
    let mut same = false;
    for w in words {
        if let Some(n) = w.strip_prefix("name=") {
            same = n == name;
        } else if let Some(r) = w.strip_prefix("rev=") {
            rev = r.parse().ok();
        }
    }
    rev.filter(|_| same)
}

fn sign(file: &Path, rev: Option<u64>, sk: &SecretKey) -> anyhow::Result<ExitCode> {
    let body = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    let mut list = FollowList::parse(&body)?;
    let sp = sig_path(file);
    let prev = std::fs::read_to_string(&sp)
        .ok()
        .and_then(|s| signed_rev(&s, &list.name))
        .unwrap_or(0);
    list.rev = match rev {
        Some(r) if r <= prev => {
            bail!(
                "{} was already signed at rev {prev}; pick a higher --rev",
                sp.display()
            )
        }
        Some(r) => r,
        None => list.rev.max(prev + 1),
    };
    list.issued_at = now_rfc3339();
    let (body, sig) = sign_list(sk, &list)?;
    std::fs::write(file, &body).with_context(|| format!("writing {}", file.display()))?;
    std::fs::write(&sp, sig).with_context(|| format!("writing {}", sp.display()))?;
    let pk = sk.public_key();
    println!(
        "signed {} rev {} ({} catalogs) with key id {}",
        list.name,
        list.rev,
        list.catalogs.len(),
        pk.key_id_hex()
    );
    println!("wrote {} and {}", file.display(), sp.display());
    println!("followers pin this key: {}", pk.to_base64());
    Ok(ExitCode::SUCCESS)
}

fn print_list(list: &FollowList, body: &[u8], json: bool) -> anyhow::Result<()> {
    if json {
        let mut v = serde_json::to_value(list)?;
        v["sha256"] = sha256_bytes(body).into();
        v["verified"] = true.into();
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    println!(
        "follow list {} rev {} (issued {}), signature OK",
        list.name, list.rev, list.issued_at
    );
    println!("sha256: {}", sha256_bytes(body));
    if list.catalogs.is_empty() {
        println!("no catalogs");
    }
    for c in &list.catalogs {
        let label = if c.label.is_empty() {
            String::new()
        } else {
            format!(" ({})", c.label)
        };
        println!("  {}{label}", c.name);
        println!("    publisher_pubkey: {}", c.publisher_pubkey);
        println!("    base_url:         {}", c.base_url);
        if !c.store_id.is_empty() {
            println!("    store_id:         {}", c.store_id);
        }
        if !c.note.is_empty() {
            println!("    note:             {}", c.note);
        }
    }
    Ok(())
}

fn print_plan(p: &Plan, prev: Option<&FollowListSub>) {
    let was = match (p.update, prev) {
        (Update::New, _) | (_, None) => "new".to_string(),
        (Update::Same, _) => "already accepted".to_string(),
        (Update::Newer, Some(prev)) => format!("was rev {}", prev.rev),
    };
    println!(
        "follow list {} rev {} ({was}), issued {}",
        p.list.name, p.list.rev, p.list.issued_at
    );
    for e in &p.add {
        println!(
            "  add       {}  {}  key {}",
            e.name, e.base_url, e.publisher_pubkey
        );
    }
    for (old, new) in &p.change {
        let mut what = Vec::new();
        for (k, a, b) in [
            (
                "publisher_pubkey",
                &old.publisher_pubkey,
                &new.publisher_pubkey,
            ),
            ("base_url", &old.base_url, &new.base_url),
            ("store_id", &old.store_id, &new.store_id),
            ("label", &old.label, &new.label),
        ] {
            if a != b {
                what.push(format!("{k} {a:?} -> {b:?}"));
            }
        }
        println!("  update    {}  {}", new.name, what.join(", "));
    }
    for e in &p.remove {
        println!("  remove    {}  (no longer listed)", e.name);
    }
    for s in &p.skipped {
        let tag = if s.conflict { "conflict" } else { "keep    " };
        println!("  {tag}  {}  {}", s.catalog.name, s.reason);
    }
    if p.is_empty() {
        println!("no changes to [[catalogs]]");
    }
}

fn catalog_block(e: &CatalogSub) -> String {
    let mut b = String::from("\n[[catalogs]]\n");
    b.push_str(&format!("name = {}\n", toml_string(&e.name)));
    if !e.label.is_empty() {
        b.push_str(&format!("label = {}\n", toml_string(&e.label)));
    }
    b.push_str(&format!(
        "publisher_pubkey = {}\n",
        toml_string(&e.publisher_pubkey)
    ));
    if !e.store_id.is_empty() {
        b.push_str(&format!("store_id = {}\n", toml_string(&e.store_id)));
    }
    b.push_str(&format!("base_url = {}\n", toml_string(&e.base_url)));
    if let Some(m) = &e.chain_check {
        b.push_str(&format!("chain_check = {}\n", toml_string(m)));
    }
    if let Some(p) = e.priority {
        b.push_str(&format!("priority = {p}\n"));
    }
    if let Some(d) = &e.state_dir {
        b.push_str(&format!("state_dir = {}\n", toml_string(d)));
    }
    for (k, v) in [
        ("mirror_urls", &e.mirror_urls),
        ("coin_api_urls", &e.coin_api_urls),
    ] {
        if !v.is_empty() {
            let items: Vec<String> = v.iter().map(|s| toml_string(s)).collect();
            b.push_str(&format!("{k} = [{}]\n", items.join(", ")));
        }
    }
    if !e.follow_list.is_empty() {
        b.push_str(&format!("follow_list = {}\n", toml_string(&e.follow_list)));
    }
    b
}

fn list_block(l: &FollowListSub) -> String {
    format!(
        "\n[[follow_lists]]\nname = {}\nsource = {}\npublisher_pubkey = {}\nrev = {}\nsha256 = {}\n",
        toml_string(&l.name),
        toml_string(&l.source),
        toml_string(&l.publisher_pubkey),
        l.rev,
        toml_string(&l.sha256)
    )
}

/// Drop the `[[catalogs]]` tables named `name` that came from `list`.
fn strip_listed(text: &str, list: &str, name: &str) -> String {
    strip_tables(text, "catalogs", |b| {
        block_value(b, "name").as_deref() == Some(name)
            && block_value(b, "follow_list").as_deref() == Some(list)
    })
    .0
}

/// Write `text` to the config and check it loads; restore the old text
/// when it does not.
fn write_checked(path: &Path, old: &str, text: &str) -> anyhow::Result<()> {
    std::fs::write(path, text).with_context(|| format!("writing {}", path.display()))?;
    if let Err(e) = crate::config::load(path) {
        std::fs::write(path, old).with_context(|| format!("restoring {}", path.display()))?;
        return Err(e.context(format!(
            "the edited config did not load; {} was left unchanged",
            path.display()
        )));
    }
    Ok(())
}

async fn follow(
    cfg: &Config,
    path: &Path,
    source: &str,
    pubkey: Option<&str>,
    sig: Option<&str>,
    yes: bool,
    allow_key_change: bool,
) -> anyhow::Result<ExitCode> {
    // A followed list by name or by source, for refreshes.
    let known = cfg
        .follow_lists
        .iter()
        .find(|l| l.name == source || l.source == source);
    let (src, key) = match (known, pubkey) {
        (_, Some(k)) => (
            known
                .filter(|l| l.name == source)
                .map_or(source, |l| l.source.as_str()),
            k,
        ),
        (Some(l), None) => (l.source.as_str(), l.publisher_pubkey.as_str()),
        (None, None) => bail!(
            "{source:?} is not a list you follow; pass --pubkey with the list signer's key the first time"
        ),
    };
    if !is_url(src) && !Path::new(src).exists() {
        bail!("{src:?} is neither an http(s) URL nor a file");
    }
    let (list, body) = fetch_verified(src, sig, key).await?;
    let prev = followed(cfg, &list.name).cloned();
    let p = plan(cfg, &list, &body, key, allow_key_change)?;
    print_plan(&p, prev.as_ref());
    for s in p.skipped.iter().filter(|s| s.conflict) {
        eprintln!("warning: {}: {}", s.catalog.name, s.reason);
    }
    if !yes {
        println!("nothing written; run again with --yes to apply");
        return Ok(ExitCode::SUCCESS);
    }
    let old = if path.exists() {
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?
    } else {
        String::new()
    };
    let mut text = old.clone();
    // Keep following the built-in official catalog once this file lists
    // catalogs of its own.
    if tensorseed_app::config::follows_builtin_official(path) {
        text = tensorseed_app::config::pin_builtin_official(&text)?;
    }
    for e in p.remove.iter().chain(p.change.iter().map(|(o, _)| o)) {
        text = strip_listed(&text, &list.name, &e.name);
    }
    text = strip_tables(&text, "follow_lists", |b| {
        block_value(b, "name").as_deref() == Some(list.name.as_str())
    })
    .0;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    // A local file source is stored as an absolute path.
    let stored = if is_url(src) {
        src.to_string()
    } else {
        std::fs::canonicalize(src)
            .map_or_else(|_| src.to_string(), |p| p.to_string_lossy().into_owned())
    };
    text.push_str(&list_block(&FollowListSub {
        name: list.name.clone(),
        source: stored,
        publisher_pubkey: key.to_string(),
        rev: list.rev,
        sha256: sha256_bytes(&body),
    }));
    for e in p.change.iter().map(|(_, n)| n).chain(&p.add) {
        text.push_str(&catalog_block(e));
    }
    write_checked(path, &old, &text)?;
    println!(
        "following {} rev {}: {} added, {} updated, {} removed in {}",
        list.name,
        list.rev,
        p.add.len(),
        p.change.len(),
        p.remove.len(),
        path.display()
    );
    Ok(ExitCode::SUCCESS)
}

fn unfollow(
    cfg: &Config,
    path: &Path,
    name: &str,
    keep_catalogs: bool,
) -> anyhow::Result<ExitCode> {
    if followed(cfg, name).is_none() {
        bail!("you do not follow a list named {name:?}");
    }
    let old =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let (mut text, _) = strip_tables(&old, "follow_lists", |b| {
        block_value(b, "name").as_deref() == Some(name)
    });
    let ours: Vec<&CatalogSub> = cfg
        .catalogs
        .iter()
        .filter(|c| c.follow_list == name)
        .collect();
    text = strip_tables(&text, "catalogs", |b| {
        block_value(b, "follow_list").as_deref() == Some(name)
    })
    .0;
    if keep_catalogs {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        for c in &ours {
            let mut c = (*c).clone();
            c.follow_list.clear();
            text.push_str(&catalog_block(&c));
        }
    }
    write_checked(path, &old, &text)?;
    if keep_catalogs {
        println!(
            "stopped following {name}; kept its {} catalogs as your own entries",
            ours.len()
        );
    } else {
        println!(
            "stopped following {name}; removed its {} catalogs (verified state on disk was kept)",
            ours.len()
        );
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_rev_reads_only_matching_lists() {
        let sig = "untrusted comment: x\nAAAA\ntrusted comment: ts.follow-list/1 name=a rev=7 sha256=00\nBBBB\n";
        assert_eq!(signed_rev(sig, "a"), Some(7));
        assert_eq!(signed_rev(sig, "b"), None);
        let other = sig.replace("ts.follow-list/1", "ts.mirrors/1");
        assert_eq!(signed_rev(&other, "a"), None);
    }
}
