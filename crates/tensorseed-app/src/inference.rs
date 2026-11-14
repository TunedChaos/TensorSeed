// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Serving inference from a verified local copy (`tensorseed
//! serve-inference`, docs/SELL-INFERENCE.md).
//!
//! The flow, shared by the CLI and Tensorseed Desktop:
//!
//! 1. [`verify_copy`]: re-hash a pulled directory against the signed
//!    manifest (the stored `.tensorseed/manifest.json` entry, or the
//!    catalog). Nothing is served when a file is missing or differs.
//! 2. [`license_gate`]: refuse licenses marked `forbidden` for commercial
//!    serving unless the operator says they hold the rights, and warn on
//!    `restricted` and `unknown` ([`tensorseed_core::commercial`]).
//! 3. [`choose_runtime`] and [`LaunchPlan::build`]: pick llama.cpp
//!    (`llama-server`) for GGUF weights or vLLM for safetensors, find the
//!    binary ([`find_binary`], `[inference]` or PATH), and build its
//!    command line. No runtime is bundled. An upstream API key is passed in
//!    the runtime's own environment variable, never on the command line.
//! 4. [`wait_ready`]: poll the OpenAI compatible `/v1/models` until the
//!    served model id is listed.
//! 5. [`ServedWeights`]: write `.tensorseed/served-weights.json`, an
//!    unsigned, versioned (`ts.served-weights/0`) record of what is served.
//!    It is a placeholder for a future signed weights statement whose format
//!    is not decided yet.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use tensorseed_catalog::signed::SignedEntry;
use tensorseed_core::commercial::{Assessment, CommercialServing, assess_license_info};
use tensorseed_core::hash::sha256_bytes;
use tensorseed_core::keys::{RepoRef, RepoType};
use tensorseed_core::manifest::Manifest;
use tensorseed_core::verify::{
    ExpectedFile, FileStatus, META_DIR, VerifyOptions, VerifyReport, verify_dir,
};

use crate::config::{Config, expand};
use crate::pull::{
    Resolved, allow_missing_from, catalog_manifest, check_manifest_identity, parse_manifest_bytes,
    read_pull_record, tombstone_covers,
};

/// Schema of `.tensorseed/served-weights.json`.
pub const SERVED_WEIGHTS_SCHEMA: &str = "ts.served-weights/0";

/// File name under `.tensorseed/`.
pub const SERVED_WEIGHTS_FILE: &str = "served-weights.json";

/// Default listen port (away from llama.cpp's 8080 and vLLM's 8000, which
/// are often already taken on a workstation).
pub const DEFAULT_PORT: u16 = 8090;

/// `[inference]`: local inference runtimes for `serve-inference`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InferenceConfig {
    /// llama.cpp server binary; empty searches PATH for `llama-server`.
    pub llama_server: String,
    /// vLLM binary; empty searches PATH for `vllm`.
    pub vllm: String,
    /// Listen address. Keep the loopback default and put a TLS reverse proxy
    /// in front; see docs/SELL-INFERENCE.md.
    pub host: String,
    /// Listen port.
    pub port: u16,
    /// Extra arguments for `llama-server` (e.g. `["-c", "8192", "-ngl", "99"]`).
    pub llama_server_args: Vec<String>,
    /// Extra arguments for `vllm serve` (e.g. `["--max-model-len", "8192"]`).
    pub vllm_args: Vec<String>,
    /// Environment variable that holds an upstream API key the runtime
    /// requires (empty: none). The value is handed to the runtime through
    /// its own variable (`LLAMA_API_KEY`, `VLLM_API_KEY`) and never shown.
    pub api_key_env: String,
    /// Seconds to wait for `/v1/models` after starting the runtime.
    pub ready_timeout_secs: u64,
}

impl Default for InferenceConfig {
    fn default() -> Self {
        Self {
            llama_server: String::new(),
            vllm: String::new(),
            host: "127.0.0.1".into(),
            port: DEFAULT_PORT,
            llama_server_args: Vec::new(),
            vllm_args: Vec::new(),
            api_key_env: String::new(),
            ready_timeout_secs: 600,
        }
    }
}

impl InferenceConfig {
    /// Check values.
    ///
    /// # Errors
    ///
    /// Fails on an empty or unparsable host, port 0, a zero timeout, or an
    /// invalid environment variable name.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.host.trim().parse::<IpAddr>().is_err() {
            bail!(
                "inference.host must be an IP address such as 127.0.0.1, not {:?}",
                self.host
            );
        }
        if self.port == 0 {
            bail!("inference.port must not be 0");
        }
        if self.ready_timeout_secs == 0 {
            bail!("inference.ready_timeout_secs must be at least 1");
        }
        if !self.api_key_env.is_empty()
            && !self
                .api_key_env
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            bail!(
                "inference.api_key_env must be an environment variable name, not {:?}",
                self.api_key_env
            );
        }
        Ok(())
    }

    /// Listen socket address.
    ///
    /// # Errors
    ///
    /// Fails when the host is not an IP address.
    pub fn listen(&self) -> anyhow::Result<SocketAddr> {
        let ip: IpAddr = self
            .host
            .trim()
            .parse()
            .with_context(|| format!("inference.host {:?}", self.host))?;
        Ok(SocketAddr::new(ip, self.port))
    }

    /// Configured binary for a runtime (empty means search PATH).
    #[must_use]
    pub fn binary(&self, rt: Runtime) -> &str {
        match rt {
            Runtime::LlamaCpp => &self.llama_server,
            Runtime::Vllm => &self.vllm,
        }
    }

    /// Configured extra arguments for a runtime.
    #[must_use]
    pub fn args(&self, rt: Runtime) -> &[String] {
        match rt {
            Runtime::LlamaCpp => &self.llama_server_args,
            Runtime::Vllm => &self.vllm_args,
        }
    }
}

/// Supported OpenAI compatible runtimes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Runtime {
    /// llama.cpp `llama-server` (GGUF).
    LlamaCpp,
    /// vLLM `vllm serve` (safetensors).
    Vllm,
}

impl Runtime {
    /// Both runtimes.
    pub const ALL: [Self; 2] = [Self::LlamaCpp, Self::Vllm];

