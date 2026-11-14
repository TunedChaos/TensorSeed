<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  import Seo from "#lib/components/Seo.svelte";
  import { GUIDES } from "#lib/guides.ts";
</script>

<Seo title="Guides" description="Guides for Tensorseed operators and marketplaces: run a mirror, publish a catalog, sell inference, and integrate a marketplace." />

<header class="max-w-3xl">
  <h1 class="text-4xl font-bold">Guides</h1>
  <p class="mt-3 text-lg">Guides for operators and marketplaces. Each one is the same file as in the repo&rsquo;s <code>docs/</code> folder.</p>
</header>

<ul class="mt-8 grid gap-4 md:grid-cols-2">
  {#each GUIDES as g (g.slug)}
    <li>
      <a class="card bg-base-200 hover:bg-base-300 h-full transition-colors" href="/guides/{g.slug}">
        <div class="card-body">
          <h2 class="card-title">{g.title}</h2>
          <p>{g.summary}</p>
        </div>
      </a>
    </li>
  {/each}
</ul>
