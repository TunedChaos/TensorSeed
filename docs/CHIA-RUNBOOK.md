# Chia DataLayer runbook

This covers running the Tensorseed catalog on Chia DataLayer: what was
measured on testnet11, how to run it on mainnet, and how other people
subscribe to and mirror the catalog. It contains no keys and no private
infrastructure details. Spec background: [SPEC.md](SPEC.md) sections 6.1 to
6.7 and 6.13.

## 1. What the testnet11 run showed (2026-11-12)

Client: official `chia-blockchain` 2.7.5 (Apache-2.0), installed with pip in
its own virtualenv. Services: `wallet`, `data_layer`, and `data_layer_http`.
No full node.

| Item | Observed |
|---|---|
| Wallet sync without a full node | Light (untrusted) wallet sync from public testnet peers; synced in about one minute. |
| Disk | About 45 MB per Chia root (wallet DB, DataLayer DB, logs), plus about 200 MB for the client install. |
| Memory | About 450 MB RSS for daemon, wallet (with its worker processes), and DataLayer together. |
| Store creation cost | 1 mojo locked in the store singleton, plus the fee. With fee 0 it confirmed in 6 to 9 minutes; with a 1,000,000 mojo fee in about 90 seconds. |
| Update (one generation) cost | Fee only. With fee 0 an update took about 3 minutes and one peer rejected it as `INVALID_FEE_TOO_CLOSE_TO_ZERO` first; with a 1,000,000 mojo fee updates landed on chain in about 1 to 2 minutes. |
| Mirror coin (`add_mirror`) | `amount` (1 mojo used) plus the fee; confirmed in about 1 minute and a separate subscriber saw it through `get_mirrors`. |
| Total testnet spend | 5,000,004 mojos for 3 stores, 4 updates with fees, and 1 mirror (earlier fee 0 updates cost nothing). |
| Catalog size on DataLayer | The live catalog (4 signed entries, 1 model) is a 7.2 KB `.dat` file per generation. |
| Subscriber sync | A second, separate wallet and DataLayer subscribed with an explicit mirror URL and synced generation 1 in about one minute once the owner's `data_layer_http` was running. |
| Signatures | Entries exported from the Chia store were byte for byte identical to the signed file catalog and verified against the publisher key; `verify` passed 9 of 9 files against the manifest read from Chia. |

Things the client needs that are easy to miss:

- With only one or two reachable peers, the light wallet sometimes missed
  that its own DataLayer update had confirmed (the chain had it within a
  minute; the wallet still showed it pending, and one resend came back as
  `DOUBLE_SPEND` because the first copy was already in a block). Restarting
  the wallet (`chia start -r wallet`) refreshed it at once. On mainnet keep
  several peers, or point the wallet at your own full node.
- Do not run `clear_pending_roots` on a root that may already be on chain.
  On testnet that discarded the only local copy of a confirmed generation,
  and the store could not be updated again (it was a throwaway store). Check
  `get_root_history` after a wallet restart first.
- `chia start data_layer_http` is a separate service. Without it the owner
  has nothing to serve on port 8575 and subscribers stay at generation 0.
- A new store must confirm on chain before the first write. Until then
  `batch_update` fails with "Already have a pending root waiting for
  confirmation." `tensorseed catalog create` now waits for that.
- On networks that answer DNS with placeholder addresses (some sandboxes and
  proxies), the DNS introducer returns nothing usable. List real peers in
  `wallet.full_node_peers` in `config.yaml` (resolve
  `dns-introducer-<network>.chia.net` with a normal resolver) and the wallet
  syncs from them.

## 2. Install (any Linux host, no full node)

```sh
python3 -m venv ~/opt/chia-venv          # or: uv venv ~/opt/chia-venv --python 3.12
~/opt/chia-venv/bin/pip install chia-blockchain
export PATH="$HOME/opt/chia-venv/bin:$PATH"
```

Keep each network in its own root so testnet and mainnet never mix:

```sh
# testnet11
export CHIA_ROOT="$HOME/.chia/testnet11" CHIA_KEYS_ROOT="$HOME/.chia_keys_testnet11"
chia init && chia configure --testnet true
# mainnet
export CHIA_ROOT="$HOME/.chia/mainnet" CHIA_KEYS_ROOT="$HOME/.chia_keys"
chia init
```

