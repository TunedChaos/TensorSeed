// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright 2026 Tuned Chaos

// The current release. Every version string and download link on the site
// comes from here, so a new release only changes `VERSION`. If a release
// adds, removes, or renames a file, edit the lists below to match its
// SHA256SUMS.

export const VERSION = "0.3.2";

export const REPO_URL = "https://github.com/TunedChaos/TensorSeed";
export const TAG = `v${VERSION}`;
export const RELEASE_URL = `${REPO_URL}/releases/tag/${TAG}`;
export const DOWNLOAD_BASE = `${REPO_URL}/releases/download/${TAG}`;

export function assetUrl(file: string): string {
  return `${DOWNLOAD_BASE}/${file}`;
}

export type Os = "linux" | "windows";

export type Asset = {
  /** Short label, such as "Linux AppImage". */
  label: string;
  /** File name on the GitHub release. */
  file: string;
  os: Os | "any";
  /** Optional one line note shown after the link. */
  note?: string;
};

const v = VERSION;

export const DESKTOP_ASSETS: Asset[] = [
  { label: "Linux AppImage", file: `Tensorseed.Desktop_${v}_amd64.AppImage`, os: "linux", note: "updates itself" },
  { label: "Debian and Ubuntu .deb", file: `Tensorseed.Desktop_${v}_amd64.deb`, os: "linux", note: "updates through your package manager" },
  { label: "Fedora and openSUSE .rpm", file: `Tensorseed.Desktop-${v}-1.x86_64.rpm`, os: "linux", note: "updates through your package manager" },
  { label: "Flatpak bundle", file: `Tensorseed.Desktop_${v}_x86_64.flatpak`, os: "linux", note: "not on Flathub yet; does not update itself" },
  { label: "Windows installer .exe", file: `Tensorseed.Desktop_${v}_x64-setup.exe`, os: "windows", note: "installs for you, no administrator rights" },
  { label: "Windows .msi", file: `Tensorseed.Desktop_${v}_x64_en-US.msi`, os: "windows", note: "installs for all users" },
  { label: "Windows ARM64 installer .exe", file: `Tensorseed.Desktop_${v}_arm64-setup.exe`, os: "windows", note: "installs for you, no administrator rights; install by hand, the in-app updater does not offer ARM64 updates yet" },
  { label: "Windows ARM64 .msi", file: `Tensorseed.Desktop_${v}_arm64_en-US.msi`, os: "windows", note: "installs for all users; install by hand, the in-app updater does not offer ARM64 updates yet" },
];

/** The Desktop download each OS is pointed at first. */
export const DESKTOP_PICK: Record<Os, string> = {
  linux: `Tensorseed.Desktop_${v}_amd64.AppImage`,
  windows: `Tensorseed.Desktop_${v}_x64-setup.exe`,
};

export const FLATPAK_FILE = `Tensorseed.Desktop_${v}_x86_64.flatpak`;

export const CLI_ASSETS: Asset[] = [
  { label: "Linux x86_64, static (musl)", file: `tensorseed-${v}-x86_64-unknown-linux-musl.tar.gz`, os: "linux" },
  { label: "Linux ARM64, static (musl)", file: `tensorseed-${v}-aarch64-unknown-linux-musl.tar.gz`, os: "linux" },
  { label: "Linux x86_64, glibc 2.17", file: `tensorseed-${v}-x86_64-unknown-linux-gnu.tar.gz`, os: "linux" },
  { label: "Windows x86_64", file: `tensorseed-${v}-x86_64-pc-windows-gnu.zip`, os: "windows" },
  { label: "Windows ARM64", file: `tensorseed-${v}-aarch64-pc-windows-gnullvm.zip`, os: "windows", note: "file format confirmed; not yet run on ARM Windows hardware" },
  { label: "Debian and Ubuntu package, x86_64 (.deb)", file: `tensorseed_${v}-1_amd64.deb`, os: "linux" },
  { label: "Debian and Ubuntu package, ARM64 (.deb)", file: `tensorseed_${v}-1_arm64.deb`, os: "linux" },
  { label: "Fedora and RHEL package, x86_64 (.rpm)", file: `tensorseed-${v}-1.x86_64.rpm`, os: "linux" },
  { label: "Fedora and RHEL package, ARM64 (.rpm)", file: `tensorseed-${v}-1.aarch64.rpm`, os: "linux" },
  { label: "Source (tar.gz)", file: `tensorseed-${v}-src.tar.gz`, os: "any" },
  { label: "Source (zip)", file: `tensorseed-${v}-src.zip`, os: "any" },
];

/** The CLI archive each OS is pointed at first. */
export const CLI_PICK: Record<Os, string> = {
  linux: `tensorseed-${v}-x86_64-unknown-linux-musl.tar.gz`,
  windows: `tensorseed-${v}-x86_64-pc-windows-gnu.zip`,
};

export const VERIFY_ASSETS: Asset[] = [
  { label: "SHA256SUMS", file: "SHA256SUMS", os: "any" },
  { label: "SHA256SUMS.minisig", file: "SHA256SUMS.minisig", os: "any" },
  { label: "tensorseed-release.pub", file: "tensorseed-release.pub", os: "any" },
];

/** Release signing key (minisign). Not the catalog publisher key. */
export const RELEASE_KEY_ID = "F5D2D3C4C27D459D";
export const RELEASE_PUBKEY = "RWSdRX3CxNPS9Vl1JNjDnZ04OmM/3xrfp9vqG5FO8TNpK8Sbg6ENAI4P";

/** Every file linked from the site, for the link checker. */
export const ALL_ASSETS: Asset[] = [...DESKTOP_ASSETS, ...CLI_ASSETS, ...VERIFY_ASSETS];
