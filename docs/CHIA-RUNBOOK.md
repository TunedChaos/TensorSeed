# Chia DataLayer runbook

This covers running the Tensorseed catalog on Chia DataLayer: what was
measured on testnet11, how to run it on mainnet, and how other people
subscribe to and mirror the catalog. It contains no keys and no private
infrastructure details. Spec background: [SPEC.md](SPEC.md) sections 6.1 to
6.7, 6.13, and 6.14 (Chia as the source of truth for the file catalog).

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
4. **Config.** In `tensorseed.toml`. The public catalog stays on the file
   backend (what clients read over HTTPS); setting `chia.catalog_store_id`
   makes every file catalog revision go to Chia first (spec 6.14). Use
   `backend = "chia"` only for a Chia only catalog.
   ```toml
   [catalog]
   backend = "file"
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
5. **Create the store.** With `backend = "chia"` for this one command
   (`TENSORSEED_CATALOG__BACKEND=chia tensorseed catalog create`), it prints
   the store id and waits for it to confirm. Put it in
   `chia.catalog_store_id`.
6. **Anchor the signed file catalog.** `tensorseed catalog anchor` writes
   every file catalog entry (signatures unchanged) plus the signed
   `ts/v1/file-head` anchor in one generation, waits for confirmation, and
   uploads `chain-proof.json` next to `head.json` on every storage target.
   Re-running it with nothing new is a no-op and costs nothing. (The older
   path, `catalog export --backend file` then `catalog import --backend
   chia`, copies the entries but writes no anchor; run `catalog anchor`
   afterwards.)
7. **Advertise a mirror.** Open TCP 8575 to the `data_layer_http` service (or
   put it behind an HTTPS reverse proxy), then
   `tensorseed catalog add-mirror --url https://<host>/ --amount 1`.
   The mirror coin locks `amount` mojos (refunded when the mirror is deleted)
   plus a fee. Do not expose the RPC port 8562.
8. **Publish the store id** next to the publisher key (for example on the
   website, and as `chia.catalog_store_id` in the shipped example config), so
   clients pin both. From then on clients check the file catalog against the
   chain by default (`catalog.chain_check` is strict once a store id is set).
9. **Later updates.** `publish`, `harvest`, and `import` compute the next file
   revision, stage the changed keys and the new anchor with
   `submit_on_chain=false`, submit one root, wait for it to confirm, and only
   then upload the file catalog and `chain-proof.json`. Each run is one
   generation and one fee. If the chain write fails nothing is uploaded; if an
   upload fails after the chain confirmed, re-running finishes the uploads
   without a second transaction. If a run is interrupted after submitting, the
   next run waits for that root instead of failing. `catalog resign-head`
   (weekly) and `catalog snapshot` never touch the chain.
10. **Check.** `tensorseed catalog chain-status` prints the file revision, the
   anchored revision, the DataLayer root, the store coin, and which endpoints
   confirmed it.

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

Most clients need none of this: with `catalog.backend = "file"` and
`chia.catalog_store_id` set they check the HTTPS file catalog against the
chain without any Chia software (`chia.chain_source = "light"`, spec 6.14).
Someone who already runs a wallet and DataLayer subscribed to the store can
set `chia.chain_source = "datalayer"` to take the proof from their own node.

## 5. Start, stop, and status

```sh
export PATH="$HOME/opt/chia-venv/bin:$PATH" CHIA_ROOT=... CHIA_KEYS_ROOT=...
chia start wallet data data_layer_http
chia wallet show                    # sync status and balance
chia rpc data_layer get_root_history '{"id": "<store id>"}'
chia stop -d all                    # stops every service and the daemon
```

## 6. Mainnet publishing wallet setup (2026-11-12)

The mainnet publishing wallet runs on the publisher host with the same
2.7.5 install, in its own root and keyring so nothing is shared with the
testnet roots:

```sh
# ~/opt/chia-mainnet.env
export CHIA_ROOT="$HOME/.chia/mainnet"
export CHIA_KEYS_ROOT="$HOME/.chia_keys_mainnet"
export PATH="$HOME/opt/chia-venv/bin:$PATH"
```

Steps that were used:

1. `chia init`, then `chia keys generate --label <label>` for a new key used
   only for publishing. The mnemonic lives only in
   `$CHIA_KEYS_ROOT/keyring.yaml`; keep that directory 0700 and its files
   0600, and never print the mnemonic. Without a keyring passphrase the file
   is only protected by file permissions, so set one with
   `chia passphrase set` once unattended restarts are not needed, and keep an
   offline backup of the mnemonic.
2. Peers: on hosts whose resolver returns placeholder addresses, resolve the
   mainnet seeders listed under `dns_servers` in `config.yaml` through a
   public DNS over HTTPS resolver, check which answer on TCP 8444, and list
   about a dozen of them in `wallet.full_node_peers` (port 8444). Set
   `wallet.target_peer_count` to 5 or more so one slow peer cannot stall
   confirmations.
