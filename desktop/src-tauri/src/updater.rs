// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Desktop updates through `tauri-plugin-updater`.
//!
//! The only request is a GET of the `latest.json` endpoint in
//! `tauri.conf.json` (`plugins.updater.endpoints`), then the installer it
//! names once the user confirms. Nothing about this machine is sent beyond
//! what any HTTPS GET carries. Every installer is checked against the
//! minisign public key in `plugins.updater.pubkey` (key id
//! B81047C9B22BB400), and the version inside the signature must match the
//! announced one (`requireSignedVersion`).
//!
//! Which builds update themselves:
//!
//! - AppImage on Linux, and the MSI and NSIS installers on Windows.
//! - Not the `.deb` or `.rpm` packages and not the Flatpak: the package
//!   manager or Flathub owns those files, so Desktop only says where the
//!   update comes from.
//! - Not development builds (`cargo run`, `tauri dev`), which have no
//!   bundle type.
//!
//! The webview reaches this module only through the `update_*` commands;
//! the plugin's own JavaScript API is not exposed.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::utils::config::BundleType;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_updater::{Update, UpdaterExt};

/// Event with the [`Available`] update found by the startup check.
pub const UPDATE_AVAILABLE_EVENT: &str = "update://available";
/// Event with download progress ([`Progress`]).
pub const UPDATE_PROGRESS_EVENT: &str = "update://progress";

/// Seconds after start before the startup check runs, so it never competes
/// with the first catalog sync.
const STARTUP_DELAY_SECS: u64 = 10;

/// How this copy of Desktop was installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Channel {
    /// Linux AppImage: updates itself.
    AppImage,
    /// Windows NSIS installer (per user): updates itself.
    Nsis,
    /// Windows MSI installer (per machine): updates itself.
    Msi,
    /// Debian or Ubuntu package: the package manager updates it.
    Deb,
    /// Fedora or RHEL package: the package manager updates it.
    Rpm,
    /// Flatpak: Flatpak or Flathub updates it.
    Flatpak,
    /// macOS app bundle: not built yet.
    MacApp,
    /// A development build without a bundle type.
    Dev,
}

impl Channel {
    /// The channel of the running binary.
    #[must_use]
    pub fn current() -> Self {
        Self::detect(
            std::env::var_os("FLATPAK_ID").is_some(),
            tauri::utils::platform::bundle_type(),
        )
    }

    /// The channel for a Flatpak flag and a Tauri bundle type.
    #[must_use]
    pub fn detect(flatpak: bool, bundle: Option<BundleType>) -> Self {
        if flatpak {
            return Self::Flatpak;
        }
        match bundle {
            Some(BundleType::AppImage) => Self::AppImage,
            Some(BundleType::Nsis) => Self::Nsis,
            Some(BundleType::Msi) => Self::Msi,
            Some(BundleType::Deb) => Self::Deb,
            Some(BundleType::Rpm) => Self::Rpm,
            Some(BundleType::App | BundleType::Dmg) => Self::MacApp,
            _ => Self::Dev,
        }
    }

    /// True when Desktop can download and install updates itself.
    #[must_use]
    pub fn self_updates(self) -> bool {
        matches!(self, Self::AppImage | Self::Nsis | Self::Msi)
    }

    /// Why updates do not happen here, when they do not.
    #[must_use]
    pub fn elsewhere(self) -> Option<&'static str> {
        match self {
            Self::AppImage | Self::Nsis | Self::Msi => None,
            Self::Deb | Self::Rpm => Some(
                "This copy was installed from a system package. Update it through your package manager.",
            ),
            Self::Flatpak => Some(
                "This copy is a Flatpak. Update it through Flatpak or Flathub (flatpak update).",
            ),
            Self::MacApp => Some("Updates for macOS are not available yet."),
            Self::Dev => Some(
                "This is a development build. Updates are only available in the AppImage, MSI, and NSIS builds.",
            ),
        }
    }
}

/// Desktop's own preferences (`desktop.json` in the app config directory),
/// separate from the CLI's `tensorseed.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Prefs {
    /// Check for an update once, shortly after start.
    pub check_updates_on_start: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            check_updates_on_start: true,
        }
    }
}

