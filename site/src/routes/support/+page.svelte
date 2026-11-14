<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  import Seo from "#lib/components/Seo.svelte";
  import Pre from "#lib/components/Pre.svelte";
  import { REPO_URL } from "#lib/release.ts";
  import addressFile from "../../../xch-donate-address.txt?raw";

  // Single source: the same file the QR code is generated from.
  const XCH_ADDRESS = addressFile.trim();
</script>

<Seo title="Support and license" description="Support Tensorseed through Tuned Chaos with GitHub Sponsors, a card, or Chia (XCH). Tensorseed is licensed under AGPL-3.0-or-later." />

<header class="max-w-3xl">
  <h1 class="text-4xl font-bold">Support Tensorseed</h1>
  <p class="mt-3 text-lg">
    Tensorseed is an open source project by Tuned Chaos, and support for it goes through the studio. Support pays for object
    storage and the always-on seeder that keeps published models available. Support is not a tax-deductible donation.
  </p>
  <div class="mt-6 flex flex-wrap gap-3">
    <a class="btn btn-primary" href="https://github.com/sponsors/TunedChaos">GitHub Sponsors</a>
    <a class="btn" href="https://buy.stripe.com/6oUbJ2ejf4hRe1xboG73G00">Support with a card</a>
    <a class="btn btn-ghost" href="https://tunedchaos.com/supporters/">All ways to support</a>
  </div>
  <p class="mt-4">
    GitHub Sponsors covers monthly support. The card form is for one-time support through Stripe; pick Tensorseed under
    &ldquo;Which project would you like to support?&rdquo; to send it here.
  </p>
</header>

<section class="mt-10 grid max-w-4xl gap-6 md:grid-cols-[1fr_auto] md:items-start" aria-labelledby="xch">
  <div>
    <h2 id="xch" class="text-2xl font-bold">Chia (XCH)</h2>
    <p class="mt-2">You can also send support in Chia (XCH) to the Tuned Chaos address. Click the address to select it, then copy:</p>
    <div class="mt-2"><Pre code={XCH_ADDRESS} copyable label="Tuned Chaos XCH support address" /></div>
    <p class="mt-3">Or scan this QR code with a Chia wallet.</p>
  </div>
  <img
    class="rounded-box w-56 md:w-64"
    src="/xch-donate.svg"
    width="328"
    height="328"
    alt="QR code for the Tuned Chaos XCH support address"
    loading="lazy"
  />
</section>

<section class="mt-12 max-w-3xl" aria-labelledby="thanks">
  <h2 id="thanks" class="text-2xl font-bold">Thank you</h2>
  <p class="mt-2">
    Thank you for supporting Tensorseed. Your support helps keep storage and the official seeder running. See everyone who
    supports Tuned Chaos on the <a class="link" href="https://tunedchaos.com/supporters/#thanks">supporters page</a>.
  </p>
</section>

<section class="mt-12 max-w-3xl" aria-labelledby="license">
  <h2 id="license" class="text-2xl font-bold">Source and license</h2>
  <p class="mt-2">
    Tensorseed is licensed under <a class="link" href="https://www.gnu.org/licenses/agpl-3.0.html">AGPL-3.0-or-later</a>.
    Copyright Tuned Chaos. Source is at <a class="link" href={REPO_URL}>github.com/TunedChaos/TensorSeed</a>.
  </p>
</section>