    /// `llama.cpp` or `vLLM`.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::LlamaCpp => "llama.cpp",
            Self::Vllm => "vLLM",
        }
    }

    /// Binary name searched on PATH.
    #[must_use]
    pub fn default_binary(self) -> &'static str {
        match self {
            Self::LlamaCpp => "llama-server",
            Self::Vllm => "vllm",
        }
    }

    /// Config key holding the binary path.
    #[must_use]
    pub fn config_key(self) -> &'static str {
        match self {
            Self::LlamaCpp => "inference.llama_server",
            Self::Vllm => "inference.vllm",
        }
    }

    /// Environment variable the runtime reads its API key from.
    #[must_use]
    pub fn api_key_var(self) -> &'static str {
        match self {
            Self::LlamaCpp => "LLAMA_API_KEY",
            Self::Vllm => "VLLM_API_KEY",
        }
    }

    /// Environment variable that overrides [`Self::config_key`] for one run.
    #[must_use]
    pub fn config_env(self) -> &'static str {
        match self {
            Self::LlamaCpp => "TENSORSEED_INFERENCE__LLAMA_SERVER",
            Self::Vllm => "TENSORSEED_INFERENCE__VLLM",
        }
    }

    /// How to install this engine, in one sentence.
    #[must_use]
    pub fn install_hint(self) -> &'static str {
        match self {
            Self::LlamaCpp => {
                "install llama.cpp, which provides llama-server, with `winget install llama.cpp` (Windows), `brew install llama.cpp` (macOS, Linux), your distribution's llama-cpp package, or a release archive from https://github.com/ggml-org/llama.cpp/releases"
            }
            Self::Vllm => {
                "install vLLM with `pip install vllm` in a Python virtual environment; it needs Linux and a supported GPU (on Windows, use WSL 2)"
            }
        }
    }
}

/// The install guide for inference engines, linked from the errors.
pub const ENGINE_DOCS_URL: &str = "https://github.com/TunedChaos/TensorSeed/blob/main/docs/SELL-INFERENCE.md#3-install-an-inference-engine";

/// The message shown when an inference engine binary is not found: what is
/// missing, what to install, how to point Tensorseed at it, and the guide.
#[must_use]
pub fn missing_engine_message(rt: Runtime, lookup: &BinaryLookup) -> String {
    let place = if lookup.configured {
        format!(
            " ({} = {:?} is not an executable file)",
            rt.config_key(),
            lookup.wanted
        )
    } else {
        " on PATH".to_string()
    };
    format!(
        "{label} is not installed: `{wanted}` was not found{place}. Tensorseed does not bundle inference engines; {hint}. Then put {bin} on PATH, set {key} in the config to its full path, or set {env} for one run. See {url}",
        label = rt.label(),
        wanted = lookup.wanted,
        hint = rt.install_hint(),
        bin = rt.default_binary(),
        key = rt.config_key(),
        env = rt.config_env(),
        url = ENGINE_DOCS_URL,
    )
}

/// Where a binary was looked for and what was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryLookup {
    /// The configured value, or the default name when unset.
    pub wanted: String,
    /// True when the value came from the config.
    pub configured: bool,
    /// Resolved path, if found.
    pub found: Option<PathBuf>,
}

fn is_executable(p: &Path) -> bool {
    let Ok(md) = std::fs::metadata(p) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        md.is_file() && md.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        md.is_file()
    }
}

/// Search `path_var` (a PATH style list) for `name`, trying `PATHEXT`
/// extensions on Windows.
#[must_use]
pub fn search_path(name: &str, path_var: &std::ffi::OsStr) -> Option<PathBuf> {
    let exts: Vec<String> = if cfg!(windows) && Path::new(name).extension().is_none() {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(str::to_owned)
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(path_var) {
        for ext in &exts {
            let p = dir.join(format!("{name}{ext}"));
            if is_executable(&p) {
                return Some(p);
            }
        }
    }
    None
}

/// Resolve a runtime binary: an explicit path from the config (`~` is
/// expanded), a bare name searched on PATH, or the default name on PATH.
#[must_use]
pub fn find_binary(cfg: &InferenceConfig, rt: Runtime) -> BinaryLookup {
    let raw = cfg.binary(rt).trim();
    let configured = !raw.is_empty();
    let wanted = if configured {
        raw.to_string()
    } else {
        rt.default_binary().to_string()
    };
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    let looks_like_path = wanted.contains('/') || wanted.contains('\\') || wanted.starts_with('~');
    let found = if looks_like_path {
        let p = expand(&wanted);
        is_executable(&p).then_some(p)
    } else {
        search_path(&wanted, &path_var)
    };
    BinaryLookup {
        wanted,
        configured,
        found,
    }
}

/// What the runtime loads: one GGUF file (llama.cpp) or the directory
/// (vLLM).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "path", rename_all = "snake_case")]
pub enum Entrypoint {
    /// GGUF file, relative to the copy (`/` separators). For split GGUF
    /// files this is the first shard; llama.cpp loads the rest.
    Gguf(String),
    /// The whole directory (config.json plus safetensors).
    Directory,
}

fn lower(p: &str) -> String {
    p.to_ascii_lowercase()
}

/// True for a later shard of a split GGUF (`-00002-of-00003.gguf`).
fn is_later_shard(path: &str) -> bool {
    let l = lower(path);
    let Some(stem) = l.strip_suffix(".gguf") else {
        return false;
    };
    let parts: Vec<&str> = stem.rsplitn(3, '-').collect();
    // `name-00002-of-00003` splits from the right into ["00003", "of", "name-00002"].
    if parts.len() < 3 || parts[1] != "of" {
        return false;
    }
    let idx = parts[2].rsplit('-').next().unwrap_or("");
    !idx.is_empty() && idx.bytes().all(|b| b.is_ascii_digit()) && idx.trim_start_matches('0') != "1"
}

/// GGUF files a server could load as the model (projector files and later
/// shards excluded), sorted.
#[must_use]
pub fn gguf_candidates(paths: &[String]) -> Vec<String> {
    let mut v: Vec<String> = paths
        .iter()
        .filter(|p| lower(p).ends_with(".gguf"))
        .filter(|p| !lower(p).contains("mmproj"))
        .filter(|p| !is_later_shard(p))
        .cloned()
        .collect();
    v.sort();
    v
}

/// True when the paths look like a Transformers safetensors checkpoint.
#[must_use]
pub fn has_safetensors_checkpoint(paths: &[String]) -> bool {
    paths.iter().any(|p| p == "config.json")
        && paths.iter().any(|p| lower(p).ends_with(".safetensors"))
}

/// Pick the runtime and what it loads from the verified file list.
///
/// `requested` is `None` for auto: GGUF weights go to llama.cpp,
/// safetensors checkpoints to vLLM. `file` picks one GGUF file when the
/// repo carries several quantizations.
///
/// # Errors
///
/// Fails when the copy has no weights the runtime can load, when several
/// GGUF files exist and `file` is unset, or when `file` is not a verified
/// GGUF file of the copy.
pub fn choose_runtime(
    requested: Option<Runtime>,
    present: &[String],
    file: Option<&str>,
) -> anyhow::Result<(Runtime, Entrypoint)> {
    let ggufs = gguf_candidates(present);
    let st = has_safetensors_checkpoint(present);
    let pick_gguf = || -> anyhow::Result<Entrypoint> {
        if let Some(f) = file {
            let f = f.replace('\\', "/");
            let f = f.trim_start_matches("./").to_string();
            if !lower(&f).ends_with(".gguf") {
                bail!("--file {f:?} is not a GGUF file");
            }
            if !present.contains(&f) {
                bail!("--file {f:?} is not a verified file of this copy");
            }
            return Ok(Entrypoint::Gguf(f));
        }
        match ggufs.as_slice() {
            [] => bail!("no GGUF file in this copy"),
            [one] => Ok(Entrypoint::Gguf(one.clone())),
            many => bail!(
                "this copy has {} GGUF files; pick one with --file:\n  {}",
                many.len(),
                many.join("\n  ")
            ),
        }
    };
    match requested {
        Some(Runtime::LlamaCpp) => Ok((Runtime::LlamaCpp, pick_gguf()?)),
        Some(Runtime::Vllm) => {
            if file.is_some() {
                bail!("--file applies to llama.cpp; vLLM serves the whole directory");
            }
            if !st {
                bail!("vLLM needs config.json and .safetensors weights, and this copy has none");
            }
            Ok((Runtime::Vllm, Entrypoint::Directory))
        }
        None if file.is_some() || !ggufs.is_empty() => Ok((Runtime::LlamaCpp, pick_gguf()?)),
        None if st => Ok((Runtime::Vllm, Entrypoint::Directory)),
        None => bail!(
            "found no GGUF file and no safetensors checkpoint (config.json plus .safetensors); nothing to serve"
        ),
    }
}

/// Result of the license check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenseDecision {
    /// The mark.
    pub assessment: Assessment,
    /// Warnings to show (empty for `allowed`).
    pub warnings: Vec<String>,
}

