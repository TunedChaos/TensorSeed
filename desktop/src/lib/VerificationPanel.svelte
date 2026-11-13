<script lang="ts">
  import { shortHex } from "@tensorseed/ui/format";
  import type { CatalogEntries } from "@tensorseed/ui/source";

  let { data }: { data: CatalogEntries } = $props();

  let open = $state(false);

  let signed = $derived(data.entries.filter((e) => e.signature?.ok).length);
  let failed = $derived(data.entries.length - signed);
  let anchored = $derived(data.chain.kind === "anchored" || data.chain.kind === "datalayer");
  let chainOk = $derived(anchored || data.chain.kind === "off");
  let level = $derived(
    failed > 0 || data.problems.length > 0
      ? "alert-error"
      : !chainOk || data.syncError
        ? "alert-warning"
        : "alert-success",
  );

  function checkedLabel(unix: number): string {
    const d = new Date(unix * 1000);
    return Number.isNaN(d.getTime()) ? "unknown" : d.toLocaleString();
  }
</script>

<div class="alert {level}" role="status" aria-live="polite">
  <div class="w-full">
    <p class="font-semibold">
      {#if failed > 0}
        {failed} of {data.entries.length} entries failed signature checks
      {:else if anchored}
        Verified and anchored on Chia
      {:else if data.chain.kind === "off"}
        Signatures verified (chain check off)
      {:else}
        Signatures verified, chain anchor not confirmed
      {/if}
    </p>
    <p>
      {#if data.revision}
        Revision {data.revision}, root <code class="text-sm">{shortHex(data.root, 12, 8)}</code>.
      {/if}
      {signed} of {data.entries.length} entry signatures verified natively against the pinned publisher key.
      {data.synced ? "Synced just now." : "Showing the local copy."}
    </p>
    {#if data.syncError}
      <p class="mt-1 text-sm">Sync failed, so this is the last accepted local copy: {data.syncError}</p>
    {/if}
    {#if data.chain.kind === "unchecked"}
      <p class="mt-1 text-sm">Chain anchor: {data.chain.reason}.</p>
    {/if}
    {#each data.problems as p}
      <p class="mt-1 text-sm">Unreadable entry: {p}</p>
    {/each}
    <button type="button" class="btn btn-ghost btn-xs mt-2" onclick={() => (open = !open)} aria-expanded={open}>
      {open ? "Hide details" : "Show details"}
    </button>
    {#if open}
      <dl class="mt-2 grid gap-1 text-sm">
        <div><dt class="inline font-medium">Catalog:</dt> <dd class="inline">{data.catalog.name} ({data.catalog.backend} backend, chain check {data.catalog.chainCheck})</dd></div>
        <div><dt class="inline font-medium">Base URL:</dt> <dd class="inline break-all"><code>{data.catalog.baseUrl}</code></dd></div>
        {#if data.root}
          <div><dt class="inline font-medium">Root:</dt> <dd class="inline break-all"><code>{data.root}</code></dd></div>
        {/if}
        {#if data.chain.kind === "anchored"}
          <div><dt class="inline font-medium">Store:</dt> <dd class="inline break-all"><code>{data.chain.storeId}</code></dd></div>
          <div><dt class="inline font-medium">Chain root:</dt> <dd class="inline break-all"><code>{data.chain.datalayerRoot}</code></dd></div>
          <div><dt class="inline font-medium">Coin:</dt> <dd class="inline break-all"><code>{data.chain.coinId}</code></dd></div>
          <div><dt class="inline font-medium">Coin state:</dt> <dd class="inline">{data.chain.coinState}</dd></div>
          <div><dt class="inline font-medium">Proof from:</dt> <dd class="inline break-all">{data.chain.via}</dd></div>
          <div><dt class="inline font-medium">Checked:</dt> <dd class="inline">{checkedLabel(data.chain.checkedUnix)}</dd></div>
        {:else if data.chain.kind === "datalayer"}
          <div><dt class="inline font-medium">Store:</dt> <dd class="inline break-all"><code>{data.chain.storeId}</code></dd></div>
        {/if}
      </dl>
    {/if}
  </div>
</div>