3. Keep every listener on localhost: `self_hostname: localhost` (the
   default) keeps the daemon (55400), wallet RPC (9256), and DataLayer RPC
   (8562) local, and `data_layer.host_ip: 127.0.0.1` keeps the
   `data_layer_http` file server (8575) local until a public mirror is set up
   behind a reverse proxy.
4. `chia start wallet data data_layer_http`; `chia wallet show` reached
   `Synced` within two minutes as a light wallet, with no full node.
5. Fund the wallet from any XCH wallet. 0.01 XCH (10,000,000,000 mojos) covers
   the store (1 mojo), a mirror coin, and thousands of updates at a
   1,000,000 mojo fee.
6. Only then run `tensorseed catalog create` against this root, then
   `tensorseed catalog anchor` (section 3, steps 5 and 6).

`DataLayerWallet not available` in the log before the first DataLayer
action is normal; the DataLayer wallet is created on first use.

## 7. Chain check (spec 6.14)

What a light client does, and what it trusts:

1. Fetch `<catalog>/chain-proof.json` from `file.base_url` or a mirror.
2. Verify offline: the `ts/v1/file-head` anchor signature, the DataLayer
   Merkle path to a root, and the store singleton puzzle hash for that root.
3. Ask each `chia.coin_api_urls` endpoint (read only, full node RPC
   compatible, default `https://api.coinset.org`) for the store coin with
   `get_coin_record_by_name`. It must exist, hash correctly, carry the
   computed puzzle hash, and be unspent (or spent less than
   `chia.chain_grace_blocks` ago).
4. Accept only a file head with the anchored revision and root.

The endpoints are trusted only for whether the coin exists and is spent. List
more than one, or your own full node, to spread that trust. For testnet
clients use a testnet endpoint (for example
`https://testnet11.api.coinset.org`).

Live check, 2026-11-12: against the testnet11 store
`3bc1ab7705c525eea55ba6f76db8baff25de0bc282a4b626c6f7b070de5c5d89`, a
`get_proof` path for `ts/v1/catalog`, produced by the reference client's
DataLayer code from a local copy of the store, folded to the confirmed
generation 1 root; the puzzle hash computed from that root and the owner
inner puzzle hash matched the store coin
`ba3d4ca3f6c5c53e564d50f96f785a7588d318e372aed5bfbeb565f676f290f0`, which a
public testnet11 coin API reported unspent (the current root). That store has
no `ts/v1/file-head` yet (it predates the anchor), and the testnet services
were not started for this run because the mainnet services on the same host
use the same ports; the full publish path was tested against a fake
DataLayer and coin API instead. Re-run the live part with
`cargo test -p tensorseed-catalog -- --ignored live_`.

## 8. Mainnet store (live since 2026-11-12)

The official catalog `tensorseed` is anchored in the Chia mainnet DataLayer
store

    3dcd43dced47a32f8e38e8ec77f583d8d1bafe1ab24e381538e50a36034b7a76

Clients pin it in `chia.catalog_store_id` next to the publisher key; the
shipped example configs and the website already do, with
`catalog.chain_check = "strict"` and `https://api.coinset.org` as the coin
endpoint.

What the bootstrap cost and how long it took:

| Step | Cost | Time |
|---|---|---|
| Funding deposit, sent with fee 0 | none for us | about 30 minutes to confirm with a nearly full mempool |
| `catalog create` (store launcher) | 1 mojo plus a 1,000,000 mojo fee | about 1 minute |
| `catalog anchor` (file revision 3 plus `ts/v1/file-head`, one generation) | 1,000,000 mojo fee | about 1.5 minutes |
| `catalog resign-head` (weekly) | nothing; no chain transaction, no DataLayer or coin endpoint needed | seconds |

After `catalog anchor`, `chain-proof.json` was written to every storage
target (local publish directory and the object mirror) and has to be
deployed with the rest of the catalog tree; on the static host it uses the
same short cache policy as `head.json`.

Checks run from a clean client state directory against the public catalog:
`catalog chain-status` reported the file head and the chain anchor at the
same revision and root with the store coin current, and `catalog info` and
`list` passed in strict mode. In local tests a strict client refused a
validly signed head from a different key that the chain does not anchor, a
`chain-proof.json` with one altered Merkle layer, and a proof for a store
other than the pinned one; the same rogue head was accepted only with
`chain_check = "off"`.

Weekly routine with the chain check on: `catalog resign-head`, copy the
publish tree (including `chain-proof.json`) into the static site, deploy, and
run `tensorseed catalog chain-status` with a clean strict client config as
the final check. A new revision (`publish`, `harvest`, `import`) still goes
to Chia first and needs the publishing wallet and DataLayer running.