/// Refuse `forbidden` licenses unless `i_have_rights`; warn on the rest.
///
/// # Errors
///
/// Fails for a `forbidden` license without `i_have_rights`.
pub fn license_gate(m: &Manifest, i_have_rights: bool) -> anyhow::Result<LicenseDecision> {
    let a = assess_license_info(&m.license);
    let id = a.license.clone().unwrap_or_else(|| "(none)".into());
    let mut warnings = Vec::new();
    match a.value {
        CommercialServing::Allowed => {}
        CommercialServing::Restricted => warnings.push(format!(
            "license {id} is marked restricted for commercial serving: {}",
            a.note
        )),
        CommercialServing::Unknown => warnings.push(format!(
            "license {id} is marked unknown for commercial serving: {}",
            a.note
        )),
        CommercialServing::Forbidden if i_have_rights => warnings.push(format!(
            "license {id} is marked forbidden for commercial serving ({}); serving because --i-have-rights was passed",
            a.note
        )),
        CommercialServing::Forbidden => bail!(
            "license {id} is marked forbidden for commercial serving: {}. Pass --i-have-rights only if you hold a separate license or will not charge for access",
            a.note
        ),
    }
    if let Some(attr) = &m.license.attribution {
        warnings.push(format!("attribution required: {attr}"));
    }
    Ok(LicenseDecision {
        assessment: a,
        warnings,
    })
}

/// A file that passed verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedFile {
    /// Path relative to the copy (`/` separators).
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// SHA-256 from the signed manifest.
    pub sha256: String,
}

/// A local copy that matched its signed manifest.
#[derive(Debug, Clone)]
pub struct VerifiedCopy {
    /// The directory.
    pub dir: PathBuf,
    /// The manifest (file pages merged in).
    pub manifest: Manifest,
    /// Catalog key of the manifest entry.
    pub manifest_key: String,
    /// SHA-256 of the signed manifest value (what the signature covers).
    pub manifest_sha256: String,
    /// Catalog name the entry is signed for.
    pub catalog: String,
    /// Publisher key id (hex), when a key is pinned.
    pub publisher_key_id: Option<String>,
    /// Where the manifest came from: `stored` or `catalog`.
    pub manifest_source: &'static str,
    /// Files present and verified.
    pub files: Vec<VerifiedFile>,
    /// Files absent by choice (unselected or pickle).
    pub skipped: Vec<String>,
    /// Files on disk that are not in the manifest (`.tensorseed` excluded).
    pub extra: Vec<String>,
}

impl VerifiedCopy {
    /// Paths of verified files.
    #[must_use]
    pub fn paths(&self) -> Vec<String> {
        self.files.iter().map(|f| f.path.clone()).collect()
    }
}

/// Options for [`verify_copy`].
#[derive(Debug, Clone, Default)]
pub struct VerifyCopyOptions {
    /// Use the local catalog copy without syncing.
    pub no_wait: bool,
    /// Subscribed catalog to resolve through.
    pub catalog: Option<String>,
    /// Hashing threads.
    pub threads: usize,
}

/// SHA-256 of the signed manifest value inside `raw` (a signed entry).
///
/// # Errors
///
/// Fails when `raw` is not a signed entry.
pub fn signed_value_sha256(raw: &[u8]) -> anyhow::Result<String> {
    let e = SignedEntry::from_bytes(raw).context("manifest is not a signed entry")?;
    Ok(sha256_bytes(e.value.as_bytes()))
}

