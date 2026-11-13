<script lang="ts">
  import { untrack } from "svelte";
  import type { ThemePreference } from "../types";

  // Each app keeps its own storage key, so persistence is passed in.
  let { load, save }: { load: () => ThemePreference; save: (theme: ThemePreference) => void } = $props();

  let preference = $state<ThemePreference>(untrack(() => load()));

  function apply(next: ThemePreference) {
    preference = next;
    save(next);
    const dark =
      next === "dark" ||
      (next === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches);
    document.documentElement.setAttribute("data-theme", dark ? "dark" : "light");
  }

  $effect(() => {
    apply(preference);
    if (preference !== "system") return;
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const on = () => apply("system");
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  });
</script>

<div class="join" role="group" aria-label="Color theme">
  <button
    type="button"
    class="btn btn-sm join-item"
    class:btn-active={preference === "light"}
    aria-pressed={preference === "light"}
    onclick={() => apply("light")}>Light</button
  >
  <button
    type="button"
    class="btn btn-sm join-item"
    class:btn-active={preference === "dark"}
    aria-pressed={preference === "dark"}
    onclick={() => apply("dark")}>Dark</button
  >
  <button
    type="button"
    class="btn btn-sm join-item"
    class:btn-active={preference === "system"}
    aria-pressed={preference === "system"}
    onclick={() => apply("system")}>System</button
  >
</div>
