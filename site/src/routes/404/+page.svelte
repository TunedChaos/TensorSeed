<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  import { CATALOG_URL, NAV, SITE_NAME } from "#lib/site.ts";
</script>

<svelte:head>
  <title>Not found | {SITE_NAME}</title>
  <meta name="robots" content="noindex" />
</svelte:head>

<section class="max-w-2xl py-10">
  <p class="text-primary font-mono text-4xl font-bold">404</p>
  <h1 class="mt-2 text-3xl font-bold">This path is not on tensorseed.dev</h1>
  <p class="mt-3">
    The page you asked for is not here. The signed catalog lives at <a class="link" href={CATALOG_URL}>catalog.tensorseed.dev</a>.
  </p>
  <ul class="mt-6 flex flex-wrap gap-2">
    <li><a class="btn btn-primary btn-sm" href="/">Back to Tensorseed</a></li>
    {#each NAV as item (item.href)}<li><a class="btn btn-sm" href={item.href}>{item.label}</a></li>{/each}
  </ul>
</section>
