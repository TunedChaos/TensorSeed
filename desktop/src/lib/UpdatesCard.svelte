<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  // Settings: the running version, where updates come from, a manual
  // check, and the startup check switch. A check fetches latest.json only.
  import { onMount } from "svelte";
  import { updateCheck, updateInfo, updateSetCheckOnStart, type UpdateInfo } from "./native";
  import { pendingUpdate } from "./update.svelte";

  let info = $state<UpdateInfo | null>(null);
  let checking = $state(false);
  let result = $state<string | null>(null);
  let error = $state<string | null>(null);

  onMount(async () => {
    try {
      info = await updateInfo();
    } catch (e) {
      error = String(e);
    }
  });

  async function check() {
    checking = true;
    result = null;
    error = null;
    try {
      const u = await updateCheck();
      if (u) {
        pendingUpdate.update = u;
        result = `Version ${u.version} is available; see the notice at the top.`;
      } else {
        result = `You have the latest version (${info?.currentVersion ?? ""}).`;
      }
    } catch (e) {
      error = String(e);
    } finally {
      checking = false;
    }
  }

  async function toggle(e: Event) {
    const on = (e.currentTarget as HTMLInputElement).checked;
    error = null;
    try {
      await updateSetCheckOnStart(on);
      if (info) info.checkOnStart = on;
    } catch (err) {
      error = String(err);
    }
  }
</script>

<section class="card bg-base-200" aria-labelledby="updates-title">
  <div class="card-body gap-2 p-4">
    <h2 id="updates-title" class="card-title text-base">Updates</h2>
    {#if info}
      <p class="text-sm">Tensorseed Desktop {info.currentVersion}</p>
      {#if info.selfUpdates}
        <div class="flex flex-wrap items-center gap-3">
          <button type="button" class="btn btn-sm" onclick={check} disabled={checking}>{checking ? "Checking..." : "Check for updates"}</button>
          <label class="label cursor-pointer gap-2 text-sm">
            <input type="checkbox" class="checkbox checkbox-sm" checked={info.checkOnStart} onchange={toggle} />
            Check for updates when Desktop starts
          </label>
        </div>
        <p class="text-xs opacity-70">
          A check downloads only the release's latest.json from GitHub; nothing about this machine is sent. An update installs only after
          you confirm it, and only if it is signed with the Tensorseed Desktop update key.
        </p>
      {:else}
        <p class="text-sm" role="status">{info.elsewhere}</p>
      {/if}
    {/if}
    {#if result}<p class="text-sm" role="status">{result}</p>{/if}
    {#if error}<p class="text-error text-sm" role="alert">{error}</p>{/if}
  </div>
</section>