`CHIA_KEYS_ROOT` holds `keyring.yaml`, the file keyring with the wallet
mnemonic. Set a keyring passphrase on mainnet (`chia passphrase set`) and
back the mnemonic up offline. Never print it into logs or chat.

## 3. Mainnet publisher runbook

1. **Wallet.** `chia keys generate` (or import an existing key in the
   official GUI). Any wallet can send XCH to it, but the key that owns the store
   must be in the reference client's keyring, because the `data_layer`
   service signs through that wallet's RPC.
2. **Start services.** `chia start wallet data data_layer_http`. Wait for
   `chia wallet show` to say `Synced`.
3. **Fund.** A few hundred thousand mojos covers store creation and many
   updates at low fees. Fees are a choice: fee 0 works when blocks are not
   full but can wait. A small fee such as 1,000,000 mojos (0.000001 XCH)
   per update is a sensible default; the config default of 0.001 XCH used by
   the stock CLI when no fee is passed is far more than needed.
4. **Config.** In `tensorseed.toml`:
   ```toml
   [catalog]
   backend = "chia"
   name = "tensorseed"
   publisher_pubkey = "<the published publisher key>"

   [chia]
   rpc_url = "https://localhost:8562"
   ssl_cert = "~/.chia/mainnet/config/ssl/data_layer/private_data_layer.crt"
   ssl_key  = "~/.chia/mainnet/config/ssl/data_layer/private_data_layer.key"
   ca_cert  = "~/.chia/mainnet/config/ssl/ca/private_ca.crt"
   fee_mojos = 1000000
   catalog_mirror_urls = ["http://<public host>:8575"]
   ```
5. **Create the store.** `tensorseed catalog create` prints the store id and
   waits for it to confirm. Put it in `chia.catalog_store_id`.
6. **Copy the signed file catalog in.** On a machine with the file catalog:
   `tensorseed catalog export --backend file --out catalog.json`, then
   `tensorseed catalog import catalog.json` against the Chia config. Entries
   keep their signatures; the import is one generation. Re-running an import
   with nothing new is a no-op and costs nothing.
7. **Advertise a mirror.** Open TCP 8575 to the `data_layer_http` service (or
   put it behind an HTTPS reverse proxy), then
   `tensorseed catalog add-mirror --url https://<host>/ --amount 1`.
   The mirror coin locks `amount` mojos (refunded when the mirror is deleted)
   plus a fee. Do not expose the RPC port 8562.
8. **Publish the store id** next to the publisher key (for example on the
   website), so clients can pin both.
9. **Later updates.** `publish`, `harvest`, and `import` stage every changed
   key with `submit_on_chain=false` and then submit one root, so each run is
   one generation and one fee. If a run is interrupted after submitting, the
   next run waits for that root instead of failing.

## 4. Subscribers and volunteer mirrors

```sh
chia start wallet data              # add data_layer_http to re-serve it
tensorseed catalog subscribe        # uses chia.catalog_mirror_urls, or --url
tensorseed catalog info             # generation N of N when synced
tensorseed list && tensorseed show <org/name>
```

Subscribing needs no XCH. Mirrors advertised on chain are found
automatically; `--url` adds more. A volunteer who wants to re-serve the
catalog starts `data_layer_http`, opens port 8575, and may call
`catalog add-mirror` (needs a little XCH for the mirror coin and fee).

Every read verifies each entry against the pinned publisher key, and every
read through the Chia backend also checks the store's on-chain history
against a local anchor (`<file.state_dir>/<catalog>/chia-<store>.json`). A
node that reports a lower generation (rollback) or a history without the
anchored root (fork) is refused.

## 5. Start, stop, and status

```sh
export PATH="$HOME/opt/chia-venv/bin:$PATH" CHIA_ROOT=... CHIA_KEYS_ROOT=...
chia start wallet data data_layer_http
chia wallet show                    # sync status and balance
chia rpc data_layer get_root_history '{"id": "<store id>"}'
chia stop -d all                    # stops every service and the daemon
```
