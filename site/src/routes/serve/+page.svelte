<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<!-- Copyright 2026 Tuned Chaos -->
<script lang="ts">
  import Seo from "#lib/components/Seo.svelte";
  import Pre from "#lib/components/Pre.svelte";

  const config = `[inference]
llama_server = "/opt/llama.cpp/bin/llama-server"   # "" = llama-server on PATH
vllm = "~/vllm-env/bin/vllm"                      # "" = vllm on PATH`;
</script>

<Seo
  title="Serve and verify"
  description="Serve a verified copy through an OpenAI compatible engine you install separately, sign every response, and let clients and marketplaces check which exact weights answered."
/>

<header class="max-w-3xl">
  <h1 class="text-4xl font-bold">Serve and verify</h1>
  <p class="mt-3 text-lg">
    Optional. Tensorseed can launch and vouch for an OpenAI compatible inference engine that you install separately
    (llama.cpp or vLLM). It serves only a copy that verified against the signed catalog, and can sign every response so clients can check which
    exact weights answered. Pulling, verifying, and seeding never need an engine.
  </p>
</header>

<section class="mt-10 grid gap-4 md:grid-cols-2" aria-label="Guides">
  <a class="card bg-base-200 hover:bg-base-300 transition-colors" href="/guides/sell-inference">
    <div class="card-body">
      <h2 class="card-title">Selling inference?</h2>
      <p>Pull and verify a model, serve it, sign responses, publish a signed claim about the weights, and list the endpoint.</p>
      <span class="link link-primary">Read the seller guide</span>
    </div>
  </a>
  <a class="card bg-base-200 hover:bg-base-300 transition-colors" href="/guides/integrate-a-marketplace">
    <div class="card-body">
      <h2 class="card-title">Running a marketplace or gateway?</h2>
      <p>Trust catalogs, check a seller&rsquo;s claim, verify signed responses, and run logprob spot challenges.</p>
      <span class="link link-primary">Read the integration guide</span>
    </div>
  </a>
</section>

<section class="mt-12 max-w-3xl" aria-labelledby="how">
  <h2 id="how" class="text-2xl font-bold">How it fits together</h2>
  <ol class="mt-3 list-decimal space-y-2 pl-6">
    <li>A seller pulls a model and verifies it against a catalog the buyer can trust.</li>
    <li><code>tensorseed serve-inference</code> starts the engine on that verified copy only.</li>
    <li>The seller publishes a signed claim naming the exact model, revision, and file hashes behind the endpoint.</li>
    <li>With signing on, every answer carries an Ed25519 signature from the claim&rsquo;s response key.</li>
    <li>A marketplace checks the claim against the signed catalog, verifies responses, and runs rare logprob challenges.</li>
  </ol>
  <p class="mt-3">
    A claim is a statement the seller makes and can be held to, not proof of what runs. Signed responses make a swap
    attributable to the seller&rsquo;s key, and challenges make it detectable at a low, tunable rate.
  </p>
</section>

<section class="mt-12 max-w-3xl" aria-labelledby="commands">
  <h2 id="commands" class="text-2xl font-bold">Commands</h2>
  <p class="mt-2">
    <code>tensorseed serve-inference</code> serves a verified copy through an OpenAI compatible engine, and
    <code>tensorseed sign-responses</code> puts a signing proxy in front of one. Tensorseed Desktop does not serve inference
    yet; use the CLI.
  </p>
</section>

<section class="mt-12 max-w-3xl" aria-labelledby="engine">
  <h2 id="engine" class="text-2xl font-bold">Install an engine</h2>
  <p class="mt-2">
    The engine is not part of Tensorseed. Engines are large, need builds that match your GPU, and update often, so they come
    from their own projects.
  </p>
  <ul class="mt-3 list-disc space-y-2 pl-6">
    <li>
      <strong>llama.cpp</strong> (<code>llama-server</code>) serves GGUF models. Install it with
      <code>winget install llama.cpp</code> on Windows, <code>brew install llama.cpp</code> on macOS or Linux, your
      distribution&rsquo;s package, or an archive from the
      <a class="link" href="https://github.com/ggml-org/llama.cpp/releases">llama.cpp releases page</a>.
    </li>
    <li>
      <strong>vLLM</strong> (<code>vllm</code>) serves safetensors models. Install it with <code>pip install vllm</code> in a
      Python virtual environment. It needs Linux and a supported GPU (on Windows, use WSL 2).
    </li>
  </ul>
  <p class="mt-3">
    Tensorseed finds the engine on <code>PATH</code>, or at <code>inference.llama_server</code> or
    <code>inference.vllm</code> in the config:
  </p>
  <div class="mt-2"><Pre code={config} /></div>
  <p class="mt-3">
    Without an engine, <code>serve-inference</code> stops with &ldquo;llama.cpp is not installed&rdquo; (or &ldquo;vLLM is not
    installed&rdquo;), the install options, and a link to the
    <a class="link" href="/guides/sell-inference#3-install-an-inference-engine">inference guide</a>.
  </p>
</section>
