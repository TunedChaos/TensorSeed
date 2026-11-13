<script lang="ts">
  import { expiresLabel, shortHex } from "../format";
  import type { VerifyStatus } from "../types";

  let { status }: { status: VerifyStatus } = $props();

  let open = $state(false);
</script>

{#if status.kind === "loading"}
  <div class="alert" role="status" aria-live="polite">
    <span class="loading loading-spinner loading-sm" aria-hidden="true"></span>
    <span>{status.detail}</span>
  </div>
{:else if status.kind === "verified"}
  <div class="alert alert-success" role="status" aria-live="polite">
    <div class="w-full">
      <p class="font-semibold">Verified</p>
      <p>
        Revision {status.revision}, root
        <code class="text-sm">{shortHex(status.root, 12, 8)}</code>. Head expires
        {expiresLabel(status.expiresUnix)}.
      </p>
      {#if status.warnings.length}
        <p class="mt-1">Notes: {status.warnings.join(" ")}</p>
      {/if}
      <button type="button" class="btn btn-ghost btn-xs mt-2" onclick={() => (open = !open)} aria-expanded={open}>
        {open ? "Hide details" : "Show details"}
      </button>
      {#if open}
        <dl class="mt-2 grid gap-1 text-sm">
          <div><dt class="inline font-medium">Root:</dt> <dd class="inline break-all"><code>{status.root}</code></dd></div>
          {#if status.proof}
            <div><dt class="inline font-medium">Store:</dt> <dd class="inline break-all"><code>{status.proof.store_id}</code></dd></div>
            <div><dt class="inline font-medium">Chain root:</dt> <dd class="inline break-all"><code>{status.proof.root}</code></dd></div>
            <div><dt class="inline font-medium">Coin:</dt> <dd class="inline break-all"><code>{status.proof.coin_id}</code></dd></div>
            <div><dt class="inline font-medium">Puzzle hash:</dt> <dd class="inline break-all"><code>{status.proof.puzzle_hash}</code></dd></div>
          {/if}
          {#if status.coin}
            <div>
              <dt class="inline font-medium">Coin state:</dt>
              <dd class="inline">
                {#if status.coin.state === "Current"}
                  unspent (current store root), created at height {status.coin.confirmed_block_index}
                {:else if typeof status.coin.state === "object" && "RecentlySpent" in status.coin.state}
                  spent {status.coin.state.RecentlySpent.blocks_ago} blocks ago, within the grace window
                {:else if typeof status.coin.state === "object" && "Stale" in status.coin.state}
                  stale (spent {status.coin.state.Stale.blocks_ago} blocks ago)
                {/if}
              </dd>
            </div>
          {/if}
        </dl>
      {/if}
    </div>
  </div>
{:else}
  <div class="alert alert-error" role="alert" aria-live="assertive">
    <div class="w-full">
      <p class="font-semibold">Unverified</p>
      <p>{status.reason}</p>
      {#if status.warnings.length > 1}
        <ul class="mt-2 list-disc ps-5">
          {#each status.warnings as w}
            <li>{w}</li>
          {/each}
        </ul>
      {/if}
      {#if status.revision}
        <p class="mt-2 text-sm">Stopped at revision {status.revision}{status.root ? `, root ${shortHex(status.root, 12, 8)}` : ""}.</p>
      {/if}
    </div>
  </div>
{/if}
