# Tensorseed Desktop

A Tauri 2 app over the `tensorseed-app` library: browse and verify
Tensorseed catalogs natively, and (in later 0.3.0 milestones) pull and seed
with the same config, job state, and seeder as the `tensorseed` CLI. See
`docs/plans/0.3.0-desktop.md`.

Status: M3.5. The window has six views: the followed catalogs and their
entries, an entry page, downloads, seeding, the library of local copies,
and settings (Hugging Face login, updates, and the torrent options). A tray icon shows what is running. Pull, verify, and
seed run in process. The AppImage, MSI, and NSIS
builds update themselves (see Updates).

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
  - **Pull** runs the same pull pipeline as `tensorseed pull`, in
    process, with the same config: a progress bar (files, bytes, rate,
    files in flight) and the CLI's output lines in the page. **Pause**
    stops the transfers and keeps the partial downloads; **Resume**
    continues from them. **Cancel** deletes the partial downloads; files
    that finished and verified stay. Tick files in the file list and use
    **Pull N files** to download only those.
  - **Verify** checks a local model copy natively (datasets go through
    `tensorseed verify` for now, found through `TENSORSEED_CLI`, next to
    the Desktop binary, `PATH`, `/usr/bin`, `/usr/local/bin`,
    `~/.local/bin`, then `~/.cargo/bin`). A complete signed copy is checked
    against the signed catalog manifest; any other copy is checked against
    its own pull record (`.tensorseed/pull.json`) by default, and
    **Verify against catalog** is offered next to it.
- **Downloads.** Pulls running in the window, with their progress,
  Pause, and Cancel, and every pull in the job database the CLI uses
  (`tensorseed jobs`): a pull started in the CLI shows here and can be
  resumed here, and one paused in Desktop resumes with the same
  `tensorseed pull` command. A pull writes
  `.tensorseed/pull-pending.json` (its options, pinned to the resolved
  commit) until it completes, and holds `.tensorseed/pull.lock` while it
  runs, so the CLI and Desktop never write the same folder at once. A job
  recorded as running that no process holds (a crash or a closed
  terminal) shows as interrupted and can be resumed. **Discard** deletes
  an unfinished pull's partial downloads and forgets it. Quitting Desktop
  pauses its running pulls; closing the window while a pull runs keeps
  the app in the tray.
- **Library.** Pulled copies under `general.download_dir`, with
  **Verify**, **Open folder**, and **Delete** (after a confirmation;
  refused while the copy seeds or a pull writes to it).
- **Copy status.** The library and the entry page label each
  copy as complete, partial (a file selection or a subset manifest),
  unsigned (pulled from a manifest file or URL, or Hugging Face), or at a
  different revision than the catalog entry, with a one line reason
  such as "Partial copy: 1 of 9 files, not from the signed catalog", and
  show the copy's own commit, file count, and size on disk. The label
  describes the copy only: the catalog entry keeps its own signature and
  anchor status. **Complete this copy** pulls the missing files from the
  signed catalog into the same folder, keeping the files already there.
  - **Seed** starts the in process torrent engine on the pulled copy with
    the `[torrent]` settings, including the caps, schedule, and stop
    rules. One seeder per machine: the engine holds the same seeder lock
    as `tensorseed serve` and `tensorseed seed` (`seeder.lock` next to the
    job database) while it runs, so Desktop never starts next to them and
    they refuse to start next to Desktop.
- **Seeding.** What Desktop seeds, with listen address, speed, peers,
  and upload, and Pause, Resume, and Stop (files stay) for each copy;
  stopping the last copy stops the engine and frees the lock. When a
  `tensorseed serve` with the same config runs, Desktop attaches to it
  through its local control endpoint (127.0.0.1 only; the address and
  token are in its owner only `seeder.json`) and shows each revision's
  state, size, upload, and peers, with Pause and Resume per revision and
  for all. A serve under another state folder (a service account) shows
  read only through `serve.health_listen`. Pause all and Resume all in
  the tray and status bar cover an attached serve too.
