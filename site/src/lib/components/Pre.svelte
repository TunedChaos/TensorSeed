<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  let { code, copyable = false, label = "" }: { code: string; copyable?: boolean; label?: string } = $props();
</script>

<!-- Scrollable code must be reachable by keyboard (WCAG scrollable region). -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<pre
  tabindex="0"
  class="bg-base-200 rounded-box overflow-x-auto p-4 text-sm leading-relaxed"
  class:copyable
  title={label || undefined}><code>{code}</code></pre>
