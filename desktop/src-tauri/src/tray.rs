// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

//! The system tray: show or hide the window, torrent activity, pause and
//! resume all, and quit. The status line refreshes every two seconds and
//! the same status goes to the window as [`crate::status::STATUS_EVENT`].

use std::time::Duration;

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Emitter, Manager, Runtime};

use crate::jobs::Jobs;
use crate::seeder::Seeder;
use crate::status::{STATUS_EVENT, gather};

/// Tray icon id.
const TRAY_ID: &str = "main";
/// Status refresh period.
const TICK: Duration = Duration::from_secs(2);

/// Show the main window and bring it to the front.
pub fn show_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn toggle_window<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window("main") {
        if w.is_visible().unwrap_or(false) {
            let _ = w.hide();
        } else {
            show_window(app);
        }
    }
}

fn toggle_label<R: Runtime>(app: &AppHandle<R>) -> &'static str {
    match app.get_webview_window("main") {
        Some(w) if w.is_visible().unwrap_or(false) => "Hide window",
        _ => "Show window",
    }
}

/// Quit: stop the seeder (files are never touched), then exit.
pub fn quit<R: Runtime>(app: &AppHandle<R>) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        app.state::<Seeder>().stop().await;
        app.exit(0);
    });
}

/// Build the tray and start its refresh loop.
///
/// # Errors
///
/// Fails when the platform has no tray or the menu cannot be built.
pub fn build<R: Runtime>(app: &tauri::App<R>) -> tauri::Result<()> {
    let toggle = MenuItem::with_id(app, "toggle", "Hide window", true, None::<&str>)?;
    let status = MenuItem::with_id(
        app,
        "status",
        "Seeding 0, downloading 0",
        false,
        None::<&str>,
    )?;
    let pause = MenuItem::with_id(app, "pause", "Pause all", false, None::<&str>)?;
    let resume = MenuItem::with_id(app, "resume", "Resume all", false, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "Quit Tensorseed Desktop", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &toggle,
            &PredefinedMenuItem::separator(app)?,
            &status,
            &PredefinedMenuItem::separator(app)?,
            &pause,
            &resume,
            &PredefinedMenuItem::separator(app)?,
            &quit_item,
        ],
    )?;
    let mut tb = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Tensorseed Desktop")
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, ev| match ev.id().as_ref() {
            "toggle" => toggle_window(app),
            "pause" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = app.state::<Seeder>().pause_all().await {
                        tracing::warn!("pause all: {e:#}");
                    }
                });
            }
            "resume" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Err(e) = app.state::<Seeder>().resume_all().await {
                        tracing::warn!("resume all: {e:#}");
                    }
                });
            }
            "quit" => quit(app),
            _ => {}
        });
    if let Some(icon) = app.default_window_icon() {
        tb = tb.icon(icon.clone());
    }
    if let Some(dir) = crate::platform::tray_icon_dir() {
        tb = tb.temp_dir_path(dir);
    }
    let tray = tb.build(app)?;

    let handle = app.handle().clone();
    tauri::async_runtime::spawn(async move {
        let mut last = String::new();
        loop {
            let cfg = crate::commands::load_config().ok().map(|(_, c)| c);
            let st = gather(
                &handle.state::<Seeder>(),
                &handle.state::<Jobs>(),
                cfg.as_ref(),
            )
            .await;
            let line = st.summary();
            if line != last {
                let _ = status.set_text(&line);
                let _ = tray.set_tooltip(Some(format!("Tensorseed Desktop: {line}")));
                last = line;
            }
            let _ = pause.set_enabled(st.desktop.seeding > 0);
            let _ = resume.set_enabled(st.desktop.paused > 0);
            let _ = toggle.set_text(toggle_label(&handle));
            let _ = handle.emit(STATUS_EVENT, &st);
            tokio::time::sleep(TICK).await;
        }
    });
    Ok(())
}
