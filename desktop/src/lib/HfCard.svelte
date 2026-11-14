<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  // Settings: Hugging Face login for gated and private repos. The token is
  // sent once to the native side, checked with the Hub, saved owner only
  // where `tensorseed hf login` saves it, and never shown again.
  import { onMount } from "svelte";
  import { hfLogin, hfLogout, hfStatus, type HfView } from "./native";

  let view = $state<HfView | null>(null);
  let token = $state("");
  let busy = $state(false);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);

  const OTHER: Record<string, string> = {
    env: "an environment variable",
    token_file: "huggingface.token_file in the config",
    hf_cli: "the Hugging Face CLI login",
  };

  async function refresh() {
    try {
      view = await hfStatus();
    } catch (e) {
      error = String(e);
    }
  }

  onMount(refresh);

  async function login(e: Event) {
    e.preventDefault();
    const t = token;
    // Clear the field straight away, whatever the result.
    token = "";
    if (!t.trim()) return;
    busy = true;
    error = null;
    notice = null;
    try {
      view = await hfLogin(t);
      notice =
        view.source === "login_file"
          ? "Signed in. The token was saved for this user only."
          : `Saved, but ${OTHER[view.source] ?? "another token"} takes precedence while it is set.`;
    } catch (err) {
      error = view?.loginSaved ? `${String(err)}; the saved login is unchanged.` : String(err);
    } finally {
      busy = false;
    }
  }

  async function logout() {
    busy = true;
    error = null;
    notice = null;
    try {
      view = await hfLogout();
      notice = view.source === "none" ? "Signed out. The saved token was removed." : `Saved token removed. ${OTHER[view.source] ?? "Another token"} is still used.`;
    } catch (err) {
      error = String(err);
    } finally {
      busy = false;
    }
  }
</script>

<section class="card bg-base-200" aria-labelledby="hf-title">
  <div class="card-body gap-2 p-4">
    <h2 id="hf-title" class="card-title text-base">Hugging Face</h2>
    {#if view}
      <p class="text-sm" role="status">
        {#if view.source === "none"}
          Not signed in. Public repos work without a token; gated and private repos need one.
        {:else if view.user}
          Signed in as <span class="font-semibold">{view.user}</span>, using the {view.sourceLabel}.
        {:else}
          A token is set ({view.sourceLabel}) but it could not be checked.
        {/if}
      </p>
      {#if view.error}<p class="text-warning text-sm">{view.error}</p>{/if}
      {#if view.source === "env" || view.source === "token_file"}
        <p class="text-xs opacity-70">
          This token comes from {OTHER[view.source]} and takes precedence over a login saved here. Signing out here does not change it.
        </p>
      {:else if view.source === "hf_cli"}
        <p class="text-xs opacity-70">
          This token comes from {OTHER.hf_cli}. A login saved here takes precedence over it; signing out here does not change it.
        </p>
      {/if}
      <form class="flex flex-wrap items-center gap-2" onsubmit={login}>
        <label class="sr-only" for="hf-token">Hugging Face access token</label>
        <input
          id="hf-token"
          class="input input-sm w-80 max-w-full font-mono"
          type="password"
          placeholder={view.loginSaved ? "Paste a new token to replace the saved one" : "Paste a read access token (hf_...)"}
          bind:value={token}
          autocomplete="off"
          spellcheck="false"
          disabled={busy}
        />
        <button type="submit" class="btn btn-sm" disabled={busy || !token.trim()}>{busy ? "Checking..." : "Sign in"}</button>
        {#if view.loginSaved}
          <button type="button" class="btn btn-sm btn-ghost" onclick={logout} disabled={busy}>Sign out</button>
        {/if}
      </form>
      <p class="text-xs opacity-70">
        The token is checked with Hugging Face, then saved for this user only at {view.loginPath} (the same file as
        <code>tensorseed hf login</code>). It is sent only to the Hugging Face endpoint and is never shown again.
      </p>
    {/if}
    {#if notice}<p class="text-sm" role="status">{notice}</p>{/if}
    {#if error}<p class="text-error text-sm" role="alert">{error}</p>{/if}
  </div>
</section>
