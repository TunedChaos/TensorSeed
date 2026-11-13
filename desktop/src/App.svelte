<script lang="ts">
  import { onMount } from "svelte";
  import ThemeToggle from "@tensorseed/ui/components/ThemeToggle.svelte";
  import EntryList from "@tensorseed/ui/components/EntryList.svelte";
  import type { CatalogEntries, CatalogSummary } from "@tensorseed/ui/source";
  import VerificationPanel from "./lib/VerificationPanel.svelte";
  import { catalogsList, loadTheme, nativeSource, onDeepLink, saveTheme } from "./lib/native";

  let configPath = $state("");
  let catalogs = $state<CatalogSummary[]>([]);
  let listError = $state<string | null>(null);
  let selected = $state<string | null>(null);
  let data = $state<CatalogEntries | null>(null);
  let loading = $state(false);
  let loadError = $state<string | null>(null);
  let deepLink = $state<string | null>(null);

  async function refreshList() {
    listError = null;
    try {
      const res = await catalogsList();
      configPath = res.configPath;
      catalogs = res.catalogs;
    } catch (err) {
      listError = String(err);
    }
  }

  async function open(name: string, sync: boolean) {
    selected = name;
    loading = true;
    loadError = null;
    try {
      data = await nativeSource.loadEntries(name, { sync });
      await refreshList();
    } catch (err) {
      data = null;
      loadError = String(err);
    } finally {
      loading = false;
    }
  }

  onMount(() => {
    let unlisten: (() => void) | undefined;
    onDeepLink((url) => (deepLink = url)).then((u) => (unlisten = u));
    (async () => {
      await refreshList();
      const first = catalogs.find((c) => c.isDefault) ?? catalogs[0];
      if (first) await open(first.name, true);
    })();
    return () => unlisten?.();
  });
</script>

<div class="flex min-h-screen flex-col">
  <header class="navbar bg-base-200 gap-4 px-4">
    <div class="flex-1">
      <h1 class="text-lg font-semibold">Tensorseed Desktop</h1>
    </div>
    <ThemeToggle load={loadTheme} save={saveTheme} />
  </header>

  {#if deepLink}
    <div class="alert alert-info m-4" role="status">
      <span>Opened <code>{deepLink}</code>. Entry links open here in a later release.</span>
      <button type="button" class="btn btn-sm" onclick={() => (deepLink = null)}>Dismiss</button>
    </div>
  {/if}

  <div class="flex flex-1 flex-col gap-4 p-4 md:flex-row">
    <nav class="md:w-72 md:shrink-0" aria-label="Followed catalogs">
      <h2 class="mb-2 font-semibold">Catalogs</h2>
      {#if listError}
        <div class="alert alert-error" role="alert"><span class="min-w-0 wrap-anywhere">{listError}</span></div>
      {:else if catalogs.length === 0}
        <p class="text-sm">No catalogs are configured yet. Add one with <code>tensorseed catalog add</code>.</p>
      {/if}
      <ul class="menu bg-base-200 rounded-box w-full">
        {#each catalogs as c (c.name)}
          <li>
            <button type="button" class:menu-active={selected === c.name} onclick={() => open(c.name, !c.localRevision)}>
              <span class="flex flex-col items-start">
                <span class="font-medium">{c.label}
                  {#if c.isDefault}<span class="badge badge-sm ms-1">default</span>{/if}
                </span>
                <span class="text-xs opacity-70">
                  chain check {c.chainCheck}, {c.localRevision ? `revision ${c.localRevision}` : "not synced"}
                </span>
                {#if c.error}<span class="text-error text-xs">{c.error}</span>{/if}
              </span>
            </button>
          </li>
        {/each}
      </ul>
      {#if configPath}
        <p class="mt-2 text-xs break-all opacity-70">Config: <code>{configPath}</code></p>
      {/if}
    </nav>

    <main class="flex min-w-0 flex-1 flex-col gap-4">
      {#if selected}
        <div class="flex flex-wrap items-center gap-2">
          <h2 class="text-xl font-semibold">{data?.catalog.label ?? selected}</h2>
          <div class="flex-1"></div>
          <button type="button" class="btn btn-primary btn-sm" disabled={loading} onclick={() => selected && open(selected, true)}>
            Sync and verify
          </button>
        </div>
        {#if loading}
          <div class="alert" role="status" aria-live="polite">
            <span class="loading loading-spinner loading-sm" aria-hidden="true"></span>
            <span>Syncing and verifying natively</span>
          </div>
        {:else if loadError}
          <div class="alert alert-error" role="alert"><span class="min-w-0 wrap-anywhere">{loadError}</span></div>
        {:else if data}
          <VerificationPanel {data} />
          <EntryList entries={data.entries} showSignature entryHref={(e) => `#/entry/${e.repoType}/${e.id}`} />
        {/if}
      {:else}
        <p>Pick a catalog.</p>
      {/if}
    </main>
  </div>
</div>
