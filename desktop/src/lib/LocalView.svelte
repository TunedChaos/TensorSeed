<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  import { onMount } from "svelte";
  import { formatBytes, shortHex } from "@tensorseed/ui/format";
  import { localList, onStatus, type LocalList, type Status } from "./native";
  import { entryHash } from "./router";
  import { copyBadge, fileCount } from "./copy";

  let data = $state<LocalList | null>(null);
  let error = $state<string | null>(null);
  let st = $state<Status | null>(null);

  async function refresh() {
    try {
      data = await localList();
      error = null;
    } catch (err) {
      error = String(err);
    }
  }

  onMount(() => {
    refresh();
    let unlisten: (() => void) | undefined;
    onStatus((s) => (st = s)).then((u) => (unlisten = u));
    return () => unlisten?.();
  });

  function seedState(dir: string): string | null {
    const t = st?.desktop.torrents.find((x) => x.dir === dir);
    return t ? (t.detail ? `${t.state}: ${t.detail}` : t.state) : null;
  }
</script>

<section class="flex min-w-0 flex-col gap-4" aria-labelledby="local-title">
  <div class="flex flex-wrap items-center gap-2">
    <h2 id="local-title" class="text-xl font-semibold">Local copies</h2>
    <div class="flex-1"></div>
    <button type="button" class="btn btn-sm" onclick={refresh}>Refresh</button>
  </div>
  {#if error}
    <div class="alert alert-error" role="alert"><span class="min-w-0 wrap-anywhere">{error}</span></div>
  {/if}
  {#if data}
    <p class="text-xs opacity-70">Pulled copies under <code class="break-all">{data.downloadDir}</code> (general.download_dir).</p>
    {#if data.copies.length === 0}
      <p>Nothing pulled yet. Open an entry and pull it, or run <code>tensorseed pull org/name</code>.</p>
    {:else}
      <div class="overflow-x-auto">
        <table class="table table-sm">
          <thead><tr><th>Repo</th><th>Commit</th><th class="text-right">Size</th><th>Pulled</th><th>Seeding</th></tr></thead>
          <tbody>
            {#each data.copies as c (c.dir)}
              {@const b = copyBadge(c.status)}
              <tr>
                <td>
                  <a class="link font-medium" href={entryHash(c.repoType, c.id)}>{c.id}</a>
                  {#if c.repoType === "dataset"}<span class="badge badge-xs ms-1">dataset</span>{/if}
                  {#if b}<span class="badge {b.cls} badge-xs ms-1" data-testid="copy-kind">{b.text}</span>{/if}
                  {#if c.status && c.status.kind !== "complete"}<div class="text-xs opacity-80" data-testid="copy-summary">{c.status.summary}</div>{/if}
                  {#if c.error}<div class="text-error text-xs">{c.error}</div>{/if}
                </td>
                <td class="font-mono text-xs" title={c.commit ?? ""}>{shortHex(c.commit ?? "")}</td>
                <td class="text-right whitespace-nowrap" title="What this copy holds on disk">{fileCount(c.files)}, {formatBytes(c.bytes)}</td>
                <td class="text-xs">{c.pulledAt ? new Date(c.pulledAt).toLocaleString() : ""}</td>
                <td class="text-xs">{seedState(c.dir) ?? (c.hasTorrent ? "ready" : "no torrent")}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  {/if}
</section>
