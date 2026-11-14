<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  import { assetUrl, type Asset } from "#lib/release.ts";

  let { assets, pick = "" }: { assets: Asset[]; pick?: string } = $props();
</script>

<ul class="divide-base-300 border-base-300 rounded-box divide-y border">
  {#each assets as a (a.file)}
    <li class="flex flex-col gap-1 px-4 py-3 sm:flex-row sm:items-baseline sm:justify-between sm:gap-4" class:bg-base-200={a.file === pick}>
      <span>
        <a class="link link-primary font-medium" href={assetUrl(a.file)}>{a.label}</a>
        {#if a.file === pick}<span class="badge badge-primary badge-sm ml-2">Suggested for you</span>{/if}
        {#if a.note}<span class="text-base-content/70 block text-sm sm:inline sm:before:content-['_\00b7_']">{a.note}</span>{/if}
      </span>
      <code class="text-base-content/70 text-xs break-all sm:text-right">{a.file}</code>
    </li>
  {/each}
</ul>
