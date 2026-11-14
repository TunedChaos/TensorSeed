// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! One seeder per machine. `tensorseed serve`, `tensorseed seed`, and the
//! Desktop seeder take an exclusive lock on `seeder.lock` next to the job
//! database (`general.state_db`) before they start a listening engine,
//! and describe themselves in `seeder.json` beside it. Two engines on the
//! same data would fight over the listen port, DHT state, and files.
//!
//! A `serve` also opens a control endpoint on 127.0.0.1 (a random port)
//! and writes its address and a random token to `seeder.json`, which is
//! created readable by its owner only on Unix (on Windows it lives in the
//! user's own local app data folder). Desktop reads the file to attach:
//! every control request carries the token, and the endpoint only listens
//! on the loopback interface.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};

use crate::config::{Config, expand};

/// Lock file name.
pub const LOCK_FILE: &str = "seeder.lock";
/// Holder description file name.
pub const INFO_FILE: &str = "seeder.json";
/// Schema of [`INFO_FILE`].
pub const INFO_SCHEMA: &str = "ts.seeder/1";

/// Who holds the seeder lock (`seeder.json`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeederInfo {
    /// [`INFO_SCHEMA`].
    pub schema: String,
    /// `serve`, `seed`, or `desktop`.
    pub kind: String,
    /// Process id.
    pub pid: u32,
    /// Start time (unix seconds).
    pub started_unix: u64,
    /// Control endpoint (`127.0.0.1:port`), `serve` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<String>,
    /// Bearer token for the control endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

impl SeederInfo {
    /// A description of this process as `kind`.
    pub fn this_process(kind: &str) -> Self {
        Self {
            schema: INFO_SCHEMA.into(),
            kind: kind.into(),
            pid: std::process::id(),
            started_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            control: None,
            token: None,
        }
    }

    /// "tensorseed serve (pid 12)" and the like.
    pub fn describe(&self) -> String {
        let what = match self.kind.as_str() {
            "serve" => "tensorseed serve",
            "seed" => "tensorseed seed",
            "desktop" => "Tensorseed Desktop",
            other => other,
        };
        format!("{what} (pid {})", self.pid)
    }
}

/// The folder of the lock: next to `general.state_db`.
pub fn seeder_dir(cfg: &Config) -> PathBuf {
    let db = expand(&cfg.general.state_db);
    db.parent()
        .map(Path::to_owned)
        .unwrap_or_else(std::env::temp_dir)
}

/// The held lock. Dropping it removes `seeder.json` and releases the lock.
#[derive(Debug)]
pub struct SeederLock {
    _file: File,
    info: PathBuf,
}

impl SeederLock {
    /// Describe the holder in `seeder.json` (owner only on Unix).
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be written.
    pub fn publish(&self, info: &SeederInfo) -> anyhow::Result<()> {
        write_private(&self.info, &serde_json::to_vec_pretty(info)?)
    }
}

impl Drop for SeederLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.info);
    }
}

/// Why the lock could not be taken.
#[derive(Debug)]
pub enum LockError {
    /// Another seeder holds it; its description, when readable.
    Busy(Option<SeederInfo>),
    /// The lock file could not be opened or locked.
    Io(anyhow::Error),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Busy(Some(i)) => write!(
                f,
                "another seeder is running on this machine: {} (one seeder per machine; stop it first)",
                i.describe()
            ),
            Self::Busy(None) => f.write_str(
                "another seeder is running on this machine (one seeder per machine; stop it first)",
            ),
            Self::Io(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for LockError {}

fn lock_in(dir: &Path, kind: &str) -> Result<SeederLock, LockError> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating {}", dir.display()))
        .map_err(LockError::Io)?;
    let p = dir.join(LOCK_FILE);
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&p)
        .with_context(|| format!("opening {}", p.display()))
        .map_err(LockError::Io)?;
    match fs4::FileExt::try_lock(&f) {
        Ok(()) => {}
        Err(fs4::TryLockError::WouldBlock) => return Err(LockError::Busy(read_info(dir))),
        Err(fs4::TryLockError::Error(e)) => {
            return Err(LockError::Io(
                anyhow::Error::new(e).context(format!("locking {}", p.display())),
            ));
        }
    }
    let lock = SeederLock {
        _file: f,
        info: dir.join(INFO_FILE),
    };
    lock.publish(&SeederInfo::this_process(kind))
        .map_err(LockError::Io)?;
    Ok(lock)
}

/// Take the seeder lock as `kind` (`serve`, `seed`, `desktop`).
///
/// # Errors
///
/// [`LockError::Busy`] when another seeder holds it.
pub fn acquire(cfg: &Config, kind: &str) -> Result<SeederLock, LockError> {
    lock_in(&seeder_dir(cfg), kind)
}

fn read_info(dir: &Path) -> Option<SeederInfo> {
    let b = std::fs::read(dir.join(INFO_FILE)).ok()?;
    serde_json::from_slice::<SeederInfo>(&b)
        .ok()
        .filter(|i| i.schema == INFO_SCHEMA)
}

fn holder_in(dir: &Path) -> Option<SeederInfo> {
    // Read first: with no description there is no holder, and the probe
    // below (which briefly takes a free lock) never races a starting seeder.
    let info = read_info(dir)?;
    let f = std::fs::OpenOptions::new()
        .write(true)
        .open(dir.join(LOCK_FILE))
        .ok()?;
    match fs4::FileExt::try_lock(&f) {
        // Nobody holds it: a seeder.json left by a crash is stale.
        Ok(()) => None,
        Err(_) => Some(info),
    }
}