async fn resolve_signed(
    cfg: &Config,
    dir: &Path,
    repo: &RepoRef,
    commit: Option<&str>,
    opts: &VerifyCopyOptions,
) -> anyhow::Result<(Resolved, &'static str)> {
    let stored = dir.join(META_DIR).join("manifest.json");
    if opts.catalog.is_none() && stored.is_file() {
        match parse_manifest_bytes(cfg, &std::fs::read(&stored)?, &stored.display().to_string()) {
            Ok(r) if r.signed => return Ok((r, "stored")),
            Ok(_) => tracing::info!(
                "{} is unsigned; resolving the manifest through the catalog",
                stored.display()
            ),
            Err(e) => tracing::info!("{e:#}; resolving the manifest through the catalog"),
        }
    }
    let r = catalog_manifest(cfg, repo, commit, opts.no_wait, opts.catalog.as_deref())
        .await
        .with_context(|| {
            format!(
                "no signed catalog manifest for {}; refusing to serve (only copies that match a signed manifest are served)",
                repo.id
            )
        })?;
    Ok((r, "catalog"))
}

/// Re-hash `dir` against its signed manifest and return what was verified.
///
/// The repo and commit come from `repo` or from `.tensorseed/pull.json`.
/// Files the pull deliberately skipped may be absent; every other file
/// must match.
///
/// # Errors
///
/// Fails when no signed manifest can be found, the revision is taken down,
/// or any file is missing or differs.
pub async fn verify_copy(
    cfg: &Config,
    dir: &Path,
    repo: Option<(RepoRef, Option<String>)>,
    opts: &VerifyCopyOptions,
) -> anyhow::Result<VerifiedCopy> {
    if !dir.is_dir() {
        bail!("{} is not a directory", dir.display());
    }
    let record = read_pull_record(dir)?;
    let (repo, commit) = match repo {
        Some((r, rev)) => (r, rev.or_else(|| record.as_ref().map(|x| x.commit.clone()))),
        None => {
            let r = record.as_ref().ok_or_else(|| {
                anyhow::anyhow!(
                    "{} has no {META_DIR}/pull.json; pull it first or name the repo",
                    dir.display()
                )
            })?;
            (
                RepoRef::new(r.repo_type, r.repo.clone())?,
                Some(r.commit.clone()),
            )
        }
    };
    if repo.repo_type != RepoType::Model {
        bail!("{} is a dataset; only models can be served", repo.id);
    }
    let (res, source) = resolve_signed(cfg, dir, &repo, commit.as_deref(), opts).await?;
    if !res.signed {
        bail!(
            "no signature verified manifest for {}; refusing to serve",
            repo.id
        );
    }
    if let Some(c) = &commit {
        check_manifest_identity(&res.manifest, &repo, c)?;
    }
    if let Some(t) = &res.tombstone
        && tombstone_covers(t, &res.manifest.hf_commit)
    {
        bail!(
            "{}@{} is taken down ({:?}: {}); refusing to serve",
            repo.id,
            res.manifest.hf_commit,
            t.reason,
            t.detail
        );
    }
    if !res.taken_down_in.is_empty() {
        bail!(
            "{} is taken down in catalog(s) {}; refusing to serve",
            repo.id,
            res.taken_down_in.join(", ")
        );
    }
    let expected: Vec<ExpectedFile> = res
        .manifest
        .files
        .iter()
        .map(ExpectedFile::from_manifest)
        .collect();
    let report = verify_dir(
        dir,
        &expected,
        VerifyOptions {
            allow_missing_pickle: true,
            allow_missing: allow_missing_from(&record),
            threads: opts.threads.max(1),
        },
    )?;
    collect_verified(cfg, dir, &res, source, &report)
}

fn collect_verified(
    cfg: &Config,
    dir: &Path,
    res: &Resolved,
    source: &'static str,
    report: &VerifyReport,
) -> anyhow::Result<VerifiedCopy> {
    if !report.passed() {
        let bad: Vec<String> = report
            .files
            .iter()
            .filter(|f| !f.status.is_pass())
            .take(8)
            .map(|f| format!("{} {}", f.status.label(), f.path))
            .collect();
        bail!(
            "{} of {} files failed verification; refusing to serve:\n  {}",
            report.failures(),
            report.files.len(),
            bad.join("\n  ")
        );
    }
    let by_path: std::collections::HashMap<&str, &tensorseed_core::manifest::FileEntry> = res
        .manifest
        .files
        .iter()
        .map(|f| (f.path.as_str(), f))
        .collect();
    let mut files = Vec::new();
    let mut skipped = Vec::new();
    for f in &report.files {
        if f.status == FileStatus::Ok {
            if let Some(e) = by_path.get(f.path.as_str()) {
                files.push(VerifiedFile {
                    path: e.path.clone(),
                    size: e.size,
                    sha256: e.sha256.clone(),
                });
            }
        } else {
            skipped.push(f.path.clone());
        }
    }
    let repo = RepoRef::new(res.manifest.repo.repo_type, res.manifest.repo.id.clone())?;
    Ok(VerifiedCopy {
        dir: dir.to_path_buf(),
        manifest_key: repo.manifest_key(&res.manifest.hf_commit)?,
        manifest_sha256: signed_value_sha256(&res.raw)?,
        catalog: cfg.catalog.name.clone(),
        publisher_key_id: crate::backend::pinned_key(cfg).ok().map(|k| k.key_id_hex()),
        manifest_source: source,
        manifest: res.manifest.clone(),
        files,
        skipped,
        extra: report.extra.clone(),
    })
}

/// Refuse to hand vLLM a directory with files outside the manifest: it
/// loads weights and code from the whole directory, so an unverified file
/// would be served as if it were verified.
///
/// # Errors
///
/// Fails for [`Runtime::Vllm`] when `extra` is not empty.
pub fn check_extra_files(rt: Runtime, extra: &[String]) -> anyhow::Result<()> {
    if rt == Runtime::Vllm && !extra.is_empty() {
        bail!(
            "vLLM loads files from the whole directory, and {} file(s) are not in the signed manifest; move them out first:\n  {}",
            extra.len(),
            extra
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n  ")
        );
    }
    Ok(())
}

/// A runtime command ready to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchPlan {
    /// Runtime.
    pub runtime: Runtime,
    /// What it loads.
    pub entrypoint: Entrypoint,
    /// Program to run.
    pub program: PathBuf,
    /// Arguments.
    pub args: Vec<String>,
    /// Listen address.
    pub listen: SocketAddr,
    /// Model id reported on `/v1/models`.
    pub model_id: String,
    /// Environment variable that holds the upstream API key, if any:
    /// `(runtime variable, source variable)`.
    pub api_key: Option<(String, String)>,
}

