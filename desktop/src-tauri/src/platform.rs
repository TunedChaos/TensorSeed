// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Platform specific wiring. Every target gets a portable default so macOS
//! (designed for, not released in 0.3.0) can be added without restructuring.

use tauri::Runtime;

/// Linux and Windows: a second launch (`tensorseed gui`, or a
/// `tensorseed://` link) starts a second process. It hands its arguments to
/// the running one and exits; the plugin's deep-link feature routes the
/// link to `on_open_url`. Here the window is shown again (it may be hidden
/// in the tray) and brought forward.
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub fn single_instance<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
        crate::tray::show_window(app);
    }))
}

/// Other targets (macOS later): the OS hands links to the running app.
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn single_instance<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
}

/// Linux and Windows: register the scheme at runtime too, so development
/// builds and the AppImage (which has no installed .desktop file) open
/// links. Installed packages also register it through the bundle. Inside
/// a Flatpak nothing is written: the exported desktop file declares it.
///
/// On Windows the runtime registration goes to the current user's
/// classes, which an uninstaller does not remove. So it is skipped when
/// the scheme already opens this executable (the NSIS and MSI installers
/// register it), and only written for a binary run outside an installer.
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub fn register_deep_links<R: Runtime>(app: &tauri::App<R>) {
    use tauri_plugin_deep_link::DeepLinkExt;
    #[cfg(target_os = "windows")]
    if scheme_opens_this_exe() {
        tracing::debug!("the tensorseed:// scheme already opens this executable");
        return;
    }
    // A Flatpak exports its own desktop file with the scheme's MimeType,
    // and a handler written from inside the sandbox would point at a path
    // the host cannot run.
    #[cfg(target_os = "linux")]
    if tensorseed_app::config::in_flatpak() {
        tracing::debug!("Flatpak: the exported desktop file registers tensorseed://");
        return;
    }
    if let Err(e) = app.deep_link().register_all() {
        tracing::warn!("could not register the tensorseed:// scheme: {e}");
    }
}

/// True when `tensorseed://` (current user or machine wide) already opens
/// this executable. Paths are compared after resolving them, because the
/// MSI registers a short (8.3) path and the plugin's own check compares the
/// command text exactly.
#[cfg(target_os = "windows")]
fn scheme_opens_this_exe() -> bool {
    let key = format!("{}\\shell\\open\\command", tensorseed_app::links::SCHEME);
    let Ok(cmd) = windows_registry::CLASSES_ROOT
        .open(key)
        .and_then(|k| k.get_string(""))
    else {
        return false;
    };
    let Some(registered) = command_program(&cmd) else {
        return false;
    };
    match (
        std::fs::canonicalize(registered),
        std::env::current_exe().and_then(std::fs::canonicalize),
    ) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// The program of a registry `shell\open\command` value: the first token,
/// quoted or not.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn command_program(cmd: &str) -> Option<&str> {
    let cmd = cmd.trim_start();
    let p = if let Some(rest) = cmd.strip_prefix('"') {
        rest.split('"').next()?
    } else {
        cmd.split_whitespace().next()?
    };
    (!p.is_empty()).then_some(p)
}

/// Other targets: the scheme comes from the app bundle's Info.plist.
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn register_deep_links<R: Runtime>(_app: &tauri::App<R>) {}

#[cfg(test)]
mod tests {
    use super::command_program;

    #[test]
    fn registry_commands_name_their_program() {
        assert_eq!(
            command_program(r#""C:\PROGRA~1\TENSOR~1\TENSOR~1.EXE" "%1""#),
            Some(r"C:\PROGRA~1\TENSOR~1\TENSOR~1.EXE")
        );
        assert_eq!(
            command_program(r"C:\Apps\tensorseed-desktop.exe %1"),
            Some(r"C:\Apps\tensorseed-desktop.exe")
        );
        assert_eq!(command_program(r#""" "%1""#), None);
        assert_eq!(command_program("   "), None);
    }
}

/// Where the tray icon image is written, when the default does not work.
///
/// On Linux the StatusNotifierItem names the icon by file path, and the
/// tray host reads that file. Inside a Flatpak the default
/// (`$XDG_RUNTIME_DIR/tray-icon`) is private to the sandbox, so the icon
/// shows blank. `$XDG_RUNTIME_DIR/app/<app id>` is shared with the host at
/// the same path, so the icon goes there.
pub fn tray_icon_dir() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "linux")]
    if tensorseed_app::config::in_flatpak() {
        return flatpak_tray_icon_dir(
            std::env::var_os("XDG_RUNTIME_DIR").as_deref(),
            std::env::var_os("FLATPAK_ID").as_deref(),
        );
    }
    None
}

#[cfg(target_os = "linux")]
fn flatpak_tray_icon_dir(
    runtime: Option<&std::ffi::OsStr>,
    app_id: Option<&std::ffi::OsStr>,
) -> Option<std::path::PathBuf> {
    let runtime = runtime.filter(|r| !r.is_empty())?;
    let app_id = app_id.filter(|a| !a.is_empty())?;
    Some(
        std::path::Path::new(runtime)
            .join("app")
            .join(app_id)
            .join("tray-icon"),
    )
}

#[cfg(all(test, target_os = "linux"))]
mod flatpak_tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn flatpak_tray_icon_dir_is_the_shared_app_runtime_dir() {
        assert_eq!(
            flatpak_tray_icon_dir(
                Some(OsStr::new("/run/user/1000")),
                Some(OsStr::new("dev.tensorseed.desktop"))
            ),
            Some(std::path::PathBuf::from(
                "/run/user/1000/app/dev.tensorseed.desktop/tray-icon"
            ))
        );
        assert_eq!(
            flatpak_tray_icon_dir(None, Some(OsStr::new("dev.tensorseed.desktop"))),
            None
        );
        assert_eq!(
            flatpak_tray_icon_dir(Some(OsStr::new("/run/user/1000")), Some(OsStr::new(""))),
            None
        );
    }
}