/// The seeder running on this machine, if any (another process holds the
/// lock). Its `seeder.json` is only trusted while the lock is held.
pub fn holder(cfg: &Config) -> Option<SeederInfo> {
    holder_in(&seeder_dir(cfg))
}

/// Write `data` to `path` through a temporary file, readable and writable
/// by the owner only on Unix.
fn write_private(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let dir = path.parent().context("path has no parent")?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp = dir.join(format!(".{name}.tmp"));
    let _ = std::fs::remove_file(&tmp);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o
        .open(&tmp)
        .with_context(|| format!("creating {}", tmp.display()))?;
    f.write_all(data)?;
    f.sync_all().ok();
    drop(f);
    std::fs::rename(&tmp, path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// A new control token: 32 random bytes, hex.
///
/// # Errors
///
/// Fails when the system random source fails.
pub fn new_token() -> anyhow::Result<String> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("random source: {e}"))?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

/// A client for the control endpoint of the `serve` described by `info`.
#[derive(Debug, Clone)]
pub struct Control {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Control {
    /// The control client of `info`, when it has an endpoint. Only loopback
    /// addresses are accepted.
    pub fn of(info: &SeederInfo) -> Option<Self> {
        let addr: std::net::SocketAddr = info.control.as_deref()?.parse().ok()?;
        if !addr.ip().is_loopback() {
            return None;
        }
        Some(Self {
            base: format!("http://{addr}"),
            token: info.token.clone()?,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(3))
                .no_proxy()
                .build()
                .ok()?,
        })
    }

    /// `GET /control/status`: the health document.
    ///
    /// # Errors
    ///
    /// Fails when the endpoint does not answer or refuses the token.
    pub async fn status(&self) -> anyhow::Result<serde_json::Value> {
        let r = self
            .http
            .get(format!("{}/control/status", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .context("reaching tensorseed serve")?;
        if !r.status().is_success() {
            bail!("tensorseed serve answered {}", r.status());
        }
        Ok(r.json().await?)
    }

    /// `POST /control/action`: pause or resume one revision (`name`) or
    /// all. Returns how many changed.
    ///
    /// # Errors
    ///
    /// Fails when the endpoint does not answer or rejects the request.
    pub async fn action(&self, action: &str, name: Option<&str>) -> anyhow::Result<usize> {
        let r = self
            .http
            .post(format!("{}/control/action", self.base))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "action": action, "name": name }))
            .send()
            .await
            .context("reaching tensorseed serve")?;
        let ok = r.status().is_success();
        let v: serde_json::Value = r.json().await.unwrap_or_default();
        if !ok {
            bail!(
                "tensorseed serve refused: {}",
                v["error"].as_str().unwrap_or("unknown error")
            );
        }
        Ok(v["changed"].as_u64().unwrap_or(0) as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_holder_and_its_description() {
        let d = tempfile::tempdir().unwrap();
        assert!(holder_in(d.path()).is_none());
        let a = lock_in(d.path(), "serve").unwrap();
        let mut info = SeederInfo::this_process("serve");
        info.control = Some("127.0.0.1:4000".into());
        info.token = Some("t".into());
        a.publish(&info).unwrap();
        let seen = holder_in(d.path()).unwrap();
        assert_eq!(seen.kind, "serve");
        assert_eq!(seen.control.as_deref(), Some("127.0.0.1:4000"));
        match lock_in(d.path(), "desktop") {
            Err(LockError::Busy(Some(i))) => {
                assert_eq!(i.pid, std::process::id());
                assert!(
                    LockError::Busy(Some(i))
                        .to_string()
                        .contains("tensorseed serve (pid")
                );
            }
            other => panic!("expected busy, got {other:?}"),
        }
        drop(a);
        assert!(!d.path().join(INFO_FILE).exists());
        assert!(holder_in(d.path()).is_none());
        let _b = lock_in(d.path(), "desktop").unwrap();
        assert_eq!(holder_in(d.path()).unwrap().kind, "desktop");
    }

    #[test]
    fn stale_description_is_ignored() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(LOCK_FILE), b"").unwrap();
        let info = SeederInfo::this_process("serve");
        std::fs::write(d.path().join(INFO_FILE), serde_json::to_vec(&info).unwrap()).unwrap();
        assert!(holder_in(d.path()).is_none(), "no process holds the lock");
    }

    #[cfg(unix)]
    #[test]
    fn description_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let _l = lock_in(d.path(), "serve").unwrap();
        let mode = std::fs::metadata(d.path().join(INFO_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn tokens() {
        let t = new_token().unwrap();
        assert_eq!(t.len(), 64);
        assert_ne!(t, new_token().unwrap());
    }

    #[test]
    fn control_only_on_loopback() {
        let mut i = SeederInfo::this_process("serve");
        i.token = Some("t".into());
        i.control = Some("127.0.0.1:1".into());
        assert!(Control::of(&i).is_some());
        i.control = Some("192.0.2.1:1".into());
        assert!(Control::of(&i).is_none());
        i.control = Some("127.0.0.1:1".into());
        i.token = None;
        assert!(Control::of(&i).is_none());
    }
}
