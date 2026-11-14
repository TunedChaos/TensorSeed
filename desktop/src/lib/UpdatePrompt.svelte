<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  // Shows an available update with its notes and asks before installing.
  // The notes are shown as plain text, never as HTML.
  import { onMount } from "svelte";
  import { formatBytes } from "@tensorseed/ui/format";
  import { onUpdateAvailable, onUpdateProgress, updateInstall, type UpdateProgress } from "./native";
  import { pendingUpdate } from "./update.svelte";

  let installing = $state(false);
  let progress = $state<UpdateProgress | null>(null);
  let error = $state<string | null>(null);

  onMount(() => {
    let a: (() => void) | undefined;
    let p: (() => void) | undefined;
    onUpdateAvailable((u) => (pendingUpdate.update = u)).then((u) => (a = u));
    onUpdateProgress((x) => (progress = x)).then((u) => (p = u));
    return () => {
      a?.();
      p?.();
    };
  });

  async function install() {
    const u = pendingUpdate.update;
    if (!u) return;
    installing = true;
    error = null;
    try {
      await updateInstall(u.version);
    } catch (e) {
      error = String(e);
      installing = false;
    }
  }
</script>

{#if pendingUpdate.update}
  {@const u = pendingUpdate.update}
  <div class="alert alert-info m-4 flex flex-col items-stretch gap-2" role="dialog" aria-labelledby="update-title">
    <div class="flex flex-wrap items-baseline gap-x-3">
      <h2 id="update-title" class="font-semibold">Tensorseed Desktop {u.version} is available</h2>
      <span class="text-sm opacity-80">You have {u.currentVersion}{u.date ? `; released ${u.date.slice(0, 10)}` : ""}</span>
    </div>
    {#if u.notes}
      <pre class="bg-base-100 text-base-content max-h-48 overflow-auto rounded p-2 text-sm whitespace-pre-wrap">{u.notes}</pre>
    {/if}
    <p class="text-sm">
      Installing downloads the signed installer, checks its signature, replaces this copy, and restarts Desktop. Seeding stops while it
      restarts.
    </p>
    {#if installing}
      <div class="flex items-center gap-2 text-sm" role="status">
        <progress class="progress w-56" value={progress?.total ? progress.downloaded : undefined} max={progress?.total ?? undefined}></progress>
        <span>
          {#if progress?.done}Installing...{:else if progress}{formatBytes(progress.downloaded)}{progress.total ? ` of ${formatBytes(progress.total)}` : ""}{:else}Starting...{/if}
        </span>
      </div>
    {/if}
    {#if error}
      <p class="text-error-content bg-error rounded p-2 text-sm" role="alert">{error}</p>
    {/if}
    <div class="flex gap-2">
      <button type="button" class="btn btn-primary btn-sm" onclick={install} disabled={installing}>Install and restart</button>
      <button type="button" class="btn btn-sm" onclick={() => (pendingUpdate.update = null)} disabled={installing}>Not now</button>
    </div>
  </div>
{/if}
