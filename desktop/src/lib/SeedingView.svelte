<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  import { onMount } from "svelte";
  import { formatBytes } from "@tensorseed/ui/format";
  import { onStatus, seedSetPaused, seedStop, serveSetPaused, status, type SeedView, type ServeTorrent, type Status } from "./native";

  let st = $state<Status | null>(null);
  let busy = $state(false);
  let notice = $state<{ ok: boolean; text: string } | null>(null);

  function rate(mibps: number): string {
    return mibps >= 1 ? `${mibps.toFixed(1)} MiB/s` : `${Math.round(mibps * 1024)} KiB/s`;
  }

  async function act(f: () => Promise<unknown>, done?: (r: unknown) => string) {
    busy = true;
    try {
      const r = await f();
      notice = { ok: true, text: done ? done(r) : String(r) };
      st = await status();
    } catch (err) {
      notice = { ok: false, text: String(err) };
    } finally {
      busy = false;
    }
  }

  function seedBadge(s: SeedView): string {
    return s.state === "seeding" ? "badge-success" : s.state === "error" ? "badge-error" : "badge-warning";
  }

  function serveBadge(t: ServeTorrent): string {
    if (t.state === "live") return "badge-success";
    if (t.state.startsWith("error")) return "badge-error";
    if (t.state.startsWith("paused")) return "badge-warning";
    return "badge-info";
  }

  const userPaused = (t: ServeTorrent) => t.state === "paused: by the user";
  const changed = (n: unknown) => `${n} changed`;

  onMount(() => {
    let unlisten: (() => void) | undefined;
    status().then((s) => (st = s)).catch(() => {});
    onStatus((s) => (st = s)).then((u) => (unlisten = u));
    return () => unlisten?.();
  });

  const d = $derived(st?.desktop);
  const serve = $derived(st?.serve ?? null);
</script>

