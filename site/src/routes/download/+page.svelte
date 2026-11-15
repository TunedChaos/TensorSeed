<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  import { onMount } from "svelte";
  import Seo from "#lib/components/Seo.svelte";
  import AssetList from "#lib/components/AssetList.svelte";
  import Pre from "#lib/components/Pre.svelte";
  import { detectOs, type DetectedOs } from "#lib/os.ts";
  import {
    CLI_ASSETS,
    CLI_PICK,
    DESKTOP_ASSETS,
    DESKTOP_PICK,
    FLATPAK_FILE,
    RELEASE_URL,
    REPO_URL,
    TAG,
    VERIFY_ASSETS,
    VERSION,
    assetUrl,
  } from "#lib/release.ts";

  // Prerendered without a suggestion; the browser fills it in after load.
  let os = $state<DetectedOs | null>(null);
  onMount(() => (os = detectOs()));

  let desktopPick = $derived(os === "linux" || os === "windows" ? DESKTOP_PICK[os] : "");
  let cliPick = $derived(os === "linux" || os === "windows" ? CLI_PICK[os] : "");
  let pickAsset = $derived(DESKTOP_ASSETS.find((a) => a.file === desktopPick));
</script>

<Seo title="Download" description="Download Tensorseed {VERSION}: Tensorseed Desktop for Linux and Windows, and the tensorseed command line for Linux and Windows, with checksums and a signature." />

<header class="max-w-3xl">
  <h1 class="text-4xl font-bold">Download Tensorseed {VERSION}</h1>
  <p class="mt-3 text-lg">
    An early release for volunteer seeders and anyone who wants to pull and verify a published model. Formats and commands
    may still change before 1.0. Every file is on the <a class="link" href={RELEASE_URL}>{TAG} GitHub release</a> and listed in
    <code>SHA256SUMS</code>, signed with the release key. See <a class="link" href="/verify#release">Verify a release</a>.
  </p>
  {#if pickAsset}
    <div class="mt-6 flex flex-wrap items-center gap-3">
      <a class="btn btn-primary btn-lg" href={assetUrl(pickAsset.file)}>Download {pickAsset.label}</a>
      <span class="text-base-content/70 text-sm">Suggested for {os === "windows" ? "Windows" : "Linux"}. All files are below.</span>
    </div>
  {:else if os === "mac"}
    <div role="note" class="alert mt-6">There is no macOS build. You can build the CLI from source; see below.</div>
  {/if}
</header>

<section class="mt-12" aria-labelledby="desktop">
  <h2 id="desktop" class="text-2xl font-bold">Tensorseed Desktop</h2>
  <p class="mt-2 max-w-3xl">
    Browse the catalog, check each entry&rsquo;s signature and Chia anchor, pull with progress, pause and resume, and seed what
    you pulled, without a terminal. Desktop shares its config, downloads, and seeder with the CLI on the same machine, and it
    sends no telemetry. <a class="link" href="/desktop">More about Desktop</a>.
  </p>
  <div class="mt-4"><AssetList assets={DESKTOP_ASSETS} pick={desktopPick} /></div>

  <div class="mt-6 grid gap-6 lg:grid-cols-2">
    <div class="min-w-0">
      <h3 class="font-semibold" id="flatpak">Flatpak</h3>
      <p class="mt-1">
        The bundle needs the GNOME 51 runtime from Flathub. The app id is <code>dev.tensorseed.Tensorseed</code>. Add the
        remote if you do not already have it, then install and run the bundle:
      </p>
      <div class="mt-2">
        <Pre
          code={`flatpak remote-add --if-not-exists --user flathub https://flathub.org/remote-repo.flatpakrepo\nflatpak install --user ./${FLATPAK_FILE}\nflatpak run dev.tensorseed.Tensorseed`}
        />
      </div>
      <p class="mt-2">
        If you already have the 0.3.0 or 0.3.1 Flatpak (<code>dev.tensorseed.desktop</code>), this release installs beside
        it. Config, keys, catalogs, and pulled models are shared; Desktop preferences start fresh. Once the new one runs,
        you can remove the old one with <code>flatpak uninstall dev.tensorseed.desktop</code>.
      </p>
    </div>
    <div class="min-w-0">
      <h3 class="font-semibold" id="updates">Updates</h3>
      <p class="mt-1">
        The AppImage and the Windows installers update themselves after you confirm, and only with an update signed by the
        Tensorseed Desktop update key. The .deb and .rpm update through your package manager. The Flatpak bundle does not
        update itself; install the next release&rsquo;s bundle the same way.
      </p>
    </div>
  </div>

  <div role="note" class="alert alert-warning mt-6 block" id="smartscreen">
    <p>
      <strong>Windows SmartScreen.</strong> The Windows installers are not code signed yet, so Microsoft Defender SmartScreen
      may show &ldquo;Windows protected your PC&rdquo; when you start one. Check the download against <code>SHA256SUMS</code>
      first (see <a class="link" href="/verify#release">Verify a release</a>), then choose <strong>More info</strong> and
      <strong>Run anyway</strong>. Your browser may also call the file uncommon.
    </p>
    <p class="mt-2">
      Both installers offer an optional Windows Firewall rule (off by default) so other peers can connect while you seed;
      without it, Windows asks the first time Desktop seeds.
    </p>
  </div>
</section>

<section class="mt-12" aria-labelledby="cli">
  <h2 id="cli" class="text-2xl font-bold">Command line</h2>
  <p class="mt-2 max-w-3xl">The <code>tensorseed</code> CLI: binaries, packages, and source archives.</p>
  <div class="mt-4"><AssetList assets={CLI_ASSETS} pick={cliPick} /></div>
  <p class="mt-4 max-w-3xl">
    There is no macOS build. The source archives are the tracked files from tag {TAG} and are listed in
    <code>SHA256SUMS</code>. You can also build from a checkout with <code>cargo build --release</code> or
    <code>cargo install --locked --path crates/tensorseed-cli</code> (Rust 1.87 or newer), or from GitHub with
    <code>cargo install --locked --git {REPO_URL}</code>.
  </p>
</section>

<section class="mt-12 max-w-3xl" aria-labelledby="verify-js">
  <h2 id="verify-js" class="text-2xl font-bold">JavaScript package</h2>
  <p class="mt-2">
    Check served-weights claims and signed responses in Node or a browser with
    <a class="link" href="https://www.npmjs.com/package/@tensorseed/verify">@tensorseed/verify</a>
    on npm (AGPL-3.0-or-later):
  </p>
  <div class="mt-2"><Pre code={"pnpm add @tensorseed/verify\n# or: npm install @tensorseed/verify"} /></div>
  <p class="mt-3">
    Usage is in the
    <a class="link" href="/guides/integrate-a-marketplace#56-verify-in-javascript">integration guide</a>.
  </p>
</section>

<section class="mt-12" aria-labelledby="checksums">
  <h2 id="checksums" class="text-2xl font-bold">Checksums and signature</h2>
  <p class="mt-2 max-w-3xl">Also download these three files, then <a class="link" href="/verify#release">verify as described here</a>.</p>
  <div class="mt-4"><AssetList assets={VERIFY_ASSETS} /></div>
</section>
