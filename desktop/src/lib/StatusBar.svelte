<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  import { onMount } from "svelte";
  import { onStatus, seedsPause, seedsResume, status, type Status } from "./native";

  let st = $state<Status | null>(null);
  let busy = $state(false);
  let error = $state<string | null>(null);

  function rate(mibps: number): string {
    return mibps >= 1 ? `${mibps.toFixed(1)} MiB/s` : `${Math.round(mibps * 1024)} KiB/s`;
  }

  let seeding = $derived((st?.desktop.seeding ?? 0) + (st?.serve?.torrents ?? 0));
  let up = $derived((st?.desktop.uploadMibps ?? 0) + (st?.serve?.uploadMibps ?? 0));
  let down = $derived((st?.desktop.downloadMibps ?? 0) + (st?.serve?.downloadMibps ?? 0));

  async function act(f: () => Promise<number>) {
    busy = true;
    error = null;
    try {
      await f();
      st = await status();
    } catch (err) {
      error = String(err);
    } finally {
      busy = false;
    }
  }

  onMount(() => {
    let unlisten: (() => void) | undefined;
    status().then((s) => (st = s)).catch(() => {});
    onStatus((s) => (st = s)).then((u) => (unlisten = u));
    return () => unlisten?.();
  });
</script>

<div class="flex flex-wrap items-center gap-2 text-sm" role="status" aria-live="polite" aria-label="Torrent activity">
  <span class="badge badge-ghost" title="Copies seeding (Desktop and tensorseed serve)">Seeding {seeding}</span>
  <span class="badge badge-ghost" title="Pulls running">Downloading {st?.downloading ?? 0}</span>
  {#if (st?.desktop.paused ?? 0) > 0}<span class="badge badge-warning">{st?.desktop.paused} paused</span>{/if}
  {#if st?.desktop.running || st?.serve}
    <span class="opacity-70">up {rate(up)}, down {rate(down)}</span>
  {/if}
  {#if st?.serve}<span class="badge badge-info" title="A tensorseed serve on this machine is seeding">serve</span>{/if}
  <button type="button" class="btn btn-xs" disabled={busy || (st?.desktop.seeding ?? 0) === 0} onclick={() => act(seedsPause)}>Pause all</button>
  <button type="button" class="btn btn-xs" disabled={busy || (st?.desktop.paused ?? 0) === 0} onclick={() => act(seedsResume)}>Resume all</button>
  {#if error}<span class="text-error">{error}</span>{/if}
</div>
