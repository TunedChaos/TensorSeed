// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

fn main() {
    // Declare the app commands so each one needs an explicit permission in
    // capabilities/ (nothing is reachable from the webview by default).
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&["catalogs_list", "entries_list"]),
    ))
    .expect("failed to run the Tauri build script");
}
