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
        cp LICENSE README.md RELEASE-KEY.md "$stage/"
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

# Key id of the catalog publisher key. Release signing must never use it.
catalog_key_id := "CE1E0E3D543DFFB8"

# Create the offline release signing key pair in <dir> (minisign; prompts for a passphrase). See RELEASE-KEY.md.
release-keygen dir:
    #!/usr/bin/env bash
    set -euo pipefail
    command -v minisign >/dev/null || { echo "minisign is not installed (https://jedisct1.github.io/minisign/)" >&2; exit 1; }
    dir="{{ dir }}"
    sk="$dir/tensorseed-release.key"
    pk="$dir/tensorseed-release.pub"
    for f in "$sk" "$pk"; do
        if [ -e "$f" ]; then echo "refusing to overwrite $f" >&2; exit 1; fi
    done
    mkdir -p "$dir"
    chmod 700 "$dir" 2>/dev/null || true
    umask 077
    echo "Creating the Tensorseed release key. minisign asks for a passphrase:"
    echo "use a long random one and store it in your password manager."
    minisign -G -s "$sk" -p "$pk" -c "tensorseed release key"
    chmod 600 "$sk"
    chmod 644 "$pk"
    echo
    echo "Secret key: $sk (encrypted, mode 600). Keep it offline; never copy it to a server or CI."
    echo "Public key: $pk"
    echo
    sed -n 2p "$pk"
    echo
    echo "Next steps:"
    echo "  1. Back up $sk and the passphrase separately (see RELEASE-KEY.md)."
    echo "  2. Publish the public key line above in README.md, docs/RUN-A-MIRROR.md and RELEASE-KEY.md."
    echo "  3. Sign a release: just dist && just sign $sk"

# Sign dist/SHA256SUMS with the offline RELEASE key (never the catalog key). The key path is only passed through; it is never stored.
sign release_key:
    #!/usr/bin/env bash
    set -euo pipefail
    sums="{{ dist_dir }}/SHA256SUMS"
    [ -f "$sums" ] || { echo "$sums not found; run just dist first" >&2; exit 1; }
    tmp="$sums.minisig.tmp"
    trap 'rm -f "$tmp"' EXIT
    minisign -S -s "{{ release_key }}" -m "$sums" -x "$tmp" -t "tensorseed {{ version }} release SHA256SUMS"
    # The key id is bytes 2..10 of the signature line, shown reversed in hex.
    kid="$(sed -n 2p "$tmp" | python3 -c 'import sys, base64; print(base64.b64decode(sys.stdin.read().strip())[2:10][::-1].hex().upper())')"
    if [ "$kid" = "{{ catalog_key_id }}" ]; then
        echo "refusing: {{ release_key }} is the catalog publisher key ({{ catalog_key_id }}); sign releases with the release key" >&2
        exit 1
    fi
    mv "$tmp" "$sums.minisig"
    trap - EXIT
    echo "wrote $sums.minisig (release key id $kid)"

# Verify dist/SHA256SUMS with the release public key (a .pub file or the base64 key line), then the archives.
verify-dist release_pubkey:
    #!/usr/bin/env bash
    set -euo pipefail
    k="{{ release_pubkey }}"
    if [ -f "$k" ]; then minisign -V -p "$k" -m {{ dist_dir }}/SHA256SUMS; else minisign -V -P "$k" -m {{ dist_dir }}/SHA256SUMS; fi
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
