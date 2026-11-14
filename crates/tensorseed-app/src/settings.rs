// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Edit the `[torrent]` section of the shared config file without losing
//! anything else in it.
//!
//! Desktop's settings view reads and writes the same `tensorseed.toml` the
//! CLI uses. [`read_torrent`] returns the values from the file over the
//! defaults (environment overrides are reported, not baked in).
//! [`write_torrent`] edits the document with `toml_edit`, so comments,
//! formatting, every other section, and `[torrent]` keys the caller did not
//! change survive byte for byte.
//! A key is written when it is already in the file or differs from the
//! default, so an untouched default never appears. The result is loaded
//! and validated exactly like the CLI loads it before it replaces the file
//! (temporary file, then rename).

use std::path::Path;

use anyhow::{Context, bail};
use figment::Figment;
use figment::providers::{Format, Serialized, Toml};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

use crate::config::{Config, Torrent};

/// `[torrent]` values from the file at `path` over the defaults, without
/// environment overrides. A missing file gives the defaults.
///
/// # Errors
///
/// Fails when the file cannot be parsed or `[torrent]` has an invalid or
/// unknown key.
pub fn read_torrent(path: &Path) -> anyhow::Result<Torrent> {
    let cfg: Config = Figment::from(Serialized::defaults(Config::default()))
        .merge(Toml::file(path))
        .extract()
        .with_context(|| format!("reading {}", path.display()))?;
    Ok(cfg.torrent)
}

/// Environment variables that override `[torrent]` keys for the CLI and
/// Desktop (`TENSORSEED_TORRENT__*`). Values are not returned.
#[must_use]
pub fn torrent_env_overrides() -> Vec<String> {
    let mut v: Vec<String> = std::env::vars_os()
        .filter_map(|(k, _)| k.into_string().ok())
        .filter(|k| k.starts_with("TENSORSEED_TORRENT__"))
        .collect();
    v.sort();
    v
}

/// Write `t` as the `[torrent]` section of the file at `path`, keeping
/// everything else. Returns the new file text.
///
/// # Errors
///
/// Fails when the current file is not valid TOML, when the result does
/// not load and validate as a Tensorseed config, or when the file cannot
/// be written. On failure the file is left unchanged.
pub fn write_torrent(path: &Path, t: &Torrent) -> anyhow::Result<String> {
    t.validate()?;
    let current = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let text = edit_torrent(&current, t)?;
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = tempfile_in(dir)?;
    std::fs::write(&tmp, &text).with_context(|| format!("writing {}", tmp.display()))?;
    let checked = crate::config::load(&tmp).and_then(|cfg| {
        if cfg.torrent != *t && torrent_env_overrides().is_empty() {
            bail!("the written [torrent] section does not read back the same");
        }
        Ok(())
    });
    if let Err(e) = checked {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.context("the new config does not load; nothing was saved"));
    }
    #[cfg(unix)]
    if let Ok(m) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, m.permissions());
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(text)
}

fn tempfile_in(dir: &Path) -> anyhow::Result<std::path::PathBuf> {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("random source: {e}"))?;
    Ok(dir.join(format!(".tensorseed.toml.{}.tmp", hex::encode(b))))
}

/// Apply `t` to the TOML text `current` and return the edited text.
///
/// # Errors
///
/// Fails when `current` is not valid TOML or `[torrent]` is not a table.
pub fn edit_torrent(current: &str, t: &Torrent) -> anyhow::Result<String> {
    let mut doc: DocumentMut = current
        .parse()
        .context("the config file is not valid TOML")?;
    let new = toml::Value::try_from(t)?;
    let def = toml::Value::try_from(Torrent::default())?;
    let (Some(new), Some(def)) = (new.as_table(), def.as_table()) else {
        bail!("[torrent] did not serialize to a table");
    };
    let needed = new.iter().any(|(k, v)| def.get(k) != Some(v));
    if !doc.contains_key("torrent") && !needed {
        return Ok(doc.to_string());
    }
    if !doc.contains_key("torrent") {
        doc.insert("torrent", Item::Table(Table::new()));
    }
    let table = doc["torrent"]
        .as_table_mut()
        .context("[torrent] in the config file is not a table")?;
    apply(table, new, def)?;
    Ok(doc.to_string())
}

