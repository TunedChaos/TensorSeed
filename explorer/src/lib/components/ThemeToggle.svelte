<script lang="ts">
  import { loadTheme, saveTheme } from "../storage";

  let preference = $state<"light" | "dark" | "system">(loadTheme());

  function apply(next: "light" | "dark" | "system") {
    preference = next;
    saveTheme(next);
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
