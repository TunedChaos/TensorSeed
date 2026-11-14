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
- Both installers write to the
  `HKCU\Software\Tuned Chaos\Tensorseed Desktop` key. The NSIS
  uninstaller (`src-tauri/windows/nsis-hooks.nsh`) removes its own values
  and then the key once it is empty, so an MSI installed afterwards uses
  its default folder and an installed MSI keeps its values. Updates and
  reinstalls keep the key. Installing one kind at a time still avoids
  surprises; M3.6 picks which to ship.
- Config and data use the CLI's Windows paths: the config is
  `%APPDATA%\tensorseed\tensorseed.toml`, and state, the catalog cache,
  and job records are under `%LOCALAPPDATA%\tensorseed`. The WebView2
  profile is `%LOCALAPPDATA%\dev.tensorseed.desktop`. The NSIS
  uninstaller deletes it when "Delete the application data" is ticked
  and keeps it otherwise, including silent uninstalls and updates.
  Neither uninstaller touches the config, the data, pulled copies, or
  mirrors.
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

## Flatpak

`desktop/flatpak/` builds Desktop as the Flatpak `dev.tensorseed.desktop`
from this checkout, on the GNOME 51 runtime (WebKitGTK 4.1) with the
Rust and Node SDK extensions. Every Cargo crate and npm package comes
from `cargo-sources.json` and `node-sources.json`, so the build runs
offline. The tray library (libayatana-appindicator) is built from
Flathub's shared modules, a git submodule.

```fish
git submodule update --init desktop/flatpak/shared-modules
flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install --user flathub org.gnome.Platform//51 org.gnome.Sdk//51 \
    org.freedesktop.Sdk.Extension.rust-stable//26.08 \
    org.freedesktop.Sdk.Extension.node24//26.08
flatpak-builder --user --install --force-clean build-dir \
    desktop/flatpak/dev.tensorseed.desktop.yml
flatpak run dev.tensorseed.desktop
# optional single file bundle:
flatpak-builder --repo=repo --force-clean build-dir desktop/flatpak/dev.tensorseed.desktop.yml
flatpak build-bundle repo tensorseed-desktop.flatpak dev.tensorseed.desktop
```

After changing `Cargo.lock`, `desktop/src-tauri/Cargo.lock`, or
`desktop/pnpm-lock.yaml`, run `desktop/flatpak/update-sources.sh` (it
needs a flatpak-builder-tools checkout; see the script) and commit the
two JSON files. The CLI and Desktop workspaces have separate lockfiles,
so the script generates both and merges them. pnpm itself comes from its
npm tarball, pinned in the manifest.

What the Flatpak contains:

- `/app/bin/tensorseed-desktop` and the `tensorseed` CLI next to it.
  Desktop looks for the CLI next to its own binary before `PATH`, so
  pulls use the bundled CLI and nothing has to be installed on the host.
  `TENSORSEED_CLI` still overrides it. Run the bundled CLI with
  `flatpak run --command=tensorseed dev.tensorseed.desktop ...`.
- `dev.tensorseed.desktop.desktop` (`Exec=tensorseed-desktop %u`,
  `MimeType=x-scheme-handler/tensorseed;`), so `xdg-open
  tensorseed://entry/org/name` starts the Flatpak, and a running one gets
  the link through single instance forwarding. Inside the sandbox Desktop
  does not write its own handler file.
- AppStream metadata (`dev.tensorseed.desktop.metainfo.xml`, passes
  `appstreamcli validate`) and icons at 32, 128, 256, and 512 pixels.

Permissions (`finish-args`), each for one reason:

| Permission | Why |
| --- | --- |
| `--share=network` | Catalog sync, Hugging Face, trackers, the DHT, peers. |
| `--socket=wayland`, `--socket=fallback-x11`, `--share=ipc` | The window. X11 only when there is no Wayland; IPC is the X11 shared memory it needs. |
| `--device=dri` | WebKit GPU rendering. |
| `--talk-name=org.kde.StatusNotifierWatcher` | The tray icon. |
| `--filesystem=xdg-config/tensorseed:create` | The CLI's config, keys, `mirrors.toml`, and `hf_token`. |
| `--filesystem=xdg-data/tensorseed:create` | State, catalog cache, staging, seed data, publish trees, and mirrors (`~/.local/share/tensorseed`). |
| `--filesystem=xdg-download/tensorseed:create` | Where pulls go by default inside the sandbox. |