/// Set every key that is present in `table` or differs from `def`.
/// Field order of `Torrent`, `AltSpeed`, and `SpeedWindow`, so keys new to
/// the file are written in the order the structs (and the docs) use rather
/// than alphabetically.
const FIELD_ORDER: &[&str] = &[
    "enabled",
    "create",
    "hybrid",
    "piece_length",
    "trackers",
    "listen_addr",
    "listen_port",
    "upload_limit_kbps",
    "dht",
    "lsd",
    "peers",
    "pull_swarm",
    "swarm_no_peer_secs",
    "swarm_stall_secs",
    "swarm_timeout_secs",
    "download_limit_kbps",
    "max_peers_per_torrent",
    "utp",
    "proxy_url",
    "bind_interface",
    "concurrent_init_limit",
    "torrent_upload_limit_kbps",
    "torrent_download_limit_kbps",
    "seed_ratio_limit",
    "seed_time_limit",
    "seed_limit_action",
    "alt_speed",
    "window",
    "days",
    "start",
    "end",
];

fn ordered(t: &toml::Table) -> Vec<(&String, &toml::Value)> {
    let mut v: Vec<_> = t.iter().collect();
    v.sort_by_key(|(k, _)| {
        FIELD_ORDER
            .iter()
            .position(|f| f == k)
            .unwrap_or(usize::MAX)
    });
    v
}

fn apply(table: &mut Table, new: &toml::Table, def: &toml::Table) -> anyhow::Result<()> {
    for (k, v) in ordered(new) {
        let present = table.contains_key(k);
        let changed = def.get(k) != Some(v);
        if !present && !changed {
            continue;
        }
        match v {
            toml::Value::Table(sub) => {
                let empty = toml::Table::new();
                let sub_def = def.get(k).and_then(toml::Value::as_table).unwrap_or(&empty);
                if !table.contains_key(k) {
                    let mut t = Table::new();
                    t.set_implicit(false);
                    table.insert(k, Item::Table(t));
                }
                let st = table[k.as_str()]
                    .as_table_mut()
                    .with_context(|| format!("torrent.{k} in the config file is not a table"))?;
                apply(st, sub, sub_def)?;
            }
            toml::Value::Array(a) if a.first().is_some_and(toml::Value::is_table) => {
                let mut arr = ArrayOfTables::new();
                for item in a {
                    let mut t = Table::new();
                    if let Some(it) = item.as_table() {
                        for (ik, iv) in ordered(it) {
                            t.insert(ik, Item::Value(to_edit(iv)));
                        }
                    }
                    arr.push(t);
                }
                table.insert(k, Item::ArrayOfTables(arr));
            }
            toml::Value::Array(a)
                if a.is_empty() && table.get(k).is_some_and(Item::is_array_of_tables) =>
            {
                table.remove(k);
            }
            _ => {
                let mut val = to_edit(v);
                match table.get_mut(k) {
                    // Replace in place so the key keeps its comments.
                    Some(item @ Item::Value(_)) => {
                        if let Item::Value(old) = &*item {
                            *val.decor_mut() = old.decor().clone();
                        }
                        *item = Item::Value(val);
                    }
                    _ => {
                        table.insert(k, Item::Value(val));
                    }
                }
            }
        }
    }
    Ok(())
}

