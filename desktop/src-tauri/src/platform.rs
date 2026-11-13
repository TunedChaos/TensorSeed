// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Platform specific wiring. Every target gets a portable default so macOS
//! (designed for, not released in 0.3.0) can be added without restructuring.

use tauri::Runtime;

/// Linux and Windows: a `tensorseed://` link launches a second process, so
/// forward its URL (and focus) to the running window and exit.
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub fn single_instance<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    use tauri::Manager;
    builder.plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.unminimize();
            let _ = w.set_focus();
        }
    }))
}

/// Other targets (macOS later): the OS hands links to the running app.
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn single_instance<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
}

/// Linux and Windows: register the scheme at runtime too, so development
/// builds and the AppImage (which has no installed .desktop file) open
/// links. Installed packages also register it through the bundle.
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub fn register_deep_links<R: Runtime>(app: &tauri::App<R>) {
    use tauri_plugin_deep_link::DeepLinkExt;
    if let Err(e) = app.deep_link().register_all() {
        tracing::warn!("could not register the tensorseed:// scheme: {e}");
    }
}

/// Other targets: the scheme comes from the app bundle's Info.plist.
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn register_deep_links<R: Runtime>(_app: &tauri::App<R>) {}
