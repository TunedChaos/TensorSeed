# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright 2026 Tuned Chaos
#
# Task runner for Tensorseed (https://just.systems).
#
# Cross compilation uses cargo-zigbuild, which uses Zig as the C compiler and
# linker for every target, so one Linux (or macOS) host can build Linux and
# Windows binaries without system cross toolchains:
#
#   cargo install --locked just cargo-zigbuild
#   pip install ziglang            # or install zig 0.13+ from ziglang.org
#   just targets                   # rustup target add for every dist target
#   just dist                      # release archives and SHA256SUMS in dist/
#
# On Windows, run the recipes from Git Bash (shebang recipes need bash), or
# skip just entirely and build natively with MSVC: `cargo build --release`.
# See CONTRIBUTING.md.

set shell := ["bash", "-euo", "pipefail", "-c"]
set windows-shell := ["bash", "-euo", "pipefail", "-c"]

# Release targets. x86_64-unknown-linux-gnu gets a glibc floor through the
# zigbuild suffix (`.2.17`), so the binary runs on any glibc 2.17+ system.
linux_targets := "x86_64-unknown-linux-musl aarch64-unknown-linux-musl x86_64-unknown-linux-gnu.2.17"
windows_targets := "x86_64-pc-windows-gnu aarch64-pc-windows-gnullvm"
dist_targets := linux_targets + " " + windows_targets

package := "tensorseed-cli"
bin := "tensorseed"
version := `sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1`
dist_dir := "dist"

# List recipes.
default:
    @just --list --unsorted

# Install the Rust standard library for every dist target (rustup).
targets:
    rustup target add {{ replace_regex(dist_targets, '\.2\.[0-9]+', '') }}

# Print the dist targets.
list-targets:
    @for t in {{ dist_targets }}; do echo "$t"; done

# Build the release binary for one target with cargo-zigbuild (e.g. `just build x86_64-pc-windows-gnu`).
build target:
    cargo zigbuild --locked --profile dist -p {{ package }} --target {{ target }}

# Build the release binary for the host with plain cargo (no zig needed).
build-native:
    cargo build --locked --release -p {{ package }}

# Build every dist target, stripped, and package them into dist/ with SHA256SUMS.
dist: (clean-dist)
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p {{ dist_dir }}
    for t in {{ dist_targets }}; do
        just build "$t"
        triple="${t%%.2.*}"                       # drop the glibc suffix
        name="{{ bin }}-{{ version }}-${triple}"
        stage="{{ dist_dir }}/${name}"
        mkdir -p "$stage"
        case "$triple" in
            *windows*) exe="{{ bin }}.exe" ;;
            *)         exe="{{ bin }}" ;;
        esac
        cp "target/${triple}/dist/${exe}" "$stage/"
        cp LICENSE README.md "$stage/"
        mkdir -p "$stage/docs" "$stage/config"
        cp docs/RUN-A-MIRROR.md "$stage/docs/"
        cp config/tensorseed.example.toml config/mirrors.example.toml "$stage/config/"
        case "$triple" in
            *windows*) (cd {{ dist_dir }} && python3 -c 'import sys, zipfile, pathlib
    root = pathlib.Path(sys.argv[1])
    with zipfile.ZipFile(sys.argv[2], "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        for p in sorted(root.rglob("*")):
            z.write(p, p.as_posix())' "$name" "${name}.zip") ;;
            *) mkdir -p "$stage/contrib"; cp -r contrib/systemd "$stage/contrib/"
               tar -C {{ dist_dir }} --owner=0 --group=0 --numeric-owner -czf "{{ dist_dir }}/${name}.tar.gz" "$name" ;;
        esac
        rm -rf "$stage"
    done
    (cd {{ dist_dir }} && sha256sum {{ bin }}-* > SHA256SUMS && cat SHA256SUMS)

# Sign dist/SHA256SUMS with minisign. The key path is only passed through; it is never stored.
sign key:
    minisign -S -s "{{ key }}" -m {{ dist_dir }}/SHA256SUMS -t "tensorseed {{ version }} release"
    @echo "wrote {{ dist_dir }}/SHA256SUMS.minisig"

# Verify dist/SHA256SUMS against a minisign public key file, then the archives.
verify-dist pubkey:
    minisign -V -p "{{ pubkey }}" -m {{ dist_dir }}/SHA256SUMS
    cd {{ dist_dir }} && sha256sum -c SHA256SUMS

# Run the whole test suite (unit, integration, doc tests).
test:
    cargo test --workspace --locked

# Build the API docs; fails on any rustdoc warning (missing docs, broken links).
doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked

# Open the API docs in a browser.
doc-open: doc
    cargo doc --workspace --no-deps --open

# Format all code.
fmt:
    cargo fmt --all

# Check formatting without changing files.
fmt-check:
    cargo fmt --all -- --check

# Clippy on every target with warnings as errors.
lint:
    cargo clippy --workspace --all-targets --locked -- -D warnings

# Everything CI checks: formatting, clippy, tests, docs.
check-all: fmt-check lint test doc

# Remove build output and dist/.
clean: clean-dist
    cargo clean

# Remove dist/ only.
clean-dist:
    rm -rf {{ dist_dir }}
