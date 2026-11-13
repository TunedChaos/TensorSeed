<script lang="ts">
  import { formatBytes, sizeBucket } from "../format";
  import type { ListedEntry } from "../types";

  let {
    entries,
    entryHref = (e: ListedEntry) => `#/entry/${e.repoType}/${e.id}`,
    showSignature = false,
  }: {
    entries: ListedEntry[];
    /** Link target for an entry; the default is the explorer's hash route. */
    entryHref?: (e: ListedEntry) => string;
    /** Show the per entry signature column (sources that check entries one by one). */
    showSignature?: boolean;
  } = $props();

  let query = $state("");
  let typeFilter = $state<"all" | "model" | "dataset">("all");
  let licenseFilter = $state("all");
  let sizeFilter = $state<"all" | "tiny" | "small" | "medium" | "large">("all");

  let licenses = $derived.by(() => {
    const set = new Set<string>();
    for (const e of entries) {
      set.add(e.index.license_spdx ?? "unknown");
    }
    return [...set].sort();
  });

  let filtered = $derived.by(() => {
    const q = query.trim().toLowerCase();
    return entries.filter((e) => {
      if (typeFilter !== "all" && e.repoType !== typeFilter) return false;
      const lic = e.index.license_spdx ?? "unknown";
      if (licenseFilter !== "all" && lic !== licenseFilter) return false;
      if (sizeFilter !== "all" && sizeBucket(e.index.total_bytes) !== sizeFilter) return false;
      if (q && !e.id.toLowerCase().includes(q) && !lic.toLowerCase().includes(q)) return false;
      return true;
    });
  });
</script>

<div class="flex flex-col gap-4">
  <div class="flex flex-wrap items-end gap-3">
    <label class="form-control w-full max-w-xs">
      <span class="label py-1"><span class="label-text">Search</span></span>
      <input class="input input-bordered w-full" type="search" name="q" placeholder="Name or license" autocomplete="off" bind:value={query} />
    </label>
    <label class="form-control">
      <span class="label py-1"><span class="label-text">Type</span></span>
      <select class="select select-bordered" bind:value={typeFilter} aria-label="Filter by type">
        <option value="all">All types</option>
        <option value="model">Models</option>
        <option value="dataset">Datasets</option>
      </select>
    </label>
    <label class="form-control">
      <span class="label py-1"><span class="label-text">License</span></span>
      <select class="select select-bordered" bind:value={licenseFilter} aria-label="Filter by license">
        <option value="all">All licenses</option>
        {#each licenses as lic}
          <option value={lic}>{lic}</option>
        {/each}
      </select>
    </label>
    <label class="form-control">
      <span class="label py-1"><span class="label-text">Size</span></span>
      <select class="select select-bordered" bind:value={sizeFilter} aria-label="Filter by size">
        <option value="all">All sizes</option>
        <option value="tiny">Under 10 MiB</option>
        <option value="small">10 to 100 MiB</option>
        <option value="medium">100 MiB to 1 GiB</option>
        <option value="large">Over 1 GiB</option>
      </select>
    </label>
  </div>
  <p class="text-sm" aria-live="polite">{filtered.length} of {entries.length} entries</p>
  <div class="overflow-x-auto">
    <table class="table table-zebra">
      <thead>
        <tr>
          <th scope="col">Repository</th>
          <th scope="col">Type</th>
          <th scope="col">License</th>
          <th scope="col">Size</th>
          <th scope="col">Commit</th>
          {#if showSignature}
            <th scope="col">Signature</th>
          {/if}
        </tr>
      </thead>
      <tbody>
        {#each filtered as e (e.indexKey)}
          <tr>
            <th scope="row">
              <a class="link link-primary" href={entryHref(e)}>{e.id}</a>
              {#if e.index.taken_down}
                <span class="badge badge-warning ms-2">Taken down</span>
              {/if}
            </th>
            <td>{e.repoType}</td>
            <td>{e.index.license_spdx ?? "unknown"}</td>
            <td>{formatBytes(e.index.total_bytes)}</td>
            <td><code class="text-xs">{e.index.latest_commit.slice(0, 12)}</code></td>
            {#if showSignature}
              <td>
                {#if e.signature?.ok}
                  <span class="badge badge-success">Verified</span>
                {:else if e.signature}
                  <span class="badge badge-error" title={e.signature.error}>Failed</span>
                {:else}
                  <span class="badge badge-ghost">Not checked</span>
                {/if}
              </td>
            {/if}
          </tr>
        {:else}
          <tr>
            <td colspan={showSignature ? 6 : 5}>No entries match these filters.</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
</div>