fn to_edit(v: &toml::Value) -> Value {
    match v {
        toml::Value::String(s) => Value::from(s.as_str()),
        toml::Value::Integer(i) => Value::from(*i),
        toml::Value::Float(f) => Value::from(*f),
        toml::Value::Boolean(b) => Value::from(*b),
        toml::Value::Datetime(d) => Value::from(d.to_string()),
        toml::Value::Array(a) => {
            let mut arr = toml_edit::Array::new();
            for x in a {
                arr.push(to_edit(x));
            }
            Value::Array(arr)
        }
        toml::Value::Table(t) => {
            let mut it = toml_edit::InlineTable::new();
            for (k, x) in t {
                it.insert(k, to_edit(x));
            }
            Value::InlineTable(it)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::swarm::{SeedLimitAction, SpeedWindow};

    const FILE: &str = r#"# My Tensorseed config
[general]
download_dir = "~/models" # where pulls go

[torrent]
# keep it gentle
upload_limit_kbps = 500 # KiB/s
dht = true

# Health endpoint for the seeder daemon.
[serve]
health_listen = "127.0.0.1:6880" # local only
"#;

    #[test]
    fn edits_keep_comments_unknown_sections_and_defaults_out() {
        let mut t: Torrent = toml::from_str::<toml::Table>(FILE).unwrap()["torrent"]
            .clone()
            .try_into()
            .unwrap();
        t.upload_limit_kbps = 800;
        t.utp = true;
        t.proxy_url = "socks5h://127.0.0.1:9050".into();
        let out = edit_torrent(FILE, &t).unwrap();
        assert!(out.starts_with("# My Tensorseed config\n"), "{out}");
        assert!(
            out.contains("download_dir = \"~/models\" # where pulls go"),
            "{out}"
        );
        assert!(out.contains("# keep it gentle"), "{out}");
        assert!(out.contains("upload_limit_kbps = 800 # KiB/s"), "{out}");
        assert!(
            out.contains("# Health endpoint for the seeder daemon.\n[serve]\nhealth_listen = \"127.0.0.1:6880\" # local only"),
            "{out}"
        );
        assert!(out.contains("utp = true"), "{out}");
        assert!(
            out.contains("proxy_url = \"socks5h://127.0.0.1:9050\""),
            "{out}"
        );
        assert!(out.contains("dht = true"), "present keys stay");
        assert!(
            !out.contains("listen_port"),
            "untouched defaults are not written: {out}"
        );
        // Idempotent.
        assert_eq!(edit_torrent(&out, &t).unwrap(), out);
    }

    #[test]
    fn schedules_and_stop_rules_round_trip() {
        let mut t = Torrent {
            seed_ratio_limit: 2.5,
            seed_time_limit: "7d".into(),
            seed_limit_action: SeedLimitAction::Remove,
            ..Default::default()
        };
        t.alt_speed.enabled = true;
        t.alt_speed.upload_limit_kbps = 100;
        t.alt_speed.window = vec![
            SpeedWindow {
                days: vec!["weekdays".into()],
                start: "09:00".into(),
                end: "17:30".into(),
            },
            SpeedWindow {
                days: vec![],
                start: "23:00".into(),
                end: "06:00".into(),
            },
        ];
        let out = edit_torrent("", &t).unwrap();
        assert!(out.contains("[torrent.alt_speed]"), "{out}");
        assert!(out.contains("[[torrent.alt_speed.window]]"), "{out}");
        // New keys follow the struct order, not the alphabet.
        assert!(
            out.contains("days = [\"weekdays\"]\nstart = \"09:00\"\nend = \"17:30\""),
            "{out}"
        );
        assert!(
            out.find("seed_ratio_limit") < out.find("seed_time_limit")
                && out.find("seed_time_limit") < out.find("seed_limit_action"),
            "{out}"
        );
        assert!(
            out.contains("[torrent.alt_speed]\nenabled = true\nupload_limit_kbps = 100"),
            "{out}"
        );
        let back: toml::Table = toml::from_str(&out).unwrap();
        let back: Torrent = back["torrent"].clone().try_into().unwrap();
        assert_eq!(back, t);
        // Removing the windows removes the array.
        t.alt_speed.window.clear();
        let out2 = edit_torrent(&out, &t).unwrap();
        assert!(!out2.contains("[[torrent.alt_speed.window]]"), "{out2}");
        let back: toml::Table = toml::from_str(&out2).unwrap();
        let back: Torrent = back["torrent"].clone().try_into().unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn defaults_on_an_empty_file_write_nothing() {
        assert_eq!(edit_torrent("", &Torrent::default()).unwrap(), "");
        let f = "[general]\nmax_concurrent_downloads = 2\n";
        assert_eq!(edit_torrent(f, &Torrent::default()).unwrap(), f);
    }

    #[test]
    fn write_validates_and_leaves_the_file_alone_on_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tensorseed.toml");
        std::fs::write(&path, FILE).unwrap();
        let mut t = read_torrent(&path).unwrap();
        assert_eq!(t.upload_limit_kbps, 500);
        assert_eq!(t.listen_port, 6881);
        t.download_limit_kbps = 2048;
        write_torrent(&path, &t).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("download_limit_kbps = 2048"));
        assert!(text.contains("health_listen = \"127.0.0.1:6880\" # local only"));
        assert_eq!(read_torrent(&path).unwrap(), t);

        let mut bad = t.clone();
        bad.proxy_url = "http://proxy:8080".into();
        assert!(write_torrent(&path, &bad).is_err());
        let mut bad = t.clone();
        bad.alt_speed.window = vec![SpeedWindow {
            days: vec!["someday".into()],
            start: "09:00".into(),
            end: "10:00".into(),
        }];
        assert!(write_torrent(&path, &bad).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        let leftovers = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(leftovers, 1, "no temporary files are left behind");
    }

    #[test]
    fn a_broken_file_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tensorseed.toml");
        std::fs::write(&path, "[torrent\nbroken").unwrap();
        assert!(write_torrent(&path, &Torrent::default()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[torrent\nbroken");
    }
}