impl Prefs {
    /// Read the file; a missing or unreadable file gives the defaults.
    #[must_use]
    pub fn load(path: &std::path::Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Write the file atomically.
    ///
    /// # Errors
    ///
    /// When the directory cannot be created or the file cannot be written.
    pub fn save(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

fn prefs_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    app.path()
        .app_config_dir()
        .map(|d| d.join("desktop.json"))
        .map_err(|e| format!("no app config directory: {e}"))
}

/// The update found by the last check, kept until it is installed.
#[derive(Default)]
pub struct Pending(Mutex<Option<Update>>);

/// What the settings view shows (`UpdateInfo` in `src/lib/native.ts`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Info {
    /// The running version.
    pub current_version: String,
    /// How this copy was installed.
    pub channel: Channel,
    /// True when Desktop installs updates itself.
    pub self_updates: bool,
    /// Where updates come from instead, when Desktop does not install them.
    pub elsewhere: Option<String>,
    /// The startup check preference.
    pub check_on_start: bool,
}

/// An update the endpoint announced (`UpdateAvailable` in `native.ts`).
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Available {
    /// The running version.
    pub current_version: String,
    /// The new version.
    pub version: String,
    /// Release notes from `latest.json`, shown as plain text.
    pub notes: String,
    /// Publication date, when given.
    pub date: Option<String>,
}

impl Available {
    fn from_update(u: &Update) -> Self {
        Self {
            current_version: u.current_version.clone(),
            version: u.version.clone(),
            notes: u.body.clone().unwrap_or_default(),
            date: u.date.map(|d| d.to_string()),
        }
    }
}

/// Download progress (`UpdateProgress` in `native.ts`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    /// Bytes received so far.
    pub downloaded: u64,
    /// Total size, when the server sends it.
    pub total: Option<u64>,
    /// True once the download is complete and the install starts.
    pub done: bool,
}

/// The update state for the settings view.
#[tauri::command]
pub fn update_info<R: Runtime>(app: AppHandle<R>) -> Info {
    let channel = Channel::current();
    let prefs = prefs_path(&app)
        .map(|p| Prefs::load(&p))
        .unwrap_or_default();
    Info {
        current_version: app.package_info().version.to_string(),
        channel,
        self_updates: channel.self_updates(),
        elsewhere: channel.elsewhere().map(str::to_string),
        check_on_start: prefs.check_updates_on_start,
    }
}

/// Turn the startup check on or off.
///
/// # Errors
///
/// The app config directory is unknown or `desktop.json` cannot be
/// written.
#[tauri::command]
pub fn update_set_check_on_start<R: Runtime>(
    app: AppHandle<R>,
    enabled: bool,
) -> Result<(), String> {
    let path = prefs_path(&app)?;
    let mut prefs = Prefs::load(&path);
    prefs.check_updates_on_start = enabled;
    prefs.save(&path).map_err(|e| format!("{e:#}"))
}

async fn check_inner<R: Runtime>(app: &AppHandle<R>) -> Result<Option<Available>, String> {
    let channel = Channel::current();
    if let Some(why) = channel.elsewhere() {
        return Err(why.to_string());
    }
    let updater = app
        .updater_builder()
        .on_before_exit({
            let app = app.clone();
            // Windows installers end the process: stop seeding cleanly
            // first. A separate thread, because this runs inside the async
            // runtime.
            move || {
                let app = app.clone();
                let _ = std::thread::spawn(move || {
                    tauri::async_runtime::block_on(app.state::<crate::seeder::Seeder>().stop());
                })
                .join();
            }
        })
        .build()
        .map_err(|e| format!("updater is not configured: {e}"))?;
    let found = updater
        .check()
        .await
        .map_err(|e| format!("could not check for updates: {e}"))?;
    let available = found.as_ref().map(Available::from_update);
    if let Ok(mut p) = app.state::<Pending>().0.lock() {
        *p = found;
    }
    Ok(available)
}

/// Fetch `latest.json` and report a newer version, if there is one.
/// Nothing is downloaded or installed.
///
/// # Errors
///
/// This build does not self update (package, Flatpak, or development
/// builds), or `latest.json` cannot be fetched or read.
#[tauri::command]
pub async fn update_check<R: Runtime>(app: AppHandle<R>) -> Result<Option<Available>, String> {
    check_inner(&app).await
}

