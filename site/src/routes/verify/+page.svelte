<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  import Seo from "#lib/components/Seo.svelte";
  import Pre from "#lib/components/Pre.svelte";
  import AssetList from "#lib/components/AssetList.svelte";
  import { RELEASE_KEY_ID, RELEASE_PUBKEY, VERIFY_ASSETS, VERSION } from "#lib/release.ts";
  import { CATALOG_URL, CHIA_STORE_ID, PUBLISHER_KEY_ID, PUBLISHER_PUBKEY } from "#lib/site.ts";

  const check = `minisign -Vm SHA256SUMS -P ${RELEASE_PUBKEY}\nsha256sum -c --ignore-missing SHA256SUMS`;

  const clientConfig = `[catalog]
backend = "file"
name = "tensorseed"
publisher_pubkey = "${PUBLISHER_PUBKEY}"
chain_check = "strict"

[file]
base_url = "${CATALOG_URL}"

[chia]
catalog_store_id = "${CHIA_STORE_ID}"
chain_source = "light"
coin_api_urls = ["https://api.coinset.org"]`;
</script>

<Seo title="Verify" description="Verify a Tensorseed release with SHA256SUMS and the release key, and verify the Tensorseed catalog with the publisher key and its Chia anchor." />

<header class="max-w-3xl">
  <h1 class="text-4xl font-bold">Verify</h1>
  <p class="mt-3 text-lg">
    Two keys, two jobs. The <strong>release key</strong> signs the files you download. The
    <strong>catalog publisher key</strong> signs the catalog of models and datasets. Do not verify binaries with the catalog
    key.
  </p>
</header>

<section class="mt-10 max-w-3xl" aria-labelledby="release">
  <h2 id="release" class="text-2xl font-bold">Verify a release</h2>
  <p class="mt-2">
    Release archives ship with <code>SHA256SUMS</code> and <code>SHA256SUMS.minisig</code>, signed by the release key
    (minisign, key id {RELEASE_KEY_ID}):
  </p>
  <div class="mt-2"><Pre code={RELEASE_PUBKEY} copyable label="Release public key" /></div>
  <ol class="mt-4 list-decimal space-y-3 pl-6">
    <li>
      Download the files you want and these three from the {VERSION} release:
      <div class="mt-2"><AssetList assets={VERIFY_ASSETS} /></div>
    </li>
    <li>
      In the folder with the downloads, check the signature, then the checksums:
      <div class="mt-2"><Pre code={check} /></div>
    </li>
  </ol>
  <p class="mt-3">
    The first command must report a verified signature, and the second prints <code>OK</code> for every file you downloaded.
  </p>
</section>

<section class="mt-12 max-w-3xl" aria-labelledby="catalog">
  <h2 id="catalog" class="text-2xl font-bold">Verify the catalog</h2>
  <p class="mt-2">
    Pin this publisher public key (minisign, key id {PUBLISHER_KEY_ID}) in <code>catalog.publisher_pubkey</code>. Clients
    reject any catalog file that is not signed by it.
  </p>
  <div class="mt-2"><Pre code={PUBLISHER_PUBKEY} copyable label="Catalog publisher public key" /></div>
  <p class="mt-4">
    Pin the Chia mainnet DataLayer store that anchors the catalog in <code>chia.catalog_store_id</code>. Clients accept a
    catalog head only when the chain anchors it.
  </p>
  <div class="mt-2"><Pre code={CHIA_STORE_ID} copyable label="Chia DataLayer store id" /></div>
  <p class="mt-4">Client config:</p>
  <div class="mt-2"><Pre code={clientConfig} /></div>
  <p class="mt-4">
    Then run <code>tensorseed catalog chain-status</code>, <code>catalog info</code>, <code>list</code>, <code>show</code>,
    <code>pull</code>, and <code>verify</code>. No Chia software is needed: the client checks the published
    <code>chain-proof.json</code> against a public Chia full node API.
  </p>
</section>
