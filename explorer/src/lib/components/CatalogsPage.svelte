<script lang="ts">
  import { officialConfig } from "../storage";
  import type { CatalogAgreement, CatalogConfig, VerifyMode } from "@tensorseed/ui/types";

  let {
    catalogs,
    agreements,
    onadd,
    onremove,
    onmode,
  }: {
    catalogs: CatalogConfig[];
    agreements: CatalogAgreement[];
    onadd: (c: CatalogConfig) => void;
    onremove: (name: string) => void;
    onmode: (name: string, mode: VerifyMode) => void;
  } = $props();

  let name = $state("");
  let baseUrl = $state("");
  let pubkey = $state("");
  let storeId = $state("");
  let coinApi = $state("https://api.coinset.org");
  let mode = $state<VerifyMode>("strict");
  let error = $state("");

  const official = officialConfig();

  function submit(ev: Event) {
    ev.preventDefault();
    error = "";
    const n = name.trim();
    const base = baseUrl.trim();
    const key = pubkey.trim();
    const store = storeId.trim();
    if (!n || !base || !key) {
      error = "Name, base URL, and publisher public key are required.";
      return;
    }
    if (catalogs.some((c) => c.name === n)) {
      error = "A catalog with that name is already listed.";
      return;
    }
    onadd({
      name: n,
      displayName: n,
      baseUrl: base.endsWith("/") ? base : `${base}/`,
      pubkey: key,
      storeId: store,
      coinApiUrls: coinApi.split(",").map((s) => s.trim()).filter(Boolean),
      mode,
    });
    name = "";
    baseUrl = "";
    pubkey = "";
    storeId = "";
  }
</script>

<div class="flex flex-col gap-8">
  <p><a class="link" href="#/">Back to the catalog</a></p>
  <section>
    <h1 class="text-3xl font-semibold">Catalogs</h1>
    <p class="mt-2 max-w-3xl">
      The official Tensorseed catalog is pinned in this page. You can follow
      other catalogs by URL, publisher key, and Chia store id. Settings are
      stored only in this browser (localStorage).
    </p>
    <div class="mt-4 overflow-x-auto">
      <table class="table">
        <thead>
          <tr>
            <th scope="col">Name</th>
            <th scope="col">Base URL</th>
            <th scope="col">Mode</th>
            <th scope="col">Actions</th>
          </tr>
        </thead>
        <tbody>
          {#each catalogs as c}
            <tr>
              <th scope="row">{c.displayName || c.name}</th>
              <td class="break-all text-sm">{c.baseUrl}</td>
              <td>
                <select class="select select-bordered select-sm" bind:value={c.mode} onchange={() => onmode(c.name, c.mode)} aria-label="Verification mode for {c.name}">
                  <option value="strict">strict</option>
                  <option value="warn">warn</option>
                  <option value="off">off</option>
                </select>
              </td>
              <td>
                {#if c.name !== official.name}
                  <button type="button" class="btn btn-sm btn-ghost" onclick={() => onremove(c.name)}>Remove</button>
                {:else}
                  <span class="text-sm">Pinned</span>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  </section>

  {#if agreements.length}
    <section>
      <h2 class="text-xl font-medium">Agreement</h2>
      <p class="mt-1 text-sm">
        When two catalogs list the same repository, Tensorseed compares the
        latest commit they publish. A mismatch is a conflict; the CLI also
        refuses to pull it unless you choose a catalog.
      </p>
      <ul class="mt-3 list-disc ps-5">
        {#each agreements as a}
          <li>
            <code>{a.id}</code>:
            {#if a.same}
              {a.leftCatalog} and {a.rightCatalog} agree on {a.left.slice(0, 12)}.
            {:else}
              conflict: {a.leftCatalog} has {a.left.slice(0, 12)}, {a.rightCatalog} has {a.right.slice(0, 12)}.
            {/if}
          </li>
        {/each}
      </ul>
    </section>
  {/if}

  <section>
    <h2 class="text-xl font-medium">Use another catalog</h2>
    <form class="mt-3 grid max-w-xl gap-3" onsubmit={submit}>
      {#if error}
        <p class="text-error" role="alert">{error}</p>
      {/if}
      <label class="form-control">
        <span class="label"><span class="label-text">Catalog name</span></span>
        <input class="input input-bordered" bind:value={name} name="name" required autocomplete="off" />
      </label>
      <label class="form-control">
        <span class="label"><span class="label-text">Base URL</span></span>
        <input class="input input-bordered" bind:value={baseUrl} name="base" type="url" placeholder="https://example.org/" required />
      </label>
      <label class="form-control">
        <span class="label"><span class="label-text">Publisher public key</span></span>
        <input class="input input-bordered font-mono text-sm" bind:value={pubkey} name="pubkey" required autocomplete="off" />
      </label>
      <label class="form-control">
        <span class="label"><span class="label-text">Chia store id (optional if mode is off)</span></span>
        <input class="input input-bordered font-mono text-sm" bind:value={storeId} name="store" autocomplete="off" />
      </label>
      <label class="form-control">
        <span class="label"><span class="label-text">Coin API URLs, comma separated</span></span>
        <input class="input input-bordered" bind:value={coinApi} name="coin" />
      </label>
      <label class="form-control">
        <span class="label"><span class="label-text">Verification mode</span></span>
        <select class="select select-bordered" bind:value={mode}>
          <option value="strict">strict (default)</option>
          <option value="warn">warn</option>
          <option value="off">off</option>
        </select>
      </label>
      <button type="submit" class="btn btn-primary w-fit">Add catalog</button>
    </form>
  </section>
</div>
