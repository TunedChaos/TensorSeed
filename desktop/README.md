# Tensorseed Desktop

A Tauri 2 app over the `tensorseed-app` library: browse and verify
Tensorseed catalogs natively, and (in later 0.3.0 milestones) pull and seed
with the same config, job state, and seeder as the `tensorseed` CLI. See
`docs/plans/0.3.0-desktop.md`.

Status: M3.2 shell. The window has four views: the followed catalogs and
their entries, an entry page, the local copies on this machine, and the
torrent settings. A tray icon shows what is running. Pull runs the CLI
for now; verify and seed run in process. The updater is not wired.

## What it does

- **Catalogs.** Lists the followed catalogs from the shared config and one
  catalog's entries. Syncing runs the same chain check as `tensorseed
  list` (`strict`, `warn`, or `off`), and every entry signature is checked
  in Rust against the pinned publisher key.
- **Entry pages.** One page per model or dataset: manifest (commit,
  upstream, size, publish time, webseeds, torrent ids), signature and
  chain anchor status, license class and the commercial use mark, the
  file list with sizes, formats, and hashes, and what this machine has
  (pulled revision, latest or not, signed pull, torrent). If the catalog
  cannot be reached the page falls back to the manifest stored with the
  local copy and says so. Actions:
  - **Pull** runs `tensorseed pull` with the same config and streams its
    output into the page and the status bar. The CLI is found through
    `TENSORSEED_CLI`, next to the Desktop binary, `PATH`, `/usr/bin`,
    `/usr/local/bin`, `~/.local/bin`, then `~/.cargo/bin`. An in process
    pull with progress events is M3.3.
  - **Verify** checks a local model copy natively against its signed
    manifest (datasets go through `tensorseed verify` for now).
  - **Seed** starts the in process torrent engine on the pulled copy with
    the `[torrent]` settings, including the caps, schedule, and stop
    rules. One seeder per machine: if `tensorseed serve` answers on its
    health address, Desktop shows its status and refuses to start a second
    seeder.
- **Settings.** Every `[torrent]` option: speed caps, per torrent caps,
  peer and connection limits, DHT, LSD, uTP, SOCKS5 proxy, bind interface,
  ratio and time stop rules and their action, the scheduled
  (`alt_speed`) limits with their day and time windows, swarm on pull, and
  torrent creation. Inputs are validated as you type and again with the
  same loader the CLI uses before anything is written. Saving edits only
  the `[torrent]` table of the CLI's config file with `toml_edit`:
  comments, other sections, and keys Desktop does not know about stay as
  they are, and keys left at their default are not added. The file is
  written to a temporary file and renamed into place. Values set through
  `TENSORSEED_TORRENT__*` variables are flagged, since they win over the
  file. A running seeder picks up the new settings.
- **Tray.** Show or hide the window, a live status line (seeding and
  downloading counts, paused torrents, upload and download rates, and
  whether `serve` is running), pause all and resume all, and quit (stops
  the seeder first). Closing the window while something is seeding or
  pulling hides it to the tray; otherwise closing quits. On Linux the tray
  needs a StatusNotifierItem host (most desktops have one; GNOME needs the
  AppIndicator extension). Without one there is no icon, and the window's
  status bar has the same controls.

## Deep links

Desktop registers the `tensorseed` URL scheme. Links only navigate; they
never start a pull, seed, or any other action.

| Link | Opens |
| --- | --- |
| `tensorseed://` or `tensorseed://catalogs` | the catalog list |
| `tensorseed://settings` | the settings view |
| `tensorseed://entry/<org>/<name>` | a model entry page |
| `tensorseed://entry/model/<org>/<name>` | the same, type spelled out |
| `tensorseed://entry/dataset/<org>/<name>` | a dataset entry page |

Entry links take two optional query parameters: `revision=<40 hex
commit>` and `catalog=<followed catalog name>`. Other parameters are
ignored. The scheme name is case insensitive; org and name follow the Hugging
Face repo id rules, at most 2048 characters, and percent encoding is
rejected. A link that does not parse opens the window with an error
instead of a page. Parsing lives in `tensorseed_app::links`, shared with
the CLI.

Only one Desktop runs at a time. Opening a link (or running `tensorseed
gui`) while it is running hands the link to the running window, shows it
(also when it is hidden in the tray), and the second process exits.

On Linux the app also writes
`~/.local/share/applications/tensorseed-desktop-handler.desktop` at start
and makes it the `x-scheme-handler/tensorseed` default, so a binary run
outside a package still gets links. Packages ship
`src-tauri/linux/tensorseed-desktop.desktop` (`Exec=... %u`,
`MimeType=x-scheme-handler/tensorseed;`). On Windows the NSIS and MSI
installers register the scheme and their uninstallers remove it. A binary
run outside an installer registers it for the current user at start;
an installed one leaves the installer's registration alone, so nothing
is left behind after uninstalling.

## `tensorseed gui`

The CLI starts Desktop without depending on Tauri:

```fish
tensorseed gui                          # open the window
tensorseed gui Qwen/Qwen2.5-1.5B-Instruct-GGUF
tensorseed gui org/data --type dataset
tensorseed gui tensorseed://settings
tensorseed gui --print-command          # show what would run
```

