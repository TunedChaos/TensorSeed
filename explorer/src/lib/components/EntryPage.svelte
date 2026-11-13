<script lang="ts">
  import { formatBytes, pullCommand } from "../format";
  import { loadManifest } from "../catalog";
  import type { CatalogVerifier } from "../wasm";
  import type { ListedEntry } from "../types";
  import CommandBox from "./CommandBox.svelte";

  let { verifier, entry }: { verifier: CatalogVerifier; entry: ListedEntry } = $props();

  let manifest = $derived(loadManifest(verifier, entry));
  let selected = $state<Record<string, boolean>>({});

  let includes = $derived(Object.entries(selected).filter(([, v]) => v).map(([k]) => k));
  let command = $derived(pullCommand(entry.id, entry.repoType, includes));
  let fullCommand = $derived(pullCommand(entry.id, entry.repoType, []));

  function toggle(path: string, on: boolean) {
    selected = { ...selected, [path]: on };
  }
</script>

<article class="flex flex-col gap-6">
  <div>
    <p class="text-sm"><a class="link" href="#/">All entries</a></p>
    <h1 class="mt-2 text-3xl font-semibold">{entry.id}</h1>
    <p class="mt-1">
      <span class="badge badge-outline">{entry.repoType}</span>
      {#if entry.index.license_spdx}
        <span class="badge badge-outline">{entry.index.license_spdx}</span>
      {/if}
      {#if entry.index.taken_down}
        <span class="badge badge-warning">Taken down</span>
      {/if}
    </p>
  </div>

  {#if !manifest}
    <p role="status">The manifest for this revision is not in the catalog.</p>
  {:else}
    <section class="grid gap-3 sm:grid-cols-2">
      <div>
        <h2 class="text-lg font-medium">Revision</h2>
        <dl class="mt-2 space-y-1 text-sm">
          <div><dt class="inline font-medium">Commit:</dt> <dd class="inline break-all"><code>{manifest.hf_commit}</code></dd></div>
          <div><dt class="inline font-medium">Size:</dt> <dd class="inline">{formatBytes(manifest.totals.bytes)} in {manifest.totals.files} files</dd></div>
          <div>
            <dt class="inline font-medium">License:</dt>
            <dd class="inline">{manifest.license.spdx ?? manifest.license.hf_tag ?? "unknown"} ({manifest.license.class})</dd>
          </div>
          {#if manifest.license.attribution}
            <div><dt class="font-medium">Attribution:</dt> <dd>{manifest.license.attribution}</dd></div>
          {/if}
          <div>
            <dt class="inline font-medium">Upstream:</dt>
            <dd class="inline"><a class="link" href={manifest.repo.hf_url} rel="noreferrer">{manifest.repo.hf_url}</a></dd>
          </div>
        </dl>
      </div>
      <div>
        <h2 class="text-lg font-medium">Distribution</h2>
        {#if manifest.torrent}
          <ul class="mt-2 space-y-1 text-sm">
            <li>
              Torrent:
              {#each manifest.torrent.torrent_file_urls as url, i}
                <a class="link" href={url}>{i === 0 ? "download .torrent" : url}</a>
              {:else}
                no torrent URL published
              {/each}
            </li>
            <li class="break-all">
              Magnet: <a class="link" href={manifest.torrent.magnet}>open magnet link</a>
            </li>
            <li>Name: <code>{manifest.torrent.name}</code></li>
          </ul>
        {:else}
          <p class="mt-2 text-sm">No torrent is published for this revision.</p>
        {/if}
        {#if manifest.webseeds.length}
          <h3 class="mt-3 font-medium">Mirrors (webseeds)</h3>
          <ul class="list-disc ps-5 text-sm">
            {#each manifest.webseeds as ws}
              <li class="break-all"><a class="link" href={ws} rel="noreferrer">{ws}</a></li>
            {/each}
          </ul>
        {/if}
      </div>
    </section>

    <section class="flex flex-col gap-3">
      <h2 class="text-lg font-medium">Pull command</h2>
      <p class="text-sm">
        Run this with Tensorseed 0.2.0 or later. Tick files below to add
        <code>--include</code> flags. Datasets always get <code>--type dataset</code>.
      </p>
      <CommandBox label="Pull the whole revision" command={fullCommand} />
      {#if includes.length}
        <CommandBox label="Pull selected files" command={command} />
      {/if}
    </section>

    <section>
      <h2 class="text-lg font-medium">Files</h2>
      <div class="mt-2 overflow-x-auto">
        <table class="table table-sm">
          <thead>
            <tr>
              <th scope="col">Include</th>
              <th scope="col">Path</th>
              <th scope="col">Size</th>
              <th scope="col">SHA-256</th>
            </tr>
          </thead>
          <tbody>
            {#each manifest.files as f (f.path)}
              <tr>
                <td>
                  <input type="checkbox" class="checkbox checkbox-sm" checked={!!selected[f.path]} aria-label="Include {f.path}" onchange={(ev) => toggle(f.path, ev.currentTarget.checked)} />
                </td>
                <td>
                  <code class="text-xs">{f.path}</code>
                  {#if f.pickle_risk}
                    <span class="badge badge-warning badge-sm ms-1">pickle</span>
                  {/if}
                </td>
                <td>{formatBytes(f.size)}</td>
                <td class="break-all"><code class="text-xs">{f.sha256}</code></td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    </section>
  {/if}
</article>