/// Inputs for [`LaunchPlan::build`].
#[derive(Debug, Clone)]
pub struct PlanInput<'a> {
    /// Runtime.
    pub runtime: Runtime,
    /// What it loads.
    pub entrypoint: Entrypoint,
    /// Program (resolved path or bare name).
    pub program: PathBuf,
    /// The verified copy's directory.
    pub dir: &'a Path,
    /// Model id to report.
    pub model_id: &'a str,
    /// Listen address.
    pub listen: SocketAddr,
    /// `[inference]` settings (extra args, API key variable).
    pub cfg: &'a InferenceConfig,
    /// Extra arguments from the command line.
    pub extra: &'a [String],
}

impl LaunchPlan {
    /// Build the runtime command line.
    #[must_use]
    pub fn build(i: PlanInput<'_>) -> Self {
        let host = i.listen.ip().to_string();
        let port = i.listen.port().to_string();
        let mut args: Vec<String> = Vec::new();
        match (&i.runtime, &i.entrypoint) {
            (Runtime::LlamaCpp, Entrypoint::Gguf(f)) => {
                let model = tensorseed_core::paths::join_slash(i.dir, f);
                args.extend([
                    "--model".into(),
                    model.to_string_lossy().into_owned(),
                    "--alias".into(),
                    i.model_id.to_string(),
                    "--host".into(),
                    host,
                    "--port".into(),
                    port,
                ]);
            }
            (_, _) => {
                args.extend([
                    "serve".into(),
                    i.dir.to_string_lossy().into_owned(),
                    "--served-model-name".into(),
                    i.model_id.to_string(),
                    "--host".into(),
                    host,
                    "--port".into(),
                    port,
                ]);
            }
        }
        args.extend(i.cfg.args(i.runtime).iter().cloned());
        args.extend(i.extra.iter().cloned());
        let api_key = (!i.cfg.api_key_env.is_empty()).then(|| {
            (
                i.runtime.api_key_var().to_string(),
                i.cfg.api_key_env.clone(),
            )
        });
        Self {
            runtime: i.runtime,
            entrypoint: i.entrypoint,
            program: i.program,
            args,
            listen: i.listen,
            model_id: i.model_id.to_string(),
            api_key,
        }
    }

    /// Local base URL (`http://host:port/v1`); an unspecified listen
    /// address (`0.0.0.0`, `::`) is reached through loopback.
    #[must_use]
    pub fn base_url(&self) -> String {
        local_base_url(self.listen)
    }

    /// The command as one shell line, quoted for the current platform.
    /// The API key is referenced by variable name, never by value.
    #[must_use]
    pub fn shell_line(&self) -> String {
        let mut parts = vec![quote(&self.program.to_string_lossy())];
        parts.extend(self.args.iter().map(|a| quote(a)));
        let cmd = parts.join(" ");
        match &self.api_key {
            Some((dst, src)) if cfg!(windows) => format!("$env:{dst} = $env:{src}; {cmd}"),
            Some((dst, src)) => format!("{dst}=\"${src}\" {cmd}"),
            None => cmd,
        }
    }
}

/// `http://host:port/v1` for a listen address.
#[must_use]
pub fn local_base_url(listen: SocketAddr) -> String {
    let ip = match listen.ip() {
        IpAddr::V4(v) if v.is_unspecified() => IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        IpAddr::V6(v) if v.is_unspecified() => IpAddr::V6(std::net::Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    format!("http://{}/v1", SocketAddr::new(ip, listen.port()))
}

/// Quote one argument for a shell (POSIX single quotes, or PowerShell
/// single quotes on Windows).
#[must_use]
pub fn quote(s: &str) -> String {
    let safe = !s.is_empty()
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric() || b"-_./:=@+,%".contains(&b) || (cfg!(windows) && b == b'\\')
        });
    if safe {
        s.to_string()
    } else {
        format!(
            "'{}'",
            s.replace('\'', if cfg!(windows) { "''" } else { "'\\''" })
        )
    }
}

/// Default model id: the repo id (`org/name`).
#[must_use]
pub fn default_model_id(m: &Manifest) -> String {
    m.repo.id.clone()
}

/// Parse an OpenAI `/v1/models` body into model ids.
///
/// # Errors
///
/// Fails when the body is not a model list.
pub fn parse_models(body: &[u8]) -> anyhow::Result<Vec<String>> {
    let v: serde_json::Value = serde_json::from_slice(body).context("/v1/models is not JSON")?;
    let data = v
        .get("data")
        .and_then(|d| d.as_array())
        .context("/v1/models has no data array")?;
    Ok(data
        .iter()
        .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(str::to_owned))
        .collect())
}

