<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  import { onMount } from "svelte";
  import CommercialBadge from "@tensorseed/ui/components/CommercialBadge.svelte";
  import { formatBytes, shortHex } from "@tensorseed/ui/format";
  import {
    entryShow,
    onJob,
    pullStart,
    seedStart,
    verifyStart,
    type EntryArgs,
    type EntryPage,
    type JobEvent,
  } from "./native";

  let { entry }: { entry: EntryArgs } = $props();

  let page = $state<EntryPage | null>(null);
  let loading = $state(false);
  let loadError = $state<string | null>(null);
  let log = $state<string[]>([]);
  let job = $state<{ id: number; kind: string } | null>(null);
  let result = $state<{ ok: boolean; text: string } | null>(null);
  let seeding = $state(false);
  let fileQuery = $state("");

  const key = $derived(`${entry.repoType}/${entry.id}/${entry.revision ?? ""}/${entry.catalog ?? ""}`);

  async function load(sync: boolean) {
    loading = true;
    loadError = null;
    try {
      let p = await entryShow(entry, sync);
      // Nothing local yet: sync once so a fresh link still resolves.
      if (!sync && !p.manifest) p = await entryShow(entry, true);
      page = p;
    } catch (err) {
      page = null;
      loadError = String(err);
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    void key;
    log = [];
    result = null;
    job = null;
    load(false);
  });

  // Events can arrive before the start call returns the job id; keep them
  // until the id is known.
  let early: JobEvent[] = [];

  function apply(e: JobEvent) {
    if (e.line !== null) log = [...log.slice(-499), e.line];
    if (e.state !== "running") {
      result = { ok: e.state === "ok", text: e.message ?? e.state };
      job = null;
      load(false);
    }
  }

  onMount(() => {
    let unlisten: (() => void) | undefined;
    onJob((e: JobEvent) => {
      if (job && e.job === job.id) apply(e);
      else if (!job && e.repoType === entry.repoType && e.id === entry.id) early = [...early.slice(-99), e];
    }).then((u) => (unlisten = u));
    return () => unlisten?.();
  });

  async function start(kind: "pull" | "verify") {
    result = null;
    log = [];
    early = [];
    try {
      const id = kind === "pull" ? await pullStart(entry) : await verifyStart(entry);
      job = { id, kind };
      const mine = early.filter((e) => e.job === id);
      early = [];
      for (const e of mine) if (job) apply(e);
    } catch (err) {
      result = { ok: false, text: String(err) };
    }
  }

  async function seed() {
    result = null;
    seeding = true;
    try {
      result = { ok: true, text: await seedStart(entry) };
      await load(false);
    } catch (err) {
      result = { ok: false, text: String(err) };
    } finally {
      seeding = false;
    }
  }

  let m = $derived(page?.manifest ?? null);
  let files = $derived(
    (m?.files ?? [])
      .filter((f) => !fileQuery || f.path.toLowerCase().includes(fileQuery.toLowerCase()))
      .sort((a, b) => a.path.localeCompare(b.path)),
  );
  let localMatches = $derived(page?.local.commit && m ? page.local.commit === m.hf_commit : null);
  let busy = $derived(job !== null || seeding);

  function chainLabel(p: EntryPage): { cls: string; text: string; detail?: string } {
    const c = p.chain;
    if (p.manifestSource === "local") return { cls: "badge-ghost", text: "Not checked", detail: "Shown from the local copy's stored manifest; sync to check the catalog." };
    if (!c) return { cls: "badge-ghost", text: "Unknown" };
    switch (c.kind) {
      case "anchored":
        return {
          cls: "badge-success",
          text: "Anchored on chain",
          detail: `store ${shortHex(c.storeId)}, coin ${shortHex(c.coinId)} (${c.coinState}), checked ${new Date(c.checkedUnix * 1000).toLocaleString()} via ${c.via}`,
        };
      case "datalayer":
        return { cls: "badge-success", text: "Read from Chia DataLayer", detail: `store ${shortHex(c.storeId)}` };
      case "off":
        return { cls: "badge-warning", text: "Chain check off", detail: "chain_check is off for this catalog" };
      case "unchecked":
        return { cls: "badge-warning", text: "Not anchored", detail: c.reason };
    }
  }
</script>

