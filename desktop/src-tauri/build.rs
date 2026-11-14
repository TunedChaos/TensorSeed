// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

fn main() {
    // Declare the app commands so each one needs an explicit permission in
    // capabilities/ (nothing is reachable from the webview by default).
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "catalogs_list",
            "entries_list",
            "entry_show",
            "local_list",
            "settings_get",
            "settings_check",
            "settings_set",
            "pull_start",
            "verify_start",
            "seed_start",
            "seeds_pause",
            "seeds_resume",
            "status",
            "initial_route",
        ]),
    ))
    .expect("failed to run the Tauri build script");
}
