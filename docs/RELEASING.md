# Releasing Tensorseed

CI builds every unsigned release asset when you push a `v*` tag, or when
you run the **Release build** workflow by hand. The workflow does not
create a GitHub Release and does not sign anything. You download the
artifact on your signing machine, check the attestations and the
checksums, sign with the keys that stay on that machine, write
`latest.json`, and only then create the GitHub Release.

The Desktop updater signatures (`.sig`) and `latest.json` are also made
on your signing machine. CI never holds those keys.

## What CI produces

The `tensorseed-release-unsigned` artifact contains:

- The CLI archives, `.deb` and `.rpm` packages, and source archives
  (`just dist`)
- Tensorseed Desktop: Linux AppImage, `.deb`, `.rpm`, and `.flatpak`,
  plus Windows x64 and ARM64 NSIS and MSI installers (unsigned)
- `SHA256SUMS` over those files (the same set as 0.3.1, plus the Windows
  ARM64 installers)
- `tensorseed-release.pub` (the published release public key)
- `tensorseed-<version>-unsigned-bundle.zip`, a convenience zip of the
  same files. Do not upload this zip to the GitHub Release; it is not
  listed in `SHA256SUMS`.

Spaces in Desktop file names are already turned into dots, as GitHub
does on upload.

CI does not produce `SHA256SUMS.minisig`, the Desktop `.sig` files, or
`latest.json`.

## On your signing machine (fish)

Use a checkout of the same tag, plus [GitHub CLI](https://cli.github.com/)
(`gh`), `minisign`, `pnpm`, and the `just` task runner. Replace `0.3.2`
with the version you are shipping.

```fish
set ver 0.3.2
set repo TunedChaos/TensorSeed
set tag v$ver

# The release commit is already on main.
git tag $tag
git push origin $tag

# Wait until the Release build workflow for that tag finishes.
set run (gh run list --repo $repo --workflow release-build.yml --branch $tag --limit 1 --json databaseId --jq '.[0].databaseId')
gh run watch $run --repo $repo --exit-status

# Download the unsigned artifact.
mkdir -p ~/releases/$ver
cd ~/releases/$ver
gh run download $run --repo $repo --name tensorseed-release-unsigned

# Check build provenance on a couple of assets.
gh attestation verify --repo $repo tensorseed-$ver-x86_64-unknown-linux-musl.tar.gz
gh attestation verify --repo $repo Tensorseed.Desktop_{$ver}_amd64.AppImage

# Check every file listed in SHA256SUMS.
sha256sum -c SHA256SUMS
```

Sign `SHA256SUMS` with the release key. The easiest way is to copy the
artifact into `dist/` of the tagged checkout and use `just sign`, which
refuses the catalog publisher key and writes the trusted comment
`tensorseed <version> release SHA256SUMS`:

```fish
cd /path/to/TensorSeed
git checkout $tag
mkdir -p dist
cp ~/releases/$ver/SHA256SUMS ~/releases/$ver/tensorseed-* ~/releases/$ver/tensorseed_* ~/releases/$ver/Tensorseed.Desktop_* dist/
just sign ~/.tensorseed-release/tensorseed-release.key
just verify-dist
cp dist/SHA256SUMS.minisig ~/releases/$ver/
```

Then sign each Desktop installer that the in-app updater may download.
`pnpm --dir desktop tauri signer sign` writes a `.sig` next to the file
and binds it to this version. Sign the exact files from the artifact;
rebuilding them changes the bytes and breaks the signature.

```fish
cd ~/releases/$ver
read -s -P 'updater key password: ' -x TAURI_SIGNING_PRIVATE_KEY_PASSWORD
for f in \
    Tensorseed.Desktop_{$ver}_amd64.AppImage \
    Tensorseed.Desktop_{$ver}_x64-setup.exe \
    Tensorseed.Desktop_{$ver}_x64_en-US.msi
    pnpm --dir /path/to/TensorSeed/desktop tauri signer sign \
        -f ~/.tauri/tensorseed-desktop-updater.key --app-version $ver $f
end
set -e TAURI_SIGNING_PRIVATE_KEY_PASSWORD
```

Windows ARM64 installers are part of the release and of `SHA256SUMS`, but
`scripts/make-latest-json` and the in-app updater do not yet have
`windows-aarch64` entries. Leave those `.sig` files out until the feed
grows that channel. They can still be installed by hand.

Write `latest.json` from the signed x64 and Linux AppImage files. The
script reads each `.sig`, checks the signed version and file name, and
with `--verify` checks the signature against the public key in
`desktop/src-tauri/tauri.conf.json`:

```fish
cd ~/releases/$ver
/path/to/TensorSeed/scripts/make-latest-json --version $ver --notes-file notes.md --verify \
    --appimage Tensorseed.Desktop_{$ver}_amd64.AppImage \
    --nsis Tensorseed.Desktop_{$ver}_x64-setup.exe \
    --msi Tensorseed.Desktop_{$ver}_x64_en-US.msi \
    --out latest.json
```

Prepare release notes (the Unreleased section of `CHANGELOG.md` is a
good start) in `notes.md`. Then create the GitHub Release. Upload
`SHA256SUMS` and `SHA256SUMS.minisig` with the archives first, and
`latest.json` last. Desktop follows `releases/latest`, which GitHub
points at the newest release that is not a draft or a pre-release.

```fish
cd ~/releases/$ver
# The convenience zip is only for the workflow artifact; keep it out of the release.
mkdir -p ~/releases/$ver-ci
mv tensorseed-$ver-unsigned-bundle.zip ~/releases/$ver-ci/
gh release create $tag --repo $repo --title "Tensorseed $ver" --notes-file notes.md     SHA256SUMS SHA256SUMS.minisig tensorseed-release.pub     tensorseed-$ver-* tensorseed_$ver-*     Tensorseed.Desktop_*     *.sig
gh release upload $tag --repo $repo latest.json
```

Check that `latest.json` is present, then open the release page and
verify a download against `SHA256SUMS` as in
[RELEASE-KEY.md](../RELEASE-KEY.md).

## Running the workflow by hand

`workflow_dispatch` builds whatever commit you select in the Actions UI.
The asset names still come from the version in `Cargo.toml`. Source
archives use that commit. Prefer a `v*` tag so the source archives are
tagged and the tag matches the crate version (the workflow fails if they
differ).

## See also

- [RELEASE-KEY.md](../RELEASE-KEY.md) for the three keys, verification,
  and key storage
- [desktop/README.md](../desktop/README.md#releasing-an-update) for
  updater signature details
