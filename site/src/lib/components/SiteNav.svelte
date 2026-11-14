<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  import { onMount } from "svelte";
  import { page } from "$app/state";
  import ThemeToggle from "@tensorseed/ui/components/ThemeToggle.svelte";
  import type { ThemePreference } from "@tensorseed/ui/types";
  import { NAV } from "#lib/site.ts";

  const THEME_KEY = "tensorseed.site.theme";

  function loadTheme(): ThemePreference {
    try {
      const t = localStorage.getItem(THEME_KEY);
      return t === "light" || t === "dark" ? t : "system";
    } catch {
      return "system";
    }
  }

  function saveTheme(t: ThemePreference) {
    try {
      localStorage.setItem(THEME_KEY, t);
    } catch {
      // Private mode: the choice lasts for this page only.
    }
  }

  // The theme buttons need JavaScript, so they only appear once it runs.
  // One toggle at a time: in the bar on wider screens, in the menu on phones.
  let mounted = $state(false);
  let wide = $state(true);
  onMount(() => {
    mounted = true;
    const mq = window.matchMedia("(min-width: 640px)");
    wide = mq.matches;
    const on = () => (wide = mq.matches);
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  });

  let menu: HTMLDetailsElement | undefined = $state();

  function current(href: string): boolean {
    const path = page.url.pathname;
    return path === href || path.startsWith(`${href}/`);
  }
</script>

<header class="bg-base-100/90 border-base-300 sticky top-0 z-20 border-b backdrop-blur">
  <a href="#main" class="sr-only focus:not-sr-only focus:absolute focus:left-2 focus:top-2 btn btn-sm">Skip to content</a>
  <nav class="navbar mx-auto max-w-6xl gap-2 px-4" aria-label="Main">
    <div class="flex-1">
      <a href="/" class="flex items-center gap-2 text-lg font-bold" aria-current={page.url.pathname === "/" ? "page" : undefined}>
        <img src="/favicon.svg" alt="" width="28" height="28" />
        Tensorseed
      </a>
    </div>
    <ul class="menu menu-horizontal hidden gap-1 px-1 lg:flex">
      {#each NAV as item (item.href)}
        <li>
          <a href={item.href} class:menu-active={current(item.href)} aria-current={current(item.href) ? "page" : undefined}>{item.label}</a>
        </li>
      {/each}
    </ul>
    {#if mounted && wide}
      <ThemeToggle load={loadTheme} save={saveTheme} />
    {/if}
    <details class="dropdown dropdown-end lg:hidden" bind:this={menu}>
      <summary class="btn btn-ghost" aria-label="Menu">
        <svg xmlns="http://www.w3.org/2000/svg" class="h-6 w-6" fill="none" viewBox="0 0 24 24" stroke="currentColor" aria-hidden="true"
          ><path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 6h16M4 12h16M4 18h16" /></svg
        >
        <span class="sr-only sm:not-sr-only">Menu</span>
      </summary>
      <div class="dropdown-content bg-base-100 rounded-box border-base-300 z-30 mt-2 w-64 border p-2 shadow-lg">
        <ul class="menu w-full p-0">
          {#each NAV as item (item.href)}
            <li>
              <a
                href={item.href}
                class:menu-active={current(item.href)}
                aria-current={current(item.href) ? "page" : undefined}
                onclick={() => menu?.removeAttribute("open")}>{item.label}</a
              >
            </li>
          {/each}
        </ul>
        {#if mounted && !wide}
          <p class="text-base-content/60 mt-2 px-3 text-sm font-semibold">Theme</p>
          <div class="px-3 py-2"><ThemeToggle load={loadTheme} save={saveTheme} /></div>
        {/if}
      </div>
    </details>
  </nav>
</header>