- **Hugging Face login.** Settings shows who Hub reads run as and where
  the token comes from (the `HF_TOKEN` variable, `huggingface.token_file`,
  the login file, or the Hugging Face CLI token). Sign in checks a pasted
  token with the Hub first and saves it only if accepted, owner only, in
  the file `tensorseed hf login` writes; a rejected token leaves the saved
  login alone. The field is cleared on submit and the token is never
  logged, returned to the window, or shown again. Sign out removes that
  file only, and the card says when a variable or token file still wins.
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
`~/.local/bin`, and the `dev.tensorseed.Tensorseed` Flatpak; Windows:
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
  verify jobs), `copies.rs` (copy status and the pull record check),
  `status.rs` (what the tray and status bar show), `tray.rs`.
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
pnpm --dir desktop build:unsigned   # release binary plus .deb and AppImage, no updater signatures
```

`tauri build` with no flags also writes updater signatures, so it stops
with "A public key has been found, but no private key" unless the updater
key is set (see Releasing an update). `build:unsigned` passes
`--config src-tauri/tauri.unsigned.conf.json`, which turns
`createUpdaterArtifacts` off for that build only; use it for local, test,
and CI builds. Extra arguments pass through, for example
`pnpm --dir desktop build:unsigned --bundles appimage`.

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
pnpm --dir desktop build:unsigned --bundles msi,nsis
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
  of the app version (`0.3.0.0` for `0.3.0`); keep it in step when the
  version changes. The WiX upgrade code is pinned there too and must
  never change.
- **Windows Firewall (opt in, off by default).** The NSIS welcome page
  has a checkbox and the MSI has a "Windows Firewall" page after the
  folder page. Ticked, the installer adds an inbound allow rule for the
  installed `tensorseed-desktop.exe` on all network types ("Tensorseed
  Desktop incoming peers" for the MSI, "Tensorseed Desktop incoming
  peers (per user)" for NSIS, which runs netsh elevated, so Windows asks
  for approval). Uninstalling removes it; updates keep it (the MSI keeps
  the choice in `HKLM\Software\Tuned Chaos\Tensorseed Desktop`,
  `FirewallRule`). Silent installs add no rule unless the MSI gets
  `FIREWALLRULE=1`. Without the rule, Windows asks the first time Desktop
  seeds.
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
- Dataset verify runs `tensorseed.exe` with no console window. Desktop
  finds it through `TENSORSEED_CLI`, next to `tensorseed-desktop.exe`,
  `PATH`, or `%USERPROFILE%\.cargo\bin`. Pulls run in process.
- The first time Desktop seeds, Windows asks whether to allow
  `tensorseed-desktop.exe` through the firewall, and blocks incoming
  connections until it is allowed, so Desktop only seeds to peers it
  dials. An installer firewall rule is an
  M3.6 decision.
- Bind interface is not supported on Windows, and the settings view
  disables it.

## Updates

Desktop checks for a new version by fetching one file,
`https://github.com/TunedChaos/TensorSeed/releases/latest/download/latest.json`.
Nothing else is sent: no identifiers, no usage data, and no telemetry.
The check runs 10 seconds after start (Settings, Updates, "Check for
updates when Desktop starts"; on by default) and whenever you press
"Check for updates". The setting is stored in `desktop.json` in the app
config directory (`~/.config/dev.tensorseed.desktop` on Linux,
`%APPDATA%\dev.tensorseed.desktop` on Windows), not in
`tensorseed.toml`.

When a newer version is listed, Desktop shows its version and release
notes and waits. Nothing downloads until you press "Install and
restart". The installer is then downloaded, its signature is checked
against the updater public key compiled into the app (id
`B81047C9B22BB400`, also in `keys/tensorseed-desktop-updater.pub`), the
signed version must match the announced one (`requireSignedVersion`), the
seeder is stopped, and the update is installed:

- **AppImage:** the AppImage file is replaced in place and Desktop
  restarts.
- **NSIS and MSI:** the installer runs in passive mode (a progress window,
  no questions) and Desktop restarts. The MSI asks for elevation, as on a
  first install.
- **.deb and .rpm:** no self update. Settings says to update through your
  package manager.
- **Flatpak:** no self update. Settings says to update through Flatpak or
  Flathub (`flatpak update`).
- **Development builds** (`tauri dev`, plain `cargo build`) never check.

A failed signature check, a version mismatch, or a missing entry for this
build's installer type leaves the installed copy untouched and shows the
error.

## Releasing an update

The full release flow (tag, CI, attestations, checksums, signing, and
`gh release create`) is in [docs/RELEASING.md](../docs/RELEASING.md).
CI builds the unsigned AppImage, `.deb`, `.rpm`, Flatpak, and Windows
installers. Updater signatures and `latest.json` are made on your
signing machine, which holds the updater private key and its password.
They are never made in CI, and the key never goes to a build host. CI
and test builds use `build:unsigned` and need no key.

Sign the exact files from the CI artifact; rebuilding them changes the
bytes and breaks the signature. Spaces in those file names are already
dots. After `SHA256SUMS` is signed (see docs/RELEASING.md):

```fish
set ver 0.3.2
read -s -P 'updater key password: ' -x TAURI_SIGNING_PRIVATE_KEY_PASSWORD
for f in     Tensorseed.Desktop_{$ver}_amd64.AppImage     Tensorseed.Desktop_{$ver}_x64-setup.exe     Tensorseed.Desktop_{$ver}_x64_en-US.msi
    pnpm --dir desktop tauri signer sign         -f ~/.tauri/tensorseed-desktop-updater.key --app-version $ver $f
end
set -e TAURI_SIGNING_PRIVATE_KEY_PASSWORD
```

`scripts/make-latest-json` reads the `.sig` next to each installer,
checks the signed version and file name, verifies each signature against
the public key in `tauri.conf.json` with `minisign` (`--verify`), and
writes the feed:

```fish
scripts/make-latest-json --version 0.3.2 --notes-file notes.md --verify     --appimage Tensorseed.Desktop_0.3.2_amd64.AppImage     --nsis Tensorseed.Desktop_0.3.2_x64-setup.exe     --msi Tensorseed.Desktop_0.3.2_x64_en-US.msi     --out latest.json
```

Download URLs default to the `v<version>` release on GitHub, with
spaces in file names turned into dots as GitHub does on upload
(`--base-url` changes the prefix). The entries are
`linux-x86_64-appimage`, `windows-x86_64-nsis`, and
`windows-x86_64-msi`, so each install type gets its own installer.
Windows ARM64 installers ship in the release and in `SHA256SUMS`, but
this feed does not list them yet.

Publish as in [docs/RELEASING.md](../docs/RELEASING.md): upload the
installers, their `.sig` files, and `SHA256SUMS` and `SHA256SUMS.minisig`
first, then `latest.json` last. Desktop follows `releases/latest`, which
GitHub points at the newest release that is not a draft or a pre-release,
so a pre-release never reaches installed copies.

Updater signatures only cover the in app update. The release key and
`SHA256SUMS.minisig` still cover every download (see `RELEASE-KEY.md`).

## Inference serving

Desktop does not serve inference yet; serving is CLI only
(`tensorseed serve-inference`, and `tensorseed sign-responses` for the
signing proxy). Neither Desktop nor the CLI bundles an inference engine.
The engines are large, need builds that match your GPU, and update
often, so you install the one you need yourself:

- **llama.cpp** (`llama-server`) for GGUF models: `winget install
  llama.cpp` on Windows, `brew install llama.cpp` on macOS or Linux, your
  distribution's package, or the
  [llama.cpp releases page](https://github.com/ggml-org/llama.cpp/releases).
- **vLLM** (`vllm`) for safetensors models: `pip install vllm` in a
  Python virtual environment, on Linux with a supported GPU (on Windows,
  inside WSL 2).

The CLI finds the engine on `PATH` or at `inference.llama_server` or
`inference.vllm` in the config Desktop shares with it, for example
`llama_server = "C:\\llama.cpp\\llama-server.exe"`. Without one,
`serve-inference` stops with "llama.cpp is not installed" (or "vLLM is
not installed"), the install options, and a link to
[docs/SELL-INFERENCE.md](../docs/SELL-INFERENCE.md#3-install-an-inference-engine).
Pulling, verifying, and seeding in Desktop never need an engine.

## Flatpak

`desktop/flatpak/` builds Desktop as the Flatpak `dev.tensorseed.Tensorseed`
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
    desktop/flatpak/dev.tensorseed.Tensorseed.yml
flatpak run dev.tensorseed.Tensorseed
# optional single file bundle:
flatpak-builder --repo=repo --force-clean build-dir desktop/flatpak/dev.tensorseed.Tensorseed.yml
flatpak build-bundle repo tensorseed-desktop.flatpak dev.tensorseed.Tensorseed
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
  dataset verify uses the bundled CLI and nothing has to be installed on
  the host.
  `TENSORSEED_CLI` still overrides it. Run the bundled CLI with
  `flatpak run --command=tensorseed dev.tensorseed.Tensorseed ...`.
- `dev.tensorseed.Tensorseed.desktop` (`Exec=tensorseed-desktop %u`,
  `MimeType=x-scheme-handler/tensorseed;`), so `xdg-open
  tensorseed://entry/org/name` starts the Flatpak, and a running one gets
  the link through single instance forwarding. Inside the sandbox Desktop
  does not write its own handler file.
- AppStream metadata (`dev.tensorseed.Tensorseed.metainfo.xml`, passes
  `appstreamcli validate`), icons at 32, 128, 256, and 512 pixels, and
  the scalable `dev.tensorseed.Tensorseed.svg`.

Permissions (`finish-args`), each for one reason:

| Permission | Why |
| --- | --- |
| `--share=network` | Catalog sync, Hugging Face, trackers, the DHT, peers. |
| `--socket=wayland`, `--socket=fallback-x11`, `--share=ipc` | The window. X11 only when there is no Wayland; IPC is the X11 shared memory it needs. |
| `--device=dri` | WebKit GPU rendering. |
| `--talk-name=org.kde.StatusNotifierWatcher` | The tray icon. |
| `--filesystem=xdg-config/tensorseed:create` | The CLI's config, keys, `mirrors.toml`, and `hf_token`. |
| `--filesystem=xdg-data/tensorseed:create` | State, catalog cache, staging, seed data, publish trees, and mirrors (`~/.local/share/tensorseed`). |
| `--filesystem=xdg-download/tensorseed:create` | Where pulls go by default, the same folder as outside the sandbox. |

No home directory access, no host filesystem, and no other bus names.
The single instance name (`dev.tensorseed.Tensorseed.SingleInstance`) is
under the app id, which Flatpak allows without a permission. The Tauri
identifier in `tauri.conf.json` stays `dev.tensorseed.desktop` for the
other builds; the Flatpak manifest sets `TAURI_CONFIG` to use the app id
instead, because the single instance name is built from it. The Flatpak
id is not `dev.tensorseed.desktop` because Flathub does not accept ids
that end in `.desktop`. There is no
cache permission because Tensorseed keeps nothing under `~/.cache`; the
Hugging Face token falls back to `~/.cache/huggingface/token` on the
host, which the sandbox cannot see, so set the token with Tensorseed's
own `hf_token` file instead.

Config and data are shared with a host CLI. Flatpak normally points
`XDG_CONFIG_HOME` at `~/.var/app/dev.tensorseed.Tensorseed/config`; inside a
Flatpak (`FLATPAK_ID` set) Tensorseed uses the host's directory instead
(`HOST_XDG_CONFIG_HOME`, else `~/.config`), which the permission above
exposes at the same path. Data paths are `~/.local/share/tensorseed` on
every Linux build already. So Desktop in the Flatpak and a `tensorseed`
installed on the host read and write the same files.

Downloads: the default `general.download_dir` is `tensorseed` under the
XDG download directory (`~/Downloads/tensorseed` on most systems) for every
Linux build, and `xdg-download/tensorseed` exposes exactly that folder, so
the Flatpak and a host CLI find the same copies without any config. A
narrow folder under Downloads is the smallest grant that works without
asking, and the file chooser portal does not fit: pulls and seeding run in
the background and need the same folder again on every start. A
`download_dir` set in the config wins; when it is outside the granted
folders, grant it once:

```fish
flatpak override --user --filesystem=/srv/models dev.tensorseed.Tensorseed
```

Paths in the config such as `state_db` or `[file] local_dir` outside
`~/.local/share/tensorseed` need the same override.

Inference serving in the Flatpak: the bundled CLI can run
`serve-inference`, but the sandbox cannot see an engine installed on the
host (the sandbox's `/usr` is the GNOME runtime, and there is no host
`PATH` or home directory access), and the Flatpak does not ship one. So
serving stays outside the sandbox:

- Simplest: install the `tensorseed` CLI on the host as well (a release
  archive, `.deb`, or `.rpm`) next to llama.cpp or vLLM, and serve with
  it. It reads the same config and the same pulled copies as the
  Flatpak.
- Or start the engine on the host yourself and keep the Flatpak's CLI
  for the rest. `flatpak run --command=tensorseed dev.tensorseed.Tensorseed
  serve-inference <copy> --print-command` verifies the copy and prints
  the engine command (pulled copies are at the same path inside and
  outside the sandbox); run that on the host. The signing proxy can then
  run from the Flatpak, because the network, including loopback, is
  shared: `claim-serving <copy> --engine-version <version>`, then
  `sign-responses --upstream http://127.0.0.1:<engine port> --claim
  <copy>/.tensorseed/served-weights.json`.

Pointing `inference.llama_server` at a host path does not help in the
sandbox: the path does not exist there.

Inside the sandbox the tray icon image is written to
`$XDG_RUNTIME_DIR/app/dev.tensorseed.Tensorseed/tray-icon`, the runtime
folder Flatpak shares with the host, because the tray host reads the icon
by path.

The unsigned `.flatpak` bundle is built in the Release build workflow
(about 25 minutes, including the runtime and SDK downloads). Nothing is
published to Flathub yet.

Flathub: the Flathub manifest lives in the Flathub repo for the app id,
not here. It builds the same way from a `type: git` source pinned to a
release tag and its commit, with these `cargo-sources.json` and
`node-sources.json` copied next to it and `shared-modules` as a
submodule there. The AppStream metadata, desktop file, and icons it
installs come from this repo. The in app updater stays off in every
Flatpak (it checks `FLATPAK_ID` and points to `flatpak update`), so
the Flathub build needs no separate updater setting. Screenshots are
served from `site/static/flathub/screenshots/<version>/` so their URLs
stay stable.

## Not done yet

- Dataset verify goes through the CLI.
- Settings edits only `[torrent]`; the data folder, catalogs, chain check
  mode, and disk budget are still edited in the config file or with the
  CLI.
- Packaging: the local `.deb` and AppImage `tauri build` makes by
  default, the Windows MSI and NSIS installers, and the Flatpak (also
  built unsigned in CI; not published to Flathub). Signing, the MSI build
  method, and bundling the CLI in the other packages are M3.6 decisions.
- Windows: the NSIS and MSI builds, deep links, single instance
  forwarding, close to tray, the tray icon and its status, pull, verify,
  seed, settings, `tensorseed gui`, and uninstalling have been run on
  Windows 11. The tray menu itself has only been checked on Linux so
  far. macOS is not built.

Copyright Tuned Chaos. AGPL-3.0-or-later.
