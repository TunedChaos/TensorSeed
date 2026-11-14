<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  import { onMount } from "svelte";
  import ThemeToggle from "@tensorseed/ui/components/ThemeToggle.svelte";
  import CatalogsView from "./lib/CatalogsView.svelte";
  import EntryPage from "./lib/EntryPage.svelte";
  import LocalView from "./lib/LocalView.svelte";
  import SettingsView from "./lib/SettingsView.svelte";
  import StatusBar from "./lib/StatusBar.svelte";
  import { initialRoute, loadTheme, onDeepLink, saveTheme, type DeepLink } from "./lib/native";
  import { parseHash, routeHash, type View } from "./lib/router";

  let view = $state<View>(parseHash(location.hash));
  let linkError = $state<DeepLink | null>(null);

  function follow(link: DeepLink) {
    if (link.route) {
      linkError = null;
      const h = routeHash(link.route);
      if (location.hash === h) view = parseHash(h);
      else location.hash = h;
    } else {
      linkError = link;
    }
  }

  onMount(() => {
    const onHash = () => (view = parseHash(location.hash));
    window.addEventListener("hashchange", onHash);
    let unlisten: (() => void) | undefined;
    onDeepLink(follow).then((u) => (unlisten = u));
    initialRoute()
      .then((l) => l && follow(l))
      .catch(() => {});
    return () => {
      window.removeEventListener("hashchange", onHash);
      unlisten?.();
    };
  });

  const tabs = [
    { href: "#/", label: "Catalogs", kinds: ["catalogs", "entry"] },
    { href: "#/local", label: "Local copies", kinds: ["local"] },
    { href: "#/settings", label: "Settings", kinds: ["settings"] },
  ];
</script>

<div class="flex h-screen flex-col">
  <header class="navbar bg-base-200 flex-wrap gap-x-4 gap-y-2 px-4">
    <h1 class="text-lg font-semibold">Tensorseed Desktop</h1>
    <nav aria-label="Sections">
      <ul class="menu menu-horizontal menu-sm gap-1 p-0">
        {#each tabs as t (t.href)}
          <li><a href={t.href} class:menu-active={t.kinds.includes(view.kind)} aria-current={t.kinds.includes(view.kind) ? "page" : undefined}>{t.label}</a></li>
        {/each}
      </ul>
    </nav>
    <div class="flex-1"></div>
    <ThemeToggle load={loadTheme} save={saveTheme} />
  </header>

  {#if linkError}
    <div class="alert alert-error m-4" role="alert">
      <span class="min-w-0 wrap-anywhere">Could not open <code>{linkError.url}</code>: {linkError.error}</span>
      <button type="button" class="btn btn-sm" onclick={() => (linkError = null)}>Dismiss</button>
    </div>
  {/if}

  <main class="flex min-h-0 min-w-0 flex-1 flex-col gap-4 overflow-auto p-4">
    {#if view.kind === "catalogs"}
      <CatalogsView />
    {:else if view.kind === "local"}
      <LocalView />
    {:else if view.kind === "settings"}
      <SettingsView />
    {:else if view.kind === "entry"}
      <EntryPage entry={{ repoType: view.repoType, id: view.id, revision: view.revision, catalog: view.catalog }} />
    {:else}
      <div class="alert alert-warning" role="status"><span>Nothing here: <code>{view.hash}</code>. <a class="link" href="#/">Back to catalogs</a></span></div>
    {/if}
  </main>

  <footer class="bg-base-200 border-base-300 border-t px-4 py-1">
    <StatusBar />
  </footer>
</div>
