<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  import Seo from "#lib/components/Seo.svelte";
  import { CATALOG_URL, EXPLORER_URL, HASH_REDIRECTS, SUBLINE, TAGLINE } from "#lib/site.ts";
  import { VERSION } from "#lib/release.ts";

  // Links into the old one page site (/#download and so on) keep working:
  // this runs before the page is drawn and forwards to the new route.
  const redirectScript = `<script>(function(){var m=${JSON.stringify(HASH_REDIRECTS)};var h=location.hash.slice(1);if(h&&Object.prototype.hasOwnProperty.call(m,h))location.replace(m[h]);})();<\/script>`;
</script>

<Seo />

<svelte:head>
  {@html redirectScript}
</svelte:head>

<section class="py-6 sm:py-12">
  <p class="text-primary font-mono text-sm font-semibold uppercase tracking-[0.16em]">Tensorseed</p>
  <h1 class="mt-3 max-w-[24ch] text-4xl font-bold leading-tight tracking-tight sm:text-5xl">{TAGLINE}</h1>
  <p class="text-base-content/80 mt-3 text-xl sm:text-2xl">{SUBLINE}</p>
  <p class="mt-6 max-w-2xl text-lg leading-relaxed">
    Tensorseed distributes verified weights. It mirrors Hugging Face models and datasets and moves the bytes over BitTorrent v2 and hybrid torrents, plus HTTP
    and S3-compatible mirrors. A signed catalog, anchored on Chia, sits on top: find a revision, check its signature, and
    fetch the same files from more than one place. Every byte is checked.
  </p>
  <div class="mt-8 flex flex-wrap gap-3">
    <a class="btn btn-primary btn-lg" href="/download#desktop">Download Desktop {VERSION}</a>
    <a class="btn btn-lg" href="/download#cli">Command line</a>
    <a class="btn btn-ghost btn-lg" href={EXPLORER_URL}>Browse the catalog</a>
  </div>
  <p class="text-base-content/70 mt-3 text-sm">Linux and Windows. No macOS build yet.</p>
</section>

<section class="grid gap-4 md:grid-cols-3" aria-label="What you can do">
  <article class="card bg-base-200">
    <div class="card-body">
      <h2 class="card-title">Download</h2>
      <p>
        Pull a model or dataset by name. Tensorseed checks the catalog signature and Chia anchor, then every file hash,
        before it calls a copy complete.
      </p>
      <div class="card-actions mt-2"><a class="link link-primary" href="/download">Get Tensorseed</a></div>
    </div>
  </article>
  <article class="card bg-base-200">
    <div class="card-body">
      <h2 class="card-title">Seed</h2>
      <p>
        Share what you pulled and verified over BitTorrent v2, with HTTP webseeds so a pull still works when the swarm is
        quiet. One seeder per machine, shared by Desktop and the CLI.
      </p>
      <div class="card-actions mt-2"><a class="link link-primary" href="/guides/run-a-mirror">Run a mirror</a></div>
    </div>
  </article>
  <article class="card bg-base-200">
    <div class="card-body">
      <h2 class="card-title">Serve</h2>
      <p>
        Optionally launch an OpenAI compatible engine you install separately on a verified copy, and sign every response
        so clients can check which exact weights answered.
      </p>
      <div class="card-actions mt-2"><a class="link link-primary" href="/serve">Serve and verify</a></div>
    </div>
  </article>
</section>

<section class="mt-14">
  <h2 class="text-2xl font-bold">How it works</h2>
  <div class="mt-4 grid gap-6 md:grid-cols-2">
    <div>
      <h3 class="font-semibold">Signed catalog</h3>
      <p class="mt-1">
        Each revision publishes a signed head, an append-only delta, and periodic snapshots. Clients pin a publisher key,
        walk a hash chain, and reject rollback, forks, and stale heads. This project&rsquo;s catalog is served from
        <a class="link" href={CATALOG_URL}>catalog.tensorseed.dev</a>.
      </p>
    </div>
    <div>
      <h3 class="font-semibold">Anchored on Chia</h3>
      <p class="mt-1">
        Chia DataLayer is the source of truth for the catalog. Every revision is anchored on Chia mainnet first, and the
        signed file tree served over HTTPS is a cache that clients check against the chain.
      </p>
    </div>
    <div>
      <h3 class="font-semibold">BitTorrent v2</h3>
      <p class="mt-1">
        Model and dataset bytes are published as BitTorrent v2 or hybrid torrents, with HTTP webseeds. Operators can seed
        what they already verified.
      </p>
    </div>
    <div>
      <h3 class="font-semibold">HTTP and S3 mirrors</h3>
      <p class="mt-1">
        Named, repeatable provider blocks describe R2, B2, S3, and other HTTP mirrors. A signed <code>mirrors.toml</code> is
        published with the catalog so clients can discover sources without a chain.
      </p>
    </div>
  </div>
</section>

<section class="mt-14">
  <h2 class="text-2xl font-bold">Status</h2>
  <div class="mt-3 max-w-3xl space-y-3">
    <p>
      Tensorseed is under active development. Version {VERSION} is an early release and the first with
      <a class="link" href="/desktop">Tensorseed Desktop</a>, an app for Linux and Windows. Formats and commands may still
      change before 1.0.
    </p>
    <p>
      The signed catalog is live at <a class="link" href={CATALOG_URL}>catalog.tensorseed.dev</a>, at revision 4 with nine
      official entries: the first model, TunedChaos/ChaosNexus_Tuned_v1-GGUF, plus eight models and datasets added in that
      revision. Object bytes are mirrored under <code>/t/</code> on <code>mirror.tensorseed.dev</code>. See the
      <a class="link" href="{CATALOG_URL}README.txt">catalog README</a> for the file layout, or browse it at
      <a class="link" href={EXPLORER_URL}>explore.tensorseed.dev</a>. To check it yourself, see
      <a class="link" href="/verify#catalog">Verify the catalog</a>.
    </p>
  </div>
</section>
