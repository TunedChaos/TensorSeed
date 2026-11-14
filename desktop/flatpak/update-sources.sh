#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Copyright 2026 Tuned Chaos
#
# Regenerate the offline Cargo and pnpm sources for the Flatpak manifest
# after Cargo.lock, desktop/src-tauri/Cargo.lock, or desktop/pnpm-lock.yaml
# changes. Needs Python 3 with aiohttp, toml, and tomlkit, plus a checkout of
# https://github.com/flatpak/flatpak-builder-tools (FLATPAK_BUILDER_TOOLS),
# with its node generator installed (pipx install ./node).
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
tools=${FLATPAK_BUILDER_TOOLS:?set FLATPAK_BUILDER_TOOLS to a flatpak-builder-tools checkout}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# The CLI workspace and the desktop workspace have separate lockfiles. The
# generator takes one lockfile, so generate both and merge them, dropping
# entries that are identical in both.
python3 "$tools/cargo/flatpak-cargo-generator.py" "$root/Cargo.lock" -o "$tmp/cli.json"
python3 "$tools/cargo/flatpak-cargo-generator.py" "$root/desktop/src-tauri/Cargo.lock" -o "$tmp/desktop.json"
python3 - "$tmp/cli.json" "$tmp/desktop.json" "$here/cargo-sources.json" <<'PY'
import json, sys
out, seen = [], set()
for path in sys.argv[1:3]:
    for entry in json.load(open(path)):
        key = json.dumps(entry, sort_keys=True)
        if key not in seen:
            seen.add(key)
            out.append(entry)
json.dump(out, open(sys.argv[3], "w"), indent=4)
open(sys.argv[3], "a").write("\n")
print(f"cargo-sources.json: {len(out)} entries")
PY

flatpak-node-generator pnpm "$root/desktop/pnpm-lock.yaml" -o "$here/node-sources.json"
echo >> "$here/node-sources.json"
echo "node-sources.json: $(python3 -c 'import json,sys; print(len(json.load(open(sys.argv[1]))))' "$here/node-sources.json") entries"
