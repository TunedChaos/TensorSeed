<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  import Seo from "#lib/components/Seo.svelte";
  import { GUIDES } from "#lib/guides.ts";

  let { data } = $props();
  let toc = $derived(data.headings.filter((h) => h.depth === 2));
  let others = $derived(GUIDES.filter((g) => g.slug !== data.guide.slug));
</script>

<Seo title={data.guide.title} description={data.guide.summary} type="article" />

<nav class="breadcrumbs text-sm" aria-label="Breadcrumb">
  <ul>
    <li><a href="/guides">Guides</a></li>
    <li><span aria-current="page">{data.guide.title}</span></li>
  </ul>
</nav>

<div class="mt-4 grid gap-10 lg:grid-cols-[minmax(0,1fr)_16rem]">
  <article class="prose-ts min-w-0">
    <!-- Rendered at build time from the repo's own Markdown. -->
    {@html data.html}
    <p class="text-base-content/70 mt-10 text-sm">
      Source: <a href={data.sourceUrl}>docs/{data.guide.file}</a> on GitHub.
    </p>
    <nav class="border-base-300 mt-6 border-t pt-4 lg:hidden" aria-label="Other guides">
      <p class="font-semibold">Other guides</p>
      <ul>
        {#each others as g (g.slug)}<li><a href="/guides/{g.slug}">{g.title}</a></li>{/each}
      </ul>
    </nav>
  </article>
  <aside class="order-first lg:order-none">
    <div class="lg:sticky lg:top-20">
      {#if toc.length}
        <details class="bg-base-200 rounded-box p-4 lg:hidden">
          <summary class="cursor-pointer font-semibold">On this page</summary>
          <ul class="mt-2 space-y-1 text-sm">
            {#each toc as h (h.id)}<li><a class="link link-hover" href="#{h.id}">{h.text}</a></li>{/each}
          </ul>
        </details>
        <nav class="hidden lg:block" aria-label="On this page">
          <p class="font-semibold">On this page</p>
          <ul class="mt-2 space-y-1 text-sm">
            {#each toc as h (h.id)}<li><a class="link link-hover" href="#{h.id}">{h.text}</a></li>{/each}
          </ul>
        </nav>
      {/if}
      <nav class="mt-6 hidden lg:block" aria-label="Other guides">
        <p class="font-semibold">Other guides</p>
        <ul class="mt-2 space-y-1 text-sm">
          {#each others as g (g.slug)}<li><a class="link link-hover" href="/guides/{g.slug}">{g.title}</a></li>{/each}
        </ul>
      </nav>
    </div>
  </aside>
</div>
