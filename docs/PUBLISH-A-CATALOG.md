# Publish your own Tensorseed catalog

Anyone can run an independent Tensorseed catalog: your own publisher key,
your own Chia DataLayer store, and your own signed catalog files on any
static host or S3 compatible bucket. Followers get the same chain check
that clients of the official catalog get. This is how Tensorseed
decentralizes. Seeding what you already pulled (`docs/RUN-A-MIRROR.md`)
needs no catalog of your own; this guide is only for people who want to
*publish* revisions.

Nobody but the official publisher can write to the official catalog at
tensorseed.dev. Every official entry has to be signed with that catalog's
publisher key, and its Chia store only accepts updates from the wallet
that owns it. Following this guide creates a separate catalog. It cannot
add, change, or take down anything in the official one.

To ask for a repo to be added to the official catalog instead, open a
"Request a catalog mirror" issue on the project repository. Gated and
restricted-license repos are refused.

The software only publishes Hugging Face models and datasets. There is no
path for other content.

## Trust model

Chia proves integrity and latest state *of that publisher's catalog*. It
does not prove the publisher is honest. A third party catalog is exactly
what that publisher signed, and it is their latest version, but it is
still their word about what those files are.

Trust in the files comes from each entry recording the Hugging Face
commit (`hf_commit`) and the per-file sizes and SHA-256 hashes. Anyone
can check those against the Hub while the repo is still up
(`tensorseed verify --hf org/name`). When several independent catalogs
list the same hashes for the same commit, that agreement is further
evidence; clients will surface it in 0.2.0 (see SPEC section 6.15).

Torrents are built the same way every time. Two publishers who mirror the
same Hugging Face commit produce the same info hash and share one swarm,
so independent catalogs strengthen each other's seeding instead of
splitting it.

Followers still pin *your* publisher key, catalog name, store id, and
base URL. A catalog they did not pin cannot silently replace one they
did.

## What you will run

1. A minisign publisher key (`tensorseed catalog keygen`).
2. A Chia DataLayer store (`tensorseed catalog create`, then
   `catalog anchor` when you need to bootstrap or repair the chain
   proof). The Chia steps (wallet, services, fees, peers) are in
   [CHIA-RUNBOOK.md](CHIA-RUNBOOK.md).
3. `tensorseed publish` or `tensorseed harvest` for the Hugging Face
   repos you choose.
4. Hosting for the signed file catalog (head, deltas, snapshots,
   `chain-proof.json`, and the optional signed `mirrors.toml`) on any
   static host or S3 compatible target.
5. A weekly re-sign of `head.json` (`tensorseed catalog resign-head`)
   so the head does not expire. That needs no chain transaction.

A file-only catalog with no store is possible (`chia.catalog_store_id`
left empty, `chain_check` off), but followers then have no chain check.
The rest of this guide assumes you run both: Chia is the source of
truth, and the file catalog is a cache that clients check against the
chain (SPEC section 6.14).

## 1. Generate a publisher key

```sh
tensorseed catalog keygen
```

This writes a password protected minisign key pair (default name
`publisher`) under `~/.config/tensorseed/` on Unix, or
`%APPDATA%\tensorseed\` on Windows. Keep the `.key` file secret and
backed up offline. The `.pub` file is safe to publish; followers pin
that value as `catalog.publisher_pubkey`.

`catalog keygen --name my-catalog` changes the file stem. `--out-dir`
changes the directory. `--no-password` is possible and not recommended.

## 2. Write a publisher config

Use a config file that is *not* the official-catalog client config.
In 0.1.0 a process follows exactly one catalog, so publishers and
followers of a third party catalog pass `--config` (or keep this file
as their only config on a dedicated machine).

Pick a catalog `name` that is yours. The name is bound into every
signature, and using `tensorseed` will confuse people who expect the
official catalog.

```toml
[catalog]
backend = "chia"                 # only for the next step, creating the store
name = "example-catalog"
publisher_pubkey = "RW........................................"
publisher_seckey_file = "~/.config/tensorseed/publisher.key"
publisher_name = "Example Catalog"
publisher_contact = "https://example.org/contact"
publisher_policy_url = "https://example.org/policy"

[chia]
# Leave catalog_store_id empty until `catalog create` prints it.
# Point rpc_url, ssl_cert, and ssl_key at your DataLayer as in
# docs/CHIA-RUNBOOK.md.

[storage]
# Local published trees (catalog files and, for webseeds, model files
# under t/). Add [[storage.target]] blocks to upload to any static
# host or S3 compatible bucket; see config/tensorseed.example.toml
# and SPEC section 10.1.
```

Replace the pubkey with the contents of your `.pub` file (one `RW...`
line). Put your own contact and policy URLs in. The examples above are
placeholders, not real hosts.

These publisher fields are written into the signed catalog info entry
(`ts/v1/catalog`) by `catalog create`. To change them later, edit the
config and run `tensorseed catalog update-info --dry-run` to compare the
current and proposed values, then `tensorseed catalog update-info` to
sign and publish them as a new revision (file backend; anchor it on
Chia as usual afterwards).

After the store exists (section 3), switch the same file to the
recommended operating setup:

```toml
[catalog]
backend = "file"
name = "example-catalog"
publisher_pubkey = "RW........................................"
publisher_seckey_file = "~/.config/tensorseed/publisher.key"
chain_check = "strict"

