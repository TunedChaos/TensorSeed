<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  import { onMount } from "svelte";
  import JobProgress from "./JobProgress.svelte";
  import { jobDiscard, jobResume, jobsList, onJob, pullCancel, pullPause, type JobRow, type JobsList } from "./native";
  import { fileCount } from "./copy";

  let data = $state<JobsList | null>(null);
  let error = $state<string | null>(null);
  let notice = $state<{ ok: boolean; text: string } | null>(null);
  let confirmDiscard = $state<JobRow | null>(null);
  let dialog = $state<HTMLDialogElement | null>(null);

  async function refresh() {
    try {
      data = await jobsList();
      error = null;
    } catch (err) {
      error = String(err);
    }
  }

  onMount(() => {
    refresh();
    let unlisten: (() => void) | undefined;
    let pending = false;
    onJob((e) => {
      if (!data) return;
      const i = data.active.findIndex((j) => j.job === e.job);
      if (e.state === "running") {
        if (i >= 0) data.active[i] = { ...e, line: null };
        else data.active = [...data.active, { ...e, line: null }];
      } else {
        notice = { ok: e.state !== "failed", text: `${e.id}: ${e.message ?? e.state}` };
        if (i >= 0) data.active = data.active.filter((j) => j.job !== e.job);
      }
      // History rows change with job state; reload them, at most once a second.
      if (!pending) {
        pending = true;
        setTimeout(() => {
          pending = false;
          refresh();
        }, 1000);
      }
    }).then((u) => (unlisten = u));
    const timer = setInterval(refresh, 5000);
    return () => {
      unlisten?.();
      clearInterval(timer);
    };
  });

  async function act(f: () => Promise<unknown>, done?: string) {
    try {
      await f();
      if (done) notice = { ok: true, text: done };
    } catch (err) {
      notice = { ok: false, text: String(err) };
    }
    await refresh();
  }

  function askDiscard(r: JobRow) {
    confirmDiscard = r;
    dialog?.showModal();
  }

  function statusBadge(r: JobRow): string {
    switch (r.status) {
      case "done":
        return "badge-success";
      case "failed":
        return "badge-error";
      case "paused":
      case "interrupted":
        return "badge-warning";
      default:
        return "badge-info";
    }
  }

  const pulls = $derived((data?.active ?? []).filter((j) => j.kind === "pull"));
</script>

<section class="flex min-w-0 flex-col gap-4" aria-labelledby="downloads-title">
  <div class="flex flex-wrap items-center gap-2">
    <h2 id="downloads-title" class="text-xl font-semibold">Downloads</h2>
    <div class="flex-1"></div>
    <button type="button" class="btn btn-sm" onclick={refresh}>Refresh</button>
  </div>
  {#if error}
    <div class="alert alert-error" role="alert"><span class="min-w-0 wrap-anywhere">{error}</span></div>
  {/if}
  {#if notice}
    <div class="alert {notice.ok ? 'alert-success' : 'alert-error'}" role="status">
      <span class="min-w-0 wrap-anywhere">{notice.text}</span>
      <button type="button" class="btn btn-xs" onclick={() => (notice = null)}>Dismiss</button>
    </div>
  {/if}

  <section aria-labelledby="active-h" class="flex flex-col gap-2">
    <h3 id="active-h" class="font-semibold">Running here</h3>
    {#if pulls.length === 0}
      <p class="text-sm opacity-70">No pulls running in this window.</p>
    {/if}
    {#each pulls as j (j.job)}
      <div class="card bg-base-200" data-testid="active-pull">
        <div class="card-body gap-2 p-3">
          <div class="flex flex-wrap items-center gap-2">
            <span class="font-medium break-all">{j.id}</span>
            {#if j.repoType === "dataset"}<span class="badge badge-xs">dataset</span>{/if}
            <div class="flex-1"></div>
            <button type="button" class="btn btn-xs" onclick={() => act(() => pullPause(j.job))}>Pause</button>
            <button type="button" class="btn btn-xs btn-error btn-outline" onclick={() => act(() => pullCancel(j.job))}>Cancel</button>
          </div>
          {#if j.progress}<JobProgress progress={j.progress} />{/if}
          {#if j.out}<p class="text-xs break-all opacity-70"><code>{j.out}</code></p>{/if}
        </div>
      </div>
    {/each}
  </section>

  <section aria-labelledby="history-h" class="flex flex-col gap-2">
    <h3 id="history-h" class="font-semibold">All pulls on this machine</h3>
    <p class="text-xs opacity-70">The same jobs <code>tensorseed jobs</code> lists. Pulls started from the CLI show here and can be resumed here, and the other way round.</p>
    {#if data?.error}
      <div class="alert alert-warning" role="status"><span class="min-w-0 wrap-anywhere">{data.error}</span></div>
    {/if}
    {#if data && data.history.length === 0}
      <p class="text-sm opacity-70">No pulls recorded yet.</p>
    {:else if data}
      <div class="overflow-x-auto">
        <table class="table table-sm">
          <thead><tr><th>Revision</th><th>Status</th><th class="text-right">Files</th><th>Updated</th><th><span class="sr-only">Actions</span></th></tr></thead>
          <tbody>
            {#each data.history as r (r.id)}
              <tr data-testid="job-row">
                <td class="max-w-md">
                  <div class="font-mono text-xs break-all">{r.label}</div>
                  <div class="text-xs break-all opacity-60">{r.outDir}</div>
                  {#if r.error}<div class="text-error text-xs break-all">{r.error}</div>{/if}
                </td>
                <td>
                  <span class="badge badge-sm {statusBadge(r)}">{r.busy && r.windowJob === null ? "running elsewhere" : r.status}</span>
                </td>
                <td class="text-right whitespace-nowrap">{r.filesDone} of {fileCount(r.filesTotal)}</td>
                <td class="text-xs whitespace-nowrap">{new Date(r.updatedUnix * 1000).toLocaleString()}</td>
                <td class="whitespace-nowrap">
                  {#if r.resumable}
                    <button type="button" class="btn btn-xs btn-primary" onclick={() => act(() => jobResume(r.id))}>Resume</button>
                  {/if}
                  {#if !r.busy && r.status !== "done"}
                    <button type="button" class="btn btn-xs" onclick={() => askDiscard(r)}>Discard</button>
                  {/if}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </section>
</section>

<dialog class="modal" bind:this={dialog} aria-labelledby="discard-h">
  <div class="modal-box">
    <h3 id="discard-h" class="text-lg font-semibold">Discard this pull?</h3>
    {#if confirmDiscard}
      <p class="py-2 text-sm">Partial downloads in <code class="break-all">{confirmDiscard.outDir}</code> are deleted and the job is forgotten. Files that finished and verified stay.</p>
    {/if}
    <div class="modal-action">
      <form method="dialog"><button class="btn btn-sm">Keep it</button></form>
      <button
        type="button"
        class="btn btn-sm btn-error"
        onclick={() => {
          const r = confirmDiscard;
          dialog?.close();
          if (r) act(() => jobDiscard(r.id), `discarded ${r.label}`);
        }}>Discard</button
      >
    </div>
  </div>
</dialog>
