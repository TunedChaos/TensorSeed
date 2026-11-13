<script lang="ts">
  import { onMount } from "svelte";
  import ThemeToggle from "@tensorseed/ui/components/ThemeToggle.svelte";
  import StatusBanner from "@tensorseed/ui/components/StatusBanner.svelte";
  import EntryList from "@tensorseed/ui/components/EntryList.svelte";
  import EntryPage from "./lib/components/EntryPage.svelte";
  import CatalogsPage from "./lib/components/CatalogsPage.svelte";
  import { agreements, loadCatalog, type LoadedCatalog } from "./lib/catalog";
  import {
    loadActiveCatalog,
    loadCatalogs,
    loadTheme,
    saveActiveCatalog,
    saveCatalogs,
    saveTheme,
  } from "./lib/storage";
  import type { CatalogConfig, VerifyMode } from "@tensorseed/ui/types";

  type Route =
    | { page: "list" }
    | { page: "catalogs" }
    | { page: "entry"; repoType: "model" | "dataset"; id: string };

  function parseHash(): Route {
    const raw = (location.hash || "#/").replace(/^#/, "");
    const path = raw.replace(/^\/+/, "");
    const parts = path.split("/").filter(Boolean);
    if (parts[0] === "catalogs") return { page: "catalogs" };
    if (parts[0] === "entry" && (parts[1] === "model" || parts[1] === "dataset") && parts.length >= 4) {
      return { page: "entry", repoType: parts[1], id: parts.slice(2).join("/") };
    }
    return { page: "list" };
  }

  let route = $state<Route>(parseHash());
  let catalogs = $state<CatalogConfig[]>(loadCatalogs());
  let activeName = $state(loadActiveCatalog());
  let loaded = $state<LoadedCatalog[]>([]);
  let progress = $state("Starting");
  let error = $state("");
  let ready = $state(false);

  let active = $derived(loaded.find((c) => c.config.name === activeName) ?? loaded[0] ?? null);
  let allAgreements = $derived(agreements(loaded));

  async function refresh() {
    ready = false;
    error = "";
    progress = "Loading catalogs";
    const next: LoadedCatalog[] = [];
    for (const cfg of catalogs) {
      try {
        const result = await loadCatalog(cfg, (msg) => {
          progress = `${cfg.displayName || cfg.name}: ${msg}`;
        });
        next.push(result);
      } catch (err) {
        error = `${cfg.name}: ${err instanceof Error ? err.message : String(err)}`;
        next.push({
          config: cfg,
          verifier: undefined as never,
          status: { kind: "unverified", reason: error, warnings: [error] },
          entries: [],
          info: null,
        } as LoadedCatalog);
      }
    }
    loaded = next;
    if (!loaded.some((c) => c.config.name === activeName) && loaded[0]) {
      activeName = loaded[0].config.name;
      saveActiveCatalog(activeName);
    }
    ready = true;
  }

  function selectCatalog(name: string) {
    activeName = name;
    saveActiveCatalog(name);
  }

  function addCatalog(c: CatalogConfig) {
    catalogs = [...catalogs, c];
    saveCatalogs(catalogs);
    activeName = c.name;
    saveActiveCatalog(c.name);
    void refresh();
  }

  function removeCatalog(name: string) {
    catalogs = catalogs.filter((c) => c.name !== name);
    saveCatalogs(catalogs);
    if (activeName === name) {
      activeName = catalogs[0]?.name ?? "tensorseed";
      saveActiveCatalog(activeName);
    }
    void refresh();
  }

  function setMode(name: string, mode: VerifyMode) {
    catalogs = catalogs.map((c) => (c.name === name ? { ...c, mode } : c));
    saveCatalogs(catalogs);
    void refresh();
  }

  onMount(() => {
    const onHash = () => {
      route = parseHash();
    };
    window.addEventListener("hashchange", onHash);
    void refresh();
    return () => window.removeEventListener("hashchange", onHash);
  });
</script>

<a class="btn btn-sm sr-only focus:not-sr-only focus:absolute focus:m-2 focus:z-50" href="#main">Skip to content</a>

<div class="bg-base-100 min-h-screen">
  <header class="navbar bg-base-200 px-4">
    <div class="flex-1 gap-3">
      <a class="btn btn-ghost text-xl" href="#/">Tensorseed explorer</a>
      {#if catalogs.length > 1}
        <label class="flex items-center gap-2 text-sm">
          <span class="sr-only">Active catalog</span>
          <select class="select select-bordered select-sm" bind:value={activeName} onchange={() => selectCatalog(activeName)}>
            {#each catalogs as c}
              <option value={c.name}>{c.displayName || c.name}</option>
            {/each}
          </select>
        </label>
      {/if}
    </div>
    <nav class="flex flex-wrap items-center gap-2" aria-label="Site">
      <a class="btn btn-ghost btn-sm" href="#/">Catalog</a>
      <a class="btn btn-ghost btn-sm" href="#/catalogs">Catalogs</a>
      <a class="btn btn-ghost btn-sm" href="https://tensorseed.dev">tensorseed.dev</a>
      <ThemeToggle load={loadTheme} save={saveTheme} />
    </nav>
  </header>

  <main id="main" class="mx-auto flex max-w-6xl flex-col gap-6 p-4 sm:p-6">
    {#if !ready}
      <StatusBanner status={{ kind: "loading", detail: progress }} />
    {:else if active}
      {#if error && !active.verifier}
        <StatusBanner status={{ kind: "unverified", reason: error, warnings: [error] }} />
      {:else}
        <StatusBanner status={active.status} />
        {#if route.page === "catalogs"}
          <CatalogsPage catalogs={catalogs} agreements={allAgreements} onadd={addCatalog} onremove={removeCatalog} onmode={setMode} />
        {:else if route.page === "entry"}
          {@const entry = active.entries.find((e) => e.repoType === route.repoType && e.id === route.id)}
          {#if !entry || !active.verifier}
            <p role="status">That entry is not in the current catalog.</p>
            <a class="link" href="#/">Back to the list</a>
          {:else}
            <EntryPage verifier={active.verifier} {entry} />
          {/if}
        {:else}
          {#if active.info?.display_name}
            <p class="text-sm">
              Published by {active.info.display_name}
              {#if active.info.contact}({active.info.contact}){/if}
            </p>
          {/if}
          <EntryList entries={active.entries} />
        {/if}
      {/if}
    {:else}
      <p role="status">No catalog is loaded.</p>
    {/if}
  </main>

  <footer class="footer footer-center bg-base-200 text-base-content mt-8 p-4 text-sm">
    <p>
      Signatures and the Chia anchor are checked in this browser against the pinned
      publisher key. This page does not run a wallet. Source:
      <a class="link" href="https://github.com/TunedChaos/TensorSeed">TunedChaos/TensorSeed</a>.
    </p>
  </footer>
</div>