[file]
base_url = "https://catalog.example.org/"
head_ttl_secs = 604800            # 7 days; re-sign before this lapses

[chia]
catalog_store_id = "<64 hex characters from catalog create>"
```

`base_url` is wherever you will serve the signed files, with a trailing
slash. Publishing with `backend = "file"` and a store id set writes
Chia first and uploads the file catalog, including `chain-proof.json`,
only after the generation confirms (SPEC section 6.14).

## 3. Create the store and the file catalog

Set up a Chia wallet and the DataLayer services first:
[CHIA-RUNBOOK.md](CHIA-RUNBOOK.md). Then, with `catalog.backend =
"chia"` and an empty `catalog_store_id`:

```sh
tensorseed --config /path/to/publisher.toml catalog create
```

The command prints the new store id and waits until the store confirms
on chain (pass `--no-wait` only if you will wait yourself). Put that
id in `chia.catalog_store_id`, set `catalog.backend = "file"`, and
create the file catalog (revision 1, the catalog info entry):

```sh
tensorseed --config /path/to/publisher.toml catalog create
```

That write is also anchored on Chia, and `chain-proof.json` is uploaded
with the file catalog. If the store and the file catalog ever drift,
`tensorseed --config /path/to/publisher.toml catalog anchor` rebuilds
the `ts/v1/file-head` anchor and uploads a fresh `chain-proof.json`.

## 4. Publish Hugging Face repos

```sh
tensorseed --config /path/to/publisher.toml harvest --dry-run
tensorseed --config /path/to/publisher.toml harvest
# or one repo:
tensorseed --config /path/to/publisher.toml publish org/name
```

`[harvest]` in the same config is the watchlist (SPEC section 13).
Policy still applies: open licenses by default, no gated repos unless
you pass `--allow-gated` *and* the license allows it, pickle files
skipped unless `--allow-pickle`. `--allow-license` is a per-repo
exception for a license you have the rights to, not a way to loosen
the catalog for everything else.

Each published revision records `hf_commit` and the file hashes, builds
a deterministic torrent, and (if you configured storage targets)
uploads catalog files and optional webseed bytes to those targets.

## 5. Host the signed file catalog

The file layout under `file.base_url/<catalog name>/` is in SPEC
section 6.8: `head.json`, append-only deltas and snapshots, their
minisign signatures, and `chain-proof.json`. Any static host, object
bucket, or `tensorseed serve --http --serve-catalog` can serve it.
Mirrors are untrusted: clients check signatures, the hash chain, and
the chain proof before accepting a head.

If you want other HTTP or S3 locations advertised to your followers,
sign a list:

```sh
tensorseed --config /path/to/publisher.toml mirror add https://mirror.example.org/ --name eu-1 --priority 10
tensorseed --config /path/to/publisher.toml mirror publish
```

That writes `<catalog>/mirrors.toml` and `mirrors.toml.minisig` (SPEC
section 6.12). Chia DataLayer peers that serve your catalog `.dat`
files can also be advertised with `tensorseed catalog add-mirror`;
that is a chain transaction, described in the Chia runbook.

## 6. Re-sign the head every week

`head.json` carries an expiry (`file.head_ttl_secs`, default 7 days)
so a frozen HTTPS host cannot serve an old but validly signed head
forever. Re-sign before it lapses:

```sh
tensorseed --config /path/to/publisher.toml catalog resign-head
```

This only refreshes `expires_unix` and uploads `head.json`. It does
not change the revision or touch the chain. A cron job or systemd
timer once or twice a week is enough. `catalog snapshot` is the
related command that writes a full snapshot and points the head at it;
it also needs no chain transaction.

## 7. How someone follows your catalog

In 0.1.0 a client follows one catalog per config file. They copy a
separate config, pin your values, and pass `--config`:

```toml
[catalog]
backend = "file"
name = "example-catalog"
publisher_pubkey = "RW........................................"
chain_check = "strict"

[file]
base_url = "https://catalog.example.org/"

[chia]
catalog_store_id = "<your 64 hex store id>"
```

```sh
tensorseed --config /path/to/example-catalog.toml catalog chain-status
tensorseed --config /path/to/example-catalog.toml list
tensorseed --config /path/to/example-catalog.toml pull org/name
```

Give followers those four values (name, pubkey, store id, base URL)
and this snippet, not your secret key and not your Chia wallet. They
can also seed what they pull, the same way they would with the
official catalog ([RUN-A-MIRROR.md](RUN-A-MIRROR.md)).

Following several catalogs in one process, showing when catalogs agree
on a revision's hashes, and an explorer "use another catalog" form are
planned for 0.2.0 (SPEC section 6.15).

## See also

- [CHIA-RUNBOOK.md](CHIA-RUNBOOK.md) for the Chia wallet, DataLayer
  services, fees, and subscriber mirrors
- [RUN-A-MIRROR.md](RUN-A-MIRROR.md) for seeding without publishing
- [SPEC.md](SPEC.md) sections 6.8, 6.12, 6.14, 6.15, 8.1, and 10.1
- `config/tensorseed.example.toml` for storage targets, harvest, and
  file backend settings
