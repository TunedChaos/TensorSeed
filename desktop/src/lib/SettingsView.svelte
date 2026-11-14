<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  import { onMount } from "svelte";
  import { settingsCheck, settingsGet, settingsSet, type SettingsView, type TorrentSettings } from "./native";
  import { DAYS, expandDays, lines, validate } from "./settings";

  type NumKey = {
    [K in keyof TorrentSettings]: TorrentSettings[K] extends number ? K : never;
  }[keyof TorrentSettings];
  type BoolKey = {
    [K in keyof TorrentSettings]: TorrentSettings[K] extends boolean ? K : never;
  }[keyof TorrentSettings];
  type TextKey = "listen_addr" | "proxy_url" | "bind_interface" | "seed_time_limit";

  let view = $state<SettingsView | null>(null);
  let form = $state<TorrentSettings | null>(null);
  let saved = $state("");
  let trackersText = $state("");
  let peersText = $state("");
  let loadError = $state<string | null>(null);
  let serverError = $state<string | null>(null);
  let notice = $state<string | null>(null);
  let saving = $state(false);

  function snapshot(t: TorrentSettings): string {
    return JSON.stringify(t);
  }

  async function load() {
    loadError = null;
    try {
      // Clone the plain result, not the reactive proxy.
      const v = await settingsGet();
      form = structuredClone(v.torrent);
      trackersText = v.torrent.trackers.join("\n");
      peersText = v.torrent.peers.join("\n");
      saved = snapshot(v.torrent);
      view = v;
    } catch (err) {
      loadError = String(err);
    }
  }

  onMount(load);

  let current = $derived(form ? { ...form, trackers: lines(trackersText), peers: lines(peersText) } : null);
  let errors = $derived(current ? validate(current, current.trackers, current.peers) : {});
  let errorCount = $derived(Object.keys(errors).length);
  let dirty = $derived(current !== null && snapshot(current) !== saved);

  // Once the form passes the client checks, ask the native side too (the
  // exact checks the CLI runs), so messages match `tensorseed doctor`.
  let checkTimer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    const c = current;
    const n = errorCount;
    clearTimeout(checkTimer);
    if (!c || n > 0) {
      serverError = null;
      return;
    }
    const snap = $state.snapshot(c) as TorrentSettings;
    checkTimer = setTimeout(() => {
      settingsCheck(snap)
        .then(() => (serverError = null))
        .catch((err) => (serverError = String(err)));
    }, 300);
  });

  async function save() {
    if (!current || errorCount > 0) return;
    saving = true;
    notice = null;
    serverError = null;
    try {
      const t = $state.snapshot(current) as TorrentSettings;
      const r = await settingsSet(t);
      saved = snapshot(t);
      notice = `Saved to ${r.configPath}.` +
        (r.appliedToSeeder ? " The running seeder now uses the new caps, schedule, and stop rules." : "") +
        " Other options apply the next time seeding or a pull starts.";
    } catch (err) {
      serverError = String(err);
    } finally {
      saving = false;
    }
  }

  function revert() {
    notice = null;
    load();
  }

  function num(k: NumKey, v: number) {
    // An empty box is NaN, which the checks flag.
    if (form) form[k] = v;
  }

  function changed(k: keyof TorrentSettings): boolean {
    return !!view && !!form && JSON.stringify(form[k]) !== JSON.stringify(view.defaults[k]);
  }

  function toggleDay(i: number, day: string) {
    if (!form) return;
    const w = form.alt_speed.window[i];
    const set = expandDays(w.days);
    if (set.has(day)) set.delete(day);
    else set.add(day);
    w.days = DAYS.filter((d) => set.has(d));
  }

  function addWindow() {
    form?.alt_speed.window.push({ days: ["mon", "tue", "wed", "thu", "fri"], start: "09:00", end: "17:00" });
  }
</script>

