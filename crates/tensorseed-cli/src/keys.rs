// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Publisher key handling: `catalog keygen` and loading the secret key named
//! by `catalog.publisher_seckey_file`. The password comes from
//! `TENSORSEED_KEY_PASSWORD` or an interactive prompt; it is never stored.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use tensorseed_catalog::minisign::{KdfLimits, SecretKey};

use crate::config::{Config, expand};

/// Environment variable holding the secret key password.
pub const PASSWORD_ENV: &str = "TENSORSEED_KEY_PASSWORD";

fn password(prompt: &str, confirm: bool) -> anyhow::Result<Vec<u8>> {
    if let Ok(p) = std::env::var(PASSWORD_ENV) {
        return Ok(p.into_bytes());
    }
    let p = rpassword::prompt_password(prompt)
        .context("reading password (or set TENSORSEED_KEY_PASSWORD)")?;
    if confirm {
        let again = rpassword::prompt_password("Repeat password: ")?;
        if again != p {
            bail!("passwords do not match");
        }
    }
    if p.is_empty() {
        bail!("empty password; use --no-password to store the key unencrypted");
    }
    Ok(p.into_bytes())
}

/// Load the publisher secret key from config, checking it matches the pinned key.
pub fn load_secret(cfg: &Config) -> anyhow::Result<SecretKey> {
    let Some(path) = cfg
        .catalog
        .publisher_seckey_file
        .as_deref()
        .filter(|p| !p.is_empty())
    else {
        bail!(
            "catalog.publisher_seckey_file is not set; create a key with `tensorseed catalog keygen`"
        );
    };
    let path = expand(path);
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let sk = if SecretKey::file_is_encrypted(&text)? {
        let pw = password(&format!("Password for {}: ", path.display()), false)?;
        SecretKey::from_minisign_file(&text, Some(&pw))?
    } else {
        SecretKey::from_minisign_file(&text, None)?
    };
    Ok(sk)
}

/// Default key directory (`$XDG_CONFIG_HOME/tensorseed` or
/// `~/.config/tensorseed` on Unix, `%APPDATA%\tensorseed` on Windows).
pub fn default_key_dir() -> PathBuf {
    crate::config::config_dir().join("tensorseed")
}

/// Generate a key pair. Returns (secret path, public path, key).
pub fn keygen(
    out_dir: &Path,
    name: &str,
    no_password: bool,
    force: bool,
) -> anyhow::Result<(PathBuf, PathBuf, SecretKey)> {
    if name.is_empty() || name.contains(['/', '\\']) {
        bail!("invalid key name {name:?}");
    }
    let sec = out_dir.join(format!("{name}.key"));
    let pubp = out_dir.join(format!("{name}.pub"));
    for p in [&sec, &pubp] {
        if p.exists() && !force {
            bail!("{} already exists (use --force to overwrite)", p.display());
        }
    }
    let sk = SecretKey::generate()?;
    let text = if no_password {
        sk.to_minisign_file(None, KdfLimits::MINISIGN_DEFAULT)?
    } else {
        let pw = password("Password for the new publisher key: ", true)?;
        sk.to_minisign_file(Some(&pw), KdfLimits::MINISIGN_DEFAULT)?
    };
    std::fs::create_dir_all(out_dir)?;
    write_private(&sec, text.as_bytes())?;
    std::fs::write(&pubp, sk.public_key().to_minisign_file())?;
    Ok((sec, pubp, sk))
}

#[cfg(unix)]
fn write_private(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(data)?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    std::fs::write(path, data)?;
    Ok(())
}