It looks for Desktop in this order: `TENSORSEED_DESKTOP`, next to the
`tensorseed` binary, `tensorseed-desktop` on `PATH`, then the standard
install places (Linux: `/usr/bin`, `/usr/local/bin`, `~/.local/bin`, an
AppImage named `tensorseed*.AppImage` in `~/Applications` or
`~/.local/bin`, and the `dev.tensorseed.desktop` Flatpak; Windows:
`%LOCALAPPDATA%`, `%ProgramFiles%`, and `%ProgramFiles(x86)%` under
`Tensorseed Desktop`; macOS: `Tensorseed Desktop.app` in `/Applications`
or `~/Applications`). Desktop gets the CLI's config path through
`TENSORSEED_CONFIG`. If it is not installed the command lists where it
looked and exits 1.

## Layout

- `src/`: Svelte 5 front end. Shared components, types, and the theme come
  from `ui/` (`"@tensorseed/ui": "link:../ui"`).
- `src-tauri/`: the Rust app. It is **not** a member of the root Cargo
  workspace (it has its own `[workspace]`, `Cargo.lock`, and `target/`), so
  CLI, explorer, and release builds never link Tauri or WebKitGTK.
- `src-tauri/src/commands.rs`: the only API the webview has
  (`catalogs_list`, `entries_list`, `entry_show`, `local_list`,
  `settings_get`, `settings_check`, `settings_set`, `pull_start`,
  `verify_start`, `seed_start`, `seeds_pause`, `seeds_resume`, `status`,
  `initial_route`). `build.rs` declares them and
  `capabilities/default.json` allows exactly those plus event listening.
- `src-tauri/src/seeder.rs` (in process engine), `jobs.rs` (pull and
  verify jobs), `status.rs` (what the tray and status bar show),
  `tray.rs`.
- `src-tauri/src/platform.rs`: platform specific wiring behind
  `cfg(target_os = ...)` with portable defaults, so macOS can be added
  later. 0.3.0 release targets are Linux and Windows only.

Name "Tensorseed Desktop", identifier `dev.tensorseed.desktop`, and the
`tensorseed://` deep link scheme. No telemetry: the only network requests
are the catalog, mirror list, and chain check requests a sync makes, the
pulls and swarm traffic you start, and a loopback check of `serve`'s
health address. The
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

## Build (Windows)

Needs the Visual Studio 2022 Build Tools (the "Desktop development with
C++" workload and a Windows 11 SDK), Rust (MSVC toolchain), Node 20 or
newer, and pnpm. WebView2 ships with Windows 11. Tauri downloads WiX and
NSIS on the first bundle build.

```powershell
pnpm --dir desktop install --frozen-lockfile
pnpm --dir desktop tauri build --bundles msi,nsis
```

This makes `Tensorseed Desktop_<version>_x64_en-US.msi` and
`Tensorseed Desktop_<version>_x64-setup.exe` under
`desktop/src-tauri/target/release/bundle/`. Both are unsigned, so
SmartScreen warns on first run.

- **NSIS** (`-setup.exe`) installs for the current user into
  `%LOCALAPPDATA%\Tensorseed Desktop` without elevation. `/S` installs
  silently.
- **MSI** installs for the machine into `%ProgramFiles%\Tensorseed
  Desktop` and needs elevation. MSI versions must be numeric, so
  `bundle.windows.wix.version` in `tauri.conf.json` holds the numeric form
  of the app version (`0.3.0.0` for `0.3.0-dev`); keep it in step when the
  version changes. The WiX upgrade code is pinned there too and must
  never change.
- Both installers share the
  `HKCU\Software\Tuned Chaos\Tensorseed Desktop` key, which the NSIS
  uninstaller leaves behind. An MSI installed after an NSIS install and
  uninstall therefore defaults to the old NSIS folder. Installing one
  kind at a time avoids surprises; M3.6 picks which to ship.
- Config and data use the CLI's Windows paths: the config is
  `%APPDATA%\tensorseed\tensorseed.toml`, and state, the catalog cache,
  and job records are under `%LOCALAPPDATA%\tensorseed`. The WebView2
  profile is `%LOCALAPPDATA%\dev.tensorseed.desktop`; uninstalling
  silently keeps it.
- Settings edits keep the file's CRLF line endings.
- Pull runs `tensorseed.exe` with no console window. Desktop finds it
  through `TENSORSEED_CLI`, next to `tensorseed-desktop.exe`, `PATH`, or
  `%USERPROFILE%\.cargo\bin`.
- The first time Desktop seeds, Windows asks whether to allow
  `tensorseed-desktop.exe` through the firewall, and blocks incoming
  connections until it is allowed, so Desktop only seeds to peers it
  dials. An installer firewall rule is an
  M3.6 decision.
- Bind interface is not supported on Windows, and the settings view
  disables it.

## Not done yet

- Updater: deliberately absent. `tauri-plugin-updater` is added in M3.6
  once the release owner has generated the updater key on the release
  machine and its public key is committed (plan section 5, Updater). No
  key is generated or stored here.
- Pull runs the CLI as a subprocess, so there is no progress bar, pause,
  or cancel yet, and the CLI has to be installed. The in process pull is
  M3.3. Dataset verify also goes through the CLI.
- Desktop does not attach to a running `serve`: it only reads its health
  endpoint. Controlling it needs the local control endpoint from M3.4.
- Packaging: only the local `.deb` and AppImage `tauri build` makes by
  default. Flatpak, the MSI build method, signing, and bundling the CLI
  binary are M3.6 decisions.
- Windows: the NSIS and MSI builds, deep links, single instance
  forwarding, close to tray, the tray icon and its status, pull, verify,
  seed, settings, `tensorseed gui`, and uninstalling have been run on
  Windows 11. The tray menu itself has only been checked on Linux so
  far. macOS is not built.

Copyright Tuned Chaos. AGPL-3.0-or-later.
