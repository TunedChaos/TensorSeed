// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Tensorseed Desktop: a Tauri 2 shell over `tensorseed-app`. The webview
//! only gets data through the commands in [`commands`]; all catalog access
//! and verification run natively here. See `docs/plans/0.3.0-desktop.md`.
//!
//! No telemetry: the only network requests are the catalog, mirror, and
//! chain check requests `tensorseed-app` makes when the user syncs.

pub mod commands;
mod platform;

use tauri::Emitter;

/// Event the window receives for each `tensorseed://` link.
pub const DEEP_LINK_EVENT: &str = "deep-link://open";

/// Build and run the app.
///
/// # Panics
///
/// Panics when the Tauri runtime cannot start (no display, missing webview).
pub fn run() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .try_init();

    let builder = platform::single_instance(tauri::Builder::default());
    builder
        .plugin(tauri_plugin_deep_link::init())
        .invoke_handler(tauri::generate_handler![
            commands::catalogs_list,
            commands::entries_list
        ])
        .setup(|app| {
            platform::register_deep_links(app);
            let handle = app.handle().clone();
            use tauri_plugin_deep_link::DeepLinkExt;
            app.deep_link().on_open_url(move |event| {
                for url in event.urls() {
                    forward_deep_link(&handle, url.as_str());
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Tensorseed Desktop");
}

/// Pass a `tensorseed://` URL to the window. Anything else is ignored.
fn forward_deep_link(app: &tauri::AppHandle, url: &str) {
    if url.starts_with("tensorseed://") {
        let _ = app.emit(DEEP_LINK_EVENT, url.to_string());
    }
}
