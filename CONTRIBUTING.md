# Contributing to Tensorseed

Thanks for your interest. Tensorseed is pre-alpha (milestone M0), so the design in
[docs/SPEC.md](docs/SPEC.md) can still change. Opening an issue to discuss a change
before writing a large patch saves everyone time.

## Licensing of contributions

Tensorseed is licensed under **AGPL-3.0-or-later**. By submitting a contribution you
agree that it is licensed under AGPL-3.0-or-later, and you confirm that you have the
right to license it that way. New source files start with:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos
```

New dependencies must use a license compatible with AGPL-3.0-or-later; `cargo deny
check licenses` enforces the list in `deny.toml`. GPL-2.0-only code cannot be used.

## Before you open a pull request

The repo ships a [justfile](justfile) ([just](https://just.systems)) with
the same checks CI runs:

```sh
just check-all      # fmt-check, clippy -D warnings, tests (with doc tests), docs -D warnings
just fmt            # format
just lint           # clippy only
just test           # tests only
just doc            # API docs; every public item must be documented
cargo deny check licenses bans sources   # optional locally, required in CI
```

Without just, run the commands from the justfile by hand:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
```

Library crates use `#![warn(missing_docs)]`, and the workspace enables
clippy's `missing_errors_doc` and `missing_panics_doc`: public items need
`///` docs, fallible functions an `# Errors` section, and functions that can
panic a `# Panics` section. Prefer intra-doc links (``[`Type`]``) and
examples that compile as doc tests (`no_run` for network or Chia code).

## Cross platform builds

Release binaries are cross compiled from one host with
[cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild), which uses
Zig as the C compiler and linker, so no system cross toolchains are needed.
Everything in the dependency tree is pure Rust or bundled C (rustls instead
of OpenSSL, SQLite bundled by rusqlite).

| Target | Notes |
|---|---|
| `x86_64-unknown-linux-musl` | static |
| `aarch64-unknown-linux-musl` | static (Raspberry Pi 4 and 5, ARM servers) |
| `x86_64-unknown-linux-gnu.2.17` | glibc 2.17 or newer (the suffix sets the glibc floor) |
| `x86_64-pc-windows-gnu` | Windows 10 and 11, x86_64 |
| `aarch64-pc-windows-gnullvm` | Windows on ARM |

Setup on Linux or macOS:

```sh
cargo install --locked just cargo-zigbuild
pip install ziglang          # or a zig release from ziglang.org on PATH
just targets                 # rustup target add for every target above
just build x86_64-pc-windows-gnu      # one target, into target/<triple>/dist/
just dist                    # all targets: dist/tensorseed-<version>-<target>.tar.gz or .zip, plus SHA256SUMS
just sign ~/.tensorseed-release/tensorseed-release.key   # release key only, see RELEASE-KEY.md
```

Archives contain the binary, LICENSE, README.md, the volunteer guide, the
example configs, and (Linux) the systemd unit. `dist/` is ignored by git.

### On Windows

- **Native build (simplest):** install Rust with rustup (the default
  `x86_64-pc-windows-msvc` toolchain needs the Visual Studio Build Tools),
  then `cargo build --release` and `cargo test --workspace`. The binary is
  `target\release\tensorseed.exe`. No zig or just needed.
- **Using the justfile:** install just and cargo-zigbuild with
  `cargo install --locked just cargo-zigbuild`, Zig with
  `winget install zig.zig` (or `pip install ziglang`), and Git for Windows.
  The justfile runs its recipes with bash, so run `just` from Git Bash (or
  WSL). `just check-all`, `just build <target>`, and `just dist` then work as
  on Linux; `just dist` also needs `python3` for the zip archives.
- WSL2 behaves exactly like Linux and is the easiest way to run `just dist`.

## Guidelines

- Keep the catalog format chain agnostic. Chain specific code belongs behind the
  `CatalogBackend` trait in `tensorseed-catalog`.
- Never commit secrets. Configuration reads secrets only from environment variables.
- Hash and verify everything: no code path may accept bytes that were not checked
  against a manifest.
- Write plain text without em or en dashes in docs and comments.