No home directory access, no host filesystem, and no other bus names.
The single instance name (`dev.tensorseed.desktop.SingleInstance`) is
under the app id, which Flatpak allows without a permission. There is no
cache permission because Tensorseed keeps nothing under `~/.cache`; the
Hugging Face token falls back to `~/.cache/huggingface/token` on the
host, which the sandbox cannot see, so set the token with Tensorseed's
own `hf_token` file instead.

Config and data are shared with a host CLI. Flatpak normally points
`XDG_CONFIG_HOME` at `~/.var/app/dev.tensorseed.desktop/config`; inside a
Flatpak (`FLATPAK_ID` set) Tensorseed uses the host's directory instead
(`HOST_XDG_CONFIG_HOME`, else `~/.config`), which the permission above
exposes at the same path. Data paths are `~/.local/share/tensorseed` on
every Linux build already. So Desktop in the Flatpak and a `tensorseed`
installed on the host read and write the same files.

Downloads: the CLI's default `general.download_dir` is `.`, the working
directory, which the sandbox usually cannot write. Inside a Flatpak the
default becomes `tensorseed` under the XDG download directory
(`~/Downloads/tensorseed` on most systems), which `xdg-download/tensorseed`
exposes. A narrow folder under Downloads is the smallest grant that works
without asking, and the file chooser portal does not fit: pulls and
seeding run in the background and need the same folder again on every
start. A `download_dir` set in the config wins; when it is outside the
granted folders, grant it once:

```fish
flatpak override --user --filesystem=/srv/models dev.tensorseed.desktop
```

Paths in the config such as `state_db` or `[file] local_dir` outside
`~/.local/share/tensorseed` need the same override.

Inside the sandbox the tray icon image is written to
`$XDG_RUNTIME_DIR/app/dev.tensorseed.desktop/tray-icon`, the runtime
folder Flatpak shares with the host, because the tray host reads the icon
by path.

Not built in CI: a Flatpak build takes about 25 minutes and needs the
runtime and SDK downloads, so it runs by hand for now. Nothing is
published.

What a Flathub submission would still need (nothing has been submitted):

- A different app id, or an exception from the Flathub reviewers:
  `flatpak-builder-lint` rejects ids that end in `.desktop`
  (`appid-ends-with-lowercase-desktop`). Renaming (for example to
  `dev.tensorseed.Desktop`) changes the Tauri identifier too, so the
  WebView profile folder and the single instance name move with it.
- A manifest in a Flathub repo for the app id whose source is
  a `type: git` entry with a release tag and commit, not `type: dir`, with
  `shared-modules` as a submodule there.
- The `tensorseed.dev` domain verified for the app id (Flathub checks
  `https://tensorseed.dev/.well-known/org.flathub.VerifiedApps.txt`).
- Screenshots in the metainfo (hosted at stable URLs) and a `<release>`
  per version with notes. With those and the id settled,
  `flatpak-builder-lint` has no other findings for the manifest or the
  built repo.
- A review of the permissions above, in particular the downloads folder.

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
- Packaging: the local `.deb` and AppImage `tauri build` makes by
  default, the Windows MSI and NSIS installers, and the Flatpak (built by
  hand, not published). Signing, the MSI build method, and bundling the
  CLI in the other packages are M3.6 decisions.
- Windows: the NSIS and MSI builds, deep links, single instance
  forwarding, close to tray, the tray icon and its status, pull, verify,
  seed, settings, `tensorseed gui`, and uninstalling have been run on
  Windows 11. The tray menu itself has only been checked on Linux so
  far. macOS is not built.

Copyright Tuned Chaos. AGPL-3.0-or-later.
