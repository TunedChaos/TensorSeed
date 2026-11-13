# Tensorseed Desktop

A Tauri 2 app over the `tensorseed-app` library: browse and verify
Tensorseed catalogs natively, and (in later 0.3.0 milestones) pull and seed
with the same config, job state, and seeder as the `tensorseed` CLI. See
`docs/plans/0.3.0-desktop.md`.

Status: M3.2 shell, early. The window lists the followed catalogs from the
shared config and shows one catalog's entries. Syncing runs the same chain
check as `tensorseed list` (`strict`, `warn`, or `off`), and every entry
signature is checked in Rust against the pinned publisher key. Pull, seed,
tray, settings, and the updater come later.

## Layout

- `src/`: Svelte 5 front end. Shared components, types, and the theme come
  from `ui/` (`"@tensorseed/ui": "link:../ui"`).
- `src-tauri/`: the Rust app. It is **not** a member of the root Cargo
  workspace (it has its own `[workspace]`, `Cargo.lock`, and `target/`), so
  CLI, explorer, and release builds never link Tauri or WebKitGTK.
- `src-tauri/src/commands.rs`: the only API the webview has
  (`catalogs_list`, `entries_list`). `build.rs` declares them and
  `capabilities/default.json` allows exactly those plus event listening.
- `src-tauri/src/platform.rs`: platform specific wiring behind
  `cfg(target_os = ...)` with portable defaults, so macOS can be added
  later. 0.3.0 release targets are Linux and Windows only.

Name "Tensorseed Desktop", identifier `dev.tensorseed.desktop`, and the
`tensorseed://` deep link scheme. No telemetry: the only network requests
are the catalog, mirror list, and chain check requests a sync makes. The
CSP allows no remote script, style, or font origins.

## Build (Linux)

Use pnpm, not npm. Needs Rust 1.90 or newer for Tauri 2.12 (the CLI crates
keep their own minimum), Node 20 or newer, pnpm, and the WebKitGTK 4.1
development packages (`libwebkit2gtk-4.1-dev`, `libgtk-3-dev`,
`librsvg2-dev`, `libayatana-appindicator3-dev`, `build-essential`, `curl`,
`file`, `libssl-dev`, `libxdo-dev` on Debian and Ubuntu).

```fish
pnpm --dir desktop install
pnpm --dir desktop check            # svelte-check, front end and ui/
pnpm --dir desktop tauri build      # release binary plus .deb and AppImage
```

`pnpm --dir desktop tauri dev` runs against the Vite dev server. The app
reads the same config as the CLI: `TENSORSEED_CONFIG`, else the default
path. Set `RUST_LOG=info` to see sync and chain check logs.

Rust checks run from `desktop/src-tauri`:

```fish
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

## Not done yet

- Updater: deliberately absent. `tauri-plugin-updater` is added in M3.6
  once the release owner has generated the updater key on the release
  machine and its public key is committed (plan section 5, Updater). No
  key is generated or stored here.
- Packaging: only the local `.deb` and AppImage `tauri build` makes by
  default. Flatpak, the MSI build method, `.rpm`, signing, and bundling
  the CLI binary are M3.6 decisions. The generated `.desktop` file needs
  `%u` in `Exec` for installed packages to pass `tensorseed://` links; the
  app registers the scheme itself at runtime for now.

Copyright Tuned Chaos. AGPL-3.0-or-later.
