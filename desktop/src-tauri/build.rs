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
            "pull_pause",
            "pull_cancel",
            "jobs_list",
            "job_resume",
            "job_discard",
            "copy_open",
            "copy_delete",
            "all_pause",
            "all_resume",
            "verify_start",
            "seed_start",
            "seeds_pause",
            "seeds_resume",
            "seed_set_paused",
            "seed_stop",
            "serve_set_paused",
            "status",
            "initial_route",
            "update_info",
            "update_check",
            "update_install",
            "update_set_check_on_start",
        ]),
    ))
    .expect("failed to run the Tauri build script");
}
