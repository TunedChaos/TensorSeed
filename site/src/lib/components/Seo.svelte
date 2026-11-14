<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  import { page } from "$app/state";
  import { DESCRIPTION, SITE_NAME, SITE_URL } from "#lib/site.ts";

  let {
    title,
    description = DESCRIPTION,
    type = "website",
  }: { title?: string; description?: string; type?: string } = $props();

  let fullTitle = $derived(title ? `${title} | ${SITE_NAME}` : `${SITE_NAME}: download, seed, and serve AI models you can verify`);
  let canonical = $derived(SITE_URL + (page.url.pathname === "/" ? "/" : page.url.pathname.replace(/\/$/, "")));
</script>

<svelte:head>
  <title>{fullTitle}</title>
  <meta name="description" content={description} />
  <link rel="canonical" href={canonical} />
  <meta property="og:site_name" content={SITE_NAME} />
  <meta property="og:type" content={type} />
  <meta property="og:title" content={fullTitle} />
  <meta property="og:description" content={description} />
  <meta property="og:url" content={canonical} />
  <meta property="og:image" content="{SITE_URL}/og.png" />
  <meta property="og:image:width" content="1200" />
  <meta property="og:image:height" content="630" />
  <meta property="og:image:alt" content="Tensorseed: download, seed, and serve AI models you can verify." />
  <meta name="twitter:card" content="summary_large_image" />
</svelte:head>