{#snippet numField(k: NumKey, label: string, hint: string, step = 1)}
  <label class="fieldset">
    <span class="fieldset-legend">{label}{#if changed(k)}<span class="badge badge-xs badge-info ms-1">changed</span>{/if}</span>
    <input
      class="input input-sm w-full"
      class:input-error={!!errors[k]}
      type="number"
      min="0"
      {step}
      name={k}
      value={form?.[k]}
      oninput={(e) => num(k, e.currentTarget.valueAsNumber)}
      aria-invalid={!!errors[k]}
      aria-describedby="{k}-hint"
    />
    <span id="{k}-hint" class="label text-xs whitespace-normal {errors[k] ? 'text-error' : ''}">{errors[k] ?? hint}</span>
  </label>
{/snippet}

{#snippet boolField(k: BoolKey, label: string, hint: string)}
  <label class="label cursor-pointer items-start gap-2 py-1">
    <input
      type="checkbox"
      class="toggle toggle-sm"
      name={k}
      checked={form?.[k]}
      onchange={(e) => form && (form[k] = e.currentTarget.checked)}
    />
    <span class="flex flex-col">
      <span class="text-base-content text-sm">{label}{#if changed(k)}<span class="badge badge-xs badge-info ms-1">changed</span>{/if}</span>
      <span class="text-xs whitespace-normal opacity-70">{hint}</span>
    </span>
  </label>
{/snippet}

{#snippet textField(k: TextKey, label: string, hint: string, placeholder = "", disabled = false)}
  <label class="fieldset">
    <span class="fieldset-legend">{label}{#if changed(k)}<span class="badge badge-xs badge-info ms-1">changed</span>{/if}</span>
    <input
      class="input input-sm w-full"
      class:input-error={!!errors[k]}
      type="text"
      name={k}
      {placeholder}
      {disabled}
      value={form?.[k]}
      oninput={(e) => form && (form[k] = e.currentTarget.value)}
      aria-invalid={!!errors[k]}
      spellcheck="false"
      autocomplete="off"
    />
    <span class="label text-xs whitespace-normal {errors[k] ? 'text-error' : ''}">{errors[k] ?? hint}</span>
  </label>
{/snippet}

<section class="flex min-w-0 flex-col gap-4" aria-labelledby="settings-title">
  <div class="bg-base-100 sticky -top-4 z-10 -mx-4 -mt-4 flex flex-col gap-2 px-4 pt-4 pb-2">
  <div class="flex flex-wrap items-center gap-2">
    <h2 id="settings-title" class="text-xl font-semibold">Torrent settings</h2>
    {#if dirty}<span class="badge badge-info badge-sm">unsaved</span>{/if}
    {#if errorCount > 0}<span class="badge badge-warning badge-sm">{errorCount} to fix</span>{/if}
    <div class="flex-1"></div>
    <button type="button" class="btn btn-sm" disabled={!dirty || saving} onclick={revert}>Revert</button>
    <button type="button" class="btn btn-primary btn-sm" disabled={!dirty || saving || errorCount > 0 || !!serverError} onclick={save}>
      {saving ? "Saving" : "Save"}
    </button>
  </div>
  {#if notice}<div class="alert alert-success" role="status"><span>{notice}</span></div>{/if}
  {#if serverError}<div class="alert alert-error" role="alert" data-testid="settings-error"><span class="min-w-0 wrap-anywhere">{serverError}</span></div>{/if}
  </div>

  {#if loadError}
    <div class="alert alert-error" role="alert"><span class="min-w-0 wrap-anywhere">{loadError}</span></div>
  {/if}
  {#if view}
    <p class="text-xs opacity-70">
      The <code>[torrent]</code> table of <code class="break-all">{view.configPath}</code>, shared with the CLI.
      {view.exists ? "Saving keeps comments, other sections, and keys you did not change." : "The file does not exist yet; saving creates it."}
      Values left at their default are not written.
    </p>
    {#if view.envOverrides.length > 0}
      <div class="alert alert-warning" role="status">
        <span>These environment variables override the file and win over what you save here: {view.envOverrides.join(", ")}</span>
      </div>
    {/if}
  {/if}

  {#if form && view}
    <div class="grid gap-4 xl:grid-cols-2">
      <fieldset class="card bg-base-200">
        <div class="card-body gap-1 p-4">
          <legend class="card-title text-base">Speed</legend>
          <div class="grid gap-x-4 sm:grid-cols-2">
            {@render numField("upload_limit_kbps", "Upload cap (KiB/s)", "0 means unlimited")}
            {@render numField("download_limit_kbps", "Download cap (KiB/s)", "0 means unlimited; pulls and seeding")}
            {@render numField("torrent_upload_limit_kbps", "Per torrent upload cap (KiB/s)", "0 means unlimited")}
            {@render numField("torrent_download_limit_kbps", "Per torrent download cap (KiB/s)", "0 means unlimited")}
          </div>
        </div>
      </fieldset>

      <fieldset class="card bg-base-200">
        <div class="card-body gap-1 p-4">
          <legend class="card-title text-base">Peers and connections</legend>
          <div class="grid gap-x-4 sm:grid-cols-2">
            {@render numField("max_peers_per_torrent", "Peers per torrent", "0 for the engine default (128)")}
            {@render numField("concurrent_init_limit", "Torrents checked at once", "0 for the engine default (3)")}
            {@render textField("listen_addr", "Listen address", "IP address to listen on", "0.0.0.0")}
            {@render numField("listen_port", "Listen port", "TCP, and UDP when uTP is on")}
          </div>
          {@render boolField("utp", "uTP", "Also accept and dial uTP (UDP on the listen port) for pull and seed")}
          {@render boolField("dht", "DHT", "Find peers through the distributed hash table")}
          {@render boolField("lsd", "Local service discovery", "Find peers on the LAN (BEP 14)")}
          <label class="fieldset">
            <span class="fieldset-legend">Extra peers{#if changed("peers")}<span class="badge badge-xs badge-info ms-1">changed</span>{/if}</span>
            <textarea class="textarea textarea-sm w-full font-mono" rows="2" name="peers" placeholder="seedbox.example:6881" bind:value={peersText} spellcheck="false"></textarea>
            <span class="label text-xs whitespace-normal {Object.keys(errors).some((k) => k.startsWith('peers.')) ? 'text-error' : ''}">
              {Object.entries(errors).find(([k]) => k.startsWith("peers."))?.[1] ?? "host:port, one per line, dialed for every torrent"}
            </span>
          </label>
        </div>
      </fieldset>

      <fieldset class="card bg-base-200">
        <div class="card-body gap-1 p-4">
          <legend class="card-title text-base">Network path</legend>
          {@render textField("proxy_url", "SOCKS5 proxy", "socks5:// or socks5h:// (names resolved by the proxy). Empty: direct. Listening, DHT, and uTP do not use it.", "socks5h://127.0.0.1:9050")}
          {@render textField(
            "bind_interface",
            "Bind interface",
            view.bindInterfaceSupported ? "Bind torrent sockets to a device such as wg0. Empty: any interface." : "Not supported on Windows.",
            "wg0",
            !view.bindInterfaceSupported,
          )}
        </div>
      </fieldset>

      <fieldset class="card bg-base-200">
        <div class="card-body gap-1 p-4">
          <legend class="card-title text-base">Stop seeding</legend>
          <div class="grid gap-x-4 sm:grid-cols-2">
            {@render numField("seed_ratio_limit", "Share ratio", "Stop after uploading this multiple of the size; 0 means no limit", 0.1)}
            {@render textField("seed_time_limit", "Seeding time", "30m, 12h, 7d; empty means no limit", "7d")}
          </div>
          <label class="fieldset">
            <span class="fieldset-legend">Then{#if changed("seed_limit_action")}<span class="badge badge-xs badge-info ms-1">changed</span>{/if}</span>
            <select class="select select-sm w-full" name="seed_limit_action" bind:value={form.seed_limit_action}>
              <option value="pause">Pause (stays in the session)</option>
              <option value="remove">Remove from the session</option>
            </select>
            <span class="label text-xs">Files are never deleted.</span>
          </label>
        </div>
      </fieldset>

      <fieldset class="card bg-base-200 xl:col-span-2">
        <div class="card-body gap-2 p-4">
          <legend class="card-title text-base">Scheduled speed limits{#if changed("alt_speed")}<span class="badge badge-xs badge-info ms-1">changed</span>{/if}</legend>
          <label class="label cursor-pointer gap-2">
            <input type="checkbox" class="toggle toggle-sm" name="alt_speed.enabled" bind:checked={form.alt_speed.enabled} />
            <span class="text-base-content text-sm">Use alternative caps during these windows (local time)</span>
          </label>
          <div class="grid gap-x-4 sm:grid-cols-2">
            <label class="fieldset">
              <span class="fieldset-legend">Alternative upload cap (KiB/s)</span>
              <input class="input input-sm w-full" class:input-error={!!errors["alt_speed.upload_limit_kbps"]} type="number" min="0" name="alt_speed.upload_limit_kbps"
                value={form.alt_speed.upload_limit_kbps}
                oninput={(e) => form && (form.alt_speed.upload_limit_kbps = e.currentTarget.valueAsNumber)} />
              <span class="label text-xs whitespace-normal {errors['alt_speed.upload_limit_kbps'] ? 'text-error' : ''}">{errors["alt_speed.upload_limit_kbps"] ?? "0 means unlimited"}</span>
            </label>
            <label class="fieldset">
              <span class="fieldset-legend">Alternative download cap (KiB/s)</span>
              <input class="input input-sm w-full" class:input-error={!!errors["alt_speed.download_limit_kbps"]} type="number" min="0" name="alt_speed.download_limit_kbps"
                value={form.alt_speed.download_limit_kbps}
                oninput={(e) => form && (form.alt_speed.download_limit_kbps = e.currentTarget.valueAsNumber)} />
              <span class="label text-xs whitespace-normal {errors['alt_speed.download_limit_kbps'] ? 'text-error' : ''}">{errors["alt_speed.download_limit_kbps"] ?? "0 means unlimited"}</span>
            </label>
          </div>
          {#each form.alt_speed.window as w, i (i)}
            {@const days = expandDays(w.days)}
            <div class="bg-base-100 rounded-box flex flex-wrap items-end gap-3 p-2" role="group" aria-label="Window {i + 1}">
              <div class="flex flex-wrap gap-1" role="group" aria-label="Days">
                {#each DAYS as d (d)}
                  <label class="label cursor-pointer gap-1 text-xs">
                    <input type="checkbox" class="checkbox checkbox-xs" checked={days.has(d)} onchange={() => toggleDay(i, d)} />
                    {d}
                  </label>
                {/each}
                {#if days.size === 0}<span class="text-xs opacity-70">every day</span>{/if}
              </div>
              <label class="fieldset py-0">
                <span class="fieldset-legend py-0 text-xs">Start</span>
                <input class="input input-xs w-20" class:input-error={!!errors[`alt_speed.window.${i}.start`]} type="text" placeholder="09:00" bind:value={w.start} />
              </label>
              <label class="fieldset py-0">
                <span class="fieldset-legend py-0 text-xs">End</span>
                <input class="input input-xs w-20" class:input-error={!!errors[`alt_speed.window.${i}.end`]} type="text" placeholder="17:00" bind:value={w.end} />
              </label>
              <button type="button" class="btn btn-ghost btn-xs" onclick={() => form?.alt_speed.window.splice(i, 1)}>Remove</button>
              {#if errors[`alt_speed.window.${i}.start`] || errors[`alt_speed.window.${i}.end`]}
                <span class="text-error w-full text-xs">Start: {errors[`alt_speed.window.${i}.start`] ?? "ok"}; end: {errors[`alt_speed.window.${i}.end`] ?? "ok"}</span>
              {/if}
            </div>
          {/each}
          <div><button type="button" class="btn btn-sm" onclick={addWindow}>Add window</button></div>
          <p class="text-xs opacity-70">An end before the start runs past midnight; 24:00 ends at midnight.</p>
        </div>
      </fieldset>

      <fieldset class="card bg-base-200">
        <div class="card-body gap-1 p-4">
          <legend class="card-title text-base">Swarm on pull</legend>
          {@render boolField("pull_swarm", "Use the swarm on pull", "HTTP mirrors and the Hub fill gaps or take over")}
          <div class="grid gap-x-4 sm:grid-cols-3">
            {@render numField("swarm_no_peer_secs", "No peer timeout (s)", "0 disables")}
            {@render numField("swarm_stall_secs", "Stall timeout (s)", "0 disables")}
            {@render numField("swarm_timeout_secs", "Overall swarm timeout (s)", "Then HTTP takes over")}
          </div>
        </div>
      </fieldset>

      <fieldset class="card bg-base-200">
        <div class="card-body gap-1 p-4">
          <legend class="card-title text-base">Torrent creation (publishers)</legend>
          {@render boolField("create", "Create torrents", "For every harvested or published revision")}
          {@render boolField("hybrid", "Hybrid v1 and v2", "The built-in engine only seeds hybrids")}
          {@render numField("piece_length", "Piece length (bytes)", "0 for auto (4 MiB and up)")}
          <label class="fieldset">
            <span class="fieldset-legend">Trackers{#if changed("trackers")}<span class="badge badge-xs badge-info ms-1">changed</span>{/if}</span>
            <textarea class="textarea textarea-sm w-full font-mono" rows="2" name="trackers" placeholder="udp://tracker.example:6969/announce" bind:value={trackersText} spellcheck="false"></textarea>
            <span class="label text-xs whitespace-normal {Object.keys(errors).some((k) => k.startsWith('trackers.')) ? 'text-error' : ''}">
              {Object.entries(errors).find(([k]) => k.startsWith("trackers."))?.[1] ?? "One announce URL per line"}
            </span>
          </label>
        </div>
      </fieldset>
    </div>
  {/if}
</section>
