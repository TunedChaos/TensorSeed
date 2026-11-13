<script lang="ts">
  let { label, command }: { label: string; command: string } = $props();
  let copied = $state(false);

  async function copy() {
    try {
      await navigator.clipboard.writeText(command);
      copied = true;
      setTimeout(() => (copied = false), 1500);
    } catch {
      copied = false;
    }
  }
</script>

<div class="flex flex-col gap-1">
  <div class="flex flex-wrap items-center justify-between gap-2">
    <p class="font-medium">{label}</p>
    <button type="button" class="btn btn-sm" onclick={copy} aria-label="Copy {label}">
      {copied ? "Copied" : "Copy"}
    </button>
  </div>
  <pre class="bg-base-200 overflow-x-auto rounded-box p-3 text-sm"><code>{command}</code></pre>
</div>
