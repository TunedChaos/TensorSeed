# Tensorseed signing keys

Tensorseed uses two separate minisign (Ed25519) keys. They protect different
things, live in different places, and are never interchangeable.

| Key | Signs | Public key | Where the secret lives |
|---|---|---|---|
| Catalog publisher key (id `CE1E0E3D543DFFB8`) | Catalog entries, deltas, snapshots, heads, `mirrors.toml`, and the Chia anchor | `RWS4/z1UPQ4ezmjrcpKqDYpqyn5h1WUS2oDs1B73Dh91kVzzNIZd03xd` | The publishing host, because the weekly head re-sign and every catalog update need it |
| Release key (id `F5D2D3C4C27D459D`) | `SHA256SUMS` of the release archives (`SHA256SUMS.minisig`) | `RWSdRX3CxNPS9Vl1JNjDnZ04OmM/3xrfp9vqG5FO8TNpK8Sbg6ENAI4P` | Offline only, on the maintainer's workstation and an offline backup |

## Release public key

```
untrusted comment: minisign public key F5D2D3C4C27D459D
RWSdRX3CxNPS9Vl1JNjDnZ04OmM/3xrfp9vqG5FO8TNpK8Sbg6ENAI4P
```

This is the only key that may sign Tensorseed release checksums. The same
key is in [README.md](README.md), [docs/RUN-A-MIRROR.md](docs/RUN-A-MIRROR.md),
and [keys/tensorseed-release.pub](keys/tensorseed-release.pub); all copies
must match. It is not the catalog publisher key (id `CE1E0E3D543DFFB8`).

## Why two keys

- **Smaller blast radius.** The catalog key has to sit on an online
  publishing host. If that host were compromised, an attacker could sign
  catalog entries (which the Chia anchor and clients still check), but could
  not sign binaries. A leaked catalog key must never be enough to ship a
  malicious `tensorseed` executable to every volunteer.
- **Different lifetimes.** The catalog key signs every week and is pinned in
  every client configuration; the release key signs a few times a year and
  can stay encrypted and offline in between.
- **Independent rotation.** Either key can be replaced without touching the
  other.

The release tooling enforces the separation: `just sign` refuses a key whose
id is the catalog key id `CE1E0E3D543DFFB8`.

## Verifying a release

Download the archive for your platform (or the source archive,
`tensorseed-<version>-src.tar.gz` or `.zip`), `SHA256SUMS`, and
`SHA256SUMS.minisig` from the same release, then:

```sh
minisign -Vm SHA256SUMS -P RWSdRX3CxNPS9Vl1JNjDnZ04OmM/3xrfp9vqG5FO8TNpK8Sbg6ENAI4P
sha256sum -c --ignore-missing SHA256SUMS
```

On Windows (PowerShell), with minisign installed (`scoop install minisign`
or the release from https://github.com/jedisct1/minisign/releases):

```powershell
minisign -Vm SHA256SUMS -P RWSdRX3CxNPS9Vl1JNjDnZ04OmM/3xrfp9vqG5FO8TNpK8Sbg6ENAI4P
Get-FileHash .\tensorseed-<version>-x86_64-pc-windows-gnu.zip -Algorithm SHA256
# compare with the line for that file in SHA256SUMS
```

The trusted comment printed by minisign names the version
(`tensorseed <version> release SHA256SUMS`); check that it matches the
release you downloaded. From a checkout, `just verify-dist` does both
steps for everything in `dist/`, using [keys/tensorseed-release.pub](keys/tensorseed-release.pub).
Pass a `.pub` path or key line to override.

## Creating the release key (maintainers)

Create it on the offline machine that will keep it, never on a server, CI
runner, or shared machine:

```sh
just release-keygen ~/.tensorseed-release
```

This runs `minisign -G`, writes `tensorseed-release.key` (encrypted with a
passphrase that minisign asks for) and `tensorseed-release.pub`, refuses to
overwrite existing files, sets the secret key to mode 600, and prints the
public key. Without just, the same is:

```sh
mkdir -p ~/.tensorseed-release && chmod 700 ~/.tensorseed-release
minisign -G -s ~/.tensorseed-release/tensorseed-release.key \
  -p ~/.tensorseed-release/tensorseed-release.pub -c "tensorseed release key"
chmod 600 ~/.tensorseed-release/tensorseed-release.key
```

On Windows, run `just release-keygen` from Git Bash, or call minisign
directly in PowerShell with the same `-G -s ... -p ...` arguments, then
restrict the secret key file to your user (`icacls <file> /inheritance:r
/grant:r "%USERNAME%:F"`).

The current public key is published in this file, README.md,
docs/RUN-A-MIRROR.md, and keys/tensorseed-release.pub. After a rotation,
update all four plus the website in one commit.

## Storage

- The secret key file stays encrypted (never `minisign -W`) and offline.
- The passphrase lives in a password manager, never next to the key file.
- Keep at least one offline backup of the key file (for example an
  encrypted USB drive in a separate place). A backup of the key is useless
  without the passphrase, and the passphrase alone is useless without the
  key, so store them apart.
- Never copy the release key to the publishing host, CI secrets, or the
  repository. `.gitignore` excludes `*.key`, but do not rely on it.

## Signing a release

```sh
just dist                                         # binaries, source archives, and SHA256SUMS in dist/
just dist-src v0.1.0                              # source archives only, from that tag
just sign ~/.tensorseed-release/tensorseed-release.key
just verify-dist ~/.tensorseed-release/tensorseed-release.pub
```

`just sign` only passes the key path to minisign; nothing is stored.

## Rotation and compromise

- **Planned rotation:** create a new key, sign a statement naming the new
  public key with the old key (`minisign -S` over a short text file),
  publish both in a commit and on the release page, and use the new key
  from the next release on.
- **Lost key:** publish the new key in a commit and on the website, and say
  that the old key was lost, not leaked; old releases stay valid.
- **Compromised key:** publish an advisory (see [SECURITY.md](SECURITY.md)),
  mark the old key revoked here, create a new key, and re-sign the
  `SHA256SUMS` of every release still offered for download with it.
  Releases signed only by the old key after the compromise date must not be
  trusted.

Catalog key rotation is a separate procedure (spec section 6.9 and open
question 10 in SPEC.md), because every client pins that key.