<section class="flex min-w-0 flex-col gap-4" aria-labelledby="seeding-title">
  <h2 id="seeding-title" class="text-xl font-semibold">Seeding</h2>
  <p class="text-sm opacity-70">One seeder per machine: Desktop's own, or a running <code>tensorseed serve</code> that Desktop attaches to. Seeding never changes or deletes your files.</p>
  {#if notice}
    <div class="alert {notice.ok ? 'alert-success' : 'alert-error'}" role="status">
      <span class="min-w-0 wrap-anywhere">{notice.text}</span>
      <button type="button" class="btn btn-xs" onclick={() => (notice = null)}>Dismiss</button>
    </div>
  {/if}

  {#if serve}
    <section aria-labelledby="serve-h" class="card bg-base-200" data-testid="serve-card">
      <div class="card-body gap-3 p-4">
        <div class="flex flex-wrap items-center gap-2">
          <h3 id="serve-h" class="font-semibold">
            {serve.kind === "seed" ? "tensorseed seed" : "tensorseed serve"}{serve.pid ? ` (pid ${serve.pid})` : ""}
          </h3>
          {#if serve.control}
            <span class="badge badge-info badge-sm">attached</span>
          {:else}
            <span class="badge badge-ghost badge-sm">read only</span>
          {/if}
          <div class="flex-1"></div>
          {#if serve.control}
            <button type="button" class="btn btn-xs" disabled={busy} onclick={() => act(() => serveSetPaused(null, true), changed)}>Pause all</button>
            <button type="button" class="btn btn-xs" disabled={busy} onclick={() => act(() => serveSetPaused(null, false), changed)}>Resume all</button>
          {/if}
        </div>
        {#if serve.kind === "seed"}
          <p class="text-sm">A <code>tensorseed seed</code> run is seeding on this machine. Stop it to seed from Desktop.</p>
        {:else}
          <div class="flex flex-wrap gap-x-4 text-sm">
            <span>{serve.torrents} revisions</span>
            {#if serve.paused > 0}<span>{serve.paused} paused</span>{/if}
            <span>{serve.peers} peers</span>
            <span>up {rate(serve.uploadMibps)}, down {rate(serve.downloadMibps)}</span>
            <span>uploaded {formatBytes(serve.uploaded)} this run</span>
            {#if serve.status !== "ok"}<span class="badge badge-warning badge-sm">{serve.status}</span>{/if}
          </div>
          {#if !serve.control}
            <p class="text-xs opacity-70">Read through its health endpoint. It runs with another state folder (for example as a service), so Desktop cannot control it.</p>
          {/if}
          {#if serve.list.length > 0}
            <div class="overflow-x-auto">
              <table class="table table-sm">
                <thead><tr><th>Revision</th><th>State</th><th class="text-right">Size</th><th class="text-right">Uploaded</th><th class="text-right">Peers</th><th><span class="sr-only">Actions</span></th></tr></thead>
                <tbody>
                  {#each serve.list as t (t.name)}
                    <tr data-testid="serve-row">
                      <td class="font-mono text-xs break-all">{t.name}</td>
                      <td><span class="badge badge-sm {serveBadge(t)}">{t.state}</span></td>
                      <td class="text-right">{formatBytes(t.bytes)}</td>
                      <td class="text-right">{formatBytes(t.uploaded)}</td>
                      <td class="text-right">{t.peers}</td>
                      <td class="text-right">
                        {#if serve.control}
                          {#if userPaused(t)}
                            <button type="button" class="btn btn-xs" disabled={busy} onclick={() => act(() => serveSetPaused(t.name, false), () => `resumed ${t.name}`)}>Resume</button>
                          {:else if !t.state.startsWith("paused")}
                            <button type="button" class="btn btn-xs" disabled={busy} onclick={() => act(() => serveSetPaused(t.name, true), () => `paused ${t.name}`)}>Pause</button>
                          {/if}
                        {/if}
                      </td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
          {/if}
        {/if}
      </div>
    </section>
  {/if}

  <section aria-labelledby="desktop-h" class="flex flex-col gap-2">
    <h3 id="desktop-h" class="font-semibold">Seeding from Desktop</h3>
    {#if !d?.running}
      <p class="text-sm opacity-70">
        {serve ? "Desktop does not seed while another seeder runs on this machine." : "Nothing is seeding from Desktop. Seed a pulled copy from its page."}
      </p>
    {:else}
      <div class="flex flex-wrap gap-x-4 text-sm">
        {#if d.listen}<span>listening on <code>{d.listen}</code></span>{/if}
        <span>{d.seeding} seeding</span>
        {#if d.paused > 0}<span>{d.paused} paused</span>{/if}
        {#if d.stopped > 0}<span>{d.stopped} stopped by a seed limit</span>{/if}
        <span>{d.peers} peers</span>
        <span>up {rate(d.uploadMibps)}, down {rate(d.downloadMibps)}</span>
        <span>uploaded {formatBytes(d.uploaded)} this run</span>
      </div>
      <div class="overflow-x-auto">
        <table class="table table-sm">
          <thead><tr><th>Copy</th><th>State</th><th class="text-right">Size</th><th class="text-right">Uploaded</th><th class="text-right">Peers</th><th><span class="sr-only">Actions</span></th></tr></thead>
          <tbody>
            {#each d.torrents as s (s.name)}
              <tr data-testid="seed-row">
                <td class="max-w-md">
                  <div class="font-mono text-xs break-all">{s.name}</div>
                  <div class="text-xs break-all opacity-60"><code>{s.dir}</code></div>
                </td>
                <td>
                  <span class="badge badge-sm {seedBadge(s)}">{s.state}</span>
                  {#if s.detail}<div class="text-xs opacity-70">{s.detail}</div>{/if}
                </td>
                <td class="text-right">{formatBytes(s.bytes)}</td>
                <td class="text-right">{formatBytes(s.uploaded)}</td>
                <td class="text-right">{s.peers}</td>
                <td class="text-right whitespace-nowrap">
                  {#if s.state === "seeding"}
                    <button type="button" class="btn btn-xs" disabled={busy} onclick={() => act(() => seedSetPaused(s.name, true))}>Pause</button>
                  {:else if s.state === "paused"}
                    <button type="button" class="btn btn-xs" disabled={busy} onclick={() => act(() => seedSetPaused(s.name, false))}>Resume</button>
                  {/if}
                  <button type="button" class="btn btn-xs btn-outline" disabled={busy} title="Stop seeding this copy; its files stay" onclick={() => act(() => seedStop(s.name))}>Stop</button>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </section>
</section>