<article class="flex min-w-0 flex-col gap-4" aria-labelledby="entry-title">
  <div class="flex flex-wrap items-center gap-2">
    <a class="btn btn-ghost btn-sm" href="#/">Catalogs</a>
    <h2 id="entry-title" class="min-w-0 text-xl font-semibold break-all">{entry.id}</h2>
    <span class="badge">{entry.repoType}</span>
    {#if page?.catalogLabel}<span class="badge badge-outline">{page.catalogLabel}</span>{/if}
    <div class="flex-1"></div>
    <button type="button" class="btn btn-sm" disabled={loading} onclick={() => load(true)}>Sync</button>
    <button
      type="button"
      class="btn btn-primary btn-sm"
      disabled={busy || loading || !page?.cliFound || !m || page?.takenDown}
      title={page && !page.cliFound ? "Install the tensorseed CLI (or set TENSORSEED_CLI): Desktop runs it for pulls" : "Download and verify every file"}
      onclick={() => start("pull")}
    >{page?.local.pulled ? "Pull again" : "Pull"}</button>
    <button type="button" class="btn btn-sm" disabled={busy || !page?.local.pulled} onclick={() => start("verify")}>Verify</button>
    <button
      type="button"
      class="btn btn-sm"
      disabled={busy || !page?.local.hasTorrent || page?.local.seeding}
      title={page && !page.local.hasTorrent ? "Needs a pulled copy with a torrent" : "Seed this copy"}
      onclick={seed}
    >{page?.local.seeding ? "Seeding" : "Seed"}</button>
  </div>

  {#if loading && !page}
    <div class="alert" role="status"><span class="loading loading-spinner loading-sm" aria-hidden="true"></span><span>Reading the catalog</span></div>
  {:else if loadError}
    <div class="alert alert-error" role="alert"><span class="min-w-0 wrap-anywhere">{loadError}</span></div>
  {/if}

  {#if result}
    <div class="alert {result.ok ? 'alert-success' : 'alert-error'}" role="status">
      <span class="min-w-0 wrap-anywhere">{result.text}</span>
    </div>
  {/if}
  {#if job || log.length > 0}
    <section aria-label="Job output">
      <h3 class="mb-1 font-semibold">
        {#if job}<span class="loading loading-spinner loading-xs" aria-hidden="true"></span>{/if}
        {job ? (job.kind === "pull" ? "Pulling" : "Verifying") : "Output"}
      </h3>
      <pre class="bg-base-200 rounded-box max-h-64 overflow-auto p-2 text-xs" data-testid="job-log">{log.join("\n")}</pre>
    </section>
  {/if}

  {#if page}
    {#if page.syncError}
      <div class="alert alert-warning" role="status"><span class="min-w-0 wrap-anywhere">Sync failed, showing the last accepted copy: {page.syncError}</span></div>
    {/if}
    {#if !m && page.manifestError}
      <div class="alert alert-error" role="alert"><span class="min-w-0 wrap-anywhere">{page.manifestError}</span></div>
    {/if}
    {#if page.takenDown}
      <div class="alert alert-error" role="alert">
        <span>This revision is taken down{page.tombstone?.reason ? ` (${page.tombstone.reason})` : ""}. Pull is disabled.</span>
      </div>
    {/if}
    {#if page.takenDownIn.length > 0}
      <div class="alert alert-warning" role="status"><span>Taken down in: {page.takenDownIn.join(", ")}</span></div>
    {/if}

    <div class="grid gap-4 lg:grid-cols-3">
      <section class="card bg-base-200" aria-labelledby="trust-h">
        <div class="card-body gap-2 p-4">
          <h3 id="trust-h" class="card-title text-base">Signature and anchor</h3>
          <p>
            {#if !m}
              <span class="badge badge-ghost">No manifest</span>
            {:else if page.signed}
              <span class="badge badge-success" data-testid="signature">Signature verified</span>
            {:else}
              <span class="badge badge-error" data-testid="signature">Not signed</span>
            {/if}
          </p>
          <p class="text-xs opacity-80">
            {page.signed ? "Checked natively against the pinned publisher key and catalog name." : m ? "This manifest did not come with a verified signature." : ""}
          </p>
          {#if m}
            {@const c = chainLabel(page)}
            <p><span class="badge {c.cls}" data-testid="chain">{c.text}</span></p>
            {#if c.detail}<p class="text-xs break-all opacity-80">{c.detail}</p>{/if}
            <p class="text-xs opacity-70">Source: {page.manifestSource === "local" ? "stored manifest of the local copy" : "catalog"}{page.synced ? ", synced now" : ""}</p>
          {/if}
        </div>
      </section>

      <section class="card bg-base-200" aria-labelledby="license-h">
        <div class="card-body gap-2 p-4">
          <h3 id="license-h" class="card-title text-base">License</h3>
          {#if m}
            <p class="font-mono">{m.license.spdx ?? m.license.hf_tag ?? "unknown"}</p>
            <p class="text-xs">Class: {m.license.class}{m.license.gated_upstream ? ", gated upstream" : ""}</p>
            {#if page.commercial}<p><CommercialBadge assessment={page.commercial} /></p>
              <p class="text-xs opacity-80">{page.commercial.note}. Not legal advice.</p>{/if}
            {#if m.license.attribution}<p class="text-xs">Attribution: {m.license.attribution}</p>{/if}
            {#if m.license.license_files.length > 0}<p class="text-xs break-all">Files: {m.license.license_files.join(", ")}</p>{/if}
          {:else}
            <p class="text-sm opacity-70">Unknown until the manifest is read.</p>
          {/if}
        </div>
      </section>

      <section class="card bg-base-200" aria-labelledby="local-h">
        <div class="card-body gap-2 p-4">
          <h3 id="local-h" class="card-title text-base">This machine</h3>
          <p class="text-xs break-all"><code>{page.local.dir}</code></p>
          {#if page.local.pulled}
            <p class="text-sm">
              Pulled {page.local.pulledAt ? new Date(page.local.pulledAt).toLocaleString() : ""},
              {page.local.files} files, {formatBytes(page.local.bytes)}{page.local.skipped ? `, ${page.local.skipped} skipped` : ""}
            </p>
            <p class="text-xs">
              Commit <code>{shortHex(page.local.commit ?? "")}</code>
              {#if localMatches === true}<span class="badge badge-success badge-sm">latest</span>
              {:else if localMatches === false}<span class="badge badge-warning badge-sm">older than the catalog</span>{/if}
              {#if page.local.signed}<span class="badge badge-sm">signed pull</span>{/if}
            </p>
            <p class="text-xs">
              {page.local.seeding ? "Seeding from Desktop." : page.local.hasTorrent ? "Has a torrent; ready to seed." : "No torrent; cannot seed."}
            </p>
          {:else}
            <p class="text-sm opacity-70">Not pulled yet.</p>
          {/if}
          {#if page.local.error}<p class="text-error text-xs">{page.local.error}</p>{/if}
        </div>
      </section>
    </div>

    {#if m}
      <section aria-labelledby="manifest-h">
        <h3 id="manifest-h" class="mb-2 font-semibold">Manifest</h3>
        <dl class="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1 text-sm">
          <dt class="opacity-70">Commit</dt><dd class="font-mono break-all">{m.hf_commit}</dd>
          <dt class="opacity-70">Upstream</dt><dd class="break-all">{m.repo.hf_url}</dd>
          <dt class="opacity-70">Size</dt><dd>{m.totals.files} files, {formatBytes(m.totals.bytes)}</dd>
          {#if m.published_at}<dt class="opacity-70">Published</dt><dd>{new Date(m.published_at).toLocaleString()}{m.publisher_tool ? ` by ${m.publisher_tool}` : ""}</dd>{/if}
          <dt class="opacity-70">Webseeds</dt><dd>{m.webseeds.length}</dd>
          {#if m.torrent}
            <dt class="opacity-70">Torrent</dt><dd class="font-mono break-all">btmh {shortHex(m.torrent.infohash_v2, 12, 6)}{m.torrent.infohash_v1 ? `, btih ${shortHex(m.torrent.infohash_v1, 12, 6)}` : ""}</dd>
          {:else}
            <dt class="opacity-70">Torrent</dt><dd>none (HTTP sources only)</dd>
          {/if}
        </dl>
      </section>

      <section aria-labelledby="files-h">
        <div class="mb-2 flex flex-wrap items-center gap-2">
          <h3 id="files-h" class="font-semibold">Files</h3>
          <span class="badge badge-ghost">{m.files.length}</span>
          <div class="flex-1"></div>
          <label class="input input-sm">
            <span class="sr-only">Filter files</span>
            <input type="search" placeholder="Filter files" bind:value={fileQuery} />
          </label>
        </div>
        <div class="overflow-x-auto">
          <table class="table table-sm">
            <thead><tr><th>Path</th><th class="text-right">Size</th><th>Format</th><th>SHA-256</th></tr></thead>
            <tbody>
              {#each files as f (f.path)}
                <tr>
                  <td class="font-mono break-all">{f.path}
                    {#if f.pickle_risk}<span class="badge badge-warning badge-xs ms-1" title="Pickle based format, skipped by default">pickle</span>{/if}
                  </td>
                  <td class="text-right whitespace-nowrap">{formatBytes(f.size)}</td>
                  <td>{f.format ?? ""}</td>
                  <td class="font-mono text-xs" title={f.sha256}>{shortHex(f.sha256)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      </section>
    {/if}
  {/if}
</article>