/// Download, verify, and install the update the last check found, then
/// restart. Only called after the user confirmed it.
///
/// # Errors
///
/// No check found this version, or the download, the signature check,
/// or the install fails. The installed copy is left as it was.
#[tauri::command]
pub async fn update_install<R: Runtime>(app: AppHandle<R>, version: String) -> Result<(), String> {
    let update = app
        .state::<Pending>()
        .0
        .lock()
        .map_err(|_| "update state is unavailable".to_string())?
        .take()
        .ok_or("no update to install; check for updates first")?;
    if update.version != version {
        return Err(format!(
            "the update found is {}, not {version}; check again",
            update.version
        ));
    }
    let mut downloaded: u64 = 0;
    let progress = app.clone();
    let finished = app.clone();
    update
        .download_and_install(
            move |chunk, total| {
                downloaded += chunk as u64;
                let _ = progress.emit(
                    UPDATE_PROGRESS_EVENT,
                    Progress {
                        downloaded,
                        total,
                        done: false,
                    },
                );
            },
            move || {
                let _ = finished.emit(
                    UPDATE_PROGRESS_EVENT,
                    Progress {
                        downloaded: 0,
                        total: None,
                        done: true,
                    },
                );
            },
        )
        .await
        .map_err(|e| format!("update failed: {e}"))?;
    // Windows installers have already ended the process. The AppImage was
    // replaced in place; start the new one (seeding stops on exit).
    app.restart();
}

/// The startup check: once, after a short delay, when the preference is on
/// and this build updates itself. A failure is only logged.
pub fn spawn_startup_check<R: Runtime>(app: &AppHandle<R>) {
    if !Channel::current().self_updates() {
        return;
    }
    let on = prefs_path(app)
        .map(|p| Prefs::load(&p).check_updates_on_start)
        .unwrap_or(true);
    if !on {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(STARTUP_DELAY_SECS)).await;
        match check_inner(&app).await {
            Ok(Some(a)) => {
                let _ = app.emit(UPDATE_AVAILABLE_EVENT, a);
            }
            Ok(None) => {}
            Err(e) => tracing::info!("startup update check: {e}"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels() {
        assert_eq!(
            Channel::detect(false, Some(BundleType::AppImage)),
            Channel::AppImage
        );
        assert_eq!(
            Channel::detect(false, Some(BundleType::Nsis)),
            Channel::Nsis
        );
        assert_eq!(Channel::detect(false, Some(BundleType::Msi)), Channel::Msi);
        assert_eq!(Channel::detect(false, Some(BundleType::Deb)), Channel::Deb);
        assert_eq!(Channel::detect(false, Some(BundleType::Rpm)), Channel::Rpm);
        assert_eq!(Channel::detect(false, None), Channel::Dev);
        // A Flatpak wins over whatever the binary was bundled as.
        assert_eq!(
            Channel::detect(true, Some(BundleType::AppImage)),
            Channel::Flatpak
        );
        assert_eq!(Channel::detect(true, None), Channel::Flatpak);
    }

    #[test]
    fn only_appimage_msi_and_nsis_update_themselves() {
        for c in [Channel::AppImage, Channel::Nsis, Channel::Msi] {
            assert!(c.self_updates() && c.elsewhere().is_none(), "{c:?}");
        }
        for c in [Channel::Deb, Channel::Rpm] {
            assert!(!c.self_updates());
            assert!(c.elsewhere().unwrap().contains("package manager"));
        }
        assert!(Channel::Flatpak.elsewhere().unwrap().contains("Flathub"));
        assert!(!Channel::Dev.self_updates());
    }

    #[test]
    fn prefs_default_on_and_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub/desktop.json");
        assert!(Prefs::load(&p).check_updates_on_start);
        Prefs {
            check_updates_on_start: false,
        }
        .save(&p)
        .unwrap();
        assert!(!Prefs::load(&p).check_updates_on_start);
        std::fs::write(&p, b"not json").unwrap();
        assert!(Prefs::load(&p).check_updates_on_start);
        assert_eq!(
            serde_json::to_value(Prefs::default()).unwrap(),
            serde_json::json!({"checkUpdatesOnStart": true})
        );
    }

    #[test]
    fn committed_config_has_the_release_key_and_endpoint() {
        let conf: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let up = &conf["plugins"]["updater"];
        assert_eq!(
            up["endpoints"],
            serde_json::json!([
                "https://github.com/TunedChaos/TensorSeed/releases/latest/download/latest.json"
            ])
        );
        assert_eq!(up["requireSignedVersion"], true);
        assert!(up.get("dangerousInsecureTransportProtocol").is_none());
        assert_eq!(conf["bundle"]["createUpdaterArtifacts"], true);
        use base64::Engine;
        let key = base64::engine::general_purpose::STANDARD
            .decode(up["pubkey"].as_str().unwrap())
            .unwrap();
        let key = String::from_utf8(key).unwrap();
        assert!(
            key.starts_with("untrusted comment: minisign public key: B81047C9B22BB400\n"),
            "{key}"
        );
        // keys/ holds the same key in minisign's text form.
        assert_eq!(
            key,
            include_str!("../../../keys/tensorseed-desktop-updater.pub")
        );
    }
}
