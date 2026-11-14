<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  import { formatBytes } from "@tensorseed/ui/format";
  import type { PullProgress } from "./native";
  import { fileCount } from "./copy";

  let { progress }: { progress: PullProgress } = $props();

  const pct = $derived(progress.bytesTotal > 0 ? Math.min(100, (100 * progress.bytesDone) / progress.bytesTotal) : 0);
  const phase = $derived(
    progress.phase === "resolving" ? "Resolving the revision" : progress.phase === "swarm" ? "Downloading from the swarm" : "Downloading and verifying",
  );
</script>

<div class="flex min-w-0 flex-col gap-1" data-testid="job-progress">
  <div class="flex flex-wrap items-baseline gap-x-3 text-xs">
    <span class="font-medium">{phase}</span>
    {#if progress.filesTotal > 0 && progress.phase === "downloading"}
      <span>{progress.filesDone} of {fileCount(progress.filesTotal)}</span>
      <span>{formatBytes(progress.bytesDone)} of {formatBytes(progress.bytesTotal)}</span>
    {/if}
    {#if progress.rate > 0}<span>{formatBytes(progress.rate)}/s</span>{/if}
  </div>
  {#if progress.phase !== "downloading"}
    <progress class="progress w-full" aria-label="Pull progress"></progress>
  {:else}
    <progress class="progress progress-primary w-full" value={pct} max="100" aria-label="Pull progress" aria-valuetext="{pct.toFixed(0)} percent"></progress>
  {/if}
  {#if progress.current.length > 0}
    <p class="truncate font-mono text-xs opacity-70" title={progress.current.join("\n")}>{progress.current.join(", ")}</p>
  {/if}
</div>
