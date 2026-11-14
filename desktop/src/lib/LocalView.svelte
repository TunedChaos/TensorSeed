<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  import { onMount } from "svelte";
  import { formatBytes, shortHex } from "@tensorseed/ui/format";
  import { copyDelete, copyOpen, localList, onJob, onStatus, verifyStart, type LocalCopy, type LocalList, type Status } from "./native";
  import { entryHash } from "./router";
  import { copyBadge, fileCount } from "./copy";

  let data = $state<LocalList | null>(null);
  let error = $state<string | null>(null);
  let st = $state<Status | null>(null);
  let notice = $state<{ ok: boolean; text: string } | null>(null);
  /** Running verify job per copy folder. */
  let verifying = $state<Record<string, number>>({});
  let confirmDelete = $state<LocalCopy | null>(null);
  let dialog = $state<HTMLDialogElement | null>(null);

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
    const un: (() => void)[] = [];
    onStatus((s) => (st = s)).then((u) => un.push(u));
    onJob((e) => {
      const dir = Object.keys(verifying).find((d) => verifying[d] === e.job);
      if (dir !== undefined && e.state !== "running") {
        delete verifying[dir];
        notice = { ok: e.state === "ok", text: `${e.id}: ${e.message ?? e.state}` };
      }
      if (e.kind === "pull" && e.state !== "running") refresh();
    }).then((u) => un.push(u));
    return () => un.forEach((u) => u());
  });

  function seedState(dir: string): string | null {
    const t = st?.desktop.torrents.find((x) => x.dir === dir);
    return t ? (t.detail ? `${t.state}: ${t.detail}` : t.state) : null;
  }

  function pulling(dir: string): boolean {
    return (st?.jobs ?? []).some((j) => j.kind === "pull" && j.out === dir);
  }

  async function verify(c: LocalCopy) {
    notice = null;
    try {
      verifying[c.dir] = await verifyStart({ repoType: c.repoType, id: c.id });
    } catch (err) {
      notice = { ok: false, text: String(err) };
    }
  }

  async function open(c: LocalCopy) {
    try {
      await copyOpen(c.repoType, c.id);
    } catch (err) {
      notice = { ok: false, text: String(err) };
    }
  }

  function askDelete(c: LocalCopy) {
    confirmDelete = c;
    dialog?.showModal();
  }

  async function doDelete(c: LocalCopy) {
    dialog?.close();
    try {
      notice = { ok: true, text: await copyDelete(c.repoType, c.id) };
    } catch (err) {
      notice = { ok: false, text: String(err) };
    }
    await refresh();
  }
</script>

<section class="flex min-w-0 flex-col gap-4" aria-labelledby="local-title">
  <div class="flex flex-wrap items-center gap-2">
    <h2 id="local-title" class="text-xl font-semibold">Library</h2>
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
  {#if data}
    <p class="text-xs opacity-70">Pulled copies under <code class="break-all">{data.downloadDir}</code> (general.download_dir). Unfinished pulls are in <a class="link" href="#/downloads">Downloads</a>.</p>
    {#if data.copies.length === 0}
      <p>Nothing pulled yet. Open an entry and pull it, or run <code>tensorseed pull org/name</code>.</p>
    {:else}
      <div class="overflow-x-auto">
        <table class="table table-sm">
          <thead><tr><th>Repo</th><th>Commit</th><th class="text-right">Size</th><th>Pulled</th><th>Seeding</th><th><span class="sr-only">Actions</span></th></tr></thead>
          <tbody>
            {#each data.copies as c (c.dir)}
              {@const b = copyBadge(c.status)}
              <tr data-testid="library-row">
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
                <td class="whitespace-nowrap">
                  <button type="button" class="btn btn-xs" disabled={c.dir in verifying || pulling(c.dir)} onclick={() => verify(c)}>
                    {#if c.dir in verifying}<span class="loading loading-spinner loading-xs" aria-hidden="true"></span>{/if}Verify
                  </button>
                  <button type="button" class="btn btn-xs" onclick={() => open(c)}>Open folder</button>
                  <button
                    type="button"
                    class="btn btn-xs btn-error btn-outline"
                    disabled={c.seeding || pulling(c.dir) || c.dir in verifying}
                    title={c.seeding ? "Stop seeding first" : "Delete this copy from disk"}
                    onclick={() => askDelete(c)}>Delete</button
                  >
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  {/if}
</section>

<dialog class="modal" bind:this={dialog} aria-labelledby="delete-h">
  <div class="modal-box">
    <h3 id="delete-h" class="text-lg font-semibold">Delete this copy?</h3>
    {#if confirmDelete}
      <p class="py-2 text-sm">
        Every file in <code class="break-all">{confirmDelete.dir}</code> ({fileCount(confirmDelete.files)}, {formatBytes(confirmDelete.bytes)}) is deleted from disk. This cannot be undone.
      </p>
    {/if}
    <div class="modal-action">
      <form method="dialog"><button class="btn btn-sm">Keep it</button></form>
      <button type="button" class="btn btn-sm btn-error" onclick={() => confirmDelete && doDelete(confirmDelete)}>Delete</button>
    </div>
  </div>
</dialog>
