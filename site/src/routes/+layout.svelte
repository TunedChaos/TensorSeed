<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  import "../app.css";
  import SiteNav from "#lib/components/SiteNav.svelte";
  import { REPO_URL, VERSION } from "#lib/release.ts";

  let { children } = $props();
</script>

<div class="flex min-h-screen flex-col">
  <SiteNav />
  <main id="main" class="mx-auto w-full max-w-6xl flex-1 px-4 py-8 sm:py-12">
    {@render children()}
  </main>
  <footer class="border-base-300 bg-base-200 border-t">
    <div class="mx-auto flex max-w-6xl flex-col gap-3 px-4 py-6 text-sm sm:flex-row sm:items-center sm:justify-between">
      <p>Copyright Tuned Chaos. Licensed under AGPL-3.0-or-later. Version {VERSION}.</p>
      <ul class="flex flex-wrap gap-x-4 gap-y-1">
        <li><a class="link" href="/support">Support Tensorseed</a></li>
        <li><a class="link" href="/support#license">License</a></li>
        <li><a class="link" href={REPO_URL}>Source</a></li>
      </ul>
    </div>
  </footer>
</div>
