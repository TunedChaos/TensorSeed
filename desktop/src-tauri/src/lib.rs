// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! Tensorseed Desktop: a Tauri 2 shell over `tensorseed-app`. The webview
//! only gets data through the commands in [`commands`]; catalog access,
//! verification, settings, and seeding run natively here. See
//! `docs/plans/0.3.0-desktop.md`.
//!
//! No telemetry: the only network requests are the catalog, mirror, and
//! chain check requests `tensorseed-app` makes when the user syncs, the
//! BitTorrent swarm when the user seeds, and a probe of the local
//! `tensorseed serve` health endpoint (`serve.health_listen`).

pub mod commands;
pub mod copies;
pub mod jobs;
mod platform;
pub mod seeder;
pub mod status;
mod tray;

use std::sync::Mutex;

use serde::Serialize;
use tauri::{Emitter, Manager, RunEvent, WindowEvent};
use tensorseed_app::links::{self, Route};

/// Event the window receives for each `tensorseed://` link (a
/// [`DeepLink`]).
pub const DEEP_LINK_EVENT: &str = "deep-link://open";

/// A `tensorseed://` link and where it goes (`DeepLink` in
/// `src/lib/native.ts`). Links only navigate; they never start a pull,
/// verify, or seed.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DeepLink {
    /// The link as received.
    pub url: String,
    /// Where it goes, when it parses.
    pub route: Option<Route>,
    /// Why it does not parse.
    pub error: Option<String>,
}

impl DeepLink {
    /// Parse `url`.
    pub fn from_url(url: &str) -> Self {
        match links::parse(url) {
            Ok(r) => Self {
                url: url.to_string(),
                route: Some(r),
                error: None,
            },
            Err(e) => Self {
                url: url.chars().take(200).collect(),
                route: None,
                error: Some(format!("{e:#}")),
            },
        }
    }
}

/// The link Desktop was started with, taken once by the window.
#[derive(Default)]
pub struct InitialLink(pub Mutex<Option<DeepLink>>);

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

    let initial = links::find_in_args(std::env::args()).map(|u| DeepLink::from_url(&u));

    let builder = platform::single_instance(tauri::Builder::default());
    let app = builder
        .plugin(tauri_plugin_deep_link::init())
        .manage(seeder::Seeder::default())
        .manage(jobs::Jobs::default())
        .manage(InitialLink(Mutex::new(initial)))
        .invoke_handler(tauri::generate_handler![
            commands::catalogs_list,
            commands::entries_list,
            commands::entry_show,
            commands::local_list,
            commands::settings_get,
            commands::settings_check,
            commands::settings_set,
            commands::pull_start,
            commands::verify_start,
            commands::seed_start,
            commands::seeds_pause,
            commands::seeds_resume,
            commands::status,
            commands::initial_route,
        ])
        .setup(|app| {
            platform::register_deep_links(app);
            use tauri_plugin_deep_link::DeepLinkExt;
            // macOS hands the start link to the plugin, not argv.
            if let Ok(Some(urls)) = app.deep_link().get_current()
                && let Some(u) = urls.first()
                && let Ok(mut l) = app.state::<InitialLink>().0.lock()
                && l.is_none()
            {
                *l = Some(DeepLink::from_url(u.as_str()));
            }
            let handle = app.handle().clone();
            app.deep_link().on_open_url(move |event| {
                for url in event.urls() {
                    forward_deep_link(&handle, url.as_str());
                }
            });
            if let Err(e) = tray::build(app) {
                tracing::warn!("no system tray: {e}");
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // While seeding, closing the window keeps the app in the tray.
            if let WindowEvent::CloseRequested { api, .. } = event
                && window.label() == "main"
                && window.state::<seeder::Seeder>().active_hint()
            {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building Tensorseed Desktop");
    app.run(|app, event| {
        if let RunEvent::Exit = event {
            tauri::async_runtime::block_on(app.state::<seeder::Seeder>().stop());
        }
    });
}

/// Pass a `tensorseed://` link to the window and bring it forward.
/// Anything else is ignored.
pub(crate) fn forward_deep_link<R: tauri::Runtime>(app: &tauri::AppHandle<R>, url: &str) {
    if url
        .get(..links::SCHEME.len() + 3)
        .is_some_and(|p| p.eq_ignore_ascii_case("tensorseed://"))
    {
        tray::show_window(app);
        let _ = app.emit(DEEP_LINK_EVENT, DeepLink::from_url(url));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deep_link_payloads() {
        let ok = serde_json::to_value(DeepLink::from_url("tensorseed://entry/org/name")).unwrap();
        assert_eq!(ok["route"]["kind"], "entry");
        assert_eq!(ok["route"]["repoType"], "model");
        assert_eq!(ok["route"]["id"], "org/name");
        assert!(ok["error"].is_null());
        let bad = DeepLink::from_url("tensorseed://entry/../x");
        assert!(bad.route.is_none() && bad.error.is_some());
    }
}
