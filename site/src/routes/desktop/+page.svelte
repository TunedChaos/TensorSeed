<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  import Seo from "#lib/components/Seo.svelte";
  import { VERSION } from "#lib/release.ts";

  const features = [
    {
      title: "Catalogs",
      text: "Lists the catalogs you follow and their entries. Syncing runs the same Chia chain check as the CLI, and every entry signature is checked natively against the pinned publisher key.",
    },
    {
      title: "Entry pages",
      text: "One page per model or dataset: manifest, signature and anchor status, license class and commercial serving mark, the file list with hashes, and what this machine already has.",
    },
    {
      title: "Pull with progress",
      text: "Files, bytes, rate, and peers as it runs. Pause and resume keep partial downloads; cancel deletes them and keeps finished files. Tick files to pull only those.",
    },
    {
      title: "Downloads and Library",
      text: "Every pull in the job database the CLI uses, so either can resume the other's. The Library lists your copies with Verify, Open folder, and Delete.",
    },
    {
      title: "Seeding",
      text: "One seeder per machine: Desktop's own, or a running tensorseed serve that Desktop attaches to, with Pause and Resume per revision and for all. A tray icon shows what is running.",
    },
    {
      title: "Signed updates and Hugging Face sign in",
      text: "The AppImage and Windows builds update only after you confirm, and only with an update signed by the Desktop update key. Sign in to Hugging Face from Settings, which also holds every torrent option.",
    },
  ];

  const shots = [
    {
      src: "/screenshots/catalog.webp",
      w: 1181,
      h: 803,
      alt: "Tensorseed Desktop catalog view: verified and anchored on Chia, revision 4, 9 of 9 entry signatures verified, with a filterable list of models and datasets.",
      caption: "The catalog, verified against the pinned key and the Chia anchor.",
    },
    {
      src: "/screenshots/entry-pulled.webp",
      w: 1186,
      h: 813,
      alt: "An entry page after a pull on Windows: every file verified, signature verified, anchored on chain, license mit, commercial serving allowed.",
      caption: "An entry page after a pull: every file verified.",
    },
    {
      src: "/screenshots/seeding.webp",
      w: 1186,
      h: 813,
      alt: "The Seeding view with one dataset seeding, and Pause and Stop buttons.",
      caption: "Seeding what you pulled.",
    },
  ];
</script>

<Seo title="Desktop" description="Tensorseed Desktop for Linux and Windows: browse and verify catalogs, pull with progress, seed, signed in app updates, and Hugging Face sign in. No telemetry." />

<header class="max-w-3xl">
  <h1 class="text-4xl font-bold">Tensorseed Desktop</h1>
  <p class="mt-3 text-lg">
    Browse the catalog, check each entry&rsquo;s signature and Chia anchor, pull with progress, pause and resume, and seed
    what you pulled, without a terminal. Desktop shares its config, downloads, and seeder with the CLI on the same machine,
    and it sends no telemetry.
  </p>
  <div class="mt-6 flex flex-wrap gap-3">
    <a class="btn btn-primary" href="/download#desktop">Download Desktop {VERSION}</a>
    <span class="text-base-content/70 self-center text-sm">AppImage, .deb, .rpm, Flatpak, and Windows .exe or .msi.</span>
  </div>
</header>

<section class="mt-12 grid gap-4 sm:grid-cols-2 lg:grid-cols-3" aria-label="Features">
  {#each features as f (f.title)}
    <article class="card bg-base-200">
      <div class="card-body">
        <h2 class="card-title text-lg">{f.title}</h2>
        <p>{f.text}</p>
      </div>
    </article>
  {/each}
</section>

<section class="mt-12" aria-labelledby="screenshots">
  <h2 id="screenshots" class="text-2xl font-bold">Screenshots</h2>
  <div class="mt-4 grid gap-8">
    {#each shots as s, i (s.src)}
      <figure>
        <img
          class="border-base-300 rounded-box w-full border shadow"
          src={s.src}
          width={s.w}
          height={s.h}
          alt={s.alt}
          loading={i === 0 ? "eager" : "lazy"}
          decoding="async"
        />
        <figcaption class="text-base-content/70 mt-2 text-sm">{s.caption}</figcaption>
      </figure>
    {/each}
  </div>
</section>

<section class="mt-12 max-w-3xl">
  <h2 class="text-2xl font-bold">Good to know</h2>
  <ul class="mt-3 list-disc space-y-2 pl-6">
    <li>Desktop does not serve inference yet; use the CLI. See <a class="link" href="/serve">Serve and verify</a>.</li>
    <li>The Windows installers are not code signed yet. See the <a class="link" href="/download#smartscreen">SmartScreen note</a>.</li>
    <li>The Flatpak bundle is not on Flathub yet and does not update itself.</li>
  </ul>
</section>
