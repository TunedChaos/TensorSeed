# Sell inference from a verified mirror

You already pull or seed models with Tensorseed. This guide turns a
verified local copy into an OpenAI compatible endpoint you can list on an
inference marketplace, or keep for your own apps. The steps are:

1. pull a model and check its license;
2. verify the copy against the signed catalog;
3. start it with `tensorseed serve-inference`;
4. put an authenticating TLS reverse proxy in front of it;
5. list the endpoint on a marketplace.

Tensorseed does not bundle an inference runtime and has no marketplace
code. It verifies the weights, starts a runtime you installed (llama.cpp
for GGUF files, vLLM for safetensors), waits until the runtime answers, and
prints the values a marketplace asks for. Any OpenAI compatible marketplace
or your own gateway works the same way. [Kompute](https://kompute.gg) is the
worked example below.

> **Not legal advice.** The commercial serving marks in this guide come from
> the license id only. A license can carry terms the id does not show, and
> your situation can differ. Read the model's license, and ask a lawyer when
> money is involved.

## 1. Pick a model you may serve commercially

Every catalog entry shows a commercial serving mark derived from its
license id:

| Mark | Meaning | Examples |
|---|---|---|
| `allowed` | Permissive license without field of use limits. Keep the license and notices. | `apache-2.0`, `mit`, `bsd-3-clause`, `cc-by-4.0` |
| `restricted` | Commercial use is allowed with conditions you must follow. | `llama3.x` (separate license from Meta above 700M monthly active users, Acceptable Use Policy, "Built with Llama"), `gemma` (Prohibited Use Policy), OpenRAIL family (use based restrictions), `agpl-3.0` |
| `forbidden` | Non-commercial or research only. | `cc-by-nc-4.0`, `cc-by-nc-sa-4.0`, `mrl`, `apple-amlr` |
| `unknown` | Missing, `other`, or custom. Read the license yourself. | `other` |

```sh
tensorseed list --commercial              # allowed or restricted only
tensorseed list --commercial-serving allowed
tensorseed list --json                    # ts.list/1 with commercial_serving and commercial_note
tensorseed show org/name                  # the manifest; the mark is printed on stderr
```

The mark is computed on your machine from fields the catalog already signs
(the license id and class). It is not part of the signed catalog, so older
clients and existing signatures are unaffected. The table lives in one
place, `crates/tensorseed-core/src/commercial.rs`, and the explorer and
Tensorseed Desktop use the same table.

## 2. Pull and verify

Pull only the file you will serve. For a GGUF repo with several
quantizations, pick one:

```sh
tensorseed pull Qwen/Qwen2.5-1.5B-Instruct-GGUF \
  --include 'qwen2.5-1.5b-instruct-q4_k_m.gguf' --include LICENSE --include README.md
tensorseed verify ~/.local/share/tensorseed/downloads/Qwen/Qwen2.5-1.5B-Instruct-GGUF
```

For vLLM, pull the whole safetensors checkpoint (`config.json`, the
tokenizer files, and every `.safetensors` shard).

## 3. Install a runtime

Install one of these yourself. Tensorseed finds it on `PATH`, or you point
to it in the config:

- **llama.cpp** (`llama-server`) for GGUF files: a release archive from
  the llama.cpp project, your package manager, or a source build.
- **vLLM** (`vllm`) for safetensors checkpoints: usually a Python virtual
  environment on a machine with a supported GPU.

```toml
[inference]
llama_server = ""                # "" = llama-server on PATH; or a full path
vllm = ""                        # "" = vllm on PATH; or a full path
host = "127.0.0.1"               # keep loopback; the reverse proxy is public
port = 8090
llama_server_args = []           # e.g. ["-c", "8192", "-ngl", "99"]
vllm_args = []                   # e.g. ["--max-model-len", "8192"]
api_key_env = ""                 # env var holding an upstream API key, e.g. "TS_UPSTREAM_KEY"
ready_timeout_secs = 600         # wait this long for /v1/models
```

`tensorseed doctor` checks this section when it is present: whether each
binary is found, whether the port is free, and whether the API key
variable is set (it never prints the value).

## 4. Serve it

```sh
tensorseed serve-inference ~/.local/share/tensorseed/downloads/Qwen/Qwen2.5-1.5B-Instruct-GGUF \
  --model-id qwen2.5-1.5b-instruct --public-url https://llm.example.com/v1
```

You can also pass a repo id (`tensorseed serve-inference Qwen/Qwen2.5-1.5B-Instruct-GGUF`)
to serve the default pull directory. What happens:

1. **Verification first.** Every file is hashed against the signed
   manifest (the stored `.tensorseed/manifest.json`, or the catalog). A
   missing or changed file, an unsigned manifest, or a takedown stops here,
   and nothing is served.
2. **License check.** `forbidden` licenses are refused unless you pass
   `--i-have-rights` (for example, you hold a separate license or will not
   charge). `restricted` and `unknown` print a warning with the main
   conditions.
3. **Runtime choice.** GGUF goes to llama.cpp and safetensors to vLLM
   (`--runtime` overrides). With several GGUF files, pick one with
   `--file`. vLLM reads the whole directory, so Tensorseed refuses to start
   it while files that are not in the manifest sit next to the weights.
4. **Start and wait.** The runtime starts with the model id as its alias
   (`--alias` for llama.cpp, `--served-model-name` for vLLM) and Tensorseed
   waits until `/v1/models` lists it.
5. **Record.** Tensorseed writes `.tensorseed/served-weights.json` and
   prints:

```text
serving Qwen/Qwen2.5-1.5B-Instruct-GGUF with llama.cpp
  base_url  http://127.0.0.1:8090/v1
  model id  qwen2.5-1.5b-instruct
  public    https://llm.example.com/v1
  record    .../.tensorseed/served-weights.json (unsigned, ts.served-weights/0)
```

Ctrl-C stops the runtime. To run it yourself or under a service manager,
`--print-command` prints the exact command after verification and the
license check, and exits:

```sh
tensorseed serve-inference Qwen/Qwen2.5-1.5B-Instruct-GGUF --print-command
# llama-server --model /home/me/.local/share/tensorseed/downloads/Qwen/Qwen2.5-1.5B-Instruct-GGUF/qwen2.5-1.5b-instruct-q4_k_m.gguf --alias Qwen/Qwen2.5-1.5B-Instruct-GGUF --host 127.0.0.1 --port 8090
```

Arguments after `--` go to the runtime:
`tensorseed serve-inference <dir> -- -ngl 99 -c 8192`.

### The served weights record

`.tensorseed/served-weights.json` (schema `ts.served-weights/0`) records the
catalog, the manifest key and the SHA-256 of the signed manifest value, the
revision, the license and its mark, the runtime and the file it loads, the
served model id, the local and public base URLs, and every verified file
with its SHA-256.

It is **unsigned and provisional**. It is a placeholder for a signed
"weights statement" a seller can publish so buyers and marketplaces can see
which catalog revision is behind a model id. That format is being designed
with a marketplace partner and is not decided yet; the `/0` version and
`"signed": false` mark this file as a draft. Even a signed statement would
prove what the seller claims to serve, not what actually runs; nothing in
this file is proof.

With `--sign` (see [Sign responses](#sign-responses)), the same path holds
a `ts.served-weights/1` claim instead: the same facts plus the engine
version, the response signing public key, an optional seller wallet, and
`claim_sha256`, which your wallet can sign. It is a statement you make and
can be held to, not proof of what runs.

## 5. Expose it safely

Never put the runtime itself on the internet. Keep `host = "127.0.0.1"` and
put a reverse proxy in front that terminates TLS and checks an API key.

1. Pick a long random upstream key and keep it out of shell history and
   config files, for example in a root only environment file loaded by your
   service manager.
2. Optionally have the runtime require the same key: set
   `api_key_env = "TS_UPSTREAM_KEY"`. Tensorseed passes the value to
   llama.cpp as `LLAMA_API_KEY` or to vLLM as `VLLM_API_KEY`, never on the
   command line, so it does not show in `ps`. This protects the runtime
   from other local users too.
3. Have the proxy require the key on every request. Some marketplaces
   health check `GET /v1/models` without the key; if yours does, allow only
   that one path without it (it lists model ids, nothing else).

A Caddy example (automatic TLS). Caddy substitutes `{$TS_UPSTREAM_KEY}`
from its own environment when it loads the file, so start it with that
variable set:

```caddyfile
llm.example.com {
	@models {
		method GET
		path /v1/models
	}
	@authorized header Authorization "Bearer {$TS_UPSTREAM_KEY}"

	handle @models {
		reverse_proxy 127.0.0.1:8090 {
			header_up Authorization "Bearer {$TS_UPSTREAM_KEY}"
		}
	}
	handle @authorized {
		reverse_proxy 127.0.0.1:8090
	}
	handle {
		respond "unauthorized" 401
	}
	request_body {
		max_size 1MB
	}
}
```

With nginx, the same shape: a `location = /v1/models` block, and a
`location /v1/` block that returns 401 unless `$http_authorization`
matches, both with `proxy_pass http://127.0.0.1:8090;`,
`proxy_buffering off;` (for streamed responses), and a TLS certificate.

Also consider a request rate limit at the proxy, a firewall that only
opens 443, and watching the runtime's logs for abuse. Check the result
from outside:

```sh
curl -s https://llm.example.com/v1/models
curl -s -o /dev/null -w '%{http_code}\n' https://llm.example.com/v1/chat/completions   # expect 401
curl -s https://llm.example.com/v1/chat/completions \
  -H "Authorization: Bearer $TS_UPSTREAM_KEY" -H 'content-type: application/json' \
  -d '{"model":"qwen2.5-1.5b-instruct","messages":[{"role":"user","content":"hello"}],"max_tokens":16}'
```

## 6. List it on a marketplace (Kompute example)

Most OpenAI compatible marketplaces ask for the same three things: a model
id, a base URL, and a price. On Kompute (see https://kompute.gg/docs), sign
in and open **Sell**, then fill in:

| Field | Value |
|---|---|
| Model | the `model id` that `serve-inference` printed, e.g. `qwen2.5-1.5b-instruct` |
| base_url | your public URL, e.g. `https://llm.example.com/v1` |
| $/M input | your price per million input tokens (Kompute bills output at 3x that rate) |
| Upstream key (optional) | the `TS_UPSTREAM_KEY` value; the marketplace stores it and sends it as a bearer token when it proxies calls |

Kompute health checks offers against the seller's `/models` endpoint
(`<base_url>/models`), so that URL must answer from the internet with a
list that contains your model id. If the offer never turns healthy, check
`curl https://llm.example.com/v1/models` from another network first.

Use a model id buyers will search for. If another seller already offers the
same weights, matching their id puts you in the same order book. With
signed responses (below), a buyer can tell which offers claim the exact
catalog revision you verified, and tie every answer to that claim.

### Publish reference logprobs and check yourself

A marketplace that wants to see that your endpoint really runs the
weights you listed can send rare logprob challenges: ordinary requests
whose next-token logprobs were computed ahead of time from a verified copy
of the same file (design: [plans/verified-serving.md](plans/verified-serving.md),
section 5.2). Two commands cover this today. Neither holds a wallet or
marketplace key.

Generate a reference set from your verified copy. It re-hashes the copy
like `serve-inference`, starts the runtime on a private loopback port,
asks each challenge with fixed decoding (`temperature` 0, a fixed seed,
`logprobs`, `top_logprobs` 5), and stops the runtime:

```sh
tensorseed reference-logprobs Qwen/Qwen2.5-1.5B-Instruct-GGUF \
  --file qwen2.5-1.5b-instruct-q4_k_m.gguf \
  --count 32 --out ref-q4_k_m.json --commitment ref-q4_k_m.commit.json
```

- `ref-q4_k_m.json` (`ts.reference-logprobs/1`) holds the prompts and the
  expected values. It is written readable only by you. Treat it as
  secret until the set is retired: a seller who knows the prompts can
  special-case them. Whoever runs the checks (usually the marketplace)
  should generate and hold its own set.
- `ref-q4_k_m.commit.json` (`ts.reference-logprobs-commitment/1`) is the
  public part: `set_hash`, the manifest hash, revision, file, engine, and
  decoding, with no prompts. Publishing it commits to the set; after the
  set is revealed, anyone can check the file against `set_hash` and
  regenerate the expectations with the same command.
- Each quantization is its own set. A q2_k reference is not valid for a
  q4_k_m listing.
- Prompts are generated from a random seed by a published method
  (`ts.challenge-gen/1`), or read from `--prompts file.json` (chat
  `messages` or completions `prompt`). `--gen-seed` makes the set
  reproducible.
- `--endpoint http://127.0.0.1:8090/v1` uses a server that is already
  running instead of starting one. It must be serving the same file.

Then check an endpoint against the set:

```sh
TS_CHECK_KEY=... tensorseed check-endpoint https://llm.example.com/v1 \
  --reference ref-q4_k_m.json --sample 8 --api-key-env TS_CHECK_KEY
```

It sends a sample of the challenges (`--sample`, `--concurrency`,
`--timeout`), compares the returned `top_logprobs` with the expected ones,
and prints a verdict per challenge and overall: `pass`, `fail`,
`inconclusive` (errors, timeouts, or attempts that disagree), or
`unsupported` (the endpoint returns no logprobs). Exit status is 0 for
pass, 1 for fail, and 2 for inconclusive, unsupported, or an error that
prevents a verdict. `--json` prints a `ts.check-endpoint/1` report with no
prompt text and no credentials. The API key is read from the variable you
name, never from the command line.

Defaults are starting points, not calibrated limits: a mean absolute
deviation of at most 0.5 nats and no single token off by more than 2
nats (`--max-mean-abs`, `--max-token-abs`), and a challenge fails only
when all 3 attempts (`--repeats`) fail. Even the same file on the same
llama.cpp build is not bit-identical across requests, because the prompt
cache and batching change the arithmetic. For a self-check against your
own llama.cpp server, `--no-prompt-cache` sends llama.cpp's
`cache_prompt: false`; a marketplace should leave it off so challenges
look like normal traffic.

### Sign responses

Signed responses tie every answer to a claim about the exact weights you
serve (design: [plans/verified-serving.md](plans/verified-serving.md),
sections 3, 5.1, and 5.8). They do not prove which model produced an
answer; logprob challenges do that. They make the claim attributable to
you. Tensorseed never holds your wallet key or any marketplace key.

1. Create a response key once. It is written owner-only next to your
   config file; the secret is never printed, and only the public key is
   shown:

   ```sh
   tensorseed response-key create
   tensorseed response-key show
   ```

2. Serve with the signing proxy in front of the runtime. The runtime moves
   to a private loopback port, the proxy takes `127.0.0.1:8090` (so the
   reverse proxy setup above does not change), and a signed-format claim
   `ts.served-weights/1` is written to `.tensorseed/served-weights.json`
   with the response public key, the manifest hash, repo, revision, file,
   engine and version, and endpoint:

   ```sh
   tensorseed serve-inference Qwen/Qwen2.5-1.5B-Instruct-GGUF \
     --file qwen2.5-1.5b-instruct-q4_k_m.gguf --model-id qwen2.5-1.5b-instruct \
     --public-url https://llm.example.com/v1 \
     --sign --wallet 0xYourSellerWallet
   ```

   If you run your own engine, write the claim and run the proxy
   yourself:

   ```sh
   tensorseed claim-serving Qwen/Qwen2.5-1.5B-Instruct-GGUF \
     --file qwen2.5-1.5b-instruct-q4_k_m.gguf --model-id qwen2.5-1.5b-instruct \
     --public-url https://llm.example.com/v1 --wallet 0xYourSellerWallet
   tensorseed sign-responses --upstream http://127.0.0.1:8081 \
     --claim ~/.local/share/tensorseed/downloads/Qwen/Qwen2.5-1.5B-Instruct-GGUF/.tensorseed/served-weights.json
   ```

   `--upstream` is the engine root, without `/v1`. The proxy listens on
   `127.0.0.1:8090` unless you pass `--listen`.

3. Bind the claim to your wallet (optional, but marketplaces will want
   it). Both commands print the exact message, for example:

   ```
   tensorseed served-weights/1:
   0ede3bbbbaf3d449e0ebc66470d2646ccf6033805120dcc077cf555a3d2bef70
   ```

   Sign it with EIP-191 (`personal_sign`) in your own wallet, then attach
   the signature. Tensorseed checks that it recovers to the wallet in the
   claim:

   ```sh
   tensorseed claim-serving <path to served-weights.json> --sig 0x...
   ```

   Restarting `serve-inference --sign` with the same settings keeps the
   signed claim. Changing the file, model id, endpoint, engine version,
   wallet, or key writes a new claim that needs a new signature.

What the proxy does:

- Forwards every request with its headers unchanged (your
  `Authorization` check still happens at the reverse proxy and, with
  `api_key_env`, at the runtime).
- Uses the buyer's `X-Tensorseed-Request-Id` (1 to 128 of
  `A-Z a-z 0-9 . _ ~ : -`), or mints one, and echoes it.
- Adds `X-Tensorseed-Claim-SHA256`, `-Served-Model`, `-Timestamp`,
  `-Body-SHA256`, and `-Signature` to every JSON response.
- Streams (`"stream": true`) pass through as they arrive and end with
  one extra SSE comment line, `: ts.response-sig/1 timestamp=...
  body_sha256=... signature=...`, which SSE clients ignore. The headers
  say `X-Tensorseed-Signature-Mode: sse-trailer`.
- Serves the claim at
  `https://llm.example.com/.well-known/tensorseed/served-weights.json`
  once your reverse proxy forwards that path too (the Caddy example
  above needs a `handle /.well-known/tensorseed/*` block without the key
  check; the claim holds no secrets).

Do not let the reverse proxy compress or rewrite these responses (no
Caddy `encode`, no nginx `gzip` for JSON or event streams on these
paths): the signature covers the exact bytes the engine returned.

Check a signed response, or your whole endpoint, from anywhere:

```sh
curl -s -D h.txt -o b.json https://llm.example.com/v1/chat/completions \
  -H "Authorization: Bearer $TS_UPSTREAM_KEY" -H 'content-type: application/json' \
  -H 'X-Tensorseed-Request-Id: test-1' \
  -d '{"model":"qwen2.5-1.5b-instruct","messages":[{"role":"user","content":"hello"}],"max_tokens":16}'
tensorseed verify-response --claim https://llm.example.com/v1 \
  --headers h.txt --body b.json --request-id test-1
TS_CHECK_KEY=... tensorseed check-endpoint https://llm.example.com/v1 \
  --reference ref-q4_k_m.json --api-key-env TS_CHECK_KEY --require-signatures
```

`verify-response` exits 0 when the signature, body hash, model, request
id, claim hash, and timestamp (within `--max-skew`, 300 s) all check
out, 1 when anything fails, and 2 on an error. For a stream, save it with
`curl -N` and pass the raw file as `--body`. `check-endpoint` finds the
claim at the well-known path (or takes `--claim`), sends a fresh request
id with every challenge, and fails any challenge whose answer is
unsigned or badly signed; the report lists the signature status of each
challenge and a summary.

## 7. License duties when you sell access

Serving a model commercially usually keeps obligations even when the mark
says `allowed` or `restricted`:

- Keep the license file and any NOTICE or attribution text available to
  your users (a link on your listing works for most permissive licenses).
- `restricted` licenses: follow their conditions. Llama licenses need a
  separate license from Meta above 700M monthly active users, require the
  Acceptable Use Policy to bind your users, and ask for "Built with Llama".
  Gemma and OpenRAIL licenses require you to pass their use restrictions on
  to your users, usually in your terms of service.
- `agpl-3.0`: serving over a network can oblige you to offer the
  corresponding source.
- `forbidden`: do not charge for access. `--i-have-rights` exists for the
  case where you hold a separate commercial license; it does not change the
  license.
- `unknown`: read the license before listing anything.
- Quantizing or merging weights may count as a derivative. Check `-nd`
  licenses before serving a quantization you made yourself.

Again: these notes are not legal advice.

## 8. Troubleshooting

- `refusing to serve`: run `tensorseed verify <dir>` to see which file
  failed, then pull again.
- `this copy has N GGUF files; pick one with --file`: name the
  quantization to load.
- `llama-server ... was not found`: install it, put it on `PATH`, or set
  `inference.llama_server`. `tensorseed doctor` shows what it finds.
- `check-endpoint` says `unsupported`: the runtime or proxy does not
  return `logprobs`. Such an offer cannot be logprob checked.
- `check-endpoint` says `inconclusive` with HTTP 503: the runtime is still
  loading the model; wait for `/v1/models` and rerun.
- `did not list a model within 600s`: large models load slowly; raise
  `inference.ready_timeout_secs` or `--ready-timeout`, and read the
  runtime's own log lines above the error.