/// Poll `{base_url}/models` until it answers with a model list, the
/// timeout passes, or `alive` reports the runtime exited.
///
/// # Errors
///
/// Fails on timeout or when the runtime exits first.
pub async fn wait_ready(
    base_url: &str,
    api_key: Option<&str>,
    timeout: Duration,
    mut alive: impl FnMut() -> anyhow::Result<()>,
) -> anyhow::Result<Vec<String>> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()?;
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let start = Instant::now();
    let mut last: String;
    loop {
        alive()?;
        let mut req = http.get(&url);
        if let Some(k) = api_key {
            req = req.bearer_auth(k);
        }
        match req.send().await {
            Ok(r) if r.status().is_success() => match parse_models(&r.bytes().await?) {
                Ok(ids) if !ids.is_empty() => return Ok(ids),
                Ok(_) => last = "empty model list".into(),
                Err(e) => last = format!("{e:#}"),
            },
            Ok(r) => last = format!("HTTP {}", r.status()),
            Err(e) => last = format!("{e}"),
        }
        if start.elapsed() >= timeout {
            bail!(
                "{url} did not list a model within {}s (last: {last})",
                timeout.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Catalog provenance in a [`ServedWeights`] record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServedCatalog {
    /// Catalog name.
    pub name: String,
    /// Publisher key id (hex).
    pub publisher_key_id: Option<String>,
    /// Catalog key of the manifest entry.
    pub manifest_key: String,
    /// SHA-256 of the signed manifest value.
    pub manifest_sha256: String,
}

/// License facts in a [`ServedWeights`] record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServedLicense {
    /// License id.
    pub id: Option<String>,
    /// Publisher license class.
    pub class: tensorseed_core::manifest::LicenseClass,
    /// Derived commercial serving mark.
    pub commercial_serving: CommercialServing,
    /// The operator passed `--i-have-rights`.
    pub rights_asserted: bool,
}

/// `.tensorseed/served-weights.json` (`ts.served-weights/0`).
///
/// Unsigned and provisional: a placeholder for a signed weights statement
/// whose format is still being designed. Readers must check `schema` and
/// must not treat this file as proof of anything; it only records what this
/// machine verified and started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServedWeights {
    /// [`SERVED_WEIGHTS_SCHEMA`].
    pub schema: String,
    /// Always false in version 0.
    pub signed: bool,
    /// Human note about the format status.
    pub note: String,
    /// Catalog provenance.
    pub catalog: ServedCatalog,
    /// `model` and repo id.
    #[serde(rename = "type")]
    pub repo_type: RepoType,
    /// Repo id.
    pub repo: String,
    /// Commit (revision).
    pub revision: String,
    /// License facts.
    pub license: ServedLicense,
    /// Runtime.
    pub runtime: Runtime,
    /// What the runtime loads.
    pub entrypoint: Entrypoint,
    /// Model id on `/v1/models`.
    pub served_model_id: String,
    /// Local OpenAI compatible base URL.
    pub base_url: String,
    /// Public base URL (reverse proxy), when given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_base_url: Option<String>,
    /// Every verified file of the copy.
    pub files: Vec<VerifiedFile>,
    /// When verification finished and the runtime answered (RFC 3339 UTC).
    pub created_at: String,
}

/// Note stored in every version 0 record.
pub const SERVED_WEIGHTS_NOTE: &str = "Unsigned placeholder. The signed weights statement format is not decided yet; do not rely on this file as proof.";

impl ServedWeights {
    /// Build the record for a started server.
    #[must_use]
    pub fn new(
        copy: &VerifiedCopy,
        plan: &LaunchPlan,
        license: &LicenseDecision,
        rights_asserted: bool,
        public_base_url: Option<String>,
        created_at: String,
    ) -> Self {
        Self::from_parts(
            copy,
            plan.runtime,
            plan.entrypoint.clone(),
            plan.model_id.clone(),
            plan.base_url(),
            license,
            rights_asserted,
            public_base_url,
            created_at,
        )
    }

    /// Build the record for an engine Tensorseed did not start.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        copy: &VerifiedCopy,
        runtime: Runtime,
        entrypoint: Entrypoint,
        served_model_id: String,
        base_url: String,
        license: &LicenseDecision,
        rights_asserted: bool,
        public_base_url: Option<String>,
        created_at: String,
    ) -> Self {
        Self {
            schema: SERVED_WEIGHTS_SCHEMA.into(),
            signed: false,
            note: SERVED_WEIGHTS_NOTE.into(),
            catalog: ServedCatalog {
                name: copy.catalog.clone(),
                publisher_key_id: copy.publisher_key_id.clone(),
                manifest_key: copy.manifest_key.clone(),
                manifest_sha256: copy.manifest_sha256.clone(),
            },
            repo_type: copy.manifest.repo.repo_type,
            repo: copy.manifest.repo.id.clone(),
            revision: copy.manifest.hf_commit.clone(),
            license: ServedLicense {
                id: license.assessment.license.clone(),
                class: copy.manifest.license.class,
                commercial_serving: license.assessment.value,
                rights_asserted,
            },
            runtime,
            entrypoint,
            served_model_id,
            base_url,
            public_base_url,
            files: copy.files.clone(),
            created_at,
        }
    }

    /// Write to `<dir>/.tensorseed/served-weights.json`.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be written.
    pub fn write(&self, dir: &Path) -> anyhow::Result<PathBuf> {
        let mut data = serde_json::to_vec_pretty(self)?;
        data.push(b'\n');
        crate::pull::write_meta(dir, SERVED_WEIGHTS_FILE, &data)?;
        Ok(dir.join(META_DIR).join(SERVED_WEIGHTS_FILE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tensorseed_core::manifest::LicenseClass;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn config_defaults_and_validation() {
        let c = InferenceConfig::default();
        c.validate().unwrap();
        assert_eq!(c.listen().unwrap().to_string(), "127.0.0.1:8090");
        let bad = |f: fn(&mut InferenceConfig)| {
            let mut c = InferenceConfig::default();
            f(&mut c);
            c.validate().is_err()
        };
        assert!(bad(|c| c.host = "localhost".into()));
        assert!(bad(|c| c.port = 0));
        assert!(bad(|c| c.ready_timeout_secs = 0));
        assert!(bad(|c| c.api_key_env = "MY KEY".into()));
        assert!(!bad(|c| c.api_key_env = "TS_UPSTREAM_KEY".into()));
        assert!(!bad(|c| c.host = "::".into()));
    }

    #[test]
    fn gguf_selection() {
        let files = s(&[
            "README.md",
            "model-q4_k_m.gguf",
            "mmproj-f16.gguf",
            "big/model-q8_0-00001-of-00002.gguf",
            "big/model-q8_0-00002-of-00002.gguf",
        ]);
        assert_eq!(
            gguf_candidates(&files),
            s(&["big/model-q8_0-00001-of-00002.gguf", "model-q4_k_m.gguf"])
        );
        let err = choose_runtime(None, &files, None).unwrap_err().to_string();
        assert!(err.contains("--file"), "{err}");
        let (rt, e) = choose_runtime(None, &files, Some("model-q4_k_m.gguf")).unwrap();
        assert_eq!(rt, Runtime::LlamaCpp);
        assert_eq!(e, Entrypoint::Gguf("model-q4_k_m.gguf".into()));
        assert!(choose_runtime(None, &files, Some("missing.gguf")).is_err());
        assert!(choose_runtime(None, &files, Some("README.md")).is_err());
        let one = s(&["a.gguf", "README.md"]);
        assert_eq!(
            choose_runtime(None, &one, None).unwrap(),
            (Runtime::LlamaCpp, Entrypoint::Gguf("a.gguf".into()))
        );
        assert!(choose_runtime(Some(Runtime::Vllm), &one, None).is_err());
    }

    #[test]
    fn safetensors_selection() {
        let st = s(&["config.json", "model.safetensors", "tokenizer.json"]);
        assert_eq!(
            choose_runtime(None, &st, None).unwrap(),
            (Runtime::Vllm, Entrypoint::Directory)
        );
        assert!(choose_runtime(Some(Runtime::LlamaCpp), &st, None).is_err());
        assert!(choose_runtime(Some(Runtime::Vllm), &st, Some("x.gguf")).is_err());
        let none = s(&["README.md", "pytorch_model.bin"]);
        assert!(choose_runtime(None, &none, None).is_err());
        assert!(check_extra_files(Runtime::Vllm, &s(&["evil.safetensors"])).is_err());
        assert!(check_extra_files(Runtime::LlamaCpp, &s(&["notes.txt"])).is_ok());
        assert!(check_extra_files(Runtime::Vllm, &[]).is_ok());
    }

    fn manifest(license: Option<&str>, class: LicenseClass) -> Manifest {
        let json = serde_json::json!({
            "schema": "ts.manifest/1",
            "repo": {"type": "model", "id": "org/tiny", "hf_url": "https://huggingface.co/org/tiny"},
            "hf_commit": "0123456789abcdef0123456789abcdef01234567",
            "hf_last_modified": null,
            "published_at": "2026-11-13T00:00:00Z",
            "publisher_tool": "tensorseed/test",
            "license": {"spdx": license, "hf_tag": license, "class": class,
                        "license_files": [], "attribution": null, "gated_upstream": false},
            "totals": {"files": 1, "bytes": 3},
            "file_pages": 0,
            "files": [{"path": "tiny.gguf", "size": 3, "sha256": "ab".repeat(32),
                       "hf_lfs_oid": null, "bt_v2_pieces_root": null, "format": "gguf", "pickle_risk": false}],
            "webseeds": [],
            "torrent": null,
            "backups": []
        });
        serde_json::from_value(json).expect("test manifest")
    }

    #[test]
    fn license_refusals() {
        let m = manifest(Some("apache-2.0"), LicenseClass::Open);
        let d = license_gate(&m, false).unwrap();
        assert_eq!(d.assessment.value, CommercialServing::Allowed);
        assert!(d.warnings.is_empty());

        let m = manifest(Some("cc-by-nc-4.0"), LicenseClass::Open);
        let err = license_gate(&m, false).unwrap_err().to_string();
        assert!(
            err.contains("forbidden") && err.contains("--i-have-rights"),
            "{err}"
        );
        let d = license_gate(&m, true).unwrap();
        assert_eq!(d.warnings.len(), 1);
        assert!(d.warnings[0].contains("--i-have-rights"));

        let m = manifest(Some("llama3.2"), LicenseClass::Attribution);
        let d = license_gate(&m, false).unwrap();
        assert_eq!(d.assessment.value, CommercialServing::Restricted);
        assert!(d.warnings[0].contains("700M"));

        let m = manifest(None, LicenseClass::Unknown);
        let d = license_gate(&m, false).unwrap();
        assert_eq!(d.assessment.value, CommercialServing::Unknown);
        assert_eq!(d.warnings.len(), 1);
    }

    fn plan(rt: Runtime, entry: Entrypoint, cfg: &InferenceConfig, extra: &[String]) -> LaunchPlan {
        LaunchPlan::build(PlanInput {
            runtime: rt,
            entrypoint: entry,
            program: PathBuf::from(rt.default_binary()),
            dir: Path::new("/data/org/tiny"),
            model_id: "org/tiny",
            listen: "127.0.0.1:8090".parse().unwrap(),
            cfg,
            extra,
        })
    }

    #[test]
    fn llama_command_line() {
        let cfg = InferenceConfig {
            llama_server_args: s(&["-c", "4096"]),
            ..InferenceConfig::default()
        };
        let p = plan(
            Runtime::LlamaCpp,
            Entrypoint::Gguf("sub/tiny q4.gguf".into()),
            &cfg,
            &s(&["-ngl", "99"]),
        );
        let model =
            tensorseed_core::paths::join_slash(Path::new("/data/org/tiny"), "sub/tiny q4.gguf")
                .to_string_lossy()
                .into_owned();
        assert_eq!(
            p.args,
            vec![
                "--model".to_string(),
                model,
                "--alias".into(),
                "org/tiny".into(),
                "--host".into(),
                "127.0.0.1".into(),
                "--port".into(),
                "8090".into(),
                "-c".into(),
                "4096".into(),
                "-ngl".into(),
                "99".into(),
            ]
        );
        assert_eq!(p.base_url(), "http://127.0.0.1:8090/v1");
        assert!(p.api_key.is_none());
        if !cfg!(windows) {
            assert_eq!(
                p.shell_line(),
                "llama-server --model '/data/org/tiny/sub/tiny q4.gguf' --alias org/tiny --host 127.0.0.1 --port 8090 -c 4096 -ngl 99"
            );
        }
    }

    #[test]
    fn vllm_command_line_and_api_key() {
        let cfg = InferenceConfig {
            vllm_args: s(&["--max-model-len", "8192"]),
            api_key_env: "TS_UPSTREAM_KEY".into(),
            ..InferenceConfig::default()
        };
        let p = plan(Runtime::Vllm, Entrypoint::Directory, &cfg, &[]);
        assert_eq!(
            &p.args[..2],
            &["serve".to_string(), "/data/org/tiny".to_string()]
        );
        assert!(
            p.args
                .windows(2)
                .any(|w| w == ["--served-model-name", "org/tiny"])
        );
        assert!(p.args.ends_with(&s(&["--max-model-len", "8192"])));
        assert_eq!(
            p.api_key,
            Some(("VLLM_API_KEY".to_string(), "TS_UPSTREAM_KEY".to_string()))
        );
        // The key is referenced, never inlined.
        assert!(p.shell_line().contains("TS_UPSTREAM_KEY"));
        assert!(!p.args.iter().any(|a| a.contains("TS_UPSTREAM_KEY")));
    }

    #[test]
    fn base_urls() {
        assert_eq!(
            local_base_url("0.0.0.0:9000".parse().unwrap()),
            "http://127.0.0.1:9000/v1"
        );
        assert_eq!(
            local_base_url("[::]:9000".parse().unwrap()),
            "http://[::1]:9000/v1"
        );
        assert_eq!(
            local_base_url("10.1.2.3:9000".parse().unwrap()),
            "http://10.1.2.3:9000/v1"
        );
    }

    #[test]
    fn models_body_parsing() {
        let ids = parse_models(br#"{"object":"list","data":[{"id":"org/tiny","object":"model"}]}"#)
            .unwrap();
        assert_eq!(ids, vec!["org/tiny".to_string()]);
        assert!(parse_models(b"not json").is_err());
        assert!(parse_models(br#"{"models":[]}"#).is_err());
    }

    #[test]
    fn binary_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let name = if cfg!(windows) {
            "llama-server.exe"
        } else {
            "llama-server"
        };
        let bin = dir.path().join(name);
        std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = std::env::join_paths([dir.path()]).unwrap();
        assert_eq!(search_path("llama-server", &path), Some(bin.clone()));
        assert_eq!(search_path("vllm", &path), None);
        let mut cfg = InferenceConfig {
            llama_server: bin.to_string_lossy().into_owned(),
            ..InferenceConfig::default()
        };
        let l = find_binary(&cfg, Runtime::LlamaCpp);
        assert!(l.configured);
        assert_eq!(l.found, Some(bin));
        cfg.llama_server = dir.path().join("nope").to_string_lossy().into_owned();
        assert_eq!(find_binary(&cfg, Runtime::LlamaCpp).found, None);
    }

    #[test]
    fn missing_engine_message_says_what_to_install() {
        let lookup = BinaryLookup {
            wanted: "llama-server".into(),
            configured: false,
            found: None,
        };
        let m = missing_engine_message(Runtime::LlamaCpp, &lookup);
        assert!(m.starts_with("llama.cpp is not installed: `llama-server` was not found on PATH."));
        assert!(m.contains("does not bundle inference engines"));
        assert!(m.contains("winget install llama.cpp"));
        assert!(m.contains("inference.llama_server"));
        assert!(m.contains("TENSORSEED_INFERENCE__LLAMA_SERVER"));
        assert!(m.ends_with(ENGINE_DOCS_URL));

        let lookup = BinaryLookup {
            wanted: "/opt/vllm/bin/vllm".into(),
            configured: true,
            found: None,
        };
        let m = missing_engine_message(Runtime::Vllm, &lookup);
        assert!(m.contains("(inference.vllm = \"/opt/vllm/bin/vllm\" is not an executable file)"));
        assert!(m.contains("pip install vllm"));
        assert!(m.contains("Linux and a supported GPU"));
    }

    #[test]
    fn served_weights_record() {
        let m = manifest(Some("apache-2.0"), LicenseClass::Open);
        let copy = VerifiedCopy {
            dir: PathBuf::from("/data/org/tiny"),
            manifest: m.clone(),
            manifest_key: "ts/v1/repo/model/org/tiny/rev/0123456789abcdef0123456789abcdef01234567"
                .into(),
            manifest_sha256: "cd".repeat(32),
            catalog: "tensorseed".into(),
            publisher_key_id: Some("CE1E0E3D543DFFB8".into()),
            manifest_source: "stored",
            files: vec![VerifiedFile {
                path: "tiny.gguf".into(),
                size: 3,
                sha256: "ab".repeat(32),
            }],
            skipped: Vec::new(),
            extra: Vec::new(),
        };
        let cfg = InferenceConfig::default();
        let p = plan(
            Runtime::LlamaCpp,
            Entrypoint::Gguf("tiny.gguf".into()),
            &cfg,
            &[],
        );
        let lic = license_gate(&m, false).unwrap();
        let sw = ServedWeights::new(
            &copy,
            &p,
            &lic,
            false,
            Some("https://llm.example.com/v1".into()),
            "2026-11-13T20:00:00Z".into(),
        );
        let v = serde_json::to_value(&sw).unwrap();
        assert_eq!(v["schema"], "ts.served-weights/0");
        assert_eq!(v["signed"], false);
        assert_eq!(v["type"], "model");
        assert_eq!(v["repo"], "org/tiny");
        assert_eq!(v["revision"], "0123456789abcdef0123456789abcdef01234567");
        assert_eq!(v["catalog"]["manifest_sha256"], "cd".repeat(32));
        assert_eq!(v["license"]["commercial_serving"], "allowed");
        assert_eq!(v["runtime"], "llama-cpp");
        assert_eq!(v["entrypoint"]["kind"], "gguf");
        assert_eq!(v["entrypoint"]["path"], "tiny.gguf");
        assert_eq!(v["served_model_id"], "org/tiny");
        assert_eq!(v["base_url"], "http://127.0.0.1:8090/v1");
        assert_eq!(v["public_base_url"], "https://llm.example.com/v1");
        assert_eq!(v["files"][0]["sha256"], "ab".repeat(32));
        let dir = tempfile::tempdir().unwrap();
        let path = sw.write(dir.path()).unwrap();
        let back: ServedWeights = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(back, sw);
    }

    #[test]
    fn later_shards() {
        assert!(is_later_shard("m-00002-of-00003.gguf"));
        assert!(!is_later_shard("m-00001-of-00003.gguf"));
        assert!(!is_later_shard("m-q4.gguf"));
    }

    /// A one thread HTTP server: 503 for the first `warmup` requests, then
    /// a model list. Records whether a bearer token arrived.
    fn mock_models(warmup: usize) -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}/v1", l.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for (n, conn) in l.incoming().enumerate() {
                let Ok(mut c) = conn else { break };
                let mut buf = [0u8; 4096];
                let k = c.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..k]).to_string();
                let _ = tx.send(req);
                let (status, body) = if n < warmup {
                    ("503 Service Unavailable", "{}".to_string())
                } else {
                    (
                        "200 OK",
                        r#"{"object":"list","data":[{"id":"org/tiny"}]}"#.to_string(),
                    )
                };
                let _ = write!(
                    c,
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        (base, rx)
    }

    #[tokio::test]
    async fn wait_ready_polls_until_models_are_listed() {
        let (base, rx) = mock_models(2);
        let ids = wait_ready(&base, Some("k3y"), Duration::from_secs(20), || Ok(()))
            .await
            .unwrap();
        assert_eq!(ids, vec!["org/tiny".to_string()]);
        let reqs: Vec<String> = rx.try_iter().collect();
        assert!(reqs.len() >= 3, "retried through the warmup");
        assert!(reqs.iter().all(|r| r.starts_with("GET /v1/models ")));
        assert!(
            reqs.iter()
                .all(|r| r.to_ascii_lowercase().contains("authorization: bearer k3y"))
        );
    }

    #[tokio::test]
    async fn wait_ready_stops_when_the_runtime_exits_or_times_out() {
        let (base, _rx) = mock_models(usize::MAX);
        let err = wait_ready(&base, None, Duration::from_secs(20), || {
            bail!("runtime exited")
        })
        .await
        .unwrap_err();
        assert!(err.to_string().contains("runtime exited"));
        let err = wait_ready(&base, None, Duration::from_millis(1), || Ok(()))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("did not list a model"), "{err}");
    }
}
